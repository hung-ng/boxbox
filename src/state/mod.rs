pub mod merge;
pub mod view;

use crate::message::FeedMessage;
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;

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
}

impl SessionState {
    pub fn apply(&mut self, msg: FeedMessage) {
        let (topic, data) = if let Some(name) = msg.topic.strip_suffix(".z") {
            match inflate_topic(&msg.data) {
                Ok(v) => (name.to_string(), v),
                Err(_) => return,
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

        match self.topics.get_mut(&topic) {
            Some(slot) => merge::merge(slot, data),
            None => {
                self.topics.insert(topic, data);
            }
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
                let x = e.get("X").and_then(num_f64).unwrap_or(0.0);
                let y = e.get("Y").and_then(num_f64).unwrap_or(0.0);
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
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// `.z` topics carry a JSON string of base64-encoded raw-deflate JSON.
pub fn inflate_topic(data: &Value) -> Result<Value> {
    let b64 = data.as_str().context("compressed topic payload is not a string")?;
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("invalid base64 in compressed topic")?;
    let mut out = String::new();
    flate2::read::DeflateDecoder::new(&compressed[..])
        .read_to_string(&mut out)
        .context("deflate decode failed")?;
    Ok(serde_json::from_str(&out)?)
}
