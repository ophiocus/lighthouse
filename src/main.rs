// Windowed (no console) only in release. Debug builds keep a console so
// `--probe`, panics, and logging are visible under `cargo run`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod analytics;
mod app;
mod config;
mod dates;
mod git_update;
mod model;
mod telemetry;
mod template;

use eframe::egui;

// These constants are the single source of truth for app identity.
// The bootstrap script (scripts/new_app.ps1) rewrites them for a new app.
pub const APP_NAME: &str = "Lighthouse";
pub const APP_WINDOW_TITLE: &str = "Lighthouse";
// GitHub repo in "owner/repo" form — used by the update checker.
pub const APP_GH_REPO: &str = "ophiocus/lighthouse";

fn main() -> eframe::Result<()> {
    // Headless one-shot: gather and print, no GUI. Handy for cron/CI checks.
    if std::env::args().any(|a| a == "--probe") {
        run_probe();
        return Ok(());
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title(APP_WINDOW_TITLE),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(|cc| Ok(Box::new(app::LighthouseApp::new(cc)))),
    )
}

/// One-line rendering of a row's GA4 state for the headless probe.
fn describe_analytics(s: &model::AnalyticsState) -> String {
    use model::AnalyticsState as A;
    match s {
        A::Disabled => "not configured on this workstation".into(),
        A::NoProperty => "no GA4 property matches this site".into(),
        A::AuthExpired => "AUTH EXPIRED — re-run infra/scripts/mint_adc.py".into(),
        A::Error(e) => format!("error: {e}"),
        A::Ok(a) => format!(
            "{} ({} {})  {}u/{}s last {}d · {}u/{}s last {}d",
            a.measurement_id,
            a.display_name,
            a.property_id.trim_start_matches("properties/"),
            a.users_recent,
            a.sessions_recent,
            a.recent_days,
            a.users_year,
            a.sessions_year,
            a.year_days,
        ),
    }
}

fn run_probe() {
    let cfg = config::Config::load();
    println!("lighthouse probe → {}", cfg.host_alias);
    match telemetry::collect(&cfg.host_alias) {
        Ok(f) => {
            println!(
                "{} projects ({} Drupal, {} Node)  healthy {}/{}  gaps {}  (snapshot {})",
                f.total, f.drupal_count, f.node_count, f.healthy, f.total, f.gaps.len(), f.generated
            );
            for r in &f.rows {
                let code = r.http.as_ref().map(|h| h.code.to_string()).unwrap_or_else(|| "---".into());
                let lat = r.http.as_ref().map(|h| format!("{}ms", h.latency_ms)).unwrap_or_else(|| "-".into());
                println!(
                    "  {:16} {:7} {:8} http={:>3} {:>7}  core={:8} db={:10} tls={}",
                    r.p.slug,
                    r.ptype.label(),
                    r.health.label(),
                    code,
                    lat,
                    r.p.core.as_deref().unwrap_or("-"),
                    r.p.db_status.as_deref().unwrap_or("-"),
                    if r.p.tls.is_empty() { "-" } else { &r.p.tls },
                );
                let emitted = r.http.as_ref().and_then(|h| h.emitted_tag.as_deref());
                let ads = r.http.as_ref().and_then(|h| h.emitted_adsense.as_deref());
                println!(
                    "                   emits={:14} verdict={:16} {}{}",
                    emitted.unwrap_or("-"),
                    model::derive_measurement(emitted, &r.analytics).label(),
                    describe_analytics(&r.analytics),
                    ads.map(|a| format!("  ads={a}")).unwrap_or_default(),
                );
            }
            if let Some(h) = &f.host {
                println!(
                    "  host: disk {} ({}%)  mem {}%  load {}  upg {}  sec {}  reboot {}",
                    h.disk, h.disk_pct, h.mem_pct, h.load, h.upgradable, h.security, h.reboot
                );
            }
            for g in &f.gaps {
                println!("  GAP [{:?}] {}: {}", g.sev, g.label, g.text);
            }
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}
