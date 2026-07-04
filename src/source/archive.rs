use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub const BASE: &str = "https://livetiming.formula1.com/static";

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
    pub fn reconstructed(
        cache_id: String,
        name: String,
        start_date: String,
        path: String,
    ) -> Self {
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
        if let Ok(cached) = std::fs::read_to_string(&cache_file) {
            return Ok(Some(cached));
        }

        let url = format!("{BASE}/{path}{topic}.jsonStream");
        let resp = self.client.get(&url).send().await?;
        // Topics that don't exist for a session type surface as 404 or 403.
        if resp.status().is_client_error() {
            return Ok(None);
        }
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status());
        }
        let body = resp.text().await?;
        let body = strip_bom(&body).to_string();
        std::fs::create_dir_all(cache_file.parent().unwrap())?;
        std::fs::write(&cache_file, &body)?;
        Ok(Some(body))
    }

    /// Track outline from the MultiViewer API, cached on disk.
    pub async fn circuit_outline(&self, circuit_key: i64, year: i64) -> Result<serde_json::Value> {
        let cache_file = self
            .cache_dir
            .join("circuits")
            .join(format!("{circuit_key}-{year}.json"));
        if let Ok(cached) = std::fs::read_to_string(&cache_file) {
            if let Ok(v) = serde_json::from_str(&cached) {
                return Ok(v);
            }
        }
        let url = format!("https://api.multiviewer.app/api/v1/circuits/{circuit_key}/{year}");
        let body = self.client.get(&url).send().await?.error_for_status()?.text().await?;
        let v: serde_json::Value = serde_json::from_str(strip_bom(&body))?;
        std::fs::create_dir_all(cache_file.parent().unwrap())?;
        std::fs::write(&cache_file, &body)?;
        Ok(v)
    }
}

pub fn strip_bom(s: &str) -> &str {
    s.trim_start_matches('\u{feff}')
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
