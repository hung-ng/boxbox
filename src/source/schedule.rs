//! Season schedule from the Jolpica (Ergast successor) API.
//!
//! F1's own `Index.json` is trimmed for past seasons — early rounds vanish from
//! the listing even though their timing streams remain on the server. Jolpica
//! gives the full schedule with per-session dates for any year, which lets us
//! reconstruct the archive path for sessions the index no longer advertises.
//!
//! Archive path convention (verified against the live server):
//!   {year}/{race_date}_{Meeting_Name}/{session_date}_{Session_Name}/
//! The meeting folder always uses the race (Sunday) date; each session folder
//! uses its own date. Meeting/session names are the human name with spaces
//! turned into underscores, UTF-8 preserved (e.g. `São_Paulo_Grand_Prix`).

use super::archive::{Archive, SCHEDULE_MAX_BYTES, Session, atomic_write, read_limited};
use anyhow::{Context, Result};
use serde::Deserialize;

const JOLPICA: &str = "https://api.jolpi.ca/ergast/f1";

/// One session within a race weekend, ready to reconstruct into an archive path.
#[derive(Debug, Clone)]
pub struct ScheduledSession {
    /// Canonical feed name, e.g. "Practice 1", "Qualifying", "Sprint".
    pub name: String,
    /// This session's own date, `YYYY-MM-DD`.
    pub date: String,
    /// UTC session start time when Jolpica provides it.
    pub time: Option<String>,
}

impl ScheduledSession {
    /// A session is available to replay once its date has passed (streams only
    /// exist on the server after the session has actually run). `today` is the
    /// current UTC date as `YYYY-MM-DD`; lexicographic compare works for ISO dates.
    pub fn is_available(&self, today: &str) -> bool {
        self.date.as_str() <= today
    }
}

/// A race weekend and its sessions.
#[derive(Debug, Clone)]
pub struct ScheduledRace {
    pub round: u32,
    /// Human meeting name, e.g. "Monaco Grand Prix".
    pub name: String,
    /// Race (Sunday) date — used for the meeting folder.
    pub race_date: String,
    /// Circuit id from Jolpica, e.g. "monza", "red_bull_ring".
    pub circuit_id: String,
    /// Circuit locality, e.g. "Monaco", "São Paulo".
    pub locality: String,
    /// Circuit country, e.g. "Italy", "USA".
    pub country: String,
    pub sessions: Vec<ScheduledSession>,
}

/// True nicknames only: names present in *no* schedule field, so normalization
/// alone can't reach them. Redundant separator/accent/spelling variants (uk,
/// vegas, brazil, mexico, abudhabi, …) are handled by [`normalize`] instead of
/// listed here. Each entry rewrites one normalized token to one (possibly
/// dashed) normalized token, applied after the query is normalized.
const ALIASES: &[(&str, &str)] = &[
    ("us", "usa"),        // country is "USA"; multi-race → browser
    ("cota", "americas"), // United States GP (Austin) nickname
    ("usgp", "americas"),
    ("holland", "dutch"),
    ("quebec", "canadian"),
    ("britain", "british"), // "British Grand Prix"; `uk` is handled by normalize
];

/// Normalize a string to a dash-joined, diacritic-free token stream:
/// lowercase, strip accents, collapse every run of non-alphanumerics to a
/// single `-`, and trim leading/trailing `-`. Applied identically to the
/// haystack and to each query token, so both sides speak the same alphabet.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_dash = false;
    for ch in s.chars() {
        let folded = fold_diacritic(ch).to_ascii_lowercase();
        if folded.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(folded);
        } else {
            // Any non-alphanumeric (space, `_`, `-`, punctuation) is a separator.
            pending_dash = true;
        }
    }
    out
}

/// Fold a single accented Latin char to its base ASCII letter. Covers the
/// diacritics that appear in the F1 calendar (locality/country names); anything
/// unmapped passes through unchanged (and non-alphanumerics get dropped by
/// [`normalize`] anyway).
fn fold_diacritic(ch: char) -> char {
    match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => ch,
    }
}

/// True iff `tok` appears in the normalized `hay` as a whole dash-bounded run:
/// a substring whose left edge is start-or-`-` and right edge is end-or-`-`.
/// This rejects short-token false positives (`us` inside `usa`/`austria`) while
/// still matching multi-word tokens joined by dashes (`sao-paulo`).
fn contains_token(hay: &str, tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(rel) = hay[from..].find(tok) {
        let start = from + rel;
        let end = start + tok.len();
        let left_ok = start == 0 || hay.as_bytes()[start - 1] == b'-';
        let right_ok = end == hay.len() || hay.as_bytes()[end] == b'-';
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Normalize each query token, then rewrite true nicknames through the alias
/// table. Aliases are looked up on the already-normalized token.
pub fn expand_query(query: &[String]) -> Vec<String> {
    query
        .iter()
        .map(|w| {
            let w = normalize(w);
            ALIASES
                .iter()
                .find(|(from, _)| *from == w)
                .map(|(_, to)| to.to_string())
                .unwrap_or(w)
        })
        .collect()
}

impl ScheduledRace {
    /// Normalized, dash-joined match haystack: race name + circuit id +
    /// locality + country, all folded through [`normalize`].
    pub fn haystack(&self) -> String {
        normalize(&format!(
            "{} {} {} {}",
            self.name, self.circuit_id, self.locality, self.country
        ))
    }

    /// Sessions in this race matching every expanded query token, where each
    /// token must sit on dash boundaries within `"{race haystack}-{session}"`.
    pub fn matches(&self, expanded: &[String]) -> Vec<&ScheduledSession> {
        let race_hay = self.haystack();
        self.sessions
            .iter()
            .filter(|s| {
                let hay = format!("{race_hay}-{}", normalize(&s.name));
                expanded.iter().all(|tok| contains_token(&hay, tok))
            })
            .collect()
    }

    /// Build the on-server archive path for one of this weekend's sessions.
    /// e.g. `2024/2024-05-26_Monaco_Grand_Prix/2024-05-25_Qualifying/`
    pub fn session_path(&self, year: u16, s: &ScheduledSession) -> String {
        let meeting = folderize(&self.name);
        let session = folderize(&s.name);
        format!("{year}/{}_{meeting}/{}_{session}/", self.race_date, s.date)
    }

    /// A stable synthetic cache key for a reconstructed session (no index Key).
    /// Round + session name is unique within a season and human-legible on disk.
    pub fn cache_key(&self, year: u16, s: &ScheduledSession) -> String {
        format!("{year}-r{}-{}", self.round, folderize(&s.name))
    }
}

/// Human name → archive folder segment: spaces to underscores, UTF-8 kept.
fn folderize(name: &str) -> String {
    name.replace(' ', "_")
}

// ---- Jolpica response shapes (only the fields we use) ----

#[derive(Deserialize)]
struct JolpicaResp {
    #[serde(rename = "MRData")]
    mr_data: MrData,
}
#[derive(Deserialize)]
struct MrData {
    #[serde(rename = "RaceTable")]
    race_table: RaceTable,
}
#[derive(Deserialize)]
struct RaceTable {
    #[serde(rename = "Races")]
    races: Vec<JolpicaRace>,
}
#[derive(Deserialize)]
struct JolpicaRace {
    round: String,
    #[serde(rename = "raceName")]
    race_name: String,
    #[serde(rename = "Circuit")]
    circuit: Circuit,
    date: String,
    time: Option<String>,
    #[serde(rename = "FirstPractice")]
    first_practice: Option<DateEntry>,
    #[serde(rename = "SecondPractice")]
    second_practice: Option<DateEntry>,
    #[serde(rename = "ThirdPractice")]
    third_practice: Option<DateEntry>,
    #[serde(rename = "Qualifying")]
    qualifying: Option<DateEntry>,
    #[serde(rename = "Sprint")]
    sprint: Option<DateEntry>,
    #[serde(rename = "SprintQualifying")]
    sprint_qualifying: Option<DateEntry>,
    #[serde(rename = "SprintShootout")]
    sprint_shootout: Option<DateEntry>,
}
#[derive(Deserialize)]
struct Circuit {
    #[serde(rename = "circuitId")]
    circuit_id: String,
    #[serde(rename = "Location")]
    location: Location,
}
#[derive(Deserialize)]
struct Location {
    locality: String,
    country: String,
}
#[derive(Deserialize)]
struct DateEntry {
    date: String,
    time: Option<String>,
}

impl JolpicaRace {
    fn into_scheduled(self) -> ScheduledRace {
        // Emit in chronological weekend order using the feed's canonical names.
        let mut sessions = Vec::new();
        let mut push = |feed: &str, e: Option<DateEntry>| {
            if let Some(e) = e {
                sessions.push(ScheduledSession {
                    name: feed.to_string(),
                    date: e.date,
                    time: e.time,
                });
            }
        };
        push("Practice 1", self.first_practice);
        // Preserve the historical feed names because the name is part of the
        // static archive folder path (2023 Shootout, 2024+ Qualifying).
        push("Sprint Shootout", self.sprint_shootout);
        push("Sprint Qualifying", self.sprint_qualifying);
        push("Practice 2", self.second_practice);
        push("Sprint", self.sprint);
        push("Practice 3", self.third_practice);
        push("Qualifying", self.qualifying);
        let race_date = self.date.clone();
        sessions.push(ScheduledSession {
            name: "Race".to_string(),
            date: self.date,
            time: self.time,
        });
        sessions.sort_by(|a, b| {
            (
                a.date.as_str(),
                a.time.as_deref().unwrap_or("99:99:99Z"),
                a.name.as_str(),
            )
                .cmp(&(
                    b.date.as_str(),
                    b.time.as_deref().unwrap_or("99:99:99Z"),
                    b.name.as_str(),
                ))
        });
        ScheduledRace {
            round: self.round.parse().unwrap_or(0),
            name: self.race_name,
            race_date,
            circuit_id: self.circuit.circuit_id,
            locality: self.circuit.location.locality,
            country: self.circuit.location.country,
            sessions,
        }
    }
}

impl Archive {
    /// Full season schedule from Jolpica, cached on disk. Works for any year,
    /// including seasons F1 has trimmed from its own index.
    pub async fn schedule(&self, year: u16) -> Result<Vec<ScheduledRace>> {
        let cache_file = self.schedule_cache_file(year);
        if let Some(schedule) = read_cached_schedule(&cache_file, year)? {
            return Ok(schedule);
        }
        let url = format!("{JOLPICA}/{year}.json");
        let resp = self.http().get(&url).send().await?.error_for_status()?;
        let body = read_limited(resp, SCHEDULE_MAX_BYTES).await?;
        let body = String::from_utf8(body).context("Jolpica schedule is not UTF-8")?;
        let schedule = parse_schedule(&body, year)?;
        atomic_write(&cache_file, body.as_bytes())?;
        Ok(schedule)
    }

    /// Turn a scheduled session into an archive `Session` the replay loader can use.
    pub fn session_from_schedule(
        &self,
        year: u16,
        race: &ScheduledRace,
        s: &ScheduledSession,
    ) -> Session {
        Session::reconstructed(
            race.cache_key(year, s),
            s.name.clone(),
            s.date.clone(),
            race.session_path(year, s),
        )
    }
}

fn parse_schedule(body: &str, year: u16) -> Result<Vec<ScheduledRace>> {
    let resp: JolpicaResp = serde_json::from_str(body)
        .with_context(|| format!("parsing Jolpica schedule for {year}"))?;
    Ok(resp
        .mr_data
        .race_table
        .races
        .into_iter()
        .map(JolpicaRace::into_scheduled)
        .collect())
}

fn read_cached_schedule(path: &std::path::Path, year: u16) -> Result<Option<Vec<ScheduledRace>>> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            std::fs::remove_file(path)
                .with_context(|| format!("removing corrupt {}", path.display()))?;
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    match parse_schedule(&body, year) {
        Ok(schedule) => Ok(Some(schedule)),
        Err(_) => {
            std::fs::remove_file(path)
                .with_context(|| format!("removing corrupt {}", path.display()))?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn race(name: &str, circuit: &str, locality: &str, country: &str) -> ScheduledRace {
        ScheduledRace {
            round: 1,
            name: name.into(),
            race_date: "2024-07-07".into(),
            circuit_id: circuit.into(),
            locality: locality.into(),
            country: country.into(),
            sessions: vec![
                ScheduledSession {
                    name: "Qualifying".into(),
                    date: "2024-07-06".into(),
                    time: None,
                },
                ScheduledSession {
                    name: "Race".into(),
                    date: "2024-07-07".into(),
                    time: None,
                },
            ],
        }
    }

    fn words(s: &str) -> Vec<String> {
        expand_query(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    /// The three 2024 races sharing `country: "USA"`, plus a couple of decoys
    /// whose fields contain `us` only as a non-bounded substring.
    fn usa_calendar() -> Vec<ScheduledRace> {
        vec![
            race("Miami Grand Prix", "miami", "Miami", "USA"),
            race("United States Grand Prix", "americas", "Austin", "USA"),
            race("Las Vegas Grand Prix", "vegas", "Las Vegas", "USA"),
            race(
                "Austrian Grand Prix",
                "red_bull_ring",
                "Spielberg",
                "Austria",
            ),
            race(
                "Australian Grand Prix",
                "albert_park",
                "Melbourne",
                "Australia",
            ),
        ]
    }

    fn hits(cal: &[ScheduledRace], q: &str) -> usize {
        cal.iter().map(|r| r.matches(&words(q)).len()).sum()
    }

    #[test]
    fn normalize_folds_accents_and_separators() {
        assert_eq!(normalize("São Paulo"), "sao-paulo");
        assert_eq!(normalize("red_bull_ring"), "red-bull-ring");
        assert_eq!(normalize("  Abu   Dhabi  "), "abu-dhabi");
        assert_eq!(normalize("Nürburgring"), "nurburgring");
    }

    #[test]
    fn matches_by_circuit_id_and_session() {
        let r = race("British Grand Prix", "silverstone", "Silverstone", "UK");
        // circuit id alone → both sessions
        assert_eq!(r.matches(&words("silverstone")).len(), 2);
        // circuit id + session name → just the Race
        let hit = r.matches(&words("silverstone race"));
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "Race");
    }

    #[test]
    fn us_matches_all_three_usa_races() {
        // `us` → `usa`, which is the (bounded) country token of all three.
        let cal = usa_calendar();
        // Each USA race contributes its Race session (Qualifying too, but the
        // query has no session token, so both sessions of each match).
        let usa_races = cal
            .iter()
            .filter(|r| r.matches(&words("us")).len() == 2)
            .count();
        assert_eq!(usa_races, 3);
    }

    #[test]
    fn us_does_not_match_austria_or_australia() {
        let cal = usa_calendar();
        let austria = &cal[3];
        let australia = &cal[4];
        assert!(austria.matches(&words("us")).is_empty());
        assert!(australia.matches(&words("us")).is_empty());
    }

    #[test]
    fn spa_matches_belgium_not_spain() {
        let belgium = race("Belgian Grand Prix", "spa", "Spa-Francorchamps", "Belgium");
        let spain = race("Spanish Grand Prix", "catalunya", "Montmeló", "Spain");
        assert_eq!(belgium.matches(&words("spa")).len(), 2);
        assert!(spain.matches(&words("spa")).is_empty());
    }

    #[test]
    fn dashed_multiword_tokens_match_without_aliases() {
        let sp = race("São Paulo Grand Prix", "interlagos", "São Paulo", "Brazil");
        assert_eq!(sp.matches(&words("sao-paulo")).len(), 2);
        let lv = race("Las Vegas Grand Prix", "vegas", "Las Vegas", "USA");
        assert_eq!(lv.matches(&words("las-vegas")).len(), 2);
        let ad = race("Abu Dhabi Grand Prix", "yas_marina", "Abu Dhabi", "UAE");
        assert_eq!(ad.matches(&words("abu-dhabi")).len(), 2);
        let er = race("Emilia Romagna Grand Prix", "imola", "Imola", "Italy");
        assert_eq!(er.matches(&words("emilia-romagna")).len(), 2);
        let at = race(
            "Austrian Grand Prix",
            "red_bull_ring",
            "Spielberg",
            "Austria",
        );
        assert_eq!(at.matches(&words("red-bull-ring")).len(), 2);
    }

    #[test]
    fn sao_paulo_as_two_args() {
        // Two args `sao` `paulo`: both are bounded tokens of the São Paulo
        // haystack (`...sao-paulo...`), so the AND across args still matches.
        let sp = race("São Paulo Grand Prix", "interlagos", "São Paulo", "Brazil");
        assert_eq!(sp.matches(&words("sao paulo")).len(), 2);
    }

    #[test]
    fn nickname_aliases() {
        let usgp = race("United States Grand Prix", "americas", "Austin", "USA");
        assert_eq!(usgp.matches(&words("cota")).len(), 2);
        assert_eq!(usgp.matches(&words("usgp")).len(), 2);
        let dutch = race("Dutch Grand Prix", "zandvoort", "Zandvoort", "Netherlands");
        assert_eq!(dutch.matches(&words("holland")).len(), 2);
        let canada = race("Canadian Grand Prix", "villeneuve", "Montreal", "Canada");
        assert_eq!(canada.matches(&words("quebec")).len(), 2);
        let britain = race("British Grand Prix", "silverstone", "Silverstone", "UK");
        assert_eq!(britain.matches(&words("britain")).len(), 2);
        // `uk` reaches the same race via the country field, no alias needed.
        assert_eq!(britain.matches(&words("uk")).len(), 2);
    }

    #[test]
    fn circuit_and_locality_tokens_match() {
        let mc = race("Monaco Grand Prix", "monaco", "Monte-Carlo", "Monaco");
        assert_eq!(mc.matches(&words("monaco")).len(), 2);
        let it = race("Italian Grand Prix", "monza", "Monza", "Italy");
        assert_eq!(it.matches(&words("monza")).len(), 2);
        let sp = race("São Paulo Grand Prix", "interlagos", "São Paulo", "Brazil");
        assert_eq!(sp.matches(&words("interlagos")).len(), 2);
        let jp = race("Japanese Grand Prix", "suzuka", "Suzuka", "Japan");
        assert_eq!(jp.matches(&words("suzuka")).len(), 2);
        let nl = race("Dutch Grand Prix", "zandvoort", "Zandvoort", "Netherlands");
        assert_eq!(nl.matches(&words("zandvoort")).len(), 2);
    }

    #[test]
    fn session_narrowing() {
        let r = race("British Grand Prix", "silverstone", "Silverstone", "UK");
        let hit = r.matches(&words("silverstone race"));
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "Race");
    }

    #[test]
    fn us_across_calendar_counts() {
        // Documents the multi-race requirement at the calendar level: `us`
        // reaches exactly the three USA races and neither Austr* decoy.
        assert_eq!(hits(&usa_calendar(), "us race"), 3);
    }

    #[test]
    fn availability_by_date() {
        let s = ScheduledSession {
            name: "Race".into(),
            date: "2024-07-07".into(),
            time: None,
        };
        assert!(s.is_available("2026-07-03"));
        assert!(s.is_available("2024-07-07"));
        assert!(!s.is_available("2024-07-06"));
    }

    fn historical_sprint(value: serde_json::Value) -> ScheduledRace {
        serde_json::from_value::<JolpicaRace>(value)
            .unwrap()
            .into_scheduled()
    }

    #[test]
    fn historical_sprint_2023_keeps_shootout_name_and_order() {
        let race = historical_sprint(serde_json::json!({
            "round": "4",
            "raceName": "Azerbaijan Grand Prix",
            "date": "2023-04-30",
            "time": "11:00:00Z",
            "Circuit": {
                "circuitId": "baku",
                "Location": {"locality": "Baku", "country": "Azerbaijan"}
            },
            "FirstPractice": {"date": "2023-04-28", "time": "09:30:00Z"},
            "Qualifying": {"date": "2023-04-28", "time": "13:00:00Z"},
            "SprintShootout": {"date": "2023-04-29", "time": "08:30:00Z"},
            "Sprint": {"date": "2023-04-29", "time": "12:30:00Z"}
        }));
        assert_eq!(
            race.sessions
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Practice 1",
                "Qualifying",
                "Sprint Shootout",
                "Sprint",
                "Race"
            ]
        );
        let shootout = &race.sessions[2];
        assert!(
            race.session_path(2023, shootout)
                .ends_with("2023/2023-04-30_Azerbaijan_Grand_Prix/2023-04-29_Sprint_Shootout/")
        );
    }

    #[test]
    fn historical_sprint_2024_uses_qualifying_name_and_order() {
        let race = historical_sprint(serde_json::json!({
            "round": "6",
            "raceName": "Miami Grand Prix",
            "date": "2024-05-05",
            "time": "20:00:00Z",
            "Circuit": {
                "circuitId": "miami",
                "Location": {"locality": "Miami", "country": "USA"}
            },
            "FirstPractice": {"date": "2024-05-03", "time": "16:30:00Z"},
            "SprintQualifying": {"date": "2024-05-03", "time": "20:30:00Z"},
            "Sprint": {"date": "2024-05-04", "time": "16:00:00Z"},
            "Qualifying": {"date": "2024-05-04", "time": "20:00:00Z"}
        }));
        assert_eq!(
            race.sessions
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Practice 1",
                "Sprint Qualifying",
                "Sprint",
                "Qualifying",
                "Race"
            ]
        );
    }

    #[test]
    fn invalid_cached_schedule_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2024.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(read_cached_schedule(&path, 2024).unwrap().is_none());
        assert!(!path.exists());

        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(read_cached_schedule(&path, 2024).unwrap().is_none());
        assert!(!path.exists());
    }
}
