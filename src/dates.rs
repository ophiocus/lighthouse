//! Tiny, dependency-free date math — just enough to turn an OpenSSL
//! `notAfter` string into "days from now". Avoids pulling in `chrono` for
//! a single subtraction. Uses Howard Hinnant's `days_from_civil` algorithm.

use std::time::{SystemTime, UNIX_EPOCH};

/// Serial day number for a proleptic-Gregorian date (days since 1970-01-01).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Today (UTC) as a serial day number.
fn today_serial() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    secs / 86_400
}

fn month_num(mon: &str) -> Option<i64> {
    Some(match mon {
        "Jan" => 1, "Feb" => 2, "Mar" => 3, "Apr" => 4,
        "May" => 5, "Jun" => 6, "Jul" => 7, "Aug" => 8,
        "Sep" => 9, "Oct" => 10, "Nov" => 11, "Dec" => 12,
        _ => return None,
    })
}

/// Parse an OpenSSL `notAfter`, e.g. `"Nov 13 01:20:47 2026 GMT"`, and return
/// the number of whole days from today (negative if already expired).
pub fn days_until(not_after: &str) -> Option<i64> {
    // Tokens: [Mon, DD, HH:MM:SS, YYYY, GMT]
    let parts: Vec<&str> = not_after.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let m = month_num(parts[0])?;
    let d: i64 = parts[1].parse().ok()?;
    let y: i64 = parts[3].parse().ok()?;
    Some(days_from_civil(y, m, d) - today_serial())
}
