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
