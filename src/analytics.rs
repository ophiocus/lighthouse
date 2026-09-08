//! GA4 analytics — a third telemetry source, measured from the workstation.
//!
//! This answers the question the HTTP probe structurally cannot: not "is the
//! property up" but **"is the property being measured"**. Those fail
//! independently. A property can serve 200s, bootstrap Drupal, hold a valid
//! cert and record nothing at all — that exact state ran for six weeks on one
//! property while every other signal on this board stayed green.
//!
//! Two design commitments, both inherited from the rest of the app:
//!
//! * **Off-box.** Every call here runs on the workstation, like the HTTP probe.
//!   No credential and no new surface on the VPS.
//! * **Discovery, not a hardcoded list.** Properties are matched to sites by the
//!   data stream's own configured URL, so a newly-provisioned property is picked
//!   up with nothing to hand-maintain.
//!
//! Blocking by design, like `telemetry` — call it off the UI thread.
//!
//! Auth reuses the Application Default Credentials file the fleet already keeps
//! (`~/.gcp/analytics-adc.json`, an `authorized_user` grant). Two plain HTTPS
//! calls, no Google SDK, no extra crates. **Never log the contents of that file
//! or the access token** — it carries a client secret and a refresh token.

use crate::model::Analytics;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ADMIN_BASE: &str = "https://analyticsadmin.googleapis.com/v1beta";
const DATA_BASE: &str = "https://analyticsdata.googleapis.com/v1beta";

/// The recent window — "is it being measured *now*".
const RECENT_DAYS: u32 = 7;
/// The long window — "has it *ever* been measured". Needed to tell a property
/// that has gone blind apart from one that was only just provisioned. A 28-day
/// window cannot make that distinction: the failure this feature exists to
/// catch ran for six weeks, so at 28 days it looks identical to a brand-new
/// property with no traffic yet.
const YEAR_DAYS: u32 = 365;

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Error {
    /// No ADC file on disk — analytics is simply not configured here.
    NoCredential,
    /// The refresh token is dead. Expected roughly weekly: the OAuth app is
    /// deliberately unpublished, so Google caps its refresh tokens at 7 days.
    /// This is an operator action, not a fault — surface it as such.
    AuthExpired,
    Http(String),
    Parse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoCredential => write!(f, "no analytics credential"),
            Error::AuthExpired => write!(f, "analytics auth expired — re-run mint_adc.py"),
            Error::Http(e) => write!(f, "analytics http: {e}"),
            Error::Parse(e) => write!(f, "analytics parse: {e}"),
        }
    }
}

// ── Credential ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Adc {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

/// Default ADC location. Matches what `mint_adc.py` writes.
pub fn default_credential_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gcp").join("analytics-adc.json"))
}

pub fn load_adc(path: &PathBuf) -> Result<Adc, Error> {
    let s = std::fs::read_to_string(path).map_err(|_| Error::NoCredential)?;
    serde_json::from_str::<Adc>(&s).map_err(|e| Error::Parse(format!("adc: {e}")))
}

// ── OAuth ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
}

/// Exchange the refresh token for a short-lived access token (~60 min).
/// Held in memory only; never written to disk.
pub fn access_token(client: &reqwest::blocking::Client, adc: &Adc) -> Result<String, Error> {
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", adc.client_id.as_str()),
            ("client_secret", adc.client_secret.as_str()),
            ("refresh_token", adc.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .map_err(|e| Error::Http(e.to_string()))?;

    let status = resp.status();
    let body = resp.text().unwrap_or_default();

    if !status.is_success() {
        // Google reports a dead/revoked refresh token as 400 invalid_grant.
        // Distinguish it: it is the weekly re-consent, not an outage.
        if body.contains("invalid_grant") {
            return Err(Error::AuthExpired);
        }
        return Err(Error::Http(format!("token endpoint {status}")));
    }

    serde_json::from_str::<TokenResp>(&body)
        .map(|t| t.access_token)
        .map_err(|e| Error::Parse(format!("token: {e}")))
}

// ── Admin API — discover properties and their web streams ───────────────────

/// One GA4 property, paired with the measurement id and site URL of its web
/// stream. `host` is what links a property to a project on the board.
#[derive(Debug, Clone)]
pub struct GaProperty {
    pub property_id: String,
    pub display_name: String,
    pub measurement_id: String,
    pub host: String,
}

#[derive(Deserialize)]
struct AccountSummaries {
    #[serde(default, rename = "accountSummaries")]
    account_summaries: Vec<AccountSummary>,
}
#[derive(Deserialize)]
struct AccountSummary {
    #[serde(default, rename = "propertySummaries")]
    property_summaries: Vec<PropertySummary>,
}
#[derive(Deserialize)]
struct PropertySummary {
    property: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
}

#[derive(Deserialize)]
struct DataStreams {
    #[serde(default, rename = "dataStreams")]
    data_streams: Vec<DataStream>,
}
#[derive(Deserialize)]
struct DataStream {
    #[serde(default, rename = "webStreamData")]
    web_stream_data: Option<WebStreamData>,
}
#[derive(Deserialize)]
struct WebStreamData {
    #[serde(default, rename = "measurementId")]
    measurement_id: String,
    #[serde(default, rename = "defaultUri")]
    default_uri: String,
}

/// Every web stream visible to this credential, across every account.
pub fn discover(
    client: &reqwest::blocking::Client,
    token: &str,
) -> Result<Vec<GaProperty>, Error> {
    let summaries: AccountSummaries = get_json(client, token, &format!("{ADMIN_BASE}/accountSummaries"))?;

    let mut out = Vec::new();
    for acct in &summaries.account_summaries {
        for prop in &acct.property_summaries {
            let url = format!("{ADMIN_BASE}/{}/dataStreams", prop.property);
            // One property failing to list must not sink the whole sweep.
            let streams: DataStreams = match get_json(client, token, &url) {
                Ok(s) => s,
                Err(Error::AuthExpired) => return Err(Error::AuthExpired),
                Err(_) => continue,
            };
            for s in &streams.data_streams {
                let Some(w) = &s.web_stream_data else { continue };
                if w.measurement_id.is_empty() {
                    continue;
                }
                out.push(GaProperty {
                    property_id: prop.property.clone(),
                    display_name: prop.display_name.clone(),
                    measurement_id: w.measurement_id.clone(),
                    host: host_of(&w.default_uri),
                });
            }
        }
    }
    Ok(out)
}

/// Bare host of a configured stream URI: "https://www.example.com/" → "example.com".
fn host_of(uri: &str) -> String {
    uri.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_ascii_lowercase()
}

// ── Data API — the numbers ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct ReportResp {
    #[serde(default)]
    rows: Vec<ReportRow>,
}
#[derive(Deserialize)]
struct ReportRow {
    #[serde(default, rename = "dimensionValues")]
    dimension_values: Vec<DimValue>,
    #[serde(default, rename = "metricValues")]
    metric_values: Vec<DimValue>,
}
#[derive(Deserialize)]
struct DimValue {
    #[serde(default)]
    value: String,
}

/// Pull both reporting windows for one property in a single request.
///
/// Two named date ranges and **no** explicit dimension: the API adds an implicit
/// `dateRange` dimension, so the response is exactly two rows, each already the
/// total for its range. That sidesteps two traps at once — summing daily rows
/// would double-count returning users, and building a per-day series would
/// require knowing "today" in the *property's* timezone, which is not the
/// workstation's and is the classic source of off-by-one "silent property"
/// false alarms around midnight. No dates are parsed here at all.
pub fn fetch(
    client: &reqwest::blocking::Client,
    token: &str,
    prop: &GaProperty,
) -> Result<Analytics, Error> {
    let body = serde_json::json!({
        "dateRanges": [
            { "name": "recent", "startDate": format!("{}daysAgo", RECENT_DAYS - 1), "endDate": "today" },
            { "name": "year",   "startDate": format!("{}daysAgo", YEAR_DAYS - 1),   "endDate": "today" },
        ],
        "metrics": [{ "name": "activeUsers" }, { "name": "sessions" }],
    });

    let url = format!("{DATA_BASE}/{}:runReport", prop.property_id);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|e| Error::Http(e.to_string()))?;

    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(Error::AuthExpired);
        }
        return Err(Error::Http(format!("runReport {status}")));
    }

    let r: ReportResp =
        serde_json::from_str(&text).map_err(|e| Error::Parse(format!("runReport: {e}")))?;

    // Two rows, identified by the implicit dateRange dimension. A range with no
    // data is omitted entirely rather than returned as zero, so default to zero
    // and only overwrite on a row that is actually present.
    let mut a = Analytics {
        property_id: prop.property_id.clone(),
        display_name: prop.display_name.clone(),
        measurement_id: prop.measurement_id.clone(),
        recent_days: RECENT_DAYS,
        year_days: YEAR_DAYS,
        users_recent: 0,
        sessions_recent: 0,
        users_year: 0,
        sessions_year: 0,
    };

    for row in &r.rows {
        let range = row.dimension_values.first().map(|d| d.value.as_str()).unwrap_or("");
        let users = num(row.metric_values.first());
        let sessions = num(row.metric_values.get(1));
        match range {
            "recent" => {
                a.users_recent = users;
                a.sessions_recent = sessions;
            }
            "year" => {
                a.users_year = users;
                a.sessions_year = sessions;
            }
            _ => {}
        }
    }

    Ok(a)
}

fn num(v: Option<&DimValue>) -> u64 {
    v.and_then(|m| m.value.parse::<u64>().ok()).unwrap_or(0)
}

fn get_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::blocking::Client,
    token: &str,
    url: &str,
) -> Result<T, Error> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .send()
        .map_err(|e| Error::Http(e.to_string()))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(Error::AuthExpired);
        }
        return Err(Error::Http(format!("{url} → {status}")));
    }
    serde_json::from_str::<T>(&text).map_err(|e| Error::Parse(e.to_string()))
}

/// Build the HTTP client used for every analytics call.
pub fn client() -> Result<reqwest::blocking::Client, Error> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("lighthouse/0.1")
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}
