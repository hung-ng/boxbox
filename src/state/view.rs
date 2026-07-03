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
    pub sectors: [TimeVal; 3],
    pub segments: Vec<Vec<Segment>>, // per sector
    pub stints: Vec<Stint>,          // full history, current last
    pub pit_count: i64,
    pub in_pit: bool,
    pub pit_out: bool,
    pub retired: bool,
    pub stopped: bool,
    pub knocked_out: bool,
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
    pub in_pit: bool,
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
}

impl ViewModel {
    pub fn is_race(&self) -> bool {
        self.session_type == "Race" || self.lap.is_some()
    }

    pub fn is_qualifying(&self) -> bool {
        self.session_type == "Qualifying"
    }

    /// Last classified position that survives the current quali segment,
    /// e.g. 15 in Q1 with 20 cars. None outside Q1/Q2.
    pub fn quali_cutoff(&self) -> Option<u32> {
        if !self.is_qualifying() {
            return None;
        }
        let part = self.session_part?;
        let n = self.rows.len() as i64;
        if n < 12 || !(1..=2).contains(&part) {
            return None;
        }
        let elim_per_part = (n - 10) / 2;
        Some((n - elim_per_part * part) as u32)
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
    vm.cars = build_cars(state, &vm.rows);
    vm
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
        });
    }
    rows.sort_by_key(|r| r.position);
    rows
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
    text.contains("IN TRACK SECTOR")
        || text.contains("BLUE FLAG")
        || text.contains("RISK OF RAIN")
}

fn build_cars(state: &SessionState, rows: &[Row]) -> Vec<CarDot> {
    rows.iter()
        .filter(|r| !r.retired && !r.stopped)
        .filter_map(|r| {
            let pos = state.positions.get(&r.number)?;
            Some(CarDot {
                x: pos.x,
                y: pos.y,
                color: r.team_color,
                in_pit: !pos.on_track,
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
