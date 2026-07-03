use anyhow::{bail, Context, Result};
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
pub struct YearIndex {
    #[serde(rename = "Meetings")]
    pub meetings: Vec<Meeting>,
}

#[derive(Debug, Deserialize)]
pub struct Meeting {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Location", default)]
    pub location: String,
    #[serde(rename = "Sessions")]
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Session {
    #[serde(rename = "Key")]
    pub key: i64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "StartDate", default)]
    pub start_date: String,
    #[serde(rename = "Path", default)]
    pub path: Option<String>,
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

    pub async fn year_index(&self, year: u16) -> Result<YearIndex> {
        let url = format!("{BASE}/{year}/Index.json");
        let body = self.client.get(&url).send().await?.error_for_status()?.text().await?;
        Ok(serde_json::from_str(strip_bom(&body))?)
    }

    /// Find sessions whose meeting + session name matches every query word.
    pub async fn find_sessions(&self, year: u16, query: &[String]) -> Result<Vec<SessionRef>> {
        let index = self.year_index(year).await?;
        let mut found = Vec::new();
        for meeting in index.meetings {
            for session in &meeting.sessions {
                if session.path.is_none() {
                    continue; // not yet archived (future session)
                }
                let hay = format!("{} {} {}", meeting.name, meeting.location, session.name)
                    .to_lowercase();
                if query.iter().all(|w| hay.contains(&w.to_lowercase())) {
                    found.push(SessionRef {
                        meeting: meeting.name.clone(),
                        session: session.clone(),
                    });
                }
            }
        }
        Ok(found)
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
            .join(session.key.to_string())
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
