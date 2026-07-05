use super::SessionState;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFlag {
    PersonalBest,
    OverallBest,
}

#[derive(Debug, Clone, Default)]
pub struct TimeVal {
    pub text: String,
    pub flag: Option<TimeFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    NotSet,       // not reached yet
    Set,          // yellow: completed, no improvement
    PersonalBest, // green
    OverallBest,  // purple
    Pit,
}

#[derive(Debug, Clone)]
pub struct Stint {
    pub compound: char, // S M H I W or ?
    pub age_laps: i64,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub position: u32,
    pub number: String,
    pub tla: String,
    pub full_name: String,
    pub team_color: (u8, u8, u8),
    pub gap: String,      // to leader / to fastest
    pub interval: String, // to car ahead
    /// Parsed interval when it's a plain seconds gap — used for battle highlighting.
    pub interval_secs: Option<f64>,
    pub last_lap: TimeVal,
    pub best_lap: String,
    pub best_lap_secs: Option<f64>,
    /// Completed laps this session (practice LAPS column). None when unknown.
    pub laps: Option<i64>,
    pub sectors: [TimeVal; 3],
    pub segments: Vec<Vec<Segment>>, // per sector
    pub stints: Vec<Stint>,          // full history, current last
    pub pit_count: i64,
    pub in_pit: bool,
    pub pit_out: bool,
    pub retired: bool,
    pub stopped: bool,
    pub knocked_out: bool,
    /// Reported pit time from PitLaneTimeCollection, when known (5.2).
    pub pit_time: Option<String>,
    /// Per-segment best lap times in qualifying (Q1/Q2/Q3), empty otherwise (5.1).
    pub segment_bests: Vec<String>,
    /// Speed-trap values I1/I2/FL/ST with PB/OB flags (5.3).
    pub speeds: [TimeVal; 4],
}

impl Row {
    pub fn stint(&self) -> Option<&Stint> {
        self.stints.last()
    }
}

#[derive(Debug, Clone)]
pub struct RcMessage {
    pub time: String, // HH:MM UTC
    pub lap: Option<i64>,
    pub category: String,
    pub flag: Option<String>,
    pub text: String,
    /// False for routine spam (track-sector yellows/clears, blue flags, rain risk).
    pub important: bool,
}

#[derive(Debug, Clone)]
pub struct Weather {
    pub air: String,
    pub track: String,
    pub wind: String,
    pub humidity: String,
    pub raining: bool,
}

#[derive(Debug, Clone)]
pub struct CarDot {
    pub x: f64,
    pub y: f64,
    pub color: (u8, u8, u8),
    pub tla: String,
    pub in_pit: bool,
    /// True when the driver appears to be on a flying/timed lap right now
    /// (out of the pits, improving). Used to spotlight cars in qualifying.
    pub hot_lap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackFlag {
    Green,
    Yellow,
    SafetyCar,
    Vsc,
    Red,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct ViewModel {
    pub meeting: String,
    pub session_name: String,
    pub session_type: String, // "Race" / "Qualifying" / "Practice"
    pub circuit_key: Option<i64>,
    pub year: Option<i64>,
    pub track_flag: Option<TrackFlag>,
    pub session_status: String,
    pub session_part: Option<i64>, // Q1/Q2/Q3 (or sprint quali SQ1..)
    pub lap: Option<(i64, i64)>,
    pub clock_remaining: Option<String>,
    pub weather: Option<Weather>,
    /// Session-best lap: (time, TLA).
    pub fastest: Option<(String, String)>,
    pub rows: Vec<Row>,
    pub race_control: Vec<RcMessage>,
    pub cars: Vec<CarDot>,
    /// Marshal-sector numbers currently under a (double) yellow (3.2).
    pub yellow_sectors: std::collections::HashSet<i64>,
}

impl ViewModel {
    pub fn is_race(&self) -> bool {
        self.session_type == "Race" || self.lap.is_some()
    }

    pub fn is_qualifying(&self) -> bool {
        self.session_type == "Qualifying"
    }

    pub fn is_practice(&self) -> bool {
        self.session_type == "Practice"
    }

    /// Segment label for the current quali part: `Q{p}` normally, `SQ{p}` for
    /// Sprint Qualifying/Shootout. None when there's no active part.
    pub fn part_label(&self) -> Option<String> {
        let p = self.session_part?;
        let prefix = if self.session_name.contains("Sprint") {
            "SQ"
        } else {
            "Q"
        };
        Some(format!("{prefix}{p}"))
    }

    /// Last classified position that survives the current quali segment.
    /// Q3 always keeps 10 cars, so each of Q1/Q2 eliminates half of the excess
    /// above 10; e.g. 20→15, 19→15, 22→16. None outside Q1/Q2. `n` is the entry
    /// count (includes already knocked-out drivers).
    pub fn quali_cutoff(&self) -> Option<u32> {
        if !self.is_qualifying() {
            return None;
        }
        let part = self.session_part?;
        let n = self.rows.len() as i64;
        if n < 12 {
            return None;
        }
        match part {
            1 => Some((n - (n - 10) / 2) as u32),
            2 => Some(10),
            _ => None,
        }
    }

    /// Best-lap seconds of the driver sitting on the elimination cutoff — the
    /// time drop-zone drivers must beat (3.5). None outside Q1/Q2 or if that
    /// driver hasn't set a time.
    pub fn cutoff_time(&self) -> Option<f64> {
        let cutoff = self.quali_cutoff()?;
        self.rows
            .iter()
            .find(|r| r.position == cutoff)
            .and_then(|r| r.best_lap_secs)
    }
}

pub fn build(state: &SessionState) -> ViewModel {
    let mut vm = ViewModel::default();

    if let Some(info) = state.topic("SessionInfo") {
        vm.meeting = str_at(info, &["Meeting", "Name"]).unwrap_or_default();
        vm.session_name = str_at(info, &["Name"]).unwrap_or_default();
        vm.session_type = str_at(info, &["Type"]).unwrap_or_default();
        vm.circuit_key = info
            .pointer("/Meeting/Circuit/Key")
            .and_then(|v| v.as_i64());
        vm.year = str_at(info, &["StartDate"])
            .and_then(|d| d.get(..4).map(str::to_string))
            .and_then(|y| y.parse().ok());
    }
    if let Some(ts) = state.topic("TrackStatus") {
        vm.track_flag = Some(match str_at(ts, &["Status"]).as_deref() {
            Some("1") => TrackFlag::Green,
            Some("2") | Some("3") => TrackFlag::Yellow,
            Some("4") => TrackFlag::SafetyCar,
            Some("5") => TrackFlag::Red,
            Some("6") | Some("7") => TrackFlag::Vsc,
            _ => TrackFlag::Unknown,
        });
    }
    if let Some(ss) = state.topic("SessionStatus") {
        vm.session_status = str_at(ss, &["Status"]).unwrap_or_default();
    }
    if let Some(lc) = state.topic("LapCount") {
        let cur = lc.get("CurrentLap").and_then(as_i64_lenient);
        let total = lc.get("TotalLaps").and_then(as_i64_lenient);
        if let (Some(c), Some(t)) = (cur, total) {
            vm.lap = Some((c, t));
        }
    }
    if let Some(clock) = state.topic("ExtrapolatedClock") {
        vm.clock_remaining = str_at(clock, &["Remaining"]);
    }
    if let Some(w) = state.topic("WeatherData") {
        vm.weather = Some(Weather {
            air: str_at(w, &["AirTemp"]).unwrap_or_default(),
            track: str_at(w, &["TrackTemp"]).unwrap_or_default(),
            wind: str_at(w, &["WindSpeed"]).unwrap_or_default(),
            humidity: str_at(w, &["Humidity"]).unwrap_or_default(),
            raining: str_at(w, &["Rainfall"]).map(|r| r != "0" && !r.is_empty()) == Some(true),
        });
    }
    vm.session_part = state
        .topic("TimingData")
        .and_then(|t| t.get("SessionPart"))
        .and_then(as_i64_lenient);

    vm.rows = build_rows(state);
    vm.fastest = vm
        .rows
        .iter()
        .filter_map(|r| Some((r.best_lap_secs?, r)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, r)| (r.best_lap.clone(), r.tla.clone()));
    vm.race_control = build_race_control(state);
    vm.yellow_sectors = build_yellow_sectors(state, vm.track_flag);
    vm.cars = build_cars(state, &vm.rows);
    vm
}

/// Marshal-sector numbers under an active (double) yellow (3.2), folded over the
/// ordered RaceControlMessages log:
/// - `YELLOW / DOUBLE YELLOW IN TRACK SECTOR n` → add n
/// - `CLEAR IN TRACK SECTOR n` → remove n
/// - a `GREEN` track-scope flag (all-clear) → clear the set
///
/// The whole set is also cleared when the overall TrackStatus is no longer
/// Green/Yellow (SC/VSC/Red tint the whole track and win over per-sector paint).
fn build_yellow_sectors(
    state: &SessionState,
    track_flag: Option<TrackFlag>,
) -> std::collections::HashSet<i64> {
    let mut set = std::collections::HashSet::new();
    // Under SC/VSC/red the whole track is already tinted; per-sector yellows are
    // subsumed and would only muddy the picture.
    if matches!(
        track_flag,
        Some(TrackFlag::SafetyCar | TrackFlag::Vsc | TrackFlag::Red)
    ) {
        return set;
    }
    let Some(msgs) = state
        .topic("RaceControlMessages")
        .and_then(|t| t.get("Messages"))
        .and_then(|m| m.as_array())
    else {
        return set;
    };
    for m in msgs.iter().filter(|m| !m.is_null()) {
        let text = str_at(m, &["Message"]).unwrap_or_default();
        let flag = str_at(m, &["Flag"]);
        let scope = str_at(m, &["Scope"]);
        // A track-wide green flag clears every sector yellow.
        if flag.as_deref() == Some("GREEN") && scope.as_deref() == Some("Track") {
            set.clear();
            continue;
        }
        if let Some(n) = sector_number(&text) {
            if text.contains("CLEAR") {
                set.remove(&n);
            } else if text.contains("YELLOW") {
                set.insert(n);
            }
        }
    }
    set
}

/// Extract the sector number from a `... IN TRACK SECTOR n` message.
fn sector_number(text: &str) -> Option<i64> {
    let tail = text.split("TRACK SECTOR").nth(1)?;
    tail.split_whitespace().next()?.parse().ok()
}

fn build_rows(state: &SessionState) -> Vec<Row> {
    let Some(lines) = state
        .topic("TimingData")
        .and_then(|t| t.get("Lines"))
        .and_then(|l| l.as_object())
    else {
        return Vec::new();
    };
    let drivers = state.topic("DriverList");
    let app_lines = state.topic("TimingAppData").and_then(|t| t.get("Lines"));
    // Pit-lane times keyed by car number (5.2); absent for sessions that don't
    // carry the topic (older archives), which degrades cleanly to None.
    let pit_times = state
        .topic("PitLaneTimeCollection")
        .and_then(|t| t.get("PitTimes"));

    let mut rows: Vec<Row> = Vec::with_capacity(lines.len());
    for (num, line) in lines {
        if num.starts_with('_') {
            continue;
        }
        let driver = drivers.and_then(|d| d.get(num));
        let tla = driver
            .and_then(|d| str_at(d, &["Tla"]))
            .unwrap_or_else(|| num.clone());
        let full_name = driver
            .and_then(|d| str_at(d, &["FullName"]))
            .unwrap_or_default();
        let team_color = driver
            .and_then(|d| str_at(d, &["TeamColour"]))
            .and_then(|h| parse_hex_color(&h))
            .unwrap_or((160, 160, 160));

        // In qualifying, gaps live under Stats — an array with one entry per Q segment.
        let quali_stats = line
            .get("Stats")
            .and_then(|s| s.as_array())
            .and_then(|arr| {
                arr.iter()
                    .rev()
                    .find(|e| str_at(e, &["TimeDiffToFastest"]).is_some_and(|v| !v.is_empty()))
                    .or_else(|| arr.last())
            });
        let gap = first_nonempty(&[
            str_at(line, &["GapToLeader"]),
            quali_stats.and_then(|s| str_at(s, &["TimeDiffToFastest"])),
            str_at(line, &["TimeDiffToFastest"]),
        ]);
        let interval = first_nonempty(&[
            str_at(line, &["IntervalToPositionAhead", "Value"]),
            quali_stats.and_then(|s| str_at(s, &["TimeDiffToPositionAhead"])),
            str_at(line, &["TimeDiffToPositionAhead"]),
        ]);
        let interval_secs = interval
            .strip_prefix('+')
            .and_then(|s| s.parse::<f64>().ok());

        let mut sectors: [TimeVal; 3] = Default::default();
        let mut segments: Vec<Vec<Segment>> = Vec::new();
        if let Some(secs) = line.get("Sectors").and_then(|s| s.as_array()) {
            for (i, sec) in secs.iter().take(3).enumerate() {
                sectors[i] = time_val(sec);
                let segs = sec
                    .get("Segments")
                    .and_then(|s| s.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|seg| {
                                match seg.get("Status").and_then(as_i64_lenient).unwrap_or(0) {
                                    2049 => Segment::PersonalBest,
                                    2051 => Segment::OverallBest,
                                    2064 => Segment::Pit,
                                    0 => Segment::NotSet,
                                    _ => Segment::Set,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                segments.push(segs);
            }
        }

        let stints = app_lines
            .and_then(|a| a.get(num))
            .and_then(|a| a.get("Stints"))
            .map(stints_from)
            .unwrap_or_default();

        let best_lap = str_at(line, &["BestLapTime", "Value"]).unwrap_or_default();

        // Speed trap (5.3): I1/I2/FL/ST, each with its PB/OB flag.
        let speeds = {
            let s = line.get("Speeds");
            [
                speed_val(s, "I1"),
                speed_val(s, "I2"),
                speed_val(s, "FL"),
                speed_val(s, "ST"),
            ]
        };
        // Quali per-segment bests (5.1): BestLapTimes[0..] = Q1/Q2/Q3.
        let segment_bests = best_lap_times(line.get("BestLapTimes"));

        rows.push(Row {
            position: str_at(line, &["Position"])
                .and_then(|p| p.parse().ok())
                .unwrap_or(99),
            number: num.clone(),
            tla,
            full_name,
            team_color,
            gap,
            interval,
            interval_secs,
            last_lap: time_val(line.get("LastLapTime").unwrap_or(&Value::Null)),
            best_lap_secs: parse_laptime(&best_lap),
            best_lap,
            laps: line.get("NumberOfLaps").and_then(as_i64_lenient),
            sectors,
            segments,
            stints,
            pit_count: line
                .get("NumberOfPitStops")
                .and_then(as_i64_lenient)
                .unwrap_or(0),
            in_pit: bool_at(line, "InPit"),
            pit_out: bool_at(line, "PitOut"),
            retired: bool_at(line, "Retired"),
            stopped: bool_at(line, "Stopped"),
            knocked_out: bool_at(line, "KnockedOut"),
            pit_time: pit_times.and_then(|p| str_at(p, &[num, "Duration"])),
            segment_bests,
            speeds,
        });
    }
    rows.sort_by_key(|r| r.position);
    rows
}

/// One speed-trap entry (5.3): text value + PB/OB flag, styled like lap times.
fn speed_val(speeds: Option<&Value>, key: &str) -> TimeVal {
    match speeds.and_then(|s| s.get(key)) {
        Some(v) => time_val(v),
        None => TimeVal::default(),
    }
}

/// Per-segment quali bests from `BestLapTimes` (5.1). Arrives as an array or an
/// index-keyed object (like Stints); each entry is `{"Value": "1:29.8"}`.
fn best_lap_times(v: Option<&Value>) -> Vec<String> {
    let items: Vec<&Value> = match v {
        Some(Value::Array(arr)) => arr.iter().collect(),
        Some(Value::Object(map)) => {
            let mut keyed: Vec<(usize, &Value)> = map
                .iter()
                .filter_map(|(k, e)| Some((k.parse::<usize>().ok()?, e)))
                .collect();
            keyed.sort_by_key(|(i, _)| *i);
            keyed.into_iter().map(|(_, e)| e).collect()
        }
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .map(|e| str_at(e, &["Value"]).unwrap_or_default())
        .collect()
}

/// Stints may arrive as an array or as an index-keyed object.
fn stints_from(stints: &Value) -> Vec<Stint> {
    let items: Vec<&Value> = match stints {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(map) => {
            let mut keyed: Vec<(usize, &Value)> = map
                .iter()
                .filter_map(|(k, v)| Some((k.parse::<usize>().ok()?, v)))
                .collect();
            keyed.sort_by_key(|(i, _)| *i);
            keyed.into_iter().map(|(_, v)| v).collect()
        }
        _ => Vec::new(),
    };
    items
        .into_iter()
        .map(|s| Stint {
            compound: match str_at(s, &["Compound"]).as_deref() {
                Some("SOFT") => 'S',
                Some("MEDIUM") => 'M',
                Some("HARD") => 'H',
                Some("INTERMEDIATE") => 'I',
                Some("WET") => 'W',
                _ => '?',
            },
            age_laps: s.get("TotalLaps").and_then(as_i64_lenient).unwrap_or(0),
        })
        .collect()
}

fn build_race_control(state: &SessionState) -> Vec<RcMessage> {
    let Some(msgs) = state
        .topic("RaceControlMessages")
        .and_then(|t| t.get("Messages"))
        .and_then(|m| m.as_array())
    else {
        return Vec::new();
    };
    msgs.iter()
        .filter(|m| !m.is_null())
        .map(|m| {
            let text = str_at(m, &["Message"]).unwrap_or_default();
            RcMessage {
                time: str_at(m, &["Utc"])
                    .and_then(|u| u.get(11..16).map(str::to_string))
                    .unwrap_or_default(),
                lap: m.get("Lap").and_then(as_i64_lenient),
                category: str_at(m, &["Category"]).unwrap_or_default(),
                flag: str_at(m, &["Flag"]),
                important: !is_noise(&text),
                text,
            }
        })
        .collect()
}

/// Routine chatter that shouldn't surface in the ticker.
fn is_noise(text: &str) -> bool {
    text.contains("IN TRACK SECTOR") || text.contains("BLUE FLAG") || text.contains("RISK OF RAIN")
}

fn build_cars(state: &SessionState, rows: &[Row]) -> Vec<CarDot> {
    rows.iter()
        .filter(|r| !r.retired && !r.stopped)
        .filter_map(|r| {
            let pos = state.positions.get(&r.number)?;
            let in_pit = !pos.on_track || r.in_pit;
            // A car is "on a hot lap" when it's out of the pits and either just
            // exited (out-lap becomes a flyer) or is currently posting sectors.
            let posting = r.segments.iter().flatten().any(|s| {
                matches!(
                    s,
                    Segment::PersonalBest | Segment::OverallBest | Segment::Set
                )
            });
            let hot_lap = !in_pit && !r.pit_out && posting;
            Some(CarDot {
                x: pos.x,
                y: pos.y,
                color: r.team_color,
                tla: r.tla.clone(),
                in_pit,
                hot_lap,
            })
        })
        .collect()
}

fn time_val(v: &Value) -> TimeVal {
    let flag = if v.get("OverallFastest").and_then(|b| b.as_bool()) == Some(true) {
        Some(TimeFlag::OverallBest)
    } else if v.get("PersonalFastest").and_then(|b| b.as_bool()) == Some(true) {
        Some(TimeFlag::PersonalBest)
    } else {
        None
    };
    TimeVal {
        text: str_at(v, &["Value"]).unwrap_or_default(),
        flag,
    }
}

/// "1:31.107" or "58.123" → seconds.
fn parse_laptime(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    match s.split_once(':') {
        Some((m, rest)) => Some(m.parse::<f64>().ok()? * 60.0 + rest.parse::<f64>().ok()?),
        None => s.parse().ok(),
    }
}

fn str_at(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn bool_at(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

fn as_i64_lenient(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn first_nonempty(opts: &[Option<String>]) -> String {
    opts.iter()
        .flatten()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default()
}

fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    Some((
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::FeedMessage;
    use serde_json::json;

    /// Build a SessionState by applying one full-snapshot message per topic,
    /// mirroring how the feed delivers its initial state.
    fn state(topics: &[(&str, Value)]) -> SessionState {
        let mut s = SessionState::default();
        for (topic, data) in topics {
            s.apply(FeedMessage {
                topic: topic.to_string(),
                data: data.clone(),
                ts: None,
            });
        }
        s
    }

    fn vm_with(session_type: &str, part: Option<i64>, positions: &[u32]) -> ViewModel {
        let mut vm = ViewModel {
            session_type: session_type.into(),
            session_part: part,
            ..Default::default()
        };
        vm.rows = positions
            .iter()
            .map(|&p| Row {
                position: p,
                number: p.to_string(),
                tla: format!("D{p}"),
                full_name: String::new(),
                team_color: (0, 0, 0),
                gap: String::new(),
                interval: String::new(),
                interval_secs: None,
                last_lap: TimeVal::default(),
                best_lap: String::new(),
                best_lap_secs: None,
                laps: None,
                sectors: Default::default(),
                segments: Vec::new(),
                stints: Vec::new(),
                pit_count: 0,
                in_pit: false,
                pit_out: false,
                retired: false,
                stopped: false,
                knocked_out: false,
                pit_time: None,
                segment_bests: Vec::new(),
                speeds: Default::default(),
            })
            .collect();
        vm
    }

    #[test]
    fn parse_laptime_forms() {
        assert_eq!(parse_laptime("1:31.107"), Some(91.107));
        assert_eq!(parse_laptime("58.123"), Some(58.123));
        assert_eq!(parse_laptime(""), None);
        assert_eq!(parse_laptime("--"), None);
    }

    #[test]
    fn first_nonempty_skips_empty_and_none() {
        assert_eq!(
            first_nonempty(&[
                None,
                Some(String::new()),
                Some("+1.2".into()),
                Some("x".into())
            ]),
            "+1.2"
        );
        assert_eq!(first_nonempty(&[None, Some(String::new())]), "");
    }

    #[test]
    fn stints_from_array_and_object() {
        let arr = json!([
            {"Compound": "SOFT", "TotalLaps": 11},
            {"Compound": "MEDIUM", "TotalLaps": 14},
        ]);
        let s = stints_from(&arr);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].compound, 'S');
        assert_eq!(s[1].age_laps, 14);

        // Index-keyed object form must sort by numeric key, not string order.
        let obj = json!({
            "0": {"Compound": "HARD", "TotalLaps": 5},
            "10": {"Compound": "INTERMEDIATE", "TotalLaps": 3},
            "2": {"Compound": "WET", "TotalLaps": 8},
        });
        let s = stints_from(&obj);
        assert_eq!(
            s.iter().map(|x| x.compound).collect::<Vec<_>>(),
            vec!['H', 'W', 'I']
        );
    }

    #[test]
    fn quali_cutoff_matrix() {
        // Q1 keeps n - (n-10)/2; Q2 keeps 10.
        assert_eq!(
            vm_with("Qualifying", Some(1), &grid(20)).quali_cutoff(),
            Some(15)
        );
        assert_eq!(
            vm_with("Qualifying", Some(1), &grid(19)).quali_cutoff(),
            Some(15)
        );
        assert_eq!(
            vm_with("Qualifying", Some(1), &grid(22)).quali_cutoff(),
            Some(16)
        );
        assert_eq!(
            vm_with("Qualifying", Some(2), &grid(20)).quali_cutoff(),
            Some(10)
        );
        assert_eq!(
            vm_with("Qualifying", Some(2), &grid(19)).quali_cutoff(),
            Some(10)
        );
        assert_eq!(
            vm_with("Qualifying", Some(2), &grid(22)).quali_cutoff(),
            Some(10)
        );
        // Q3 (part 3) and non-quali → None.
        assert_eq!(
            vm_with("Qualifying", Some(3), &grid(10)).quali_cutoff(),
            None
        );
        assert_eq!(vm_with("Race", Some(1), &grid(20)).quali_cutoff(), None);
    }

    fn grid(n: u32) -> Vec<u32> {
        (1..=n).collect()
    }

    #[test]
    fn part_label_prefixes_sprint() {
        let mut vm = vm_with("Qualifying", Some(2), &grid(20));
        vm.session_name = "Qualifying".into();
        assert_eq!(vm.part_label().as_deref(), Some("Q2"));
        vm.session_name = "Sprint Qualifying".into();
        assert_eq!(vm.part_label().as_deref(), Some("SQ2"));
    }

    #[test]
    fn build_rows_race_line() {
        let s = state(&[
            (
                "DriverList",
                json!({"1": {"Tla": "VER", "FullName": "Max VERSTAPPEN", "TeamColour": "3671C6"}}),
            ),
            (
                "TimingData",
                json!({"Lines": {"1": {
                    "Position": "1",
                    "GapToLeader": "+2.502",
                    "IntervalToPositionAhead": {"Value": "+0.8"},
                    "NumberOfPitStops": 1,
                    "InPit": false,
                    "LastLapTime": {"Value": "1:32.807", "PersonalFastest": true},
                    "BestLapTime": {"Value": "1:31.107"},
                    "NumberOfLaps": 32
                }}}),
            ),
            (
                "TimingAppData",
                json!({"Lines": {"1": {"Stints": [
                    {"Compound": "SOFT", "TotalLaps": 12},
                    {"Compound": "MEDIUM", "TotalLaps": 17}
                ]}}}),
            ),
        ]);
        let rows = build_rows(&s);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.tla, "VER");
        assert_eq!(r.team_color, (0x36, 0x71, 0xC6));
        assert_eq!(r.gap, "+2.502");
        assert_eq!(r.interval, "+0.8");
        assert_eq!(r.interval_secs, Some(0.8));
        assert_eq!(r.pit_count, 1);
        assert_eq!(r.laps, Some(32));
        assert_eq!(r.last_lap.flag, Some(TimeFlag::PersonalBest));
        assert_eq!(r.best_lap_secs, Some(91.107));
        assert_eq!(r.stints.len(), 2);
        assert_eq!(r.stint().unwrap().compound, 'M');
    }

    #[test]
    fn build_rows_quali_stats_fallback() {
        // In quali, the gap lives in the Stats array (last non-empty segment).
        let s = state(&[(
            "TimingData",
            json!({"Lines": {"7": {
                "Position": "3",
                "Stats": [
                    {"TimeDiffToFastest": "+0.100", "TimeDiffToPositionAhead": "+0.100"},
                    {"TimeDiffToFastest": "+0.250", "TimeDiffToPositionAhead": "+0.050"}
                ],
                "BestLapTime": {"Value": "1:29.500"}
            }}}),
        )]);
        let rows = build_rows(&s);
        assert_eq!(rows[0].gap, "+0.250");
        assert_eq!(rows[0].interval, "+0.050");
        assert_eq!(rows[0].best_lap_secs, Some(89.5));
    }

    #[test]
    fn build_rows_speeds_and_segment_bests() {
        let s = state(&[(
            "TimingData",
            json!({"Lines": {"1": {
                "Position": "1",
                "BestLapTime": {"Value": "1:10.0"},
                "Speeds": {
                    "I1": {"Value": "312", "PersonalFastest": true},
                    "ST": {"Value": "318", "OverallFastest": true}
                },
                "BestLapTimes": [{"Value": "1:11.393"}, {"Value": "1:10.811"}, {}]
            }}}),
        )]);
        let r = &build_rows(&s)[0];
        assert_eq!(r.speeds[0].text, "312");
        assert_eq!(r.speeds[0].flag, Some(TimeFlag::PersonalBest));
        assert_eq!(r.speeds[3].text, "318");
        assert_eq!(r.speeds[3].flag, Some(TimeFlag::OverallBest));
        assert_eq!(r.speeds[1].text, ""); // I2 absent
        assert_eq!(r.segment_bests, vec!["1:11.393", "1:10.811", ""]);
    }

    #[test]
    fn build_rows_pit_time_from_collection() {
        let s = state(&[
            (
                "TimingData",
                json!({"Lines": {"23": {"Position": "1", "BestLapTime": {"Value": "1:10.0"}}}}),
            ),
            (
                "PitLaneTimeCollection",
                json!({"PitTimes": {"23": {"RacingNumber": "23", "Duration": "21.2", "Lap": "12"}}}),
            ),
        ]);
        let r = &build_rows(&s)[0];
        assert_eq!(r.pit_time.as_deref(), Some("21.2"));
    }

    #[test]
    fn build_rows_knocked_out_driver() {
        let s = state(&[(
            "TimingData",
            json!({"Lines": {"5": {
                "Position": "18",
                "KnockedOut": true,
                "BestLapTime": {"Value": "1:33.000"}
            }}}),
        )]);
        let rows = build_rows(&s);
        assert!(rows[0].knocked_out);
        assert_eq!(rows[0].position, 18);
    }

    fn rc(msgs: Value) -> SessionState {
        state(&[("RaceControlMessages", json!({ "Messages": msgs }))])
    }

    #[test]
    fn yellow_sectors_fold_add_and_clear() {
        let s = rc(json!([
            {"Flag": "YELLOW", "Message": "YELLOW IN TRACK SECTOR 16"},
            {"Flag": "DOUBLE YELLOW", "Message": "DOUBLE YELLOW IN TRACK SECTOR 17"},
            {"Flag": "CLEAR", "Message": "CLEAR IN TRACK SECTOR 16"},
        ]));
        let set = build_yellow_sectors(&s, Some(TrackFlag::Yellow));
        assert_eq!(set, std::collections::HashSet::from([17]));
    }

    #[test]
    fn yellow_sectors_green_flag_clears_all() {
        let s = rc(json!([
            {"Flag": "YELLOW", "Message": "YELLOW IN TRACK SECTOR 3"},
            {"Flag": "YELLOW", "Message": "YELLOW IN TRACK SECTOR 5"},
            {"Flag": "GREEN", "Scope": "Track", "Message": "GREEN LIGHT - PIT EXIT OPEN"},
        ]));
        let set = build_yellow_sectors(&s, Some(TrackFlag::Green));
        assert!(set.is_empty());
    }

    #[test]
    fn yellow_sectors_empty_under_safety_car() {
        // SC tints the whole track; per-sector yellows are subsumed.
        let s = rc(json!([
            {"Flag": "YELLOW", "Message": "YELLOW IN TRACK SECTOR 3"},
        ]));
        let set = build_yellow_sectors(&s, Some(TrackFlag::SafetyCar));
        assert!(set.is_empty());
    }

    #[test]
    fn cutoff_time_reads_cutoff_drivers_best() {
        let mut vm = vm_with("Qualifying", Some(1), &grid(20)); // cutoff = P15
        vm.rows[14].best_lap_secs = Some(90.5); // P15 driver
        assert_eq!(vm.cutoff_time(), Some(90.5));
    }
}
