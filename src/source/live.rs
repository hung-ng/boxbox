use crate::message::{FeedMessage, SourceEvent};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::mpsc::Sender;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// F1 moved live timing to SignalR Core (the classic /signalr endpoint 401s).
const NEGOTIATE_URL: &str =
    "https://livetiming.formula1.com/signalrcore/negotiate?negotiateVersion=1";
const CONNECT_URL: &str = "wss://livetiming.formula1.com/signalrcore";

/// SignalR Core frames are JSON records terminated by 0x1e.
const RECORD_SEP: char = '\u{1e}';

const LIVE_TOPICS: &[&str] = &[
    "Heartbeat",
    "SessionInfo",
    "SessionStatus",
    "TrackStatus",
    "LapCount",
    "ExtrapolatedClock",
    "WeatherData",
    "DriverList",
    "TimingData",
    "TimingAppData",
    "RaceControlMessages",
    "Position.z",
];

/// Connect to the live feed and pump messages until the receiver hangs up.
/// Reconnects automatically on drop-outs.
pub async fn run(tx: Sender<SourceEvent>) {
    loop {
        if tx.send(SourceEvent::Info("connecting to live feed…".into())).is_err() {
            return;
        }
        match connect_and_stream(&tx).await {
            Ok(()) => return, // receiver gone, clean exit
            Err(e) => {
                if tx
                    .send(SourceEvent::Info(format!("feed error: {e:#} — reconnecting in 5s")))
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn connect_and_stream(tx: &Sender<SourceEvent>) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("boxbox/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client.post(NEGOTIATE_URL).send().await?.error_for_status()?;
    let cookies: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .map(str::to_string)
        .collect();
    let negotiation: Value = resp.json().await?;
    let token = negotiation["connectionToken"]
        .as_str()
        .context("negotiate response missing connectionToken")?;

    let ws_url =
        reqwest::Url::parse_with_params(CONNECT_URL, &[("id", token)])?;
    let mut request = ws_url.as_str().into_client_request()?;
    if !cookies.is_empty() {
        request
            .headers_mut()
            .insert("Cookie", cookies.join("; ").parse()?);
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;

    // Protocol handshake, then subscribe.
    ws.send(WsMessage::Text(
        format!("{}{RECORD_SEP}", json!({"protocol": "json", "version": 1})).into(),
    ))
    .await?;
    let subscribe = json!({
        "type": 1,
        "invocationId": "1",
        "target": "Subscribe",
        "arguments": [LIVE_TOPICS],
    });
    ws.send(WsMessage::Text(format!("{subscribe}{RECORD_SEP}").into()))
        .await?;
    if tx.send(SourceEvent::Info("live feed connected".into())).is_err() {
        return Ok(());
    }

    let mut ping_timer = tokio::time::interval(Duration::from_secs(15));
    loop {
        let frame = tokio::select! {
            frame = ws.next() => frame,
            _ = ping_timer.tick() => {
                ws.send(WsMessage::Text(format!("{}{RECORD_SEP}", json!({"type": 6})).into())).await?;
                continue;
            }
            _ = tokio::time::sleep(Duration::from_secs(90)) => {
                anyhow::bail!("no data for 90s");
            }
        };
        let Some(frame) = frame else {
            anyhow::bail!("websocket closed");
        };
        let text = match frame? {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(_) => anyhow::bail!("websocket closed by server"),
            _ => continue,
        };
        for record in text.split(RECORD_SEP).filter(|r| !r.is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(record) else {
                continue;
            };
            if handle_record(tx, &v).is_err() {
                return Ok(()); // receiver hung up
            }
        }
    }
}

fn handle_record(tx: &Sender<SourceEvent>, v: &Value) -> Result<(), ()> {
    match v.get("type").and_then(|t| t.as_i64()) {
        // Server-to-client invocation: feed updates.
        Some(1) => {
            if v.get("target").and_then(|t| t.as_str()) != Some("feed") {
                return Ok(());
            }
            let Some(args) = v.get("arguments").and_then(|a| a.as_array()) else {
                return Ok(());
            };
            let (Some(topic), Some(data)) =
                (args.first().and_then(|t| t.as_str()), args.get(1))
            else {
                return Ok(());
            };
            send_msg(tx, topic.to_string(), data.clone())
        }
        // Completion of our Subscribe call: result holds full snapshots per topic.
        Some(3) => {
            if let Some(snapshot) = v.get("result").and_then(|r| r.as_object()) {
                for (topic, data) in snapshot {
                    send_msg(tx, topic.clone(), data.clone())?;
                }
            }
            Ok(())
        }
        _ => Ok(()), // pings, handshake ack, etc.
    }
}

fn send_msg(tx: &Sender<SourceEvent>, topic: String, data: Value) -> Result<(), ()> {
    tx.send(SourceEvent::Message(FeedMessage {
        topic,
        data,
        ts: None,
    }))
    .map_err(|_| ())
}
