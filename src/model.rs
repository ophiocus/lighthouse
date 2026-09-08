//! Data model: the discovery payload from `scripts/gather.sh` ([`Gather`] →
//! [`Project`]) plus the derived, render-ready [`Fleet`]. Project *type* drives
//! which control template applies (see `template.rs`).

use serde::Deserialize;

// ── Discovery payload ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Gather {
    pub generated: String,
    pub projects: Vec<Project>,
    pub host: Host,
}

/// One discovered project on the VPS, classified by its compose stack.
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub slug: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub service: String,
    pub container: String,
    /// db container name (Drupal stacks) — reserved for future db-level actions.
    #[allow(dead_code)]
    pub db_container: String,
    pub apex: String,
    pub url: String,
    pub probe_path: String,
    pub domains: Vec<String>,
    pub status: String,
    pub health: String,
    pub started: String,
    pub restarts: String,
    #[allow(dead_code)]
    pub image: String,
    pub core: Option<String>,
    pub maintenance: Option<String>,
    pub db_status: Option<String>,
    pub tls: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Host {
    pub disk: String,
    pub disk_pct: u8,
    pub mem: String,
    pub mem_pct: u8,
    pub load: String,
    pub uptime: String,
    pub upgradable: u32,
    pub security: u32,
    pub reboot: String,
}

// ── Project type — the axis the control template keys off ───────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Unknown is a defensive fallback for unclassified stacks
pub enum ProjectType {
    Drupal,
    Node,
    Unknown,
}

impl ProjectType {
    pub fn parse(s: &str) -> Self {
        match s {
            "drupal" => ProjectType::Drupal,
            "node" => ProjectType::Node,
            _ => ProjectType::Unknown,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ProjectType::Drupal => "Drupal",
            ProjectType::Node => "Node",
            ProjectType::Unknown => "Unknown",
        }
    }
}

// ── HTTP probe (measured from the workstation) ──────────────────────────────

#[derive(Debug, Clone)]
pub struct HttpProbe {
    pub code: u16,
    pub latency_ms: u128,
    pub ok: bool,
}

// ── Analytics (GA4 Data API, also measured from the workstation) ────────────

/// What a GA4 property actually recorded. Distinct from every other signal on
/// this board: those say the property *serves*, this says it is *measured*.
#[derive(Debug, Clone)]
pub struct Analytics {
    pub property_id: String,
    pub display_name: String,
    pub measurement_id: String,
    /// De-duplicated users over the window (API TOTAL, not a sum of days).
    pub users_window: u64,
    pub sessions_window: u64,
    pub window_days: u32,
    /// Days in the window that recorded at least one user. Zero here on a
    /// property that serves fine is the "measured but blind" failure.
    pub active_days: usize,
    /// Most recent day with events, `YYYYMMDD` in the *property's* timezone.
    /// Compare only against other API dates, never against a local date.
    pub last_event_date: Option<String>,
    /// Trailing daily users, oldest first, for a sparkline.
    pub series: Vec<u64>,
}

/// Analytics is an independent lane and never gates health, so every outcome —
/// including "not configured" — is a state rather than an error.
#[derive(Debug, Clone)]
pub enum AnalyticsState {
    /// No credential on this workstation. Not a fault.
    Disabled,
    Ok(Analytics),
    /// No GA4 property matches this site.
    NoProperty,
    /// Refresh token dead. Expected roughly weekly; an operator action.
    AuthExpired,
    Error(String),
}

// ── Derived, render-ready fleet ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Up,
    Warn,
    Down,
    Unknown,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Health::Up => "healthy",
            Health::Warn => "degraded",
            Health::Down => "down",
            Health::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sev {
    Crit,
    Warn,
}

#[derive(Debug, Clone)]
pub struct Gap {
    pub sev: Sev,
    pub label: String,
    pub text: String,
}

/// A project merged with its workstation HTTP probe and derived health.
#[derive(Debug, Clone)]
pub struct Row {
    pub p: Project,
    pub ptype: ProjectType,
    pub http: Option<HttpProbe>,
    pub health: Health,
    pub analytics: AnalyticsState,
}

#[derive(Debug, Clone)]
pub struct Fleet {
    pub generated: String,
    pub rows: Vec<Row>,
    pub host: Option<Host>,
    pub gaps: Vec<Gap>,
    pub healthy: usize,
    pub total: usize,
    pub drupal_count: usize,
    pub node_count: usize,
}
