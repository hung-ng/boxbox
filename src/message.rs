use serde_json::Value;
use std::time::Duration;

pub const EVENT_CHANNEL_CAPACITY: usize = 4_096;
pub const CONTROL_CHANNEL_CAPACITY: usize = 64;

/// One update from the timing feed: a topic name and a JSON payload
/// (either a full snapshot or a delta patch to merge).
#[derive(Debug, Clone)]
pub struct FeedMessage {
    pub topic: String,
    pub data: Value,
    /// Offset from stream start. Present for replay, None for live.
    #[allow(dead_code)] // kept for debugging/recording use
    pub ts: Option<Duration>,
}

#[derive(Debug)]
pub enum SourceEvent {
    Message(FeedMessage),
    Info(String),
    /// Replay clock advanced (current sim offset).
    Clock(Duration),
    /// Circuit outline JSON from the MultiViewer API, tagged so a response from
    /// an earlier session cannot replace the current track.
    Circuit {
        key: i64,
        year: i64,
        data: Value,
    },
    /// Live feed is about to reconnect: drop merged state so removed keys from
    /// the previous session don't linger into the fresh snapshot.
    Reset,
    /// The next upcoming session, for the live empty state: a display string
    /// like "British Grand Prix — Qualifying (2026-07-04)".
    NextSession(String),
    Ended,
}

#[derive(Debug, Clone, Copy)]
pub enum PlaybackControl {
    SetSpeed(f64),
    TogglePause,
    /// Jump forward by this much sim time (messages in between apply instantly).
    Jump(Duration),
    /// Seek backward by this much sim time. The replay resets and fast-applies
    /// the whole prefix up to the new target (1.3).
    JumpBack(Duration),
    /// Absolute seek to this sim-time offset (clamped to the recording).
    /// `0` sends `SeekTo(ZERO)`; `g` sends `SeekTo(green)` with the green-flag
    /// time `main.rs` hands the UI alongside the timeline total.
    SeekTo(Duration),
}
