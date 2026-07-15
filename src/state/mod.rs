pub mod merge;
pub mod view;

use crate::message::FeedMessage;
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;

const MAX_INFLATED_TOPIC_BYTES: usize = 32 * 1024 * 1024;

/// Latest known on-track position of one car, in the circuit's coordinate space.
#[derive(Debug, Clone)]
pub struct CarPosition {
    pub x: f64,
    pub y: f64,
    pub on_track: bool,
}

/// The merged session state: one JSON tree per feed topic, patched in place
/// as delta messages arrive, plus a specially-maintained per-car position map.
#[derive(Default)]
pub struct SessionState {
    topics: HashMap<String, Value>,
    pub positions: HashMap<String, CarPosition>,
    pub dirty: bool,
    /// Count of feed messages dropped because they failed to decode/inflate
    /// (surfaced in the footer so silent corruption is visible — plan 2.10).
    pub dropped: u32,
}

impl SessionState {
    /// Drop all merged topics and positions but keep the running `dropped`
    /// count. Used on live reconnect so a fresh snapshot starts clean (plan 2.9).
    pub fn reset(&mut self) {
        self.topics.clear();
        self.positions.clear();
        self.dirty = true;
    }

    pub fn apply(&mut self, msg: FeedMessage) {
        let (topic, data) = if let Some(name) = msg.topic.strip_suffix(".z") {
            match inflate_topic(&msg.data) {
                Ok(v) => (name.to_string(), v),
                Err(_) => {
                    self.dropped = self.dropped.saturating_add(1);
                    self.dirty = true;
                    return;
                }
            }
        } else {
            (msg.topic, msg.data)
        };

        if topic == "Position" {
            self.apply_positions(&data);
            self.dirty = true;
            return;
        }
        if topic == "CarData" {
            return; // telemetry channels unused for now
        }

        let merged = match self.topics.get_mut(&topic) {
            Some(slot) => merge::merge(slot, data),
            None => {
                self.topics.insert(topic, data);
                true
            }
        };
        if !merged {
            self.dropped = self.dropped.saturating_add(1);
        }
        self.dirty = true;
    }

    pub fn topic(&self, name: &str) -> Option<&Value> {
        self.topics.get(name)
    }

    /// Position payload: {"Position": [{"Timestamp": ..., "Entries": {"44": {"X":..,"Y":..,"Status":".."}}}]}
    fn apply_positions(&mut self, data: &Value) {
        let Some(batches) = data.get("Position").and_then(|v| v.as_array()) else {
            return;
        };
        for batch in batches {
            let Some(entries) = batch.get("Entries").and_then(|v| v.as_object()) else {
                continue;
            };
            for (num, e) in entries {
                let Some(x) = e.get("X").and_then(num_f64).filter(|v| v.is_finite()) else {
                    continue;
                };
                let Some(y) = e.get("Y").and_then(num_f64).filter(|v| v.is_finite()) else {
                    continue;
                };
                if x == 0.0 && y == 0.0 {
                    continue; // no fix yet
                }
                let on_track = e.get("Status").and_then(|s| s.as_str()) == Some("OnTrack");
                self.positions
                    .insert(num.clone(), CarPosition { x, y, on_track });
            }
        }
    }
}

fn num_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// `.z` topics carry a JSON string of base64-encoded raw-deflate JSON.
pub fn inflate_topic(data: &Value) -> Result<Value> {
    inflate_topic_with_limit(data, MAX_INFLATED_TOPIC_BYTES)
}

fn inflate_topic_with_limit(data: &Value, limit: usize) -> Result<Value> {
    let b64 = data
        .as_str()
        .context("compressed topic payload is not a string")?;
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("invalid base64 in compressed topic")?;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(&compressed[..])
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut out)
        .context("deflate decode failed")?;
    if out.len() > limit {
        anyhow::bail!("inflated topic exceeds {limit} bytes");
    }
    let out = String::from_utf8(out).context("inflated topic is not UTF-8")?;
    Ok(serde_json::from_str(&out)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    #[test]
    fn inflate_rejects_output_over_limit() {
        let json = format!("\"{}\"", "x".repeat(16));
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(json.as_bytes()).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap());

        assert!(inflate_topic_with_limit(&Value::String(encoded), 16).is_err());
    }

    #[test]
    fn positions_require_two_finite_coordinates() {
        let mut state = SessionState::default();
        state.apply(FeedMessage {
            topic: "Position".into(),
            data: serde_json::json!({"Position": [{"Entries": {
                "1": {"X": "inf", "Y": 10},
                "2": {"Y": 10},
                "3": {"X": 5, "Y": 10, "Status": "OnTrack"}
            }}]}),
            ts: None,
        });

        assert_eq!(state.positions.len(), 1);
        assert!(state.positions.contains_key("3"));
    }
}
