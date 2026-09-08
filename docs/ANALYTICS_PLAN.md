# Feature plan — analytics in Lighthouse

> Status: proposed, 2026-09-07. Nothing here is implemented yet.

## The gap this closes

Lighthouse answers **"is the property up?"**. It cannot answer **"is the
property being measured?"** Those two fail independently, and the second failure
is silent.

The case that proves it: `zero-shot-games` emitted a correct GA4 tag and recorded
**zero events for six weeks** (2026-07-26 → 2026-09-04). Consent Mode v2 was set
to `analytics_storage:'denied'` with `wait_for_update:500`, and no consent
platform ever issued the update — one was assumed and never configured. `gtag.js`
loaded on every page and measured nothing. Throughout, *every signal Lighthouse
has today was green*: container running, HTTP 200, Drupal bootstrapped, DB
connected, TLS valid. The board would have shown a healthy property for six weeks
while the business signal was dead.

An HTTP probe cannot see this. `drush` cannot see this. Only the analytics
back-end knows, and asking it is one HTTPS call.

## Principles this keeps

The README makes two architectural commitments. Both survive intact.

- **Off-box.** The API calls run on the workstation, exactly like the existing
  HTTPS probe. No credential, no agent, and no new surface on the VPS.
- **Discovery, not a hardcoded list.** No hand-maintained property map. The
  measurement id comes from *what the page emits*; the emitted id resolves to a
  GA4 property through the Admin API. A newly-deployed property with a tag is
  covered the moment it ships, same as TLS targets are today.

A third commitment is added:

- **Analytics never gates health.** It is an independent lane with its own
  cadence and its own failure states. A dead credential must never delay or
  degrade the health board.

---

## P0 — prerequisite bug fix (do this first, it is not optional)

`scripts/gather.sh` picks each property's public URL as the **first non-`www`,
non-`api` Traefik `Host()` label, alphabetically**:

```bash
for h in "${hosts[@]}"; do case "$h" in www.*|api.*) ;; *) primary="$h"; break;; esac; done
url="https://$primary"
```

Running the real `gather.sh` against the node on 2026-09-07 shows **three of six
properties are probed at the wrong hostname**:

| slug | apex (correct) | url actually probed | TLS read |
| --- | --- | --- | --- |
| monpetitcafe | `monpetitcafe.com.co` | `monpetitcafe.co` | wrong cert |
| myevery | `myevery.ai` | `myevery.tecnocratica.com.co` | — *(see below)* |
| oidoenvivo | `oidoenvivo.com` | `oidoenvivo.club` | **empty** |
| tecnocratica, tempowatch, zero-shot-games | — | correct | correct |

`oidoenvivo` is the worst case. Its first label is `oidoenvivo.club`, whose DNS
still points at **GoDaddy parking**, not the VPS. So today Lighthouse probes a
114-byte parking redirect, gets **HTTP 200**, and reports the property healthy on
the strength of a page that is not the site. Its TLS field comes back **empty**,
so cert-expiry warning is silently blind for that property.

`monpetitcafe.co` is a redirect-only domain. The probe follows the redirect and
still returns 200, so health is accidentally right, but latency measures a
redirect chain and the TLS expiry belongs to the wrong certificate.

**`myevery` is the trap, and it is why the fix must be guarded.** Its apex is the
directory name `myevery.ai`, which is deliberately email/redirect-only and was
never registered with Traefik — `myevery.tecnocratica.com.co` is the correct
target. A naive "always trust the apex" fix would break the one property that is
currently right. Prefer the apex **only when it actually appears in the label
set**:

```bash
primary=""
printf '%s\n' "${hosts[@]}" | grep -qx "$apex" && primary="$apex"
if [ -z "$primary" ]; then
  for h in "${hosts[@]}"; do case "$h" in www.*|api.*) ;; *) primary="$h"; break;; esac; done
fi
```

Verified against the live label sets: `monpetitcafe.com.co` and `oidoenvivo.com`
are both present, so both are corrected; `myevery.ai` is absent, so that property
falls through to today's behaviour unchanged.

This is the identical defect that produced a false failure in the fleet's
`analytics-audit` script on 2026-09-07, fixed there the same way. It matters
doubly for this feature: emission scraping against a parking page would find no
tag and report a live property as **DARK**.

---

## Data sources

A third source joins the two the README documents.

| Source | Runs where | Provides |
| --- | --- | --- |
| HTTPS probe (`reqwest`) | workstation | *(existing)* status, latency — **plus** the `G-…` and `ca-pub-…` ids the page emits |
| `gather.sh` over SSH | workstation → VPS | *(existing, unchanged)* |
| GA4 Data + Admin API | workstation | measurement-id → property map; sessions, users, event recency |

## Auth — no new dependencies

Reuse the credential the fleet already maintains: `~/.gcp/analytics-adc.json`, an
`authorized_user` ADC holding `client_id`, `client_secret`, `refresh_token`.
Two plain HTTPS calls, no Google SDK:

1. `POST https://oauth2.googleapis.com/token` with `grant_type=refresh_token`
   → a ~60-minute access token, cached in memory only.
2. `POST https://analyticsdata.googleapis.com/v1beta/properties/{id}:runReport`
   with `Authorization: Bearer …`.

`reqwest` (blocking, rustls, json) and `serde_json` are already in `Cargo.toml`.
**This feature adds zero crates.**

The OAuth app is deliberately left unpublished, so the refresh token expires
every 7 days and is re-minted by hand. That is a standing operational fact, not a
bug to design around — see P4, which turns it into a visible countdown instead of
a surprise.

Never log or persist token material. The ADC file holds a client secret.

---

## Model changes (`src/model.rs`)

```rust
pub struct Analytics {
    pub property_id: String,       // "properties/552738608"
    pub measurement_id: String,    // "G-9E24KLH3E1"
    pub users_28d: u64,
    pub sessions_28d: u64,
    pub users_today: u64,
    pub last_event_date: Option<String>,  // YYYYMMDD, property timezone
    pub days_series: Vec<u64>,     // last 7 days of users, for a sparkline
}

pub enum AnalyticsState {
    Disabled,                 // no credential file present
    Loading,
    Ok(Analytics),
    NoProperty,               // emits a tag, no GA4 property matches it
    NotProvisioned,           // emits nothing, no property — informational
    AuthExpired,              // refresh token dead — actionable, not an error
    Error(String),
}
```

`HttpProbe` gains `emitted_tag: Option<String>` and `emitted_adsense:
Option<String>`. `Row` gains `analytics: AnalyticsState`.

`http_probe()` currently discards the response body. It must read it, capped at
~256 KB, and scrape the ids with the same pattern `analytics-audit` uses.

---

## P2 is the payoff — reconciliation

Crossing *emission* against *data* yields four verdicts. This is the whole point
of the feature; the numbers on the card are secondary.

| Emits a tag | Has recent data | Verdict | Severity |
| --- | --- | --- | --- |
| yes | yes | measured | ok |
| yes | **no** | **BLIND** — tag ships, nothing recorded | **crit** |
| no | property has history | **DARK** — tag stopped shipping | **crit** |
| no | no property | not provisioned | info |

BLIND is the zero-shot-games failure. DARK is a lost env var or a bad deploy.
Neither is visible to any signal Lighthouse has today.

Guard against false alarms on genuinely new properties: only raise BLIND when the
property has **at least one prior day with events** and has since been silent for
7 days. A property that has never recorded anything is `NotProvisioned`, which is
a backlog item, not a fault — the same distinction `analytics-audit` draws
between *dark* and *drift*.

These feed `derive_gaps()` as ordinary `Gap` rows, so they surface in the
existing gaps panel with no new UI surface.

---

## Cadence and quota

Analytics must not ride `auto_refresh_secs`. Add a separate
`analytics_refresh_secs`, default **900** (15 min). The measurement-id → property
map is cached to the config directory and refreshed daily. The realtime endpoint
is called **only on an explicit click**, never on a timer — it has its own quota
and answers a different question.

At five properties on a 15-minute cadence this is trivial against the Data API's
per-property daily token budget.

---

## Phases

| Phase | Scope | Rough lift |
| --- | --- | --- |
| **P0** | `gather.sh` apex fix | ~10 lines |
| **P1** | New `src/analytics.rs`: token refresh, Admin-API property map, one `runReport`. Wired into `--probe` JSON only. No UI. | ~150 lines + ~20 wiring |
| **P2** | Emission scraping in `http_probe`, the four verdicts, gap rows, a small badge on each card | ~80 lines |
| **P3** | Card detail: 7-day sparkline, top 3 pages, realtime users on click | ~120 lines egui |
| **P4** | Token lifecycle: "expires in N days" and a Refresh button that runs the mint command and shows the URL to click | ~80 lines |

P1 ships invisibly and is verifiable headlessly through `cargo run -- --probe`,
which keeps the risky part away from the UI. P2 is where the feature earns its
place. P4 turns the recurring 7-day chore into something the board reminds you
about instead of something that fails silently.

---

## Landmines

- **Do not introduce `tokio`.** The app is blocking by design and `collect()`
  already runs on a background thread. Analytics belongs on a sibling thread with
  its own channel, not an async runtime bolted under the UI.
- **Dates are in the property's timezone** (`America/Bogota`), not the
  workstation's. Comparing a GA4 `date` dimension against a local date will
  produce off-by-one "silent property" false alarms around midnight.
- **`can_edit: false` or a lost scope is `NoProperty`, not `Error`** — do not
  turn a permissions change into a red board.
- **Reading the probe body costs a full page fetch per property per refresh.**
  Cap the read; do not follow into assets.
- Windows locks `target\debug\lighthouse.exe` while the app runs; `taskkill //F
  //IM lighthouse.exe` before rebuilding.
- Push from WSL so the `github-ssdnodes` alias resolves.

## What this makes redundant

`infra/scripts/analytics-audit` in the webrunners repo (`I:\web_server`) audits
by emission and is the current source of truth for "is analytics running". Once
P2 lands, Lighthouse does that plus the data half the shell script structurally
cannot see. Keep the script as the headless CI check, or retarget it at
`lighthouse --probe`, but stop maintaining two answers to the same question.
