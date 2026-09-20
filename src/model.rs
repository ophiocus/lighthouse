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
    /// GA4 measurement id found in the served HTML, e.g. `G-ABC123XYZ`.
    /// Emission is the only trustworthy evidence that a tag is really shipping:
    /// committed config shows placeholders on an env-driven site, and `.env`
    /// shows a value the running container may not have picked up.
    pub emitted_tag: Option<String>,
    /// AdSense publisher id found in the served HTML, e.g. `ca-pub-123…`.
    pub emitted_adsense: Option<String>,
}

// ── Analytics (GA4 Data API, also measured from the workstation) ────────────

/// What a GA4 property actually recorded. Distinct from every other signal on
/// this board: those say the property *serves*, this says it is *measured*.
#[derive(Debug, Clone)]
pub struct Analytics {
    pub property_id: String,
    pub display_name: String,
    pub measurement_id: String,
    pub recent_days: u32,
    pub year_days: u32,
    /// Users in the recent window — "is it measured *now*".
    pub users_recent: u64,
    pub sessions_recent: u64,
    /// Users over the long window — "has it *ever* been measured". The pair is
    /// what separates a property that has gone blind from one just provisioned.
    pub users_year: u64,
    pub sessions_year: u64,
}

/// Analytics is an independent lane and never gates health, so every outcome —
/// including "not configured" — is a state rather than an error.
#[derive(Debug, Clone)]
pub enum AnalyticsState {
    /// No credential on this workstation. Not a fault.
    Disabled,
    /// The health board has painted; the analytics sweep is still in flight.
    /// Analytics runs in its own pass precisely so this state is visible rather
    /// than being hidden behind a longer spinner.
    Loading,
    Ok(Analytics),
    /// No GA4 property matches this site.
    NoProperty,
    /// Refresh token dead. Expected roughly weekly; an operator action.
    AuthExpired,
    Error(String),
}

/// The verdict from crossing **what the page emits** against **what the property
/// recorded**. This is the point of the analytics lane: the two disagree in
/// exactly the ways no other signal on this board can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measurement {
    /// Tag ships and data is arriving.
    Measured,
    /// Tag ships, the property has recorded before, and nothing has arrived in
    /// the recent window. The six-week failure. Every other signal stays green.
    Blind,
    /// Tag ships but the property has never recorded anything in a year. Either
    /// brand new, or blind since birth — worth a look, not an alarm.
    NeverRecorded,
    /// No tag on the page, yet the property has history. The tag stopped
    /// shipping: a lost env var, or a deploy that dropped the module.
    Dark,
    /// No tag, no data. Backlog, not a fault.
    NotProvisioned,
    /// The page ships a measurement id that **no property under this credential
    /// owns**. Under a single-owner analytics policy that is the ownership
    /// alarm: the tag is reporting into somebody else's account.
    Unowned,
    /// Analytics could not be consulted (no credential, dead token, error), so
    /// no verdict is possible. Never rendered as a fault.
    Unknown,
}

impl Measurement {
    pub fn label(self) -> &'static str {
        match self {
            Measurement::Measured => "measured",
            Measurement::Blind => "BLIND",
            Measurement::NeverRecorded => "never recorded",
            Measurement::Dark => "DARK",
            Measurement::NotProvisioned => "not provisioned",
            Measurement::Unowned => "UNOWNED TAG",
            Measurement::Unknown => "unknown",
        }
    }
}

/// Cross emission against recorded data. `emitted` is the measurement id found
/// in the served HTML, which is the only trustworthy evidence that the tag is
/// actually shipping — committed config and `.env` both lie about this.
pub fn derive_measurement(emitted: Option<&str>, a: &AnalyticsState) -> Measurement {
    let data = match a {
        AnalyticsState::Ok(d) => d,
        // A tag that resolves to no property this credential can see is not an
        // unknown — it is a tag reporting into an account we do not own.
        AnalyticsState::NoProperty if emitted.is_some() => return Measurement::Unowned,
        AnalyticsState::NoProperty => return Measurement::NotProvisioned,
        // No credential, expired token or an API error is an absence of
        // evidence, not evidence of absence.
        _ => return Measurement::Unknown,
    };
    // Sessions, not just users, decide whether anything was recorded. GA4 can
    // report activeUsers 0 alongside a non-zero session count, and on a
    // low-traffic property that is common. Keying on users alone called a
    // perfectly healthy property BLIND because two sessions landed with no
    // user attributed — a false critical, which is worse than no check at all.
    // A recorded session is proof the tag fired, which is the entire question.
    let recent = data.users_recent > 0 || data.sessions_recent > 0;
    let ever = data.users_year > 0 || data.sessions_year > 0;

    match (emitted.is_some(), recent, ever) {
        (true, true, _) => Measurement::Measured,
        (true, false, true) => Measurement::Blind,
        (true, false, false) => Measurement::NeverRecorded,
        (false, _, true) => Measurement::Dark,
        (false, _, false) => Measurement::NotProvisioned,
    }
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn data(users_recent: u64, users_year: u64) -> AnalyticsState {
        data4(users_recent, users_recent, users_year, users_year)
    }

    fn data4(
        users_recent: u64,
        sessions_recent: u64,
        users_year: u64,
        sessions_year: u64,
    ) -> AnalyticsState {
        AnalyticsState::Ok(Analytics {
            property_id: "properties/1".into(),
            display_name: "T".into(),
            measurement_id: "G-TESTID1".into(),
            recent_days: 7,
            year_days: 365,
            users_recent,
            sessions_recent,
            users_year,
            sessions_year,
        })
    }

    /// Regression, found against live data 2026-09-20: a real property reported
    /// activeUsers 0 with 2 sessions in the window and was called BLIND. A
    /// recorded session proves the tag fired, so this must read as measured.
    /// A false critical is worse than no check at all.
    #[test]
    fn sessions_without_attributed_users_still_count_as_measured() {
        assert_eq!(
            derive_measurement(Some("G-TESTID1"), &data4(0, 2, 18, 23)),
            Measurement::Measured
        );
    }

    /// The converse still has to hold: genuinely nothing recorded is BLIND.
    #[test]
    fn zero_users_and_zero_sessions_is_still_blind() {
        assert_eq!(
            derive_measurement(Some("G-TESTID1"), &data4(0, 0, 18, 23)),
            Measurement::Blind
        );
    }

    #[test]
    fn measured_when_tag_ships_and_data_arrives() {
        assert_eq!(
            derive_measurement(Some("G-TESTID1"), &data(5, 100)),
            Measurement::Measured
        );
    }

    /// The failure this whole lane exists for: the tag ships, the property has
    /// recorded before, and nothing has arrived recently. Every other signal on
    /// the board stays green throughout.
    #[test]
    fn blind_when_tag_ships_but_recent_is_silent() {
        assert_eq!(
            derive_measurement(Some("G-TESTID1"), &data(0, 100)),
            Measurement::Blind
        );
    }

    /// Distinct from Blind: nothing in a year means new, or blind since birth.
    /// A 28-day window could not tell these apart, which is why the long window
    /// exists.
    #[test]
    fn never_recorded_when_nothing_in_a_year() {
        assert_eq!(
            derive_measurement(Some("G-TESTID1"), &data(0, 0)),
            Measurement::NeverRecorded
        );
    }

    #[test]
    fn dark_when_tag_stopped_shipping_but_property_has_history() {
        assert_eq!(derive_measurement(None, &data(0, 100)), Measurement::Dark);
    }

    #[test]
    fn not_provisioned_when_neither_tag_nor_data() {
        assert_eq!(
            derive_measurement(None, &data(0, 0)),
            Measurement::NotProvisioned
        );
    }

    /// A tag no property under this credential owns is the single-owner alarm,
    /// not a shrug.
    #[test]
    fn unowned_when_tag_resolves_to_no_visible_property() {
        assert_eq!(
            derive_measurement(Some("G-SOMEONEELSE"), &AnalyticsState::NoProperty),
            Measurement::Unowned
        );
        assert_eq!(
            derive_measurement(None, &AnalyticsState::NoProperty),
            Measurement::NotProvisioned
        );
    }

    /// A dead credential must never be rendered as a property fault.
    #[test]
    fn auth_and_error_states_never_produce_a_fault() {
        for s in [
            AnalyticsState::AuthExpired,
            AnalyticsState::Disabled,
            AnalyticsState::Error("boom".into()),
        ] {
            assert_eq!(derive_measurement(Some("G-TESTID1"), &s), Measurement::Unknown);
        }
    }
}
