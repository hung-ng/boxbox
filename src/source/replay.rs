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
        let Some(ts) = parse_offset(ts_str) else {
            continue;
        };
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

/// During catch-up, only this many trailing `Position.z` messages are
/// forwarded. Position is cumulative per car — each message overwrites that
/// car's fix — so older ones cannot affect the state at the target, and every
/// skipped message saves the UI a base64+inflate (a backward seek replays the
/// whole prefix: ~10k Position messages for a full race).
const CATCHUP_POSITION_KEEP: usize = 50;

/// How many catch-up messages to send between control-channel polls, so seeks
/// that arrive mid-catch-up (key mashing) fold into the running target instead
/// of each queuing its own full prefix replay.
const CATCHUP_POLL_EVERY: usize = 4096;

/// Play entries in sim time. Speed/pause/jump arrive on `ctrl`.
/// Messages skipped by a jump are still sent (instantly) so state stays correct.
///
/// Playback is driven by an index `cursor` into `entries` (rather than a
/// consuming iterator) so a backward seek can rewind to any earlier point
/// (1.1). On `JumpBack`, we emit a `Reset` and replay the whole prefix up to
/// the new target instantly, exactly like the initial `--start-at` seek (1.3).
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
    let mut cursor: usize = 0;
    let mut last_clock = Duration::ZERO;
    let mut ended = false; // Ended already sent; idle on ctrl until a rewind re-arms.
    // Recording length: forward seeks clamp here so the stamped clock can't
    // read past the end of the timeline.
    let total = entries.last().map(|e| e.ts).unwrap_or(Duration::ZERO);

    /// Apply a backward seek: reset, rewind the cursor, fast-apply the prefix.
    macro_rules! jump_back {
        ($d:expr) => {{
            let target = sim_t.saturating_sub($d);
            // Reset goes down the same channel before the backlog, so the UI can
            // never interleave old and new state (1.3 ordering guarantee).
            if tx.send(SourceEvent::Reset).is_err() {
                return;
            }
            cursor = 0;
            sim_t = target;
            skip_until = Some(target);
            paused = false;
            ended = false;
            // Clock emission is gated on `sim_t - last_clock` (saturating): a
            // high-water mark left over from before the rewind would starve
            // Clock events until playback re-passed it, freezing the UI
            // timeline at the seek target while the state plays on underneath.
            // Rewind it with the cursor.
            last_clock = target;
        }};
    }

    loop {
        // Past the last entry: send Ended once, then idle on control input only
        // so the session stays rewindable after it finishes (1.2).
        let Some(next_ts) = entries.get(cursor).map(|e| e.ts) else {
            if !ended {
                let _ = tx.send(SourceEvent::Ended);
                ended = true;
            }
            match ctrl.recv().await {
                Some(PlaybackControl::JumpBack(d)) => jump_back!(d),
                Some(PlaybackControl::SetSpeed(s)) => speed = s.max(0.1),
                // Pause/forward jump are no-ops at the end; keep idling.
                Some(_) => {}
                None => return,
            }
            continue;
        };

        // Instant catch-up region (initial seek or jump): fast-apply the whole
        // backlog up to the target in one pass, then stamp the clock.
        if let Some(target) = skip_until {
            let mut until = target.min(total);
            let mut since_poll = 0usize;
            'catchup: loop {
                // Entries [cursor, end) are the backlog for the current target.
                let end = cursor + entries[cursor..].partition_point(|e| e.ts <= until);
                // Only the trailing CATCHUP_POSITION_KEEP Position messages can
                // matter for the state at `until`; skip the dead weight.
                let mut positions_left = entries[cursor..end]
                    .iter()
                    .filter(|e| e.topic == "Position.z")
                    .count();
                while cursor < end {
                    let e = &entries[cursor];
                    if e.topic == "Position.z" {
                        positions_left -= 1;
                        if positions_left >= CATCHUP_POSITION_KEEP {
                            cursor += 1;
                            continue;
                        }
                    }
                    if tx
                        .send(SourceEvent::Message(FeedMessage {
                            topic: e.topic.to_string(),
                            data: e.data.clone(),
                            ts: Some(e.ts),
                        }))
                        .is_err()
                    {
                        return;
                    }
                    cursor += 1;
                    since_poll += 1;
                    // Absorb seeks that queue up mid-catch-up (key mashing)
                    // into the running target, so five ← presses cost one
                    // replay rather than five.
                    if since_poll >= CATCHUP_POLL_EVERY {
                        since_poll = 0;
                        let mut moved = false;
                        while let Ok(cmd) = ctrl.try_recv() {
                            match cmd {
                                PlaybackControl::Jump(d) => {
                                    until = (until + d).min(total);
                                    moved = true;
                                }
                                PlaybackControl::JumpBack(d) => {
                                    until = until.saturating_sub(d);
                                    moved = true;
                                }
                                PlaybackControl::SetSpeed(s) => speed = s.max(0.1),
                                PlaybackControl::TogglePause => paused = !paused,
                            }
                        }
                        if moved {
                            // Already sent past the new target? Start the
                            // prefix over, exactly like a fresh rewind.
                            if entries[..cursor]
                                .last()
                                .map(|e| e.ts > until)
                                .unwrap_or(false)
                            {
                                if tx.send(SourceEvent::Reset).is_err() {
                                    return;
                                }
                                cursor = 0;
                            }
                            continue 'catchup;
                        }
                    }
                }
                break;
            }
            sim_t = until;
            skip_until = None;
            last_clock = sim_t;
            let _ = tx.send(SourceEvent::Clock(sim_t));
            // The cursor moved (possibly past the last entry) — re-derive
            // next_ts, and let the end-of-entries branch handle an overrun.
            continue;
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
                    Some(PlaybackControl::JumpBack(d)) => jump_back!(d),
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

        let e = &entries[cursor];
        sim_t = e.ts;
        if tx
            .send(SourceEvent::Message(FeedMessage {
                topic: e.topic.to_string(),
                data: e.data.clone(),
                ts: Some(e.ts),
            }))
            .is_err()
        {
            return;
        }
        cursor += 1;
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
        ReplayEntry {
            ts: Duration::from_secs(secs),
            topic,
            data,
        }
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

    use crate::message::{PlaybackControl, SourceEvent};
    use std::time::Duration as D;

    /// Collect events until the catch-up's Clock stamp (inclusive).
    fn drain_until_clock(rx: &std::sync::mpsc::Receiver<SourceEvent>) -> Vec<SourceEvent> {
        let mut got = Vec::new();
        loop {
            let ev = rx.recv_timeout(D::from_secs(5)).expect("source stalled");
            let is_clock = matches!(ev, SourceEvent::Clock(_));
            got.push(ev);
            if is_clock {
                return got;
            }
        }
    }

    #[test]
    fn catchup_thins_positions_and_keeps_state_topics() {
        // 60 Position messages and one TrackStatus, all before the seek target:
        // the catch-up must forward every state topic but only the trailing
        // CATCHUP_POSITION_KEEP positions, then stamp the clock at the target.
        let mut entries: Vec<ReplayEntry> = (0..60)
            .map(|i| entry(i, "Position.z", json!("blob")))
            .collect();
        entries.push(entry(10, "TrackStatus", json!({"Status": "2"})));
        entries.push(entry(200, "TimingData", json!({"Lines": {}})));
        entries.sort_by_key(|e| e.ts);

        let (tx, rx) = std::sync::mpsc::channel();
        let (_ctrl_tx, ctrl_rx) = tokio::sync::mpsc::unbounded_channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.spawn(play(entries, D::from_secs(100), 1.0, tx, ctrl_rx));

        let got = drain_until_clock(&rx);
        let topics: Vec<String> = got
            .iter()
            .filter_map(|ev| match ev {
                SourceEvent::Message(m) => Some(m.topic.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            topics.iter().filter(|t| *t == "Position.z").count(),
            CATCHUP_POSITION_KEEP
        );
        assert_eq!(topics.iter().filter(|t| *t == "TrackStatus").count(), 1);
        assert_eq!(
            got.last()
                .map(|ev| matches!(ev, SourceEvent::Clock(t) if *t == D::from_secs(100))),
            Some(true)
        );
    }

    #[test]
    fn rewind_resumes_clock_events() {
        // Regression for the frozen-timeline bug: after a backward seek, a
        // stale `last_clock` high-water mark starved Clock events until sim
        // re-passed the pre-rewind point, so the UI timeline froze at the
        // target while the state played on. Rewind from 200s to 50s and expect
        // the first post-catch-up Clock to sit just past 50s, not past 200s.
        let entries: Vec<ReplayEntry> = (0..300)
            .map(|i| entry(i, "LapCount", json!({"CurrentLap": i})))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::unbounded_channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.spawn(play(entries, D::from_secs(200), 100.0, tx, ctrl_rx));

        drain_until_clock(&rx); // initial catch-up, Clock(200)
        ctrl_tx
            .send(PlaybackControl::JumpBack(D::from_secs(150)))
            .unwrap();
        // Reset, then the replayed prefix, then Clock(50).
        let got = drain_until_clock(&rx);
        assert!(matches!(got.first(), Some(SourceEvent::Reset)));
        assert_eq!(
            got.last()
                .map(|ev| matches!(ev, SourceEvent::Clock(t) if *t == D::from_secs(50))),
            Some(true)
        );
        // Playback resumed: the next Clock must be near the target, well
        // before the pre-rewind sim time.
        let next_clock = loop {
            if let SourceEvent::Clock(t) =
                rx.recv_timeout(D::from_secs(5)).expect("playback stalled")
            {
                break t;
            }
        };
        assert!(
            next_clock > D::from_secs(50) && next_clock < D::from_secs(120),
            "clock after rewind at {next_clock:?}, expected just past 50s"
        );
    }
}
