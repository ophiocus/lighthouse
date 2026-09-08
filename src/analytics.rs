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

/// Days of history requested. 28 aligns with the reporting window used
/// elsewhere in the fleet docs.
const WINDOW_DAYS: u32 = 28;
/// How many trailing days of the series to keep for a sparkline.
const SERIES_DAYS: usize = 7;

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
    #[serde(default)]
    totals: Vec<ReportRow>,
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

/// Pull the reporting window for one property.
///
/// Dates come back in the **property's** timezone, not the workstation's. They
/// are kept as opaque `YYYYMMDD` strings and only ever compared to each other —
/// never to a local date, which would produce off-by-one "silent property"
/// alarms around midnight.
pub fn fetch(
    client: &reqwest::blocking::Client,
    token: &str,
    prop: &GaProperty,
) -> Result<Analytics, Error> {
    let body = serde_json::json!({
        "dateRanges": [{ "startDate": format!("{}daysAgo", WINDOW_DAYS - 1), "endDate": "today" }],
        "dimensions": [{ "name": "date" }],
        "metrics": [{ "name": "activeUsers" }, { "name": "sessions" }],
        "metricAggregations": ["TOTAL"],
        "orderBys": [{ "dimension": { "dimensionName": "date" } }],
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

    // Daily rows, already ordered by date ascending.
    let mut series: Vec<u64> = Vec::new();
    let mut last_event_date: Option<String> = None;
    let mut active_days = 0usize;

    for row in &r.rows {
        let date = row.dimension_values.first().map(|d| d.value.clone()).unwrap_or_default();
        let users = num(row.metric_values.first());
        series.push(users);
        if users > 0 {
            active_days += 1;
            last_event_date = Some(date);
        }
    }

    // `metricAggregations: TOTAL` gives a correctly de-duplicated user count for
    // the whole range — summing the daily rows would double-count returning
    // visitors and quietly inflate every card.
    let (users_total, sessions_total) = match r.totals.first() {
        Some(t) => (num(t.metric_values.first()), num(t.metric_values.get(1))),
        None => (0, 0),
    };

    let series = series
        .iter()
        .rev()
        .take(SERIES_DAYS)
        .rev()
        .copied()
        .collect::<Vec<u64>>();

    Ok(Analytics {
        property_id: prop.property_id.clone(),
        display_name: prop.display_name.clone(),
        measurement_id: prop.measurement_id.clone(),
        users_window: users_total,
        sessions_window: sessions_total,
        window_days: WINDOW_DAYS,
        active_days,
        last_event_date,
        series,
    })
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
