//! Small shared helpers — ports of `newToken`/`randomSlug`/`prettyBytes`
//! (auth.ts / handler.ts) plus date formatting the bot texts need.

use base64::Engine;
use rand::{Rng, RngCore};

/// `newToken(bytes)` — `crypto.randomBytes(bytes).toString("base64url")`.
pub fn new_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

const SLUG_CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// `randomSlug()` — 12 chars from the handler.ts alphabet.
pub fn random_slug() -> String {
    let mut s = String::with_capacity(12);
    let mut rng = rand::rng();
    for _ in 0..12 {
        let i = rng.random_range(0..SLUG_CHARS.len());
        s.push(SLUG_CHARS[i] as char);
    }
    s
}

/// `prettyBytes` — "900 B" / "12 KB" / "3.4 MB" / "1.20 GB".
pub fn pretty_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", n as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}

/// Civil date from unix seconds (Howard Hinnant's algorithm) — no chrono dep.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `new Date(unix*1000).toISOString().slice(0,10)` → "YYYY-MM-DD".
pub fn iso_date(unix: i64) -> String {
    let (y, m, d) = civil_from_days(unix.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `toLocaleDateString("en-GB", {day:"numeric", month:"short"})` → "5 Jan".
pub fn short_date(unix: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, m, d) = civil_from_days(unix.div_euclid(86400));
    format!("{d} {}", MONTHS[(m - 1) as usize])
}

/// `audit/YYYY-MM.jsonl` shard key — UTC month of `unix` seconds.
pub fn audit_shard_key(unix: i64) -> String {
    let (y, m, _) = civil_from_days(unix.div_euclid(86400));
    format!("audit/{y:04}-{m:02}.jsonl")
}

/// `parseInt(arg, 10)`-style prefix parse (lenient like JS, unlike `str::parse`).
pub fn parse_int_prefix(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|n| n * sign)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_and_slug_shapes() {
        let t = new_token(16);
        assert_eq!(t.len(), 22); // 16 bytes -> base64url unpadded
        let s = random_slug();
        assert_eq!(s.len(), 12);
        assert!(s.chars().all(|c| SLUG_CHARS.contains(&(c as u8))));
    }

    #[test]
    fn dates_match_js() {
        // 2025-01-15T12:00:00Z
        let unix = 1736942400;
        assert_eq!(iso_date(unix), "2025-01-15");
        assert_eq!(short_date(unix), "15 Jan");
        assert_eq!(audit_shard_key(unix), "audit/2025-01.jsonl");
    }

    #[test]
    fn int_prefix_like_parseint() {
        assert_eq!(parse_int_prefix("12"), Some(12));
        assert_eq!(parse_int_prefix("3x"), Some(3));
        assert_eq!(parse_int_prefix("abc"), None);
        assert_eq!(parse_int_prefix("-2"), Some(-2));
    }

    #[test]
    fn pretty_bytes_format() {
        assert_eq!(pretty_bytes(500), "500 B");
        assert_eq!(pretty_bytes(2048), "2 KB");
        assert_eq!(pretty_bytes(3 * 1024 * 1024 + 500_000), "3.5 MB");
        assert_eq!(pretty_bytes(5 * 1024 * 1024 * 1024), "5.00 GB");
    }
}
