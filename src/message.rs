use serde_json::Value;
use std::time::Duration;

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
    /// Circuit outline JSON from the MultiViewer API.
    Circuit(Value),
    Ended,
}

#[derive(Debug, Clone, Copy)]
pub enum PlaybackControl {
    SetSpeed(f64),
    TogglePause,
    /// Jump forward by this much sim time (messages in between apply instantly).
    Jump(Duration),
}
