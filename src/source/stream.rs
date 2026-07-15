use serde_json::Value;
use std::time::Duration;

/// Parse one F1 `.jsonStream` record: `HH:MM:SS.mmm{json}`.
pub fn parse_line(raw: &str) -> Option<(Duration, Value)> {
    let line = raw.trim_start_matches('\u{feff}').trim_end_matches('\r');
    let (stamp, json) = line.split_at_checked(12)?;
    let b = stamp.as_bytes();
    if b.get(2) != Some(&b':') || b.get(5) != Some(&b':') || b.get(8) != Some(&b'.') {
        return None;
    }
    let h: u64 = stamp.get(0..2)?.parse().ok()?;
    let m: u64 = stamp.get(3..5)?.parse().ok()?;
    let s: u64 = stamp.get(6..8)?.parse().ok()?;
    let ms: u64 = stamp.get(9..12)?.parse().ok()?;
    if m >= 60 || s >= 60 {
        return None;
    }
    let millis = h
        .checked_mul(60)?
        .checked_add(m)?
        .checked_mul(60)?
        .checked_add(s)?
        .checked_mul(1000)?
        .checked_add(ms)?;
    Some((
        Duration::from_millis(millis),
        serde_json::from_str(json).ok()?,
    ))
}
