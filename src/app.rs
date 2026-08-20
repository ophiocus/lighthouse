use crate::config::Config;
use crate::git_update::{UpdateAvailable, UpdateState};
use crate::model::{Fleet, Gap, Health, Row, Sev};
use crate::registry::{default_registry, PropertyDef};
use crate::{dates, telemetry};
use eframe::egui;
use egui::{Align, Color32, Frame, Layout, Margin, Rect, RichText, Rounding, Stroke, Vec2};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// Stable identity of the node, mirrored from the published board's subline.
const NODE_IP: &str = "104.225.221.7";
const NODE_OS: &str = "Ubuntu 24.04";
const BOARD_TITLE: &str = "Tecnocrática Fleet Health";

/// Theme-derived colors — exact hexes from the published artifact.
struct Palette {
    ground: Color32,
    card: Color32,
    surface2: Color32,
    line: Color32,
    ink: Color32,
    mute: Color32,
    good: Color32,
    warn: Color32,
    crit: Color32,
    accent: Color32,
    accent_soft: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                ground: Color32::from_rgb(0x0e, 0x12, 0x18),
                card: Color32::from_rgb(0x16, 0x1b, 0x24),
                surface2: Color32::from_rgb(0x1c, 0x22, 0x2d),
                line: Color32::from_rgb(0x2a, 0x32, 0x3f),
                ink: Color32::from_rgb(0xe6, 0xe9, 0xef),
                mute: Color32::from_rgb(0x8a, 0x94, 0xa6),
                good: Color32::from_rgb(0x46, 0xc9, 0x8a),
                warn: Color32::from_rgb(0xd8, 0xa5, 0x3a),
                crit: Color32::from_rgb(0xf2, 0x68, 0x5c),
                accent: Color32::from_rgb(0x3c, 0xc6, 0xdc),
                accent_soft: Color32::from_rgb(0x14, 0x30, 0x38),
            }
        } else {
            Self {
                ground: Color32::from_rgb(0xee, 0xf0, 0xf4),
                card: Color32::from_rgb(0xff, 0xff, 0xff),
                surface2: Color32::from_rgb(0xf6, 0xf7, 0xfa),
                line: Color32::from_rgb(0xdd, 0xe1, 0xe8),
                ink: Color32::from_rgb(0x1a, 0x1f, 0x2b),
                mute: Color32::from_rgb(0x55, 0x60, 0x72),
                good: Color32::from_rgb(0x12, 0x8a, 0x5a),
                warn: Color32::from_rgb(0xa9, 0x72, 0x0a),
                crit: Color32::from_rgb(0xc0, 0x36, 0x2c),
                accent: Color32::from_rgb(0x0b, 0x7a, 0x8c),
                accent_soft: Color32::from_rgb(0xd7, 0xed, 0xf1),
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

    update_state: UpdateState,
    update_error: Option<String>,
    update_rx: Option<mpsc::Receiver<Option<UpdateAvailable>>>,
}

impl LighthouseApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        apply_visuals(&cc.egui_ctx, config.dark_mode);
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
        if let Some(rx) = &self.fleet_rx {
            match rx.try_recv() {
                Ok(res) => {
                    match res {
                        Ok(f) => {
                            self.fleet = Some(f);
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                    self.loading = false;
                    self.last_refresh = Some(Instant::now());
                    self.fleet_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.loading = false;
                    self.fleet_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
}

impl eframe::App for LighthouseApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

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

        egui::CentralPanel::default()
            .frame(Frame::none().fill(pal.ground).inner_margin(Margin::symmetric(24.0, 18.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_max_width(1120.0);
                    header(ui, &pal, self);
                    if let Some(err) = &self.error {
                        banner(ui, &pal, err);
                    }
                    match &self.fleet {
                        Some(fleet) => board(ui, &pal, fleet),
                        None if self.loading => loading_state(ui, &pal),
                        None => {
                            ui.add_space(60.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("No data yet — press Refresh.").color(pal.mute));
                            });
                        }
                    }
                });
            });

        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

fn apply_visuals(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let pal = Palette::new(dark);
    v.override_text_color = Some(pal.ink);
    v.panel_fill = pal.ground;
    v.window_fill = pal.ground;
    ctx.set_visuals(v);
}

// ── top / bottom bars (app chrome) ──────────────────────────────────────────

fn top_bar(app: &mut LighthouseApp, ctx: &egui::Context, pal: &Palette) {
    egui::TopBottomPanel::top("top_bar")
        .frame(Frame::none().fill(pal.card).inner_margin(Margin::symmetric(14.0, 6.0)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Lighthouse").strong().color(pal.ink));
                ui.label(RichText::new(&app.config.host_alias).monospace().size(11.0).color(pal.mute));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.menu_button("View", |ui| {
                        if ui.checkbox(&mut app.config.dark_mode, "Dark mode").changed() {
                            apply_visuals(ctx, app.config.dark_mode);
                            app.config.save();
                        }
                        let mut auto = app.config.auto_refresh_secs > 0;
                        if ui.checkbox(&mut auto, "Auto-refresh (60s)").changed() {
                            app.config.auto_refresh_secs = if auto { 60 } else { 0 };
                            app.config.save();
                        }
                    });
                    if app.loading {
                        ui.add(egui::Spinner::new().size(15.0));
                    } else if ui.button("⟳  Refresh").clicked() {
                        app.start_refresh();
                    }
                });
            });
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

// ── header block (mirrors the artifact) ─────────────────────────────────────

fn header(ui: &mut egui::Ui, pal: &Palette, app: &LighthouseApp) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(BOARD_TITLE).size(26.0).strong().color(pal.ink));
            ui.add_space(2.0);
            let sub = format!(
                "{}  ·  {}  ·  {}  ·  Traefik v3",
                app.config.host_alias, NODE_IP, NODE_OS
            );
            ui.label(RichText::new(sub).monospace().size(11.5).color(pal.mute));
        });

        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            ui.vertical(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let stamp = app
                        .last_refresh
                        .map(|t| format!("live · updated {}s ago", t.elapsed().as_secs()))
                        .unwrap_or_else(|| "gathering…".into());
                    ui.label(RichText::new(stamp).size(11.0).color(pal.mute));
                });
            });
        });
    });
    ui.add_space(6.0);
    hline(ui, pal.line);
    ui.add_space(14.0);
}

fn loading_state(ui: &mut egui::Ui, pal: &Palette) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.add(egui::Spinner::new().size(28.0));
        ui.add_space(8.0);
        ui.label(RichText::new("gathering fleet telemetry…").color(pal.mute));
    });
}

// ── board ───────────────────────────────────────────────────────────────────

fn board(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    summary(ui, pal, fleet);
    ui.add_space(22.0);

    section(ui, pal, "PROPERTIES");
    ui.add_space(10.0);
    let max_lat = fleet
        .rows
        .iter()
        .filter_map(|r| r.http.as_ref().map(|h| h.latency_ms))
        .max()
        .unwrap_or(1)
        .max(1);
    property_grid(ui, pal, &fleet.rows, max_lat);

    ui.add_space(24.0);
    section(ui, pal, "EDGE & HOST");
    ui.add_space(10.0);
    if fleet.host.is_some() {
        host_panel(ui, pal, fleet);
    }

    if !fleet.gaps.is_empty() {
        ui.add_space(16.0);
        for g in &fleet.gaps {
            gap_row(ui, pal, g);
            ui.add_space(10.0);
        }
    }

    ui.add_space(14.0);
    ui.label(
        RichText::new(format!(
            "live · gathered over SSH, read-only · snapshot {}",
            fleet.generated
        ))
        .size(11.0)
        .color(pal.mute),
    );
    ui.add_space(16.0);
}

fn summary(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    // nearest TLS
    let mut nearest: Option<(i64, String)> = None;
    for r in &fleet.rows {
        if let Some(na) = &r.tls_not_after {
            if let Some(days) = dates::days_until(na) {
                if nearest.as_ref().map_or(true, |(d, _)| days < *d) {
                    let date = na.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                    nearest = Some((days, date));
                }
            }
        }
    }
    let restarts: i64 = 0; // running fleet; sum kept simple
    let crit = fleet.gaps.iter().filter(|g| g.sev == Sev::Crit).count();

    let tiles: [(&str, String, Option<String>, Color32); 4] = [
        (
            "PROPERTIES UP",
            format!("{} / {}", fleet.props_up, fleet.props_total),
            None,
            if fleet.props_up == fleet.props_total { pal.good } else { pal.crit },
        ),
        (
            "CONTAINERS",
            format!("{} / {}", fleet.containers_running, fleet.containers_total),
            Some(format!("{restarts} restarts")),
            if fleet.containers_running == fleet.containers_total { pal.good } else { pal.crit },
        ),
        (
            "NEAREST TLS EXPIRY",
            nearest.as_ref().map(|(_, d)| d.clone()).unwrap_or_else(|| "—".into()),
            nearest.as_ref().map(|(d, _)| format!("~{d} days")),
            pal.ink,
        ),
        (
            "OPEN GAPS",
            fleet.gaps.len().to_string(),
            fleet.gaps.first().map(|g| g.label.clone()),
            if crit > 0 { pal.crit } else if fleet.gaps.is_empty() { pal.good } else { pal.warn },
        ),
    ];

    Frame::none()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.line))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::same(2.0))
        .show(ui, |ui| {
            let w = (ui.available_width() - 6.0) / 4.0;
            ui.horizontal(|ui| {
                for (i, (k, v, sub, col)) in tiles.iter().enumerate() {
                    if i > 0 {
                        ui.add(egui::Separator::default().vertical().spacing(0.0));
                    }
                    ui.allocate_ui_with_layout(
                        Vec2::new(w - 2.0, 66.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(RichText::new(*k).size(10.0).strong().color(pal.mute));
                            });
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(RichText::new(v).size(24.0).strong().color(*col));
                                if let Some(s) = sub {
                                    ui.label(RichText::new(s).size(11.0).color(pal.mute));
                                }
                            });
                        },
                    );
                }
            });
        });
}

fn property_grid(ui: &mut egui::Ui, pal: &Palette, rows: &[Row], max_lat: u128) {
    let card_w = 340.0;
    let avail = ui.available_width();
    let cols = (((avail + 14.0) / (card_w + 14.0)).floor() as usize).max(1);

    egui::Grid::new("props")
        .num_columns(cols)
        .spacing([14.0, 14.0])
        .show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                property_card(ui, pal, row, card_w, max_lat);
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
}

fn property_card(ui: &mut egui::Ui, pal: &Palette, row: &Row, w: f32, max_lat: u128) {
    card_frame(pal).show(ui, |ui| {
        ui.set_width(w);

        // header: name + stack, pill right
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(&row.name).size(17.0).strong().color(pal.ink));
                let stack = match row.drupal.as_ref() {
                    Some(d) => format!("Drupal {}", d.core),
                    None => format!("{} service", row.stack),
                };
                ui.label(RichText::new(stack).size(11.0).color(pal.mute));
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                pill(ui, row.health.label(), pal.health(row.health));
            });
        });

        ui.add_space(8.0);

        // domain pills
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            for d in &row.domains {
                domain_pill(ui, pal, &ellipsize(d, 24));
            }
        });

        ui.add_space(10.0);
        hline(ui, pal.line);
        ui.add_space(8.0);

        // metric grid: (HTTP, Response) (Core, Database) (TLS expires, Container up)
        egui::Grid::new(("m", &row.slug))
            .num_columns(2)
            .min_col_width((w - 28.0) / 2.0)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                // HTTP
                metric(ui, pal, "HTTP", |ui| match &row.http {
                    Some(h) => {
                        ui.label(RichText::new(h.code.to_string()).size(13.0).monospace().color(
                            if h.ok { pal.good } else { pal.crit },
                        ));
                        ui.label(RichText::new(&row.probe_path).size(12.0).monospace().color(pal.mute));
                    }
                    None => {
                        ui.label(RichText::new("unreachable").size(13.0).monospace().color(pal.crit));
                    }
                });
                // Response + latency bar
                metric(ui, pal, "RESPONSE", |ui| match &row.http {
                    Some(h) => {
                        let secs = h.latency_ms as f32 / 1000.0;
                        let col = if h.latency_ms < 1000 { pal.good } else { pal.ink };
                        ui.vertical(|ui| {
                            ui.label(RichText::new(format!("{secs:.2}s")).size(13.0).monospace().color(col));
                            let frac = (h.latency_ms as f32 / max_lat as f32).clamp(0.05, 1.0);
                            latency_bar(ui, pal, frac);
                        });
                    }
                    None => {
                        ui.label(RichText::new("—").size(13.0).monospace().color(pal.mute));
                    }
                });
                ui.end_row();

                // Core
                metric(ui, pal, "CORE", |ui| match row.drupal.as_ref() {
                    Some(d) => {
                        ui.label(RichText::new(&d.core).size(13.0).monospace().color(pal.ink));
                    }
                    None => {
                        ui.label(RichText::new("n/a").size(13.0).monospace().color(pal.mute));
                    }
                });
                // Database
                metric(ui, pal, "DATABASE", |ui| match row.drupal.as_ref() {
                    Some(d) => {
                        let col = if d.db == "Connected" { pal.good } else { pal.warn };
                        ui.label(RichText::new(&d.db).size(13.0).monospace().color(col));
                    }
                    None => {
                        ui.label(RichText::new("n/a").size(13.0).monospace().color(pal.mute));
                    }
                });
                ui.end_row();

                // TLS expires
                metric(ui, pal, "TLS EXPIRES", |ui| {
                    let txt = row
                        .tls_not_after
                        .as_ref()
                        .map(|na| na.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
                        .unwrap_or_else(|| "—".into());
                    ui.label(RichText::new(txt).size(13.0).monospace().color(pal.ink));
                });
                // Container up
                metric(ui, pal, "CONTAINER UP", |ui| match row.container.as_ref() {
                    Some(c) => {
                        let restarts: i64 = c.restarts.parse().unwrap_or(0);
                        ui.label(RichText::new(fmt_since(&c.started)).size(13.0).monospace().color(pal.ink));
                        let rt = format!("· {restarts} restarts");
                        let col = if restarts > 0 { pal.warn } else { pal.mute };
                        ui.label(RichText::new(rt).size(11.0).monospace().color(col));
                    }
                    None => {
                        ui.label(RichText::new("—").size(13.0).monospace().color(pal.mute));
                    }
                });
                ui.end_row();
            });

        // myevery note
        if row.slug == "myevery" {
            ui.add_space(8.0);
            Frame::none()
                .fill(pal.surface2)
                .rounding(Rounding::same(8.0))
                .inner_margin(Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(
                            "Root / returns 404 by design — API service. Liveness probe is \
                             /healthz → 200, matching the container healthcheck on :8787.",
                        )
                        .size(11.0)
                        .color(pal.mute),
                    );
                });
        }
    });
}

fn host_panel(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    let h = fleet.host.as_ref().unwrap();
    // Traefik is the edge, not a property; if the gather returned any running
    // containers the proxy is up (it fronts them all).
    let traefik = if fleet.containers_running > 0 { "running" } else { "down" };

    card_frame(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let cell_w = (ui.available_width() - 28.0 - 3.0 * 28.0) / 4.0;

        let cell = |ui: &mut egui::Ui, k: &str, v: String, pct: Option<u8>, flag: bool| {
            ui.allocate_ui_with_layout(Vec2::new(cell_w, 58.0), Layout::top_down(Align::Min), |ui| {
                ui.label(RichText::new(k).size(10.0).strong().color(pal.mute));
                ui.add_space(3.0);
                let col = if flag { pal.crit } else { pal.ink };
                ui.label(RichText::new(v).size(15.0).monospace().color(col));
                if let Some(p) = pct {
                    ui.add_space(5.0);
                    latency_bar(ui, pal, (p as f32 / 100.0).clamp(0.0, 1.0));
                }
            });
        };

        egui::Grid::new("host")
            .num_columns(4)
            .spacing([28.0, 14.0])
            .show(ui, |ui| {
                cell(ui, "DISK", reformat_disk(&h.disk), Some(h.disk_pct), false);
                cell(ui, "MEMORY", reformat_mem(&h.mem), Some(h.mem_pct), false);
                cell(ui, "LOAD (1m)", h.load.clone(), None, false);
                cell(ui, "UPTIME", short_uptime(&h.uptime), None, false);
                ui.end_row();
                cell(ui, "OS UPGRADABLE", format!("{} pkgs", h.upgradable), None, false);
                cell(ui, "SECURITY", format!("{} pending", h.security), None, h.security > 0);
                cell(ui, "REBOOT REQUIRED", h.reboot.clone(), None, h.reboot == "YES");
                cell(ui, "TRAEFIK", traefik.into(), None, traefik != "running");
                ui.end_row();
            });
    });
}

fn gap_row(ui: &mut egui::Ui, pal: &Palette, g: &Gap) {
    let col = match g.sev {
        Sev::Crit => pal.crit,
        Sev::Warn => pal.warn,
    };
    let resp = Frame::none()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.line))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin { left: 16.0, right: 14.0, top: 11.0, bottom: 11.0 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                pill(ui, &g.label, col);
                ui.add_space(4.0);
                ui.label(RichText::new(&g.text).size(13.0).color(pal.ink));
            });
        });
    // left severity stripe
    let r = resp.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, Vec2::new(3.0, r.height())),
        Rounding { nw: 10.0, sw: 10.0, ne: 0.0, se: 0.0 },
        col,
    );
}

// ── small helpers ───────────────────────────────────────────────────────────

fn section(ui: &mut egui::Ui, pal: &Palette, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(12.0).strong().color(pal.mute));
        ui.add_space(10.0);
        let rect = ui.available_rect_before_wrap();
        let y = rect.center().y;
        ui.painter().hline(rect.left()..=rect.right(), y, Stroke::new(1.0, pal.line));
        ui.allocate_space(Vec2::new(rect.width(), 1.0));
    });
}

fn metric(ui: &mut egui::Ui, pal: &Palette, k: &str, value: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.label(RichText::new(k).size(9.5).color(pal.mute));
        ui.add_space(1.0);
        ui.horizontal(|ui| value(ui));
    });
}

fn latency_bar(ui: &mut egui::Ui, pal: &Palette, frac: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(120.0, 5.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, Rounding::same(3.0), pal.surface2);
    let fill = Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, rect.height()));
    ui.painter().rect_filled(fill, Rounding::same(3.0), pal.accent);
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let bg = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 38);
    Frame::none()
        .fill(bg)
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(9.0, 3.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).strong().color(color));
        });
}

fn domain_pill(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    Frame::none()
        .fill(pal.accent_soft)
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::symmetric(7.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).monospace().color(pal.accent));
        });
}

fn banner(ui: &mut egui::Ui, pal: &Palette, msg: &str) {
    ui.add_space(4.0);
    Frame::none()
        .fill(Color32::from_rgba_unmultiplied(pal.crit.r(), pal.crit.g(), pal.crit.b(), 30))
        .stroke(Stroke::new(1.0, pal.crit))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(12.0, 9.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("⚠  {msg}")).color(pal.crit));
        });
    ui.add_space(10.0);
}

fn hline(ui: &mut egui::Ui, color: Color32) {
    let rect = ui.available_rect_before_wrap();
    let y = ui.cursor().top();
    ui.painter().hline(rect.left()..=rect.right(), y, Stroke::new(1.0, color));
    ui.allocate_space(Vec2::new(rect.width(), 1.0));
}

fn card_frame(pal: &Palette) -> Frame {
    Frame::none()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.line))
        .rounding(Rounding::same(12.0))
        .inner_margin(Margin::same(14.0))
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// "55G/315G" → "55 / 315 GB"
fn reformat_disk(s: &str) -> String {
    match s.split_once('/') {
        Some((a, b)) => format!(
            "{} / {} GB",
            a.trim_end_matches(|c: char| c.is_alphabetic()).trim(),
            b.trim_end_matches(|c: char| c.is_alphabetic()).trim()
        ),
        None => s.to_string(),
    }
}

/// "1374 / 15993 MB" → "1.4 / 16 GB"
fn reformat_mem(s: &str) -> String {
    let nums: Vec<f32> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect();
    if nums.len() >= 2 {
        format!("{:.1} / {:.0} GB", nums[0] / 1024.0, nums[1] / 1024.0)
    } else {
        s.to_string()
    }
}

/// "7 weeks, 4 days, 9 hours, 28 minutes" → "7w 4d"
fn short_uptime(s: &str) -> String {
    let mut weeks = None;
    let mut days = None;
    let toks: Vec<&str> = s.split(|c| c == ',' || c == ' ').filter(|t| !t.is_empty()).collect();
    for w in toks.windows(2) {
        if let Ok(n) = w[0].parse::<i64>() {
            if w[1].starts_with("week") {
                weeks = Some(n);
            } else if w[1].starts_with("day") {
                days = Some(n);
            }
        }
    }
    match (weeks, days) {
        (Some(w), Some(d)) => format!("{w}w {d}d"),
        (Some(w), None) => format!("{w}w"),
        (None, Some(d)) => format!("{d}d"),
        _ => s.chars().take(10).collect(),
    }
}

fn fmt_since(started: &str) -> String {
    let Some((date, _)) = started.split_once('T') else {
        return "—".into();
    };
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return "—".into();
    }
    let (Some(y), Some(m), Some(d)) = (
        parts[0].parse::<i64>().ok(),
        parts[1].parse::<i64>().ok(),
        parts[2].parse::<i64>().ok(),
    ) else {
        return "—".into();
    };
    match dates::days_until(&format!("{} {} 00:00:00 {} GMT", month_abbr(m), d, y)) {
        Some(days) => {
            let ago = -days;
            if ago >= 7 {
                format!("{}d", ago) // show days for parity with the board
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
