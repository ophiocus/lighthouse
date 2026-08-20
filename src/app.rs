use crate::config::Config;
use crate::git_update::{UpdateAvailable, UpdateState};
use crate::model::{Fleet, Gap, Health, Row, Sev};
use crate::registry::{default_registry, PropertyDef};
use crate::{dates, telemetry};
use eframe::egui;
use egui::{Align, Color32, Frame, Layout, Margin, RichText, Rounding, Stroke};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Theme-derived colors, recomputed each frame from `visuals().dark_mode`.
struct Palette {
    card: Color32,
    border: Color32,
    mute: Color32,
    good: Color32,
    warn: Color32,
    crit: Color32,
    accent: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                card: Color32::from_rgb(0x1b, 0x21, 0x2b),
                border: Color32::from_rgb(0x2a, 0x32, 0x3f),
                mute: Color32::from_rgb(0x8a, 0x94, 0xa6),
                good: Color32::from_rgb(0x46, 0xc9, 0x8a),
                warn: Color32::from_rgb(0xd8, 0xa5, 0x3a),
                crit: Color32::from_rgb(0xf2, 0x68, 0x5c),
                accent: Color32::from_rgb(0x3c, 0xc6, 0xdc),
            }
        } else {
            Self {
                card: Color32::from_rgb(0xff, 0xff, 0xff),
                border: Color32::from_rgb(0xdd, 0xe1, 0xe8),
                mute: Color32::from_rgb(0x55, 0x60, 0x72),
                good: Color32::from_rgb(0x12, 0x8a, 0x5a),
                warn: Color32::from_rgb(0xa9, 0x72, 0x0a),
                crit: Color32::from_rgb(0xc0, 0x36, 0x2c),
                accent: Color32::from_rgb(0x0b, 0x7a, 0x8c),
            }
        }
    }

    fn health(&self, h: Health) -> Color32 {
        match h {
            Health::Up => self.good,
            Health::Warn => self.warn,
            Health::Down => self.crit,
            Health::Unknown => self.mute,
        }
    }
}

pub struct LighthouseApp {
    pub config: Config,
    registry: Vec<PropertyDef>,

    fleet: Option<Fleet>,
    error: Option<String>,
    loading: bool,
    last_refresh: Option<Instant>,
    fleet_rx: Option<mpsc::Receiver<Result<Fleet, String>>>,

    // Self-update plumbing (inherited from the skeleton).
    update_state: UpdateState,
    update_error: Option<String>,
    update_rx: Option<mpsc::Receiver<Option<UpdateAvailable>>>,
}

impl LighthouseApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        cc.egui_ctx.set_visuals(if config.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        cc.egui_ctx.set_zoom_factor(config.zoom);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::git_update::check_latest_release());
        });

        let mut app = Self {
            config,
            registry: default_registry(),
            fleet: None,
            error: None,
            loading: false,
            last_refresh: None,
            fleet_rx: None,
            update_state: UpdateState::Checking,
            update_error: None,
            update_rx: Some(rx),
        };
        app.start_refresh();
        app
    }

    fn start_refresh(&mut self) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let (tx, rx) = mpsc::channel();
        self.fleet_rx = Some(rx);
        let host = self.config.host_alias.clone();
        let reg = self.registry.clone();
        std::thread::spawn(move || {
            let _ = tx.send(telemetry::collect(&host, &reg));
        });
    }

    fn poll(&mut self) {
        let done = matches!(&self.fleet_rx, Some(rx) if matches!(rx.try_recv(), Ok(_) | Err(mpsc::TryRecvError::Disconnected)));
        if !done {
            return;
        }
        // Re-take to read the value (the match above only peeked reachability).
        if let Some(rx) = self.fleet_rx.take() {
            if let Ok(res) = rx.try_recv() {
                match res {
                    Ok(f) => {
                        self.fleet = Some(f);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            } else if self.fleet.is_none() {
                self.error = Some("gather thread ended without a result".into());
            }
            self.loading = false;
            self.last_refresh = Some(Instant::now());
        }
    }
}

impl eframe::App for LighthouseApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

        // Auto-refresh cadence.
        if self.config.auto_refresh_secs > 0 && !self.loading {
            let due = self
                .last_refresh
                .map_or(true, |t| t.elapsed().as_secs() >= self.config.auto_refresh_secs);
            if due {
                self.start_refresh();
            }
        }

        let pal = Palette::new(ctx.style().visuals.dark_mode);

        top_bar(self, ctx, &pal);
        bottom_bar(self, ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Some(err) = &self.error {
                    banner(ui, &pal, err);
                }
                match &self.fleet {
                    Some(fleet) => board(ui, &pal, fleet),
                    None if self.loading => {
                        ui.add_space(60.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(28.0));
                            ui.add_space(8.0);
                            ui.label(RichText::new("gathering fleet telemetry…").color(pal.mute));
                        });
                    }
                    None => {
                        ui.add_space(60.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("No data yet — press Refresh.").color(pal.mute));
                        });
                    }
                }
            });
        });

        // Keep the clock ticking and the channel polled while work is in flight.
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

// ── top / bottom bars ───────────────────────────────────────────────────────

fn top_bar(app: &mut LighthouseApp, ctx: &egui::Context, pal: &Palette) {
    egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Lighthouse").size(18.0).strong());
            ui.label(
                RichText::new(app.config.host_alias.clone())
                    .monospace()
                    .size(12.0)
                    .color(pal.mute),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("View", |ui| {
                    if ui
                        .checkbox(&mut app.config.dark_mode, "Dark mode")
                        .changed()
                    {
                        ctx.set_visuals(if app.config.dark_mode {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        app.config.save();
                    }
                    let mut auto = app.config.auto_refresh_secs > 0;
                    if ui.checkbox(&mut auto, "Auto-refresh (60s)").changed() {
                        app.config.auto_refresh_secs = if auto { 60 } else { 0 };
                        app.config.save();
                    }
                });

                if app.loading {
                    ui.add(egui::Spinner::new().size(16.0));
                } else if ui.button("⟳  Refresh").clicked() {
                    app.start_refresh();
                }

                if let Some(t) = app.last_refresh {
                    ui.label(
                        RichText::new(format!("updated {}s ago", t.elapsed().as_secs()))
                            .size(11.0)
                            .color(pal.mute),
                    );
                }

                if let Some(fleet) = &app.fleet {
                    let crit = fleet.gaps.iter().filter(|g| g.sev == Sev::Crit).count();
                    let (txt, col) = if crit > 0 {
                        (format!("{crit} critical"), pal.crit)
                    } else if !fleet.gaps.is_empty() {
                        (format!("{} gap(s)", fleet.gaps.len()), pal.warn)
                    } else {
                        ("all clear".to_string(), pal.good)
                    };
                    pill(ui, &txt, col);
                }
            });
        });
        ui.add_space(4.0);
    });
}

fn bottom_bar(app: &mut LighthouseApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            crate::git_update::render(
                ui,
                &mut app.update_state,
                &mut app.update_error,
                &mut app.update_rx,
            );
        });
    });
}

// ── board ───────────────────────────────────────────────────────────────────

fn board(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    ui.add_space(10.0);
    summary(ui, pal, fleet);
    ui.add_space(18.0);

    section(ui, pal, "PROPERTIES");
    ui.add_space(8.0);
    property_grid(ui, pal, &fleet.rows);

    ui.add_space(20.0);
    section(ui, pal, "EDGE & HOST");
    ui.add_space(8.0);
    if let Some(h) = &fleet.host {
        host_panel(ui, pal, h);
    }

    if !fleet.gaps.is_empty() {
        ui.add_space(16.0);
        for g in &fleet.gaps {
            gap_row(ui, pal, g);
            ui.add_space(6.0);
        }
    }

    ui.add_space(16.0);
    ui.label(
        RichText::new(format!("snapshot {} · read-only", fleet.generated))
            .size(11.0)
            .color(pal.mute),
    );
    ui.add_space(12.0);
}

fn summary(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    let crit = fleet.gaps.iter().filter(|g| g.sev == Sev::Crit).count();
    let tiles = [
        (
            "PROPERTIES UP",
            format!("{} / {}", fleet.props_up, fleet.props_total),
            if fleet.props_up == fleet.props_total { pal.good } else { pal.crit },
        ),
        (
            "CONTAINERS",
            format!("{} / {}", fleet.containers_running, fleet.containers_total),
            if fleet.containers_running == fleet.containers_total { pal.good } else { pal.crit },
        ),
        (
            "OPEN GAPS",
            fleet.gaps.len().to_string(),
            if crit > 0 { pal.crit } else if fleet.gaps.is_empty() { pal.good } else { pal.warn },
        ),
    ];
    ui.horizontal(|ui| {
        for (k, v, col) in tiles {
            card_frame(pal).show(ui, |ui| {
                ui.set_width(180.0);
                ui.label(RichText::new(k).size(10.0).color(pal.mute).strong());
                ui.label(RichText::new(v).size(24.0).strong().color(col));
            });
        }
    });
}

fn property_grid(ui: &mut egui::Ui, pal: &Palette, rows: &[Row]) {
    let card_w = 340.0;
    let avail = ui.available_width();
    let cols = (((avail + 12.0) / (card_w + 12.0)).floor() as usize).max(1);

    egui::Grid::new("props")
        .num_columns(cols)
        .spacing([12.0, 12.0])
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                property_card(ui, pal, row, card_w);
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
}

fn property_card(ui: &mut egui::Ui, pal: &Palette, row: &Row, w: f32) {
    card_frame(pal).show(ui, |ui| {
        ui.set_width(w);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(&row.name).size(17.0).strong());
                let core = row
                    .drupal
                    .as_ref()
                    .map(|d| d.core.clone())
                    .unwrap_or_else(|| row.stack.clone());
                ui.label(RichText::new(format!("{} · {}", row.stack, core)).size(11.0).color(pal.mute));
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                pill(ui, row.health.label(), pal.health(row.health));
            });
        });

        ui.add_space(8.0);
        egui::Grid::new(("m", &row.slug))
            .num_columns(2)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                let http = match &row.http {
                    Some(h) => (
                        RichText::new(format!("{}  {}", h.code, row.probe_path))
                            .monospace()
                            .color(if h.ok { pal.good } else { pal.crit }),
                        RichText::new(format!("{} ms", h.latency_ms)).monospace(),
                    ),
                    None => (
                        RichText::new("unreachable").monospace().color(pal.crit),
                        RichText::new("—").monospace(),
                    ),
                };
                metric(ui, pal, "HTTP", http.0);
                metric(ui, pal, "LATENCY", http.1);
                ui.end_row();

                let db = row
                    .drupal
                    .as_ref()
                    .map(|d| RichText::new(&d.db).monospace().color(
                        if d.db == "Connected" { pal.good } else { pal.warn },
                    ))
                    .unwrap_or_else(|| RichText::new("n/a").monospace().color(pal.mute));
                let up = row
                    .container
                    .as_ref()
                    .map(|c| {
                        let base = fmt_since(&c.started);
                        let restarts: i64 = c.restarts.parse().unwrap_or(0);
                        if restarts > 0 {
                            RichText::new(format!("{base} · {restarts}⟳"))
                                .monospace()
                                .color(pal.warn)
                        } else {
                            RichText::new(base).monospace()
                        }
                    })
                    .unwrap_or_else(|| RichText::new("—").monospace());
                metric(ui, pal, "DATABASE", db);
                metric(ui, pal, "CONTAINER UP", up);
                ui.end_row();

                let tls = match &row.tls_not_after {
                    Some(na) => {
                        let days = dates::days_until(na);
                        let col = match days {
                            Some(d) if d <= 7 => pal.crit,
                            Some(d) if d <= 21 => pal.warn,
                            _ => pal.good,
                        };
                        let label = days
                            .map(|d| format!("{d}d"))
                            .unwrap_or_else(|| na.clone());
                        RichText::new(label).monospace().color(col)
                    }
                    None => RichText::new("—").monospace().color(pal.mute),
                };
                let maint = row
                    .drupal
                    .as_ref()
                    .map(|d| {
                        let on = d.maintenance != "0" && !d.maintenance.is_empty();
                        RichText::new(if on { "ON" } else { "off" })
                            .monospace()
                            .color(if on { pal.warn } else { pal.mute })
                    })
                    .unwrap_or_else(|| RichText::new("—").monospace().color(pal.mute));
                metric(ui, pal, "TLS EXPIRES", tls);
                metric(ui, pal, "MAINTENANCE", maint);
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.label(
            RichText::new(row.domains.join("  ·  "))
                .size(10.0)
                .monospace()
                .color(pal.accent),
        );
    });
}

fn host_panel(ui: &mut egui::Ui, pal: &Palette, h: &crate::model::Host) {
    card_frame(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("host")
            .num_columns(4)
            .spacing([28.0, 10.0])
            .show(ui, |ui| {
                host_cell(ui, pal, "DISK", &h.disk, Some(h.disk_pct), false);
                host_cell(ui, pal, "MEMORY", &h.mem, Some(h.mem_pct), false);
                host_cell(ui, pal, "LOAD (1m)", &h.load, None, false);
                host_cell(ui, pal, "UPTIME", &h.uptime, None, false);
                ui.end_row();
                host_cell(ui, pal, "OS UPGRADABLE", &format!("{} pkgs", h.upgradable), None, false);
                host_cell(
                    ui, pal, "SECURITY",
                    &format!("{} pending", h.security),
                    None,
                    h.security > 0,
                );
                host_cell(ui, pal, "REBOOT", &h.reboot, None, h.reboot == "YES");
                host_cell(ui, pal, "", "", None, false);
                ui.end_row();
            });
    });
}

fn host_cell(ui: &mut egui::Ui, pal: &Palette, k: &str, v: &str, pct: Option<u8>, flag: bool) {
    ui.vertical(|ui| {
        if k.is_empty() {
            ui.label("");
            return;
        }
        ui.label(RichText::new(k).size(10.0).color(pal.mute).strong());
        let col = if flag { pal.crit } else { ui.visuals().text_color() };
        ui.label(RichText::new(v).size(15.0).monospace().color(col));
        if let Some(p) = pct {
            let frac = (p as f32 / 100.0).clamp(0.0, 1.0);
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(140.0)
                    .desired_height(5.0)
                    .fill(pal.accent),
            );
        }
    });
}

fn gap_row(ui: &mut egui::Ui, pal: &Palette, g: &Gap) {
    let col = match g.sev {
        Sev::Crit => pal.crit,
        Sev::Warn => pal.warn,
    };
    Frame::none()
        .fill(card_fill(pal))
        .stroke(Stroke::new(1.0, pal.border))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(12.0, 9.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                pill(ui, &g.label, col);
                ui.label(RichText::new(&g.text).size(13.0));
            });
        });
}

// ── small helpers ───────────────────────────────────────────────────────────

fn section(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(12.0).strong().color(pal.mute));
    });
    let rect = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().hline(
        rect.left()..=rect.right(),
        y,
        Stroke::new(1.0, pal.border),
    );
}

fn metric(ui: &mut egui::Ui, pal: &Palette, k: &str, v: RichText) {
    ui.vertical(|ui| {
        ui.label(RichText::new(k).size(9.5).color(pal.mute));
        ui.label(v.size(13.0));
    });
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let bg = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 38);
    Frame::none()
        .fill(bg)
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).strong().color(color));
        });
}

fn banner(ui: &mut egui::Ui, pal: &Palette, msg: &str) {
    ui.add_space(8.0);
    Frame::none()
        .fill(Color32::from_rgba_unmultiplied(pal.crit.r(), pal.crit.g(), pal.crit.b(), 30))
        .stroke(Stroke::new(1.0, pal.crit))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(12.0, 9.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("⚠  {msg}")).color(pal.crit));
        });
}

fn card_fill(pal: &Palette) -> Color32 {
    pal.card
}

fn card_frame(pal: &Palette) -> Frame {
    Frame::none()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.border))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::same(14.0))
}

/// Turn an RFC3339-ish `StartedAt` into a coarse "3w" / "4d" / "5h" age.
fn fmt_since(started: &str) -> String {
    // started looks like 2026-07-29T16:17:23.368...Z
    let Some((date, _)) = started.split_once('T') else {
        return "—".into();
    };
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return "—".into();
    }
    let (y, m, d) = (
        parts[0].parse::<i64>().ok(),
        parts[1].parse::<i64>().ok(),
        parts[2].parse::<i64>().ok(),
    );
    let (Some(y), Some(m), Some(d)) = (y, m, d) else {
        return "—".into();
    };
    match dates::days_until(&format!("{} {} 00:00:00 {} GMT", month_abbr(m), d, y)) {
        Some(days) => {
            let ago = -days; // days_until is negative in the past
            if ago >= 7 {
                format!("{}w", ago / 7)
            } else if ago >= 1 {
                format!("{ago}d")
            } else {
                "today".into()
            }
        }
        None => "—".into(),
    }
}

fn month_abbr(m: i64) -> &'static str {
    ["", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
        .get(m as usize)
        .copied()
        .unwrap_or("Jan")
}
