use super::archive::{Archive, Session, TOPICS};
use crate::message::{FeedMessage, PlaybackControl, SourceEvent};
use anyhow::Result;
use serde_json::Value;
use std::sync::mpsc::Sender;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

pub struct ReplayEntry {
    pub ts: Duration,
    pub topic: &'static str,
    pub data: Value,
}

/// Parse a `.jsonStream` body: each line is `HH:MM:SS.mmm{json}`.
fn parse_stream(topic: &'static str, body: &str, out: &mut Vec<ReplayEntry>) {
    for line in body.lines() {
        let line = line.trim_start_matches('\u{feff}').trim_end_matches('\r');
        if line.len() < 13 {
            continue;
        }
        let (ts_str, json) = line.split_at(12);
        let Some(ts) = parse_offset(ts_str) else { continue };
        let Ok(data) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        out.push(ReplayEntry { ts, topic, data });
    }
}

fn parse_offset(s: &str) -> Option<Duration> {
    // HH:MM:SS.mmm
    let h: u64 = s.get(0..2)?.parse().ok()?;
    let m: u64 = s.get(3..5)?.parse().ok()?;
    let sec: u64 = s.get(6..8)?.parse().ok()?;
    let ms: u64 = s.get(9..12)?.parse().ok()?;
    Some(Duration::from_millis(((h * 60 + m) * 60 + sec) * 1000 + ms))
}

/// Download every topic for a session and build the merged, time-sorted message list.
/// `progress` is called per topic as it is fetched.
pub async fn load_session(
    archive: &Archive,
    session: &Session,
    mut progress: impl FnMut(&str, usize),
) -> Result<Vec<ReplayEntry>> {
    let mut entries = Vec::new();
    for topic in TOPICS {
        match archive.fetch_stream(session, topic).await? {
            Some(body) => {
                progress(topic, body.len());
                parse_stream(topic, &body, &mut entries);
            }
            None => progress(topic, 0),
        }
    }
    entries.sort_by_key(|e| e.ts);
    Ok(entries)
}

/// Timestamp of the green flag: the first `SessionStatus` transition to
/// "Started". Replays default to seeking here so the viewer lands on racing
/// rather than the long pre-session grid/formation window (which stays in the
/// timeline and is still reachable by rewinding). `None` for sessions that
/// never report a start (e.g. an incomplete stream).
pub fn green_flag(entries: &[ReplayEntry]) -> Option<Duration> {
    entries
        .iter()
        .find(|e| {
            e.topic == "SessionStatus"
                && e.data.get("Status").and_then(Value::as_str) == Some("Started")
        })
        .map(|e| e.ts)
}

/// Play entries in sim time. Speed/pause/jump arrive on `ctrl`.
/// Messages skipped by a jump are still sent (instantly) so state stays correct.
pub async fn play(
    entries: Vec<ReplayEntry>,
    start_at: Duration,
    initial_speed: f64,
    tx: Sender<SourceEvent>,
    mut ctrl: UnboundedReceiver<PlaybackControl>,
) {
    let mut speed = initial_speed.max(0.1);
    let mut paused = false;
    let mut sim_t = start_at;
    let mut skip_until: Option<Duration> = Some(start_at); // fast-apply backlog before start point
    let mut iter = entries.into_iter().peekable();
    let mut last_clock = Duration::ZERO;

    loop {
        let Some(next_ts) = iter.peek().map(|e| e.ts) else {
            let _ = tx.send(SourceEvent::Ended);
            return;
        };

        // Instant catch-up region (initial seek or jump).
        if let Some(until) = skip_until {
            if next_ts <= until {
                let e = iter.next().unwrap();
                if tx
                    .send(SourceEvent::Message(FeedMessage {
                        topic: e.topic.to_string(),
                        data: e.data,
                        ts: Some(e.ts),
                    }))
                    .is_err()
                {
                    return;
                }
                continue;
            }
            sim_t = until;
            skip_until = None;
            let _ = tx.send(SourceEvent::Clock(sim_t));
        }

        // Wait out the gap to the next message, honoring control input.
        while paused || sim_t < next_ts {
            let wait = if paused {
                Duration::from_secs(3600)
            } else {
                let gap = next_ts - sim_t;
                Duration::from_secs_f64((gap.as_secs_f64() / speed).min(0.25))
            };
            // At high speed the remaining gap can round below timer resolution,
            // leaving sim_t a few ns short of next_ts forever (busy-spin). Snap
            // to next_ts and emit the message instead of sleeping on ~zero.
            if !paused && wait.is_zero() {
                sim_t = next_ts;
                break;
            }
            tokio::select! {
                cmd = ctrl.recv() => match cmd {
                    Some(PlaybackControl::SetSpeed(s)) => speed = s.max(0.1),
                    Some(PlaybackControl::TogglePause) => paused = !paused,
                    Some(PlaybackControl::Jump(d)) => {
                        skip_until = Some(sim_t + d);
                        paused = false;
                    }
                    None => return,
                },
                _ = tokio::time::sleep(wait) => {
                    if !paused {
                        sim_t = (sim_t + wait.mul_f64(speed)).min(next_ts);
                    }
                }
            }
            if skip_until.is_some() {
                break;
            }
            if sim_t >= next_ts {
                break;
            }
            // Push a clock tick so the UI can show sim time / speed while idle.
            if sim_t.saturating_sub(last_clock) >= Duration::from_millis(500) {
                last_clock = sim_t;
                let _ = tx.send(SourceEvent::Clock(sim_t));
            }
        }
        if skip_until.is_some() {
            continue;
        }

        let e = iter.next().unwrap();
        sim_t = e.ts;
        if tx
            .send(SourceEvent::Message(FeedMessage {
                topic: e.topic.to_string(),
                data: e.data,
                ts: Some(e.ts),
            }))
            .is_err()
        {
            return;
        }
        if sim_t.saturating_sub(last_clock) >= Duration::from_millis(200) {
            last_clock = sim_t;
            let _ = tx.send(SourceEvent::Clock(sim_t));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(secs: u64, topic: &'static str, data: Value) -> ReplayEntry {
        ReplayEntry { ts: Duration::from_secs(secs), topic, data }
    }

    #[test]
    fn green_flag_finds_first_started() {
        let entries = vec![
            entry(6, "SessionStatus", json!({"Status": "Inactive"})),
            entry(100, "TimingData", json!({"Lines": {}})),
            entry(3334, "SessionStatus", json!({"Status": "Started"})),
            entry(9000, "SessionStatus", json!({"Status": "Finished"})),
        ];
        assert_eq!(green_flag(&entries), Some(Duration::from_secs(3334)));
    }

    #[test]
    fn green_flag_none_when_never_started() {
        // Practice/incomplete streams never report a race start.
        let entries = vec![
            entry(6, "SessionStatus", json!({"Status": "Inactive"})),
            entry(100, "TimingData", json!({"Lines": {}})),
        ];
        assert_eq!(green_flag(&entries), None);
    }
}
