use crate::actions::{self, ActionResult};
use crate::config::Config;
use crate::git_update::{UpdateAvailable, UpdateState};
use crate::model::{
    derive_measurement, AnalyticsState, Fleet, Gap, Health, Measurement, Project, ProjectType, Row,
    Sev,
};
use crate::template::{actions_for, Action};
use crate::{dates, telemetry};
use eframe::egui;
use egui::{Align, Color32, Frame, Layout, Margin, Rect, RichText, Rounding, Stroke, Vec2};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const NODE_IP: &str = "104.225.221.7";
const NODE_OS: &str = "Ubuntu 24.04";
const BOARD_TITLE: &str = "Tecnocrática Fleet Control";

/// A pending, not-yet-confirmed action against a project.
#[derive(Clone)]
struct Pending {
    action: Action,
    project: Project,
}

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
    node: Color32,
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
                node: Color32::from_rgb(0x8c, 0xb4, 0x6a),
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
                node: Color32::from_rgb(0x5a, 0x7d, 0x3a),
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
    fn type_color(&self, t: ProjectType) -> Color32 {
        match t {
            ProjectType::Drupal => self.accent,
            ProjectType::Node => self.node,
            ProjectType::Unknown => self.mute,
        }
    }
}

pub struct LighthouseApp {
    pub config: Config,

    fleet: Option<Fleet>,
    error: Option<String>,
    loading: bool,
    last_refresh: Option<Instant>,
    fleet_rx: Option<mpsc::Receiver<Result<Fleet, String>>>,
    /// Second-pass GA4 results, keyed by project slug.
    analytics_rx: Option<mpsc::Receiver<Vec<(String, AnalyticsState)>>>,

    // control-action plumbing
    pending: Option<Pending>,
    action_rx: Option<mpsc::Receiver<ActionResult>>,
    action_busy: bool,
    action_title: String,
    action_log: Option<ActionResult>,

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
            fleet: None,
            error: None,
            loading: false,
            last_refresh: None,
            fleet_rx: None,
            analytics_rx: None,
            pending: None,
            action_rx: None,
            action_busy: false,
            action_title: String::new(),
            action_log: None,
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
        std::thread::spawn(move || {
            let _ = tx.send(telemetry::collect(&host));
        });
    }

    /// Second pass: fetch GA4 state for an already-rendered fleet.
    ///
    /// Sends back `(slug, state)` pairs rather than a whole `Fleet` so a slow
    /// sweep landing after the user has hit refresh cannot overwrite newer
    /// health data — it only ever fills in the analytics cells.
    fn start_analytics(&mut self, mut rows: Vec<Row>) {
        let (tx, rx) = mpsc::channel();
        self.analytics_rx = Some(rx);
        std::thread::spawn(move || {
            telemetry::attach_analytics(&mut rows);
            let out: Vec<(String, AnalyticsState)> = rows
                .into_iter()
                .map(|r| (r.p.slug, r.analytics))
                .collect();
            let _ = tx.send(out);
        });
    }

    fn start_action(&mut self, pending: Pending) {
        self.action_busy = true;
        self.action_log = None;
        self.action_title = format!("{} · {}", pending.action.label(), pending.project.slug);
        let (tx, rx) = mpsc::channel();
        self.action_rx = Some(rx);
        let host = self.config.host_alias.clone();
        let cmd = pending.action.command(&pending.project);
        std::thread::spawn(move || {
            let _ = tx.send(actions::run(&host, &cmd));
        });
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.fleet_rx {
            match rx.try_recv() {
                Ok(res) => {
                    match res {
                        Ok(f) => {
                            // Paint health immediately, then chase analytics in
                            // a second pass. Analytics costs ~a dozen sequential
                            // round trips to Google; waiting for it before the
                            // first paint meant a ready health picture sat
                            // behind a spinner for the sum of both.
                            let rows = f.rows.clone();
                            self.fleet = Some(f);
                            self.error = None;
                            self.start_analytics(rows);
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
        if let Some(rx) = &self.analytics_rx {
            match rx.try_recv() {
                Ok(states) => {
                    if let Some(f) = &mut self.fleet {
                        for (slug, st) in states {
                            if let Some(row) = f.rows.iter_mut().find(|r| r.p.slug == slug) {
                                row.analytics = st;
                            }
                        }
                        // Measurement gaps could not exist until now, so append
                        // them here rather than recomputing the health gaps.
                        let mut extra = telemetry::analytics_gaps(&f.rows);
                        f.gaps.append(&mut extra);
                    }
                    self.analytics_rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.analytics_rx = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.action_rx {
            if let Ok(res) = rx.try_recv() {
                self.action_log = Some(res);
                self.action_busy = false;
                self.action_rx = None;
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

        let mut intent: Option<Pending> = None;
        egui::CentralPanel::default()
            .frame(Frame::none().fill(pal.ground).inner_margin(Margin::symmetric(24.0, 18.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_width(ui.available_width().min(1160.0));
                    header(ui, &pal, self);
                    if let Some(err) = &self.error {
                        banner(ui, &pal, err);
                    }
                    match &self.fleet {
                        Some(fleet) => board(ui, &pal, fleet, &mut intent),
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
        if let Some(p) = intent {
            self.pending = Some(p);
        }

        self.render_confirm(ctx, &pal);
        self.render_result(ctx, &pal);

        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

impl LighthouseApp {
    fn render_confirm(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(pending) = self.pending.clone() else { return };
        let mut decision = 0u8; // 1 = cancel, 2 = run
        egui::Window::new(RichText::new("Confirm action").strong())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.add_space(4.0);
                ui.label(RichText::new(pending.action.confirm(&pending.project)).size(14.0));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("→ {}", pending.action.command(&pending.project)))
                        .monospace()
                        .size(11.0)
                        .color(pal.mute),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        decision = 1;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let col = if pending.action.danger() { pal.crit } else { pal.accent };
                        let run = egui::Button::new(
                            RichText::new(format!("Run  {}", pending.action.label()))
                                .color(Color32::WHITE),
                        )
                        .fill(col);
                        if ui.add(run).clicked() {
                            decision = 2;
                        }
                    });
                });
            });
        match decision {
            1 => self.pending = None,
            2 => {
                self.pending = None;
                self.start_action(pending);
            }
            _ => {}
        }
    }

    fn render_result(&mut self, ctx: &egui::Context, pal: &Palette) {
        if self.action_busy {
            egui::Window::new("Running…")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(&self.action_title);
                    });
                });
            return;
        }
        let mut close = false;
        if let Some(res) = &self.action_log {
            egui::Window::new(RichText::new(format!(
                "{}  {}",
                if res.ok { "✓" } else { "✗" },
                self.action_title
            )))
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let col = if res.ok { pal.good } else { pal.crit };
                ui.label(RichText::new(if res.ok { "success" } else { "failed" }).strong().color(col));
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&res.output).monospace().size(11.5))
                            .wrap(),
                    );
                });
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        }
        if close {
            self.action_log = None;
            self.start_refresh(); // reflect any state change the action made
        }
    }
}

fn apply_visuals(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let pal = Palette::new(dark);
    v.override_text_color = Some(pal.ink);
    v.panel_fill = pal.ground;
    v.window_fill = pal.card;
    ctx.set_visuals(v);
}

// ── top / bottom bars ───────────────────────────────────────────────────────

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

// ── header ──────────────────────────────────────────────────────────────────

fn header(ui: &mut egui::Ui, pal: &Palette, app: &LighthouseApp) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(BOARD_TITLE).size(24.0).strong().color(pal.ink));
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
            let stamp = app
                .last_refresh
                .map(|t| format!("live · updated {}s ago", t.elapsed().as_secs()))
                .unwrap_or_else(|| "gathering…".into());
            ui.add_space(10.0);
            ui.label(RichText::new(stamp).size(11.0).color(pal.mute));
        });
    });
    ui.add_space(3.0);
    let sub = format!(
        "{}   ·   {}   ·   {}   ·   Traefik v3",
        app.config.host_alias, NODE_IP, NODE_OS
    );
    ui.label(RichText::new(sub).monospace().size(11.5).color(pal.mute));
    ui.add_space(10.0);
    hline(ui, pal.line);
    ui.add_space(16.0);
}

fn loading_state(ui: &mut egui::Ui, pal: &Palette) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.add(egui::Spinner::new().size(28.0));
        ui.add_space(8.0);
        ui.label(RichText::new("discovering projects…").color(pal.mute));
    });
}

// ── board ───────────────────────────────────────────────────────────────────

fn board(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet, intent: &mut Option<Pending>) {
    summary(ui, pal, fleet);
    ui.add_space(22.0);

    section(ui, pal, "PROJECTS");
    ui.add_space(10.0);
    let max_lat = fleet
        .rows
        .iter()
        .filter_map(|r| r.http.as_ref().map(|h| h.latency_ms))
        .max()
        .unwrap_or(1)
        .max(1);
    property_grid(ui, pal, &fleet.rows, max_lat, intent);

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
        RichText::new(format!("live · discovered & gathered over SSH, read-only telemetry · snapshot {}", fleet.generated))
            .size(11.0)
            .color(pal.mute),
    );
    ui.add_space(16.0);
}

fn summary(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    let mut nearest: Option<(i64, String)> = None;
    for r in &fleet.rows {
        if !r.p.tls.is_empty() {
            if let Some(days) = dates::days_until(&r.p.tls) {
                if nearest.as_ref().map_or(true, |(d, _)| days < *d) {
                    let date = r.p.tls.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                    nearest = Some((days, date));
                }
            }
        }
    }
    let crit = fleet.gaps.iter().filter(|g| g.sev == Sev::Crit).count();

    let tiles: [(&str, String, Option<String>, Color32); 4] = [
        (
            "PROJECTS",
            fleet.total.to_string(),
            Some(format!("{} Drupal · {} Node", fleet.drupal_count, fleet.node_count)),
            pal.ink,
        ),
        (
            "HEALTHY",
            format!("{} / {}", fleet.healthy, fleet.total),
            None,
            if fleet.healthy == fleet.total { pal.good } else { pal.crit },
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

fn property_grid(ui: &mut egui::Ui, pal: &Palette, rows: &[Row], max_lat: u128, intent: &mut Option<Pending>) {
    let card_w = 360.0;
    let gap = 14.0;
    let avail = ui.available_width();
    let cols = (((avail + gap) / (card_w + gap)).floor() as usize).clamp(1, rows.len().max(1));

    for chunk in rows.chunks(cols) {
        // The card frame is a hard-allocated fixed rect, so a card that gains a
        // metric row without gaining height here clips it silently — no warning,
        // no overflow, the row is simply not drawn. Derive the height instead of
        // carrying a magic number: chrome (title, pills, latency bar, padding)
        // plus METRIC_ROWS cells of ROW height separated by ROW_GAP. ROW must
        // match the cell height in `metric()` and ROW_GAP the Grid `.spacing`.
        const CHROME: f32 = 94.0;
        const ROW: f32 = 58.0;
        const ROW_GAP: f32 = 10.0;
        const METRIC_ROWS: f32 = 4.0;
        let h = CHROME + METRIC_ROWS * ROW + (METRIC_ROWS - 1.0) * ROW_GAP;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for row in chunk {
                property_card(ui, pal, row, card_w, h, max_lat, intent);
            }
        });
        ui.add_space(gap);
    }
}

fn property_card(
    ui: &mut egui::Ui,
    pal: &Palette,
    row: &Row,
    w: f32,
    h: f32,
    max_lat: u128,
    intent: &mut Option<Pending>,
) {
    let inner = w - 28.0;
    ui.allocate_ui_with_layout(Vec2::new(w, h), Layout::top_down(Align::Min), |ui| {
        let rect = ui.max_rect();
        ui.painter().rect(rect, Rounding::same(12.0), pal.card, Stroke::new(1.0, pal.line));
        ui.set_clip_rect(rect);
        ui.allocate_ui_at_rect(rect.shrink(14.0), |ui| {
            ui.set_width(inner);

            // header: name + type + health
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(&row.p.slug).size(17.0).strong().color(pal.ink));
                    let sub = match row.ptype {
                        ProjectType::Drupal => format!("Drupal {}", row.p.core.as_deref().unwrap_or("")),
                        ProjectType::Node => "Node service".to_string(),
                        ProjectType::Unknown => "Unknown".to_string(),
                    };
                    ui.label(RichText::new(sub).size(11.0).color(pal.type_color(row.ptype)));
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    pill(ui, row.health.label(), pal.health(row.health));
                    if row.p.maintenance.as_deref().map_or(false, |m| m != "0" && !m.is_empty()) {
                        pill(ui, "maint", pal.warn);
                    }
                });
            });

            ui.add_space(7.0);
            // The card is a fixed-height frame, so this label's height is not
            // free: a property carrying ten Host() labels wrapped to four lines
            // and pushed its own action buttons out of the bottom of the card,
            // silently. Drop the `www.` mirrors (implied by their apex), lead
            // with the apex since that is the probe target, and cap the rest so
            // the block can never exceed two lines.
            const MAX_DOMAINS: usize = 3;
            let mut shown: Vec<&str> = row
                .p
                .domains
                .iter()
                .map(|s| s.as_str())
                .filter(|d| !d.starts_with("www."))
                .collect();
            if let Some(i) = shown.iter().position(|d| *d == row.p.apex) {
                shown.swap(0, i);
            }
            let hidden = shown.len().saturating_sub(MAX_DOMAINS);
            let mut txt = shown
                .iter()
                .take(MAX_DOMAINS)
                .copied()
                .collect::<Vec<_>>()
                .join("   ·   ");
            if hidden > 0 {
                txt.push_str(&format!("   +{hidden}"));
            }
            ui.label(RichText::new(txt).size(11.0).monospace().color(pal.type_color(row.ptype)));
            ui.add_space(9.0);
            hline(ui, pal.line);
            ui.add_space(9.0);

            metrics(ui, pal, row, inner, max_lat);

            // actions
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for &a in actions_for(row.ptype) {
                    let col = if a.danger() { pal.crit } else { pal.accent };
                    let btn = egui::Button::new(RichText::new(a.label()).size(11.5).color(col))
                        .fill(pal.surface2)
                        .stroke(Stroke::new(1.0, pal.line));
                    if ui.add(btn).clicked() {
                        *intent = Some(Pending { action: a, project: row.p.clone() });
                    }
                }
            });
        });
    });
}

fn metrics(ui: &mut egui::Ui, pal: &Palette, row: &Row, inner: f32, max_lat: u128) {
    egui::Grid::new(("m", &row.p.slug))
        .num_columns(2)
        .min_col_width((inner - 16.0) / 2.0)
        .max_col_width((inner - 16.0) / 2.0)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            // HTTP + RESPONSE (both types)
            metric(ui, pal, "HTTP", |ui| match &row.http {
                Some(hp) => {
                    ui.label(RichText::new(hp.code.to_string()).size(13.0).monospace().color(if hp.ok { pal.good } else { pal.crit }));
                    ui.label(RichText::new(&row.p.probe_path).size(12.0).monospace().color(pal.mute));
                }
                None => {
                    ui.label(RichText::new("unreachable").size(13.0).monospace().color(pal.crit));
                }
            });
            metric(ui, pal, "RESPONSE", |ui| match &row.http {
                Some(hp) => {
                    let secs = hp.latency_ms as f32 / 1000.0;
                    let col = if hp.latency_ms < 1000 { pal.good } else { pal.ink };
                    ui.vertical(|ui| {
                        ui.label(RichText::new(format!("{secs:.2}s")).size(13.0).monospace().color(col));
                        latency_bar(ui, pal, (hp.latency_ms as f32 / max_lat as f32).clamp(0.05, 1.0));
                    });
                }
                None => {
                    ui.label(RichText::new("—").size(13.0).monospace().color(pal.mute));
                }
            });
            ui.end_row();

            // type-specific middle row
            match row.ptype {
                ProjectType::Node => {
                    metric(ui, pal, "HEALTH", |ui| {
                        let (txt, col) = match row.p.health.as_str() {
                            "healthy" => ("healthy", pal.good),
                            "none" => ("no check", pal.mute),
                            other => (other, pal.warn),
                        };
                        ui.label(RichText::new(txt).size(13.0).monospace().color(col));
                    });
                }
                _ => {
                    metric(ui, pal, "CORE", |ui| {
                        ui.label(RichText::new(row.p.core.as_deref().unwrap_or("n/a")).size(13.0).monospace().color(pal.ink));
                    });
                }
            }
            match row.ptype {
                ProjectType::Node => {
                    metric(ui, pal, "SERVICE", |ui| {
                        ui.label(RichText::new(&row.p.service).size(13.0).monospace().color(pal.ink));
                    });
                }
                _ => {
                    metric(ui, pal, "DATABASE", |ui| {
                        let db = row.p.db_status.as_deref().unwrap_or("n/a");
                        let col = if db == "Connected" { pal.good } else { pal.warn };
                        ui.label(RichText::new(db).size(13.0).monospace().color(col));
                    });
                }
            }
            ui.end_row();

            // TLS + CONTAINER UP (both)
            metric(ui, pal, "TLS EXPIRES", |ui| {
                let txt = if row.p.tls.is_empty() {
                    "—".to_string()
                } else {
                    row.p.tls.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
                };
                ui.label(RichText::new(txt).size(13.0).monospace().color(pal.ink));
            });
            metric(ui, pal, "CONTAINER UP", |ui| {
                let restarts: i64 = row.p.restarts.parse().unwrap_or(0);
                ui.label(RichText::new(fmt_since(&row.p.started)).size(13.0).monospace().color(pal.ink));
                let col = if restarts > 0 { pal.warn } else { pal.mute };
                ui.label(RichText::new(format!("· {restarts}⟳")).size(11.0).monospace().color(col));
            });
            ui.end_row();

            // MEASUREMENT + TRAFFIC — the analytics lane. Every other cell on
            // this card says the property *serves*; these say whether it is
            // actually being *measured*, which fails independently and silently.
            let emitted = row.http.as_ref().and_then(|h| h.emitted_tag.as_deref());
            let verdict = derive_measurement(emitted, &row.analytics);
            metric(ui, pal, "MEASUREMENT", |ui| {
                let col = match verdict {
                    Measurement::Measured => pal.good,
                    Measurement::Blind | Measurement::Dark | Measurement::Unowned => pal.crit,
                    Measurement::NeverRecorded => pal.warn,
                    Measurement::NotProvisioned | Measurement::Unknown => pal.mute,
                };
                ui.label(RichText::new(verdict.label()).size(13.0).monospace().color(col));
                if let Some(tag) = emitted {
                    ui.label(RichText::new(format!("· {tag}")).size(11.0).monospace().color(pal.mute));
                }
            });
            metric(ui, pal, "USERS · 7D", |ui| {
                let (txt, col) = match &row.analytics {
                    AnalyticsState::Ok(a) => (
                        format!("{} · {} sessions", a.users_recent, a.sessions_recent),
                        if a.users_recent > 0 { pal.ink } else { pal.warn },
                    ),
                    AnalyticsState::Loading => ("checking…".to_string(), pal.mute),
                    AnalyticsState::AuthExpired => ("auth expired".to_string(), pal.warn),
                    AnalyticsState::Error(_) => ("unavailable".to_string(), pal.mute),
                    AnalyticsState::NoProperty => ("—".to_string(), pal.mute),
                    AnalyticsState::Disabled => ("not configured".to_string(), pal.mute),
                };
                ui.label(RichText::new(txt).size(13.0).monospace().color(col));
            });
            ui.end_row();
        });
}

fn host_panel(ui: &mut egui::Ui, pal: &Palette, fleet: &Fleet) {
    let h = fleet.host.as_ref().unwrap();
    let traefik = if fleet.rows.iter().any(|r| r.p.status == "running") { "running" } else { "down" };

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
        egui::Grid::new("host").num_columns(4).spacing([28.0, 14.0]).show(ui, |ui| {
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
    let r = resp.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, Vec2::new(3.0, r.height())),
        Rounding { nw: 10.0, sw: 10.0, ne: 0.0, se: 0.0 },
        col,
    );
}

// ── helpers ─────────────────────────────────────────────────────────────────

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

/// "7 weeks, 4 days, ..." → "7w 4d"
fn short_uptime(s: &str) -> String {
    let (mut weeks, mut days) = (None, None);
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
    let Some((date, _)) = started.split_once('T') else { return "—".into() };
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
            if ago >= 1 {
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
