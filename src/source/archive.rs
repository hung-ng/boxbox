use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const BASE: &str = "https://livetiming.formula1.com/static";
pub(crate) const SCHEDULE_MAX_BYTES: usize = 8 * 1024 * 1024;
const CIRCUIT_MAX_BYTES: usize = 8 * 1024 * 1024;
const STREAM_MAX_BYTES: usize = 256 * 1024 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Feed topics we replay, in the order they matter least→most for a tie on timestamp.
pub const TOPICS: &[&str] = &[
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
    "PitLaneTimeCollection",
    "Position.z",
];

/// A replayable session, reconstructed from the Jolpica schedule.
#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    pub start_date: String,
    pub path: Option<String>,
    /// Directory name for this session's on-disk stream cache.
    pub cache_id: String,
}

impl Session {
    /// Build a session from a schedule-reconstructed archive path.
    pub fn reconstructed(cache_id: String, name: String, start_date: String, path: String) -> Self {
        Session {
            name,
            start_date,
            path: Some(path),
            cache_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionRef {
    pub meeting: String,
    pub session: Session,
}

pub struct Archive {
    client: reqwest::Client,
    cache_dir: PathBuf,
}

impl Archive {
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "boxbox")
            .context("cannot determine cache directory")?;
        let cache_dir = dirs.cache_dir().to_path_buf();
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(concat!("boxbox/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(120))
                .build()?,
            cache_dir,
        })
    }

    /// The shared HTTP client, for callers that fetch other resources (schedule).
    pub fn http(&self) -> &reqwest::Client {
        &self.client
    }

    /// The root cache directory (`sessions/`, `schedules/`, `circuits/` live here).
    pub fn cache_dir(&self) -> &std::path::Path {
        &self.cache_dir
    }

    /// Disk usage per cache category, in bytes: (sessions, schedules, circuits).
    pub fn cache_usage(&self) -> CacheUsage {
        CacheUsage {
            sessions: dir_size(&self.cache_dir.join("sessions")),
            schedules: dir_size(&self.cache_dir.join("schedules")),
            circuits: dir_size(&self.cache_dir.join("circuits")),
        }
    }

    /// Delete cached data. With `year`, only that season's session streams (whose
    /// cache ids are prefixed `{year}-`) are removed; the schedule and circuit
    /// caches are shared and left alone. Without a year, wipe everything.
    /// Returns the number of bytes freed.
    pub fn clear_cache(&self, year: Option<u16>) -> Result<u64> {
        match year {
            None => {
                let freed = dir_size(&self.cache_dir);
                for sub in ["sessions", "schedules", "circuits"] {
                    let path = self.cache_dir.join(sub);
                    if path.exists() {
                        std::fs::remove_dir_all(&path)
                            .with_context(|| format!("removing {}", path.display()))?;
                    }
                }
                Ok(freed)
            }
            Some(year) => {
                let sessions = self.cache_dir.join("sessions");
                let prefix = format!("{year}-");
                let mut freed = 0;
                let entries = match std::fs::read_dir(&sessions) {
                    Ok(e) => e,
                    Err(_) => return Ok(0),
                };
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with(&prefix) {
                        let path = entry.path();
                        freed += dir_size(&path);
                        std::fs::remove_dir_all(&path)
                            .with_context(|| format!("removing {}", path.display()))?;
                    }
                }
                Ok(freed)
            }
        }
    }

    /// On-disk cache location for a season's Jolpica schedule.
    pub fn schedule_cache_file(&self, year: u16) -> PathBuf {
        self.cache_dir
            .join("schedules")
            .join(format!("{year}.json"))
    }

    /// Download one topic stream for a session, using the on-disk cache.
    /// Returns None if the feed doesn't exist for this session (404).
    pub async fn fetch_stream(&self, session: &Session, topic: &str) -> Result<Option<String>> {
        let path = session
            .path
            .as_deref()
            .context("session has no archive path")?;
        let cache_file = self
            .cache_dir
            .join("sessions")
            .join(&session.cache_id)
            .join(format!("{topic}.jsonStream"));
        if let Some(cached) = read_cached_stream(&cache_file)? {
            return Ok(Some(cached));
        }

        let url = format!("{BASE}/{path}{topic}.jsonStream");
        let resp = self.client.get(&url).send().await?;
        match classify_stream_status(resp.status()) {
            StreamStatus::Missing => return Ok(None),
            StreamStatus::Error => bail!("{url}: HTTP {}", resp.status()),
            StreamStatus::Available => {}
        }
        let body = read_limited(resp, STREAM_MAX_BYTES).await?;
        let body = String::from_utf8(body).context("archive stream is not UTF-8")?;
        let body = strip_bom(&body).to_string();
        if !stream_body_valid(&body) {
            bail!("{url}: stream contains no valid records");
        }
        atomic_write(&cache_file, body.as_bytes())?;
        Ok(Some(body))
    }

    /// Track outline from the MultiViewer API, cached on disk.
    pub async fn circuit_outline(&self, circuit_key: i64, year: i64) -> Result<serde_json::Value> {
        let cache_file = self
            .cache_dir
            .join("circuits")
            .join(format!("{circuit_key}-{year}.json"));
        if let Some(cached) = read_cached_circuit(&cache_file)? {
            return Ok(cached);
        }
        let url = format!("https://api.multiviewer.app/api/v1/circuits/{circuit_key}/{year}");
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let body = read_limited(resp, CIRCUIT_MAX_BYTES).await?;
        let body = String::from_utf8(body).context("circuit response is not UTF-8")?;
        let v: serde_json::Value = serde_json::from_str(strip_bom(&body))?;
        if !circuit_shape_valid(&v) {
            bail!("{url}: invalid circuit coordinate arrays");
        }
        atomic_write(&cache_file, body.as_bytes())?;
        Ok(v)
    }
}

pub fn strip_bom(s: &str) -> &str {
    s.trim_start_matches('\u{feff}')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamStatus {
    Available,
    Missing,
    Error,
}

fn classify_stream_status(status: StatusCode) -> StreamStatus {
    match status {
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => StreamStatus::Missing,
        s if s.is_success() => StreamStatus::Available,
        _ => StreamStatus::Error,
    }
}

fn stream_body_valid(body: &str) -> bool {
    body.lines()
        .any(|line| super::stream::parse_line(line).is_some())
}

fn read_cached_stream(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(body) if stream_body_valid(&body) => Ok(Some(body)),
        Ok(_) => {
            std::fs::remove_file(path)
                .with_context(|| format!("removing corrupt {}", path.display()))?;
            Ok(None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            std::fs::remove_file(path)
                .with_context(|| format!("removing corrupt {}", path.display()))?;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn read_cached_circuit(path: &Path) -> Result<Option<serde_json::Value>> {
    match std::fs::read_to_string(path) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(value) if circuit_shape_valid(&value) => Ok(Some(value)),
            _ => {
                std::fs::remove_file(path)
                    .with_context(|| format!("removing corrupt {}", path.display()))?;
                Ok(None)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            std::fs::remove_file(path)
                .with_context(|| format!("removing corrupt {}", path.display()))?;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn circuit_shape_valid(v: &serde_json::Value) -> bool {
    let (Some(xs), Some(ys)) = (
        v.get("x").and_then(|v| v.as_array()),
        v.get("y").and_then(|v| v.as_array()),
    ) else {
        return false;
    };
    xs.len() >= 2
        && xs.len() == ys.len()
        && xs
            .iter()
            .chain(ys)
            .all(|v| v.as_f64().is_some_and(f64::is_finite))
}

pub(crate) async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        bail!("response exceeds {limit} bytes");
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len().saturating_add(chunk.len()) > limit {
            bail!("response exceeds {limit} bytes");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

pub(crate) fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    atomic_write_with(path, |file| {
        file.write_all(body)?;
        Ok(())
    })
}

fn atomic_write_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    let parent = path.parent().context("cache path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("cache");
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.tmp-{}-{id}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Cache disk usage broken down by category, in bytes.
pub struct CacheUsage {
    pub sessions: u64,
    pub schedules: u64,
    pub circuits: u64,
}

impl CacheUsage {
    pub fn total(&self) -> u64 {
        self.sessions + self.schedules + self.circuits
    }
}

/// Total size in bytes of all files under `dir`, recursively. Missing dir → 0.
fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => total += dir_size(&entry.path()),
            Ok(_) => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => {}
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn stream_status_only_treats_403_and_404_as_missing() {
        assert_eq!(
            classify_stream_status(StatusCode::FORBIDDEN),
            StreamStatus::Missing
        );
        assert_eq!(
            classify_stream_status(StatusCode::NOT_FOUND),
            StreamStatus::Missing
        );
        assert_eq!(
            classify_stream_status(StatusCode::UNAUTHORIZED),
            StreamStatus::Error
        );
        assert_eq!(
            classify_stream_status(StatusCode::TOO_MANY_REQUESTS),
            StreamStatus::Error
        );
        assert_eq!(
            classify_stream_status(StatusCode::OK),
            StreamStatus::Available
        );
    }

    #[test]
    fn circuit_shape_requires_matching_numeric_coordinates() {
        assert!(circuit_shape_valid(
            &json!({"x": [0.0, 1.0], "y": [2.0, 3.0]})
        ));
        assert!(!circuit_shape_valid(&json!({"x": [0.0], "y": [2.0]})));
        assert!(!circuit_shape_valid(
            &json!({"x": [0.0, "bad"], "y": [2.0, 3.0]})
        ));
    }

    #[test]
    fn corrupt_stream_and_circuit_caches_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("bad.jsonStream");
        let circuit = dir.path().join("bad.json");
        std::fs::write(&stream, b"not a stream").unwrap();
        std::fs::write(&circuit, b"not json").unwrap();

        assert!(read_cached_stream(&stream).unwrap().is_none());
        assert!(read_cached_circuit(&circuit).unwrap().is_none());
        assert!(!stream.exists());
        assert!(!circuit.exists());
    }

    #[test]
    fn atomic_write_publishes_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        atomic_write(&path, b"new body").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new body");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_failure_preserves_published_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        std::fs::write(&path, b"old body").unwrap();

        let result = atomic_write_with(&path, |_| anyhow::bail!("injected write failure"));

        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old body");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
