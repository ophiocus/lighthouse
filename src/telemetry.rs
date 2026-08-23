//! The collector: discover projects + gather host/docker telemetry over SSH,
//! probe each project over HTTPS from here, and merge into a [`Fleet`].
//!
//! Blocking by design — call it on a background thread (see `app.rs`).

use crate::dates;
use crate::model::*;
use std::io::Write;
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
        rows.push(Row { p: p.clone(), ptype, http, health });
    }

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

fn http_probe(client: &reqwest::blocking::Client, base: &str, path: &str) -> Option<HttpProbe> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let start = Instant::now();
    match client.get(&url).send() {
        Ok(resp) => {
            let code = resp.status().as_u16();
            Some(HttpProbe { code, latency_ms: start.elapsed().as_millis(), ok: code == 200 })
        }
        Err(_) => None,
    }
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
