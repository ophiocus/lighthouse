//! Data model: the raw shape emitted by `scripts/gather.sh` (deserialized as
//! [`Gather`]) plus the derived, render-ready [`Fleet`] the UI draws from.

use serde::Deserialize;

// ── Raw gather payload (mirrors gather.sh JSON) ─────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Gather {
    pub generated: String,
    pub containers: Vec<Container>,
    pub drupal: Vec<DrupalNode>,
    pub tls: Vec<TlsCert>,
    pub host: Host,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Container {
    pub name: String,
    pub status: String,
    pub health: String,
    pub started: String,
    pub restarts: String,
    /// Image ref (e.g. `ghcr.io/ophiocus/tempowatch:latest`). Surfaced in the
    /// `--probe` report; reserved for a future "image drift" check in the UI.
    #[allow(dead_code)]
    pub image: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DrupalNode {
    pub name: String,
    pub core: String,
    pub maintenance: String,
    pub db: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsCert {
    pub domain: String,
    pub not_after: String,
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

// ── HTTP probe (measured from the workstation) ──────────────────────────────

#[derive(Debug, Clone)]
pub struct HttpProbe {
    pub code: u16,
    pub latency_ms: u128,
    /// True when the code equals the property's expected healthy status.
    pub ok: bool,
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

/// One property, merged from registry + HTTP probe + host gather.
#[derive(Debug, Clone)]
pub struct Row {
    pub slug: String,
    pub name: String,
    pub stack: String,
    pub url: String,
    pub probe_path: String,
    pub domains: Vec<String>,
    pub http: Option<HttpProbe>,
    pub container: Option<Container>,
    pub drupal: Option<DrupalNode>,
    pub tls_not_after: Option<String>,
    pub health: Health,
}

/// The whole board.
#[derive(Debug, Clone)]
pub struct Fleet {
    pub generated: String,
    pub rows: Vec<Row>,
    pub host: Option<Host>,
    pub gaps: Vec<Gap>,
    pub containers_running: usize,
    pub containers_total: usize,
    pub props_up: usize,
    pub props_total: usize,
}
