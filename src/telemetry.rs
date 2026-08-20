//! The collector: probe each property over HTTPS from here, gather host/docker
//! state over SSH, and merge the two into a render-ready [`Fleet`].
//!
//! Blocking by design — call it on a background thread (see `app.rs`) and hand
//! the result back over a channel so the UI never stalls.

use crate::dates;
use crate::model::*;
use crate::registry::PropertyDef;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The gather script, embedded so the binary is self-contained.
const GATHER_SH: &str = include_str!("../scripts/gather.sh");

/// TLS warning threshold — a cert expiring within this many days is a gap.
const TLS_WARN_DAYS: i64 = 21;

/// Collect the whole fleet. `host_alias` is an entry in the operator's
/// `~/.ssh/config` (e.g. `tecnocratica_node_1`).
pub fn collect(host_alias: &str, registry: &[PropertyDef]) -> Result<Fleet, String> {
    let gather = ssh_gather(host_alias)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("lighthouse/0.1")
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut rows = Vec::new();
    let mut props_up = 0;
    for def in registry {
        let http = http_probe(&client, &def.url, &def.probe_path);
        let container = gather
            .containers
            .iter()
            .find(|c| c.name == def.container)
            .cloned();
        let drupal = gather
            .drupal
            .iter()
            .find(|d| d.name == def.container)
            .cloned();
        let tls_not_after = gather
            .tls
            .iter()
            .find(|t| t.domain == def.tls_domain)
            .map(|t| t.not_after.clone())
            .filter(|s| !s.is_empty());

        let health = derive_health(&http, &container, &drupal);
        if health == Health::Up {
            props_up += 1;
        }

        rows.push(Row {
            slug: def.slug.clone(),
            name: def.name.clone(),
            stack: def.stack.clone(),
            url: def.url.clone(),
            probe_path: def.probe_path.clone(),
            domains: def.domains.clone(),
            http,
            container,
            drupal,
            tls_not_after,
            health,
        });
    }

    let running = gather
        .containers
        .iter()
        .filter(|c| c.status == "running")
        .count();
    let gaps = derive_gaps(&gather, &rows);

    Ok(Fleet {
        generated: gather.generated.clone(),
        props_up,
        props_total: rows.len(),
        containers_running: running,
        containers_total: gather.containers.len(),
        host: Some(gather.host),
        gaps,
        rows,
    })
}

/// Pipe the embedded gather script to `ssh <alias> bash -s` and parse its JSON.
fn ssh_gather(host_alias: &str) -> Result<Gather, String> {
    // Windows-side source is CRLF; bash chokes on the stray \r, so strip it.
    let script = GATHER_SH.replace('\r', "");

    let mut child = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=15")
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

    let out = child
        .wait_with_output()
        .map_err(|e| format!("ssh wait: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "ssh to '{host_alias}' failed ({}): {}",
            out.status,
            err.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Gather>(&stdout)
        .map_err(|e| format!("parse gather json: {e}"))
}

/// GET the probe URL, timing the round trip. `ok` is true on a 200.
fn http_probe(client: &reqwest::blocking::Client, base: &str, path: &str) -> Option<HttpProbe> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let start = Instant::now();
    match client.get(&url).send() {
        Ok(resp) => {
            let code = resp.status().as_u16();
            Some(HttpProbe {
                code,
                latency_ms: start.elapsed().as_millis(),
                ok: code == 200,
            })
        }
        Err(_) => None,
    }
}

fn derive_health(
    http: &Option<HttpProbe>,
    container: &Option<Container>,
    drupal: &Option<DrupalNode>,
) -> Health {
    // No probe result at all → we can't say.
    let Some(h) = http else {
        return if container.is_some() {
            Health::Warn // running but unreachable over HTTPS
        } else {
            Health::Unknown
        };
    };

    if !h.ok {
        return Health::Down;
    }
    if let Some(c) = container {
        if c.status != "running" {
            return Health::Down;
        }
    }
    if let Some(d) = drupal {
        let maint = d.maintenance.as_str();
        if maint != "0" && !maint.is_empty() {
            return Health::Warn; // maintenance mode on
        }
        if d.db != "Connected" {
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
                label: r.slug.clone(),
                text: match &r.http {
                    Some(h) => format!("{} → HTTP {} at {}", r.url, h.code, r.probe_path),
                    None => format!("{} unreachable over HTTPS", r.url),
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

    // Drupal images without a Docker HEALTHCHECK — telemetry falls back to HTTP.
    let no_hc: Vec<&str> = g
        .containers
        .iter()
        .filter(|c| c.name.ends_with("-drupal-1") && c.health == "none")
        .map(|c| c.name.as_str())
        .collect();
    if !no_hc.is_empty() {
        gaps.push(Gap {
            sev: Sev::Warn,
            label: "images".into(),
            text: format!(
                "{} Drupal container(s) ship no Docker HEALTHCHECK",
                no_hc.len()
            ),
        });
    }

    // Nearest TLS expiry within the warning window.
    let mut nearest: Option<(i64, String)> = None;
    for r in rows {
        if let Some(na) = &r.tls_not_after {
            if let Some(days) = dates::days_until(na) {
                if nearest.as_ref().map_or(true, |(d, _)| days < *d) {
                    nearest = Some((days, r.name.clone()));
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
