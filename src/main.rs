// Windowed (no console) only in release. Debug builds keep a console so
// `--probe`, panics, and logging are visible under `cargo run`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod dates;
mod git_update;
mod model;
mod registry;
mod telemetry;

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

fn run_probe() {
    let cfg = config::Config::load();
    println!("lighthouse probe → {}", cfg.host_alias);
    match telemetry::collect(&cfg.host_alias, &registry::default_registry()) {
        Ok(f) => {
            println!(
                "props {}/{}  containers {}/{}  gaps {}  (snapshot {})",
                f.props_up, f.props_total, f.containers_running, f.containers_total,
                f.gaps.len(), f.generated
            );
            for r in &f.rows {
                let code = r.http.as_ref().map(|h| h.code.to_string()).unwrap_or_else(|| "---".into());
                let lat = r.http.as_ref().map(|h| format!("{}ms", h.latency_ms)).unwrap_or_else(|| "-".into());
                println!(
                    "  {:16} {:8} http={:>3} {:>7}  core={:8} db={:10} tls={}",
                    r.name,
                    r.health.label(),
                    code,
                    lat,
                    r.drupal.as_ref().map(|d| d.core.as_str()).unwrap_or("-"),
                    r.drupal.as_ref().map(|d| d.db.as_str()).unwrap_or("-"),
                    r.tls_not_after.as_deref().unwrap_or("-"),
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
