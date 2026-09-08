//! The collector: discover projects + gather host/docker telemetry over SSH,
//! probe each project over HTTPS from here, and merge into a [`Fleet`].
//!
//! Blocking by design — call it on a background thread (see `app.rs`).

use crate::analytics;
use crate::dates;
use crate::model::*;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GATHER_SH: &str = include_str!("../scripts/gather.sh");
const TLS_WARN_DAYS: i64 = 21;

/// Discover and collect the whole fleet from `host_alias` (an ~/.ssh/config entry).
pub fn collect(host_alias: &str) -> Result<Fleet, String> {
    let gather = ssh_gather(host_alias)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("lighthouse/0.1")
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut rows = Vec::new();
    let (mut healthy, mut drupal_count, mut node_count) = (0, 0, 0);

    for p in &gather.projects {
        let ptype = ProjectType::parse(&p.kind);
        match ptype {
            ProjectType::Drupal => drupal_count += 1,
            ProjectType::Node => node_count += 1,
            ProjectType::Unknown => {}
        }
        let http = if p.url.is_empty() {
            None
        } else {
            http_probe(&client, &p.url, &p.probe_path)
        };
        let health = derive_health(&http, p);
        if health == Health::Up {
            healthy += 1;
        }
        rows.push(Row {
            p: p.clone(),
            ptype,
            http,
            health,
            analytics: AnalyticsState::Loading,
        });
    }

    // NOTE: analytics is deliberately NOT collected here. It costs a dozen
    // sequential round trips to Google, and folding it into this function meant
    // the board showed a spinner for the sum of both — the health picture was
    // ready and withheld. `attach_analytics` runs as its own pass so the board
    // paints first and the analytics cells fill in behind it.
    let gaps = derive_gaps(&gather, &rows);
    Ok(Fleet {
        generated: gather.generated.clone(),
        total: rows.len(),
        healthy,
        drupal_count,
        node_count,
        host: Some(gather.host),
        gaps,
        rows,
    })
}

/// Fill in each row's GA4 state.
///
/// Deliberately infallible: analytics is a separate lane from health, so every
/// failure collapses into a per-row [`AnalyticsState`] instead of failing the
/// collection. A dead credential or a network blip must never blank the board.
///
/// Properties are matched to projects by the data stream's own configured URL,
/// so a newly-provisioned property is picked up with nothing to hand-maintain.
pub fn attach_analytics(rows: &mut [Row]) {
    fn set_all(rows: &mut [Row], st: AnalyticsState) {
        for r in rows.iter_mut() {
            r.analytics = st.clone();
        }
    }

    // Rows arrive as `Loading`. Every exit below must land them somewhere else,
    // or a workstation with no credential shows "checking…" forever.
    let Some(path) = analytics::default_credential_path() else {
        return set_all(rows, AnalyticsState::Disabled);
    };
    let Ok(adc) = analytics::load_adc(&path) else {
        return set_all(rows, AnalyticsState::Disabled);
    };
    let Ok(client) = analytics::client() else {
        return set_all(rows, AnalyticsState::Disabled);
    };

    let token = match analytics::access_token(&client, &adc) {
        Ok(t) => t,
        Err(analytics::Error::AuthExpired) => {
            for r in rows.iter_mut() {
                r.analytics = AnalyticsState::AuthExpired;
            }
            return;
        }
        Err(e) => {
            for r in rows.iter_mut() {
                r.analytics = AnalyticsState::Error(e.to_string());
            }
            return;
        }
    };

    let props = match analytics::discover(&client, &token) {
        Ok(p) => p,
        Err(e) => {
            for r in rows.iter_mut() {
                r.analytics = AnalyticsState::Error(e.to_string());
            }
            return;
        }
    };

    for r in rows.iter_mut() {
        let apex = r.p.apex.trim_start_matches("www.").to_ascii_lowercase();
        let matched = props
            .iter()
            .find(|gp| {
                !gp.host.is_empty()
                    && (gp.host == apex
                        || r.p.domains.iter().any(|d| {
                            d.trim_start_matches("www.").eq_ignore_ascii_case(&gp.host)
                        }))
            })
            .cloned();

        r.analytics = match matched {
            None => AnalyticsState::NoProperty,
            Some(gp) => match analytics::fetch(&client, &token, &gp) {
                Ok(a) => AnalyticsState::Ok(a),
                Err(analytics::Error::AuthExpired) => AnalyticsState::AuthExpired,
                Err(e) => AnalyticsState::Error(e.to_string()),
            },
        };
    }
}

fn recent_days(a: &AnalyticsState) -> u32 {
    match a {
        AnalyticsState::Ok(d) => d.recent_days,
        _ => 0,
    }
}

/// Measurement gaps, derived once analytics has landed.
///
/// Kept separate from [`derive_gaps`] because analytics arrives in a later pass
/// than the health picture: these are appended when the sweep completes, rather
/// than holding the whole board back until it does.
///
/// Every gap here is invisible to every other signal on the board — a BLIND or
/// DARK property serves 200s, bootstraps cleanly and holds a valid certificate.
pub fn analytics_gaps(rows: &[Row]) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for r in rows {
        let emitted = r.http.as_ref().and_then(|h| h.emitted_tag.as_deref());
        match derive_measurement(emitted, &r.analytics) {
            Measurement::Blind => gaps.push(Gap {
                sev: Sev::Crit,
                label: r.p.slug.clone(),
                text: format!(
                    "tag {} ships but nothing recorded in {} days — site is up and unmeasured",
                    emitted.unwrap_or("?"),
                    recent_days(&r.analytics),
                ),
            }),
            Measurement::Dark => gaps.push(Gap {
                sev: Sev::Crit,
                label: r.p.slug.clone(),
                text: "no analytics tag on the page, but its property has history — the tag stopped shipping".into(),
            }),
            Measurement::Unowned => gaps.push(Gap {
                sev: Sev::Crit,
                label: r.p.slug.clone(),
                text: format!(
                    "page ships {} — no property under this credential owns it",
                    emitted.unwrap_or("?"),
                ),
            }),
            Measurement::NeverRecorded => gaps.push(Gap {
                sev: Sev::Warn,
                label: r.p.slug.clone(),
                text: format!(
                    "tag {} ships but has never recorded — new, or blind since birth",
                    emitted.unwrap_or("?"),
                ),
            }),
            _ => {}
        }
    }
    gaps
}

fn ssh_gather(host_alias: &str) -> Result<Gather, String> {
    let script = GATHER_SH.replace('\r', "");
    let mut child = Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=15")
        .arg(host_alias)
        .arg("bash -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn ssh: {e} (is the OpenSSH client installed?)"))?;

    child
        .stdin
        .take()
        .ok_or("ssh stdin unavailable")?
        .write_all(script.as_bytes())
        .map_err(|e| format!("write script: {e}"))?;

    let out = child.wait_with_output().map_err(|e| format!("ssh wait: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("ssh to '{host_alias}' failed ({}): {}", out.status, err.trim()));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Gather>(&stdout).map_err(|e| format!("parse gather json: {e}"))
}

/// Read at most this much of a page when looking for analytics ids. Tags sit in
/// the head; there is no reason to pull a whole asset-heavy document.
const MAX_BODY_BYTES: u64 = 256 * 1024;

fn http_probe(client: &reqwest::blocking::Client, base: &str, path: &str) -> Option<HttpProbe> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let start = Instant::now();
    match client.get(&url).send() {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let latency_ms = start.elapsed().as_millis();

            // Latency is measured on headers, before the body read, so scraping
            // does not inflate the number the board reports.
            let mut buf = Vec::new();
            let _ = resp.take(MAX_BODY_BYTES).read_to_end(&mut buf);
            let body = String::from_utf8_lossy(&buf);
            let (emitted_tag, emitted_adsense) = scrape_ids(&body);

            Some(HttpProbe { code, latency_ms, ok: code == 200, emitted_tag, emitted_adsense })
        }
        Err(_) => None,
    }
}

/// Pull the GA4 measurement id and AdSense publisher id out of served HTML.
///
/// Deliberately a hand-rolled scan rather than a regex dependency. Both ids have
/// rigid shapes: `G-` then 6+ uppercase alphanumerics, `ca-pub-` then digits. The
/// preceding character must be a non-identifier one, so `IMG-FOO` or a word
/// ending in "g-" cannot masquerade as a measurement id.
fn scrape_ids(body: &str) -> (Option<String>, Option<String>) {
    let b = body.as_bytes();
    let mut tag = None;
    let mut adsense = None;

    for (i, w) in b.windows(2).enumerate() {
        if tag.is_none() && w == b"G-" {
            let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-');
            if boundary {
                let rest: String = b[i + 2..]
                    .iter()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                    .map(|c| *c as char)
                    .collect();
                if rest.len() >= 6 {
                    tag = Some(format!("G-{rest}"));
                }
            }
        }
        if adsense.is_none() && i + 7 <= b.len() && &b[i..i + 7] == b"ca-pub-" {
            let rest: String = b[i + 7..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .map(|c| *c as char)
                .collect();
            if rest.len() >= 10 {
                adsense = Some(format!("ca-pub-{rest}"));
            }
        }
        if tag.is_some() && adsense.is_some() {
            break;
        }
    }
    (tag, adsense)
}

fn derive_health(http: &Option<HttpProbe>, p: &Project) -> Health {
    let Some(h) = http else {
        return if p.status == "running" { Health::Warn } else { Health::Down };
    };
    if !h.ok {
        return Health::Down;
    }
    if p.status != "running" {
        return Health::Down;
    }
    if let Some(m) = &p.maintenance {
        if m != "0" && !m.is_empty() {
            return Health::Warn;
        }
    }
    if let Some(db) = &p.db_status {
        if db != "Connected" {
            return Health::Warn;
        }
    }
    Health::Up
}

fn derive_gaps(g: &Gather, rows: &[Row]) -> Vec<Gap> {
    let mut gaps = Vec::new();

    for r in rows {
        if r.health == Health::Down {
            gaps.push(Gap {
                sev: Sev::Crit,
                label: r.p.slug.clone(),
                text: match &r.http {
                    Some(h) => format!("{} → HTTP {} at {}", r.p.url, h.code, r.p.probe_path),
                    None => format!("{} unreachable over HTTPS", r.p.url),
                },
            });
        }
    }

    if g.host.security > 0 {
        gaps.push(Gap {
            sev: Sev::Crit,
            label: "host".into(),
            text: format!(
                "{} security OS update(s) pending{}",
                g.host.security,
                if g.host.reboot == "YES" { " · reboot required" } else { "" }
            ),
        });
    } else if g.host.reboot == "YES" {
        gaps.push(Gap {
            sev: Sev::Warn,
            label: "host".into(),
            text: "reboot required (kernel/library update applied)".into(),
        });
    }

    let no_hc = rows
        .iter()
        .filter(|r| r.ptype == ProjectType::Drupal && r.p.health == "none")
        .count();
    if no_hc > 0 {
        gaps.push(Gap {
            sev: Sev::Warn,
            label: "images".into(),
            text: format!("{no_hc} Drupal container(s) ship no Docker HEALTHCHECK"),
        });
    }

    let mut nearest: Option<(i64, String)> = None;
    for r in rows {
        if !r.p.tls.is_empty() {
            if let Some(days) = dates::days_until(&r.p.tls) {
                if nearest.as_ref().map_or(true, |(d, _)| days < *d) {
                    nearest = Some((days, r.p.slug.clone()));
                }
            }
        }
    }
    if let Some((days, name)) = nearest {
        if days <= TLS_WARN_DAYS {
            gaps.push(Gap {
                sev: if days <= 7 { Sev::Crit } else { Sev::Warn },
                label: "certs".into(),
                text: format!("{name} TLS cert expires in {days} day(s)"),
            });
        }
    }

    gaps
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::scrape_ids;

    #[test]
    fn finds_gtag_and_adsense_in_real_shaped_markup() {
        let html = r#"<!DOCTYPE html><html><head>
          <script async src="https://www.googletagmanager.com/gtag/js?id=G-MCC3W5SYV5"></script>
          <script async src="https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js?client=ca-pub-3538083895087441" crossorigin="anonymous"></script>
        </head><body>hi</body></html>"#;
        let (tag, ads) = scrape_ids(html);
        assert_eq!(tag.as_deref(), Some("G-MCC3W5SYV5"));
        assert_eq!(ads.as_deref(), Some("ca-pub-3538083895087441"));
    }

    #[test]
    fn absent_ids_are_none_not_empty_strings() {
        let (tag, ads) = scrape_ids("<html><body>no analytics here</body></html>");
        assert!(tag.is_none());
        assert!(ads.is_none());
    }

    /// The boundary check is what stops ordinary markup from being read as a
    /// measurement id. Without it an id-shaped tail inside another token would
    /// make a dark property look measured — a false green, the worst outcome.
    #[test]
    fn does_not_match_an_id_shaped_tail_inside_another_token() {
        let (tag, _) = scrape_ids(r#"<img class="XG-ABCDEF12" src="a.png">"#);
        assert!(tag.is_none(), "matched inside a larger token");
        let (tag, _) = scrape_ids(r#"<div data-x="SVG-ABCDEF12"></div>"#);
        assert!(tag.is_none(), "matched after a letter");
    }

    /// Too short to be a measurement id — must not half-match.
    #[test]
    fn rejects_short_candidates() {
        let (tag, _) = scrape_ids(r#"<p>G-AB1</p>"#);
        assert!(tag.is_none());
    }

    /// A parking page that happens to serve 200 emits nothing; that is the
    /// oidoenvivo.club case the apex fix addressed, and it must read as "no tag"
    /// rather than as a scrape failure.
    #[test]
    fn parking_page_emits_nothing() {
        let parked = r#"<!DOCTYPE html><html><head><script>window.onload=function(){window.location.href="/lander"}</script></head></html>"#;
        let (tag, ads) = scrape_ids(parked);
        assert!(tag.is_none());
        assert!(ads.is_none());
    }
}
