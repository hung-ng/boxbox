mod focus;
mod header;
mod map;
mod racecontrol;
mod tower;

use crate::message::{PlaybackControl, SourceEvent};
use crate::source::archive::Archive;
use crate::state::view::TimeVal;
use crate::state::{SessionState, view};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::TableState;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

/// A driver's position moved up (gained places) or down since the last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// How long a position-change arrow / just-completed lap stays flashed.
const POS_FLASH: Duration = Duration::from_secs(3);
const LAP_FLASH: Duration = Duration::from_secs(5);
/// How long a completed pit-stop duration stays flashed in the INT cell (5.2).
const PIT_FLASH: Duration = Duration::from_secs(5);

/// Derived pit-lane trace (3.4): stop collecting past this many points (plenty
/// for a lane), and only accept samples this far apart in raw circuit units
/// (kills parked-in-garage clusters). Raw coords span tens of thousands of
/// units, so ~40 is roughly a car length.
const PIT_LANE_CAP: usize = 2000;
const PIT_LANE_MIN_DIST: f64 = 40.0;

pub const SPEED_STEPS: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0];

/// Which panes are visible. Chosen automatically from the terminal size each
/// frame; `m` sets an override that sticks until cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Map (hero, left) beside the timing tower + driver panel (right).
    Split,
    /// Track map fills the body.
    MapOnly,
    /// Timing tower fills the body.
    TowerOnly,
}

pub struct App {
    pub state: SessionState,
    pub vm: view::ViewModel,
    pub is_replay: bool,
    pub speed: f64,
    pub paused: bool,
    pub sim_clock: Option<Duration>,
    /// Total recording length for the replay timeline (None for live) (1.5).
    pub total: Option<Duration>,
    pub status: String,
    /// User-forced view mode; `None` means auto-pick from the terminal size.
    pub view_override: Option<ViewMode>,
    /// Index into vm.rows of the driver shown in the focus panel.
    pub selected: usize,
    /// TLA of the selected driver, so selection follows them through position changes.
    selected_tla: Option<String>,
    pub rc_open: bool,
    pub rc_scroll: usize,
    /// Keybindings help overlay (toggled with `?`).
    pub help_open: bool,
    pub track: Option<map::TrackOutline>,
    pub ended: bool,
    /// Circuit-outline fetch retries: one failed fetch shouldn't cost the whole
    /// session its map. Retry up to `CIRCUIT_MAX_ATTEMPTS`, `CIRCUIT_RETRY` apart.
    circuit_attempts: u8,
    circuit_last_try: Option<Instant>,
    /// TLA holding the session-best lap, and when it was last claimed — drives
    /// the ~1s magenta pulse on the map when a new overall best appears.
    last_fastest_tla: Option<String>,
    fastest_since: Option<Instant>,
    /// Tower scroll state (3.3): keeps the selected row on screen in Split.
    pub table_state: TableState,
    /// Position-change flashes (3.1): TLA → (direction, when it changed).
    pub pos_flash: HashMap<String, (Dir, Instant)>,
    prev_positions: HashMap<String, u32>,
    /// Just-completed-lap flashes (3.4): TLA → (the lap time, when it landed).
    pub lap_flash: HashMap<String, (TimeVal, Instant)>,
    prev_last_lap: HashMap<String, String>,
    /// Pit-stop-duration flashes (5.2): TLA → (duration text, when it landed).
    pub pit_flash: HashMap<String, (String, Instant)>,
    prev_pit_time: HashMap<String, String>,
    /// Derived pit-lane trace (3.4): raw (untransformed) Position samples taken
    /// while a car is in the pits. Transformed at draw time like car dots.
    pub pit_lane: Vec<(f64, f64)>,
    /// Locally-ticking clock anchor (3.11): the last feed-reported remaining
    /// time and the moment (wall for live, sim for replay) it was observed.
    clock_base: Option<Duration>,
    clock_at_wall: Option<Instant>,
    clock_at_sim: Option<Duration>,
    prev_clock: Option<String>,
    /// Next upcoming session line for the live empty state (3.10).
    next_session: Option<String>,
}

/// How long a new fastest-lap pulse flashes on the map.
const PULSE: Duration = Duration::from_millis(1000);

/// Circuit-outline fetch: give up after this many tries, this far apart.
const CIRCUIT_MAX_ATTEMPTS: u8 = 3;
const CIRCUIT_RETRY: Duration = Duration::from_secs(15);

impl App {
    /// TLA of the driver the map should spotlight: the explicitly-followed one,
    /// else whatever row the cursor currently sits on.
    pub fn selected_tla(&self) -> Option<String> {
        self.selected_tla
            .clone()
            .or_else(|| self.vm.rows.get(self.selected).map(|r| r.tla.clone()))
    }

    /// TLA to flash magenta this frame, if a new fastest lap is still within the
    /// pulse window.
    pub fn pulsing_tla(&self) -> Option<&str> {
        let since = self.fastest_since?;
        (since.elapsed() < PULSE)
            .then_some(self.last_fastest_tla.as_deref())
            .flatten()
    }

    /// Position-change arrow to show for a driver this frame, if still fresh (3.1).
    pub fn pos_arrow(&self, tla: &str) -> Option<Dir> {
        self.pos_flash
            .get(tla)
            .filter(|(_, at)| at.elapsed() < POS_FLASH)
            .map(|(d, _)| *d)
    }

    /// Just-completed lap time to show in the LIVE LAP column, if still fresh (3.4).
    pub fn recent_lap(&self, tla: &str) -> Option<&TimeVal> {
        self.lap_flash
            .get(tla)
            .filter(|(_, at)| at.elapsed() < LAP_FLASH)
            .map(|(t, _)| t)
    }

    /// Just-completed pit-stop duration to flash in the INT cell, if fresh (5.2).
    pub fn recent_pit(&self, tla: &str) -> Option<&str> {
        self.pit_flash
            .get(tla)
            .filter(|(_, at)| at.elapsed() < PIT_FLASH)
            .map(|(t, _)| t.as_str())
    }

    /// Locally-extrapolated remaining time (3.11), only while the session is
    /// Started. Ticks between feed updates: wall-clock in live, sim-clock in
    /// replay (so it's inherently speed/pause-correct). Formatted `H:MM:SS`.
    pub fn ticking_clock(&self) -> Option<String> {
        if self.vm.session_status != "Started" {
            return None;
        }
        let base = self.clock_base?;
        let elapsed = if self.is_replay {
            let now = self.sim_clock?;
            let at = self.clock_at_sim?;
            now.checked_sub(at).unwrap_or(Duration::ZERO)
        } else {
            self.clock_at_wall?.elapsed()
        };
        let remaining = base.checked_sub(elapsed).unwrap_or(Duration::ZERO);
        let s = remaining.as_secs();
        Some(format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60))
    }
}

/// Build a color from RGB, degrading gracefully on non-truecolor terminals
/// (7.1). `Color::Rgb` silently renders as black where 24-bit color isn't
/// supported, so when `COLORTERM` doesn't advertise truecolor we quantize to
/// the xterm-256 color cube instead. Every `Color::Rgb` construction routes
/// through this so team colors and tints survive on 256-color terminals.
pub fn color(r: u8, g: u8, b: u8) -> ratatui::style::Color {
    use ratatui::style::Color;
    use std::sync::OnceLock;
    static TRUECOLOR: OnceLock<bool> = OnceLock::new();
    let truecolor = *TRUECOLOR.get_or_init(|| {
        std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false)
    });
    if truecolor {
        Color::Rgb(r, g, b)
    } else {
        Color::Indexed(quantize_256(r, g, b))
    }
}

/// Map an RGB triple to the nearest xterm-256 6×6×6 color-cube index (7.1).
/// Cube levels are at 0, 95, 135, 175, 215, 255; the standard mapping rounds
/// each channel to its nearest level, then indexes `16 + 36*r + 6*g + b`.
fn quantize_256(r: u8, g: u8, b: u8) -> u8 {
    fn level(c: u8) -> u8 {
        // Boundaries between the six cube levels (0,95,135,175,215,255).
        match c {
            0..=47 => 0,
            48..=114 => 1,
            115..=154 => 2,
            155..=194 => 3,
            195..=234 => 4,
            _ => 5,
        }
    }
    16 + 36 * level(r) + 6 * level(g) + level(b)
}

/// Parse the feed's `Remaining` clock string (`H:MM:SS` or `HH:MM:SS`) to a Duration.
fn parse_clock(s: &str) -> Option<Duration> {
    let mut parts = s.split(':');
    let h: u64 = parts.next()?.trim().parse().ok()?;
    let m: u64 = parts.next()?.parse().ok()?;
    let sec: u64 = parts.next()?.parse().ok()?;
    Some(Duration::from_secs(h * 3600 + m * 60 + sec))
}

pub fn run(
    rx: Receiver<SourceEvent>,
    tx: Sender<SourceEvent>,
    ctrl: Option<UnboundedSender<PlaybackControl>>,
    rt: tokio::runtime::Handle,
    initial_speed: f64,
    total: Option<Duration>,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App {
        state: SessionState::default(),
        vm: view::ViewModel::default(),
        is_replay: ctrl.is_some(),
        speed: initial_speed,
        paused: false,
        sim_clock: None,
        total,
        status: String::new(),
        view_override: None,
        selected: 0,
        selected_tla: None,
        rc_open: false,
        rc_scroll: 0,
        help_open: false,
        track: None,
        ended: false,
        circuit_attempts: 0,
        circuit_last_try: None,
        last_fastest_tla: None,
        fastest_since: None,
        table_state: TableState::default(),
        pos_flash: HashMap::new(),
        prev_positions: HashMap::new(),
        lap_flash: HashMap::new(),
        prev_last_lap: HashMap::new(),
        pit_flash: HashMap::new(),
        prev_pit_time: HashMap::new(),
        pit_lane: Vec::new(),
        clock_base: None,
        clock_at_wall: None,
        clock_at_sim: None,
        prev_clock: None,
        next_session: None,
    };

    // Per-frame feed-drain cap, so rendering never starves during a seek's
    // fast-applied backlog.
    const DRAIN_CAP: usize = 20_000;

    let result = loop {
        // Drain pending feed messages (bounded per frame so rendering never starves).
        let mut drained = 0;
        while drained < DRAIN_CAP {
            match rx.try_recv() {
                Ok(SourceEvent::Message(msg)) => app.state.apply(msg),
                Ok(SourceEvent::Info(s)) => app.status = s,
                Ok(SourceEvent::Clock(t)) => app.sim_clock = Some(t),
                Ok(SourceEvent::Circuit(v)) => {
                    app.track = map::TrackOutline::parse(&v);
                }
                Ok(SourceEvent::NextSession(s)) => app.next_session = Some(s),
                Ok(SourceEvent::Reset) => {
                    // Live feed reconnected, or a replay rewound: drop stale
                    // merged state and re-arm the circuit fetch so a fresh
                    // session re-fetches its map (the fetch loop is gated on
                    // track.is_none(), so keeping app.track is harmless).
                    app.state.reset();
                    app.circuit_attempts = 0;
                    app.circuit_last_try = None;
                    app.pos_flash.clear();
                    app.prev_positions.clear();
                    app.lap_flash.clear();
                    app.prev_last_lap.clear();
                    app.pit_flash.clear();
                    app.prev_pit_time.clear();
                    // A rewind revives a finished replay (1.4): clear the ended
                    // marker and its status line.
                    if app.ended {
                        app.ended = false;
                        app.status.clear();
                    }
                    // Stale ticking-clock anchors would tick from the wrong base
                    // after a seek — reset them so they re-anchor on next feed.
                    app.clock_base = None;
                    app.clock_at_wall = None;
                    app.clock_at_sim = None;
                    app.prev_clock = None;
                    // Derived pit-lane trace rebuilds from replayed data (3.4).
                    app.pit_lane.clear();
                }
                Ok(SourceEvent::Ended) => {
                    app.ended = true;
                    app.status = "end of session data".into();
                    break;
                }
                Err(_) => break,
            }
            drained += 1;
        }

        if app.state.dirty {
            app.state.dirty = false;
            app.vm = view::build(&app.state);
            // A new session-best lap-holder starts the pulse; ignore the first
            // appearance so we don't flash on initial data load.
            let fastest_tla = app.vm.fastest.as_ref().map(|(_, tla)| tla.clone());
            if fastest_tla != app.last_fastest_tla {
                if app.last_fastest_tla.is_some() && fastest_tla.is_some() {
                    app.fastest_since = Some(Instant::now());
                }
                app.last_fastest_tla = fastest_tla;
            }
            // Position-change flashes (3.1): diff each driver's position against
            // the previous frame; record an arrow that fades after POS_FLASH.
            let now = Instant::now();
            for r in &app.vm.rows {
                if let Some(&prev) = app.prev_positions.get(&r.tla) {
                    if r.position < prev {
                        app.pos_flash.insert(r.tla.clone(), (Dir::Up, now));
                    } else if r.position > prev {
                        app.pos_flash.insert(r.tla.clone(), (Dir::Down, now));
                    }
                }
                app.prev_positions.insert(r.tla.clone(), r.position);
            }
            app.pos_flash.retain(|_, (_, at)| at.elapsed() < POS_FLASH);

            // Just-completed-lap flashes (3.4): when a driver's LastLapTime text
            // changes, spotlight it in the LIVE LAP column for LAP_FLASH.
            for r in &app.vm.rows {
                let text = &r.last_lap.text;
                if !text.is_empty() {
                    let changed = app
                        .prev_last_lap
                        .get(&r.tla)
                        .map(|p| p != text)
                        .unwrap_or(false);
                    if changed {
                        app.lap_flash
                            .insert(r.tla.clone(), (r.last_lap.clone(), now));
                    }
                }
                app.prev_last_lap.insert(r.tla.clone(), text.clone());
            }
            app.lap_flash.retain(|_, (_, at)| at.elapsed() < LAP_FLASH);

            // Pit-stop-duration flashes (5.2): the feed publishes a car's
            // pit-lane time briefly (then deletes it), so flash on the rising
            // edge — a newly-present value — and hold it in the INT cell for
            // PIT_FLASH. prev_pit_time tracks presence ("" = absent) so a repeat
            // stop with the same duration still re-triggers.
            for r in &app.vm.rows {
                let cur = r.pit_time.clone().unwrap_or_default();
                let prev = app.prev_pit_time.get(&r.tla);
                let rising = !cur.is_empty() && prev.map(|p| p.is_empty()).unwrap_or(true);
                if rising {
                    app.pit_flash.insert(r.tla.clone(), (cur.clone(), now));
                }
                app.prev_pit_time.insert(r.tla.clone(), cur);
            }
            app.pit_flash.retain(|_, (_, at)| at.elapsed() < PIT_FLASH);

            // Derived pit-lane trace (3.4): while a car is in the pits, its raw
            // Position samples trace the lane. Accept a sample only if it's far
            // enough from the last accepted one (kills parked-in-garage
            // clusters) and cap the total so it never grows unbounded.
            if app.pit_lane.len() < PIT_LANE_CAP {
                for r in &app.vm.rows {
                    if !r.in_pit {
                        continue;
                    }
                    if let Some(pos) = app.state.positions.get(&r.number) {
                        let sample = (pos.x, pos.y);
                        let far = app.pit_lane.last().map(|&(lx, ly)| {
                            (lx - sample.0).powi(2) + (ly - sample.1).powi(2)
                                >= PIT_LANE_MIN_DIST * PIT_LANE_MIN_DIST
                        });
                        if far.unwrap_or(true) {
                            app.pit_lane.push(sample);
                            if app.pit_lane.len() >= PIT_LANE_CAP {
                                break;
                            }
                        }
                    }
                }
            }

            // Ticking-clock anchor (3.11): re-anchor whenever the feed reports a
            // new Remaining value, storing the moment (wall + sim) we saw it.
            if app.vm.clock_remaining != app.prev_clock {
                app.prev_clock = app.vm.clock_remaining.clone();
                if let Some(base) = app.vm.clock_remaining.as_deref().and_then(parse_clock) {
                    app.clock_base = Some(base);
                    app.clock_at_wall = Some(Instant::now());
                    app.clock_at_sim = app.sim_clock;
                }
            }

            // Keep the focus on the same driver as positions shuffle.
            if let Some(tla) = &app.selected_tla {
                if let Some(i) = app.vm.rows.iter().position(|r| &r.tla == tla) {
                    app.selected = i;
                }
            }
            app.selected = app.selected.min(app.vm.rows.len().saturating_sub(1));
        }

        // Kick off (and retry) the circuit outline fetch once we know which
        // track this is. A single transient failure shouldn't leave the session
        // mapless, so retry a few times before giving up silently.
        if app.track.is_none() && app.circuit_attempts < CIRCUIT_MAX_ATTEMPTS {
            if let (Some(key), Some(year)) = (app.vm.circuit_key, app.vm.year) {
                let due = app
                    .circuit_last_try
                    .map(|t| t.elapsed() >= CIRCUIT_RETRY)
                    .unwrap_or(true);
                if due {
                    app.circuit_attempts += 1;
                    app.circuit_last_try = Some(Instant::now());
                    let tx = tx.clone();
                    rt.spawn(async move {
                        if let Ok(archive) = Archive::new() {
                            if let Ok(v) = archive.circuit_outline(key, year).await {
                                let _ = tx.send(SourceEvent::Circuit(v));
                            }
                        }
                    });
                }
            }
        }

        terminal.draw(|f| draw(f, &mut app))?;
        // Size used to resolve `m`'s cycle relative to what's currently shown.
        let size = terminal.size().unwrap_or_default();

        // A full drain means more of a seek backlog is already queued: keep
        // consuming it at full speed instead of idling a frame in poll.
        let poll_timeout = if drained >= DRAIN_CAP {
            Duration::ZERO
        } else {
            Duration::from_millis(33)
        };
        if event::poll(poll_timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    // `q` and Esc both close the topmost overlay first, then quit.
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if app.help_open {
                            app.help_open = false;
                        } else if app.rc_open {
                            app.rc_open = false;
                        } else {
                            break Ok(());
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break Ok(());
                    }
                    KeyCode::Char('r') => {
                        app.rc_open = !app.rc_open;
                        app.rc_scroll = 0;
                    }
                    KeyCode::Char('?') => {
                        app.help_open = !app.help_open;
                    }
                    KeyCode::Up => {
                        if app.rc_open {
                            app.rc_scroll = app.rc_scroll.saturating_add(1);
                        } else if app.selected > 0 {
                            app.selected -= 1;
                            app.selected_tla = app.vm.rows.get(app.selected).map(|r| r.tla.clone());
                        }
                    }
                    KeyCode::Down => {
                        if app.rc_open {
                            app.rc_scroll = app.rc_scroll.saturating_sub(1);
                        } else if app.selected + 1 < app.vm.rows.len() {
                            app.selected += 1;
                            app.selected_tla = app.vm.rows.get(app.selected).map(|r| r.tla.clone());
                        }
                    }
                    // Paging (4.1): ±5 rows in the tower, or ±5 lines in the RC
                    // overlay (consistent with ↑↓ moving by 1). Home/End jump to
                    // P1 / last row (oldest / newest in the overlay).
                    KeyCode::PageUp => {
                        if app.rc_open {
                            app.rc_scroll = app.rc_scroll.saturating_add(5);
                        } else {
                            app.selected = app.selected.saturating_sub(5);
                            app.selected_tla = app.vm.rows.get(app.selected).map(|r| r.tla.clone());
                        }
                    }
                    KeyCode::PageDown => {
                        if app.rc_open {
                            app.rc_scroll = app.rc_scroll.saturating_sub(5);
                        } else if !app.vm.rows.is_empty() {
                            app.selected = (app.selected + 5).min(app.vm.rows.len() - 1);
                            app.selected_tla = app.vm.rows.get(app.selected).map(|r| r.tla.clone());
                        }
                    }
                    KeyCode::Home => {
                        if app.rc_open {
                            // Oldest message: scroll all the way back.
                            app.rc_scroll = app.vm.race_control.len();
                        } else {
                            app.selected = 0;
                            app.selected_tla = app.vm.rows.first().map(|r| r.tla.clone());
                        }
                    }
                    KeyCode::End => {
                        if app.rc_open {
                            app.rc_scroll = 0; // newest
                        } else if !app.vm.rows.is_empty() {
                            app.selected = app.vm.rows.len() - 1;
                            app.selected_tla = app.vm.rows.last().map(|r| r.tla.clone());
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = !app.paused;
                            let _ = ctrl.send(PlaybackControl::TogglePause);
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        app.speed = step_speed(app.speed, true);
                        if let Some(ctrl) = &ctrl {
                            let _ = ctrl.send(PlaybackControl::SetSpeed(app.speed));
                        }
                    }
                    KeyCode::Char('-') => {
                        app.speed = step_speed(app.speed, false);
                        if let Some(ctrl) = &ctrl {
                            let _ = ctrl.send(PlaybackControl::SetSpeed(app.speed));
                        }
                    }
                    KeyCode::Char('f') => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = false;
                            let _ = ctrl.send(PlaybackControl::Jump(Duration::from_secs(60)));
                        }
                    }
                    KeyCode::Char('F') => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = false;
                            let _ = ctrl.send(PlaybackControl::Jump(Duration::from_secs(300)));
                        }
                    }
                    KeyCode::Char('b') => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = false;
                            let _ = ctrl.send(PlaybackControl::JumpBack(Duration::from_secs(60)));
                        }
                    }
                    KeyCode::Char('B') => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = false;
                            let _ = ctrl.send(PlaybackControl::JumpBack(Duration::from_secs(300)));
                        }
                    }
                    // ←/→ are 1-min seek aliases (only ↑↓ are otherwise bound).
                    KeyCode::Left => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = false;
                            let _ = ctrl.send(PlaybackControl::JumpBack(Duration::from_secs(60)));
                        }
                    }
                    KeyCode::Right => {
                        if let Some(ctrl) = &ctrl {
                            app.paused = false;
                            let _ = ctrl.send(PlaybackControl::Jump(Duration::from_secs(60)));
                        }
                    }
                    KeyCode::Char('m') => {
                        // Cycle Split → MapOnly → TowerOnly → auto, starting from
                        // whatever is currently on screen.
                        let cur = app.view_override.unwrap_or(auto_mode(&app, size));
                        app.view_override = match cur {
                            ViewMode::Split => Some(ViewMode::MapOnly),
                            ViewMode::MapOnly => Some(ViewMode::TowerOnly),
                            ViewMode::TowerOnly => None,
                        };
                    }
                    _ => {}
                }
            }
        }
    };

    ratatui::restore();
    result
}

fn step_speed(cur: f64, up: bool) -> f64 {
    // Snap to the nearest step, so a non-step `--speed` (e.g. 3) moves toward
    // the intended direction instead of jumping to step index 1 (plan 2.6).
    let idx = SPEED_STEPS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (**a - cur).abs().total_cmp(&(**b - cur).abs()))
        .map(|(i, _)| i)
        .unwrap_or(1);
    let next = if up {
        (idx + 1).min(SPEED_STEPS.len() - 1)
    } else {
        idx.saturating_sub(1)
    };
    SPEED_STEPS[next]
}

/// Pick the best view mode for the given terminal size, honoring the fact that
/// the map needs a circuit outline to draw anything.
fn auto_mode(app: &App, size: ratatui::layout::Size) -> ViewMode {
    let (w, h) = (size.width, size.height);
    // The body loses 3 rows to header/ticker/footer; a map needs real vertical
    // room to be worth showing, so short terminals fall back to the tower.
    if app.track.is_none() || h < 14 {
        return ViewMode::TowerOnly;
    }
    if w >= 100 {
        ViewMode::Split
    } else {
        // Narrow but tall enough: the map is the more glanceable single view.
        ViewMode::MapOnly
    }
}

/// The effective mode after applying the user's override, downgrading if the
/// override can't actually render (e.g. MapOnly with no track yet).
fn effective_mode(app: &App, area: Rect) -> ViewMode {
    let size = ratatui::layout::Size {
        width: area.width,
        height: area.height,
    };
    match app.view_override {
        Some(ViewMode::MapOnly | ViewMode::Split) if app.track.is_none() => ViewMode::TowerOnly,
        Some(m) => m,
        None => auto_mode(app, size),
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [status_area, body_area, ticker_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    header::draw(f, status_area, app);

    // Live empty state (3.10): before any session data arrives, show a waiting
    // message + the next scheduled session instead of an empty tower.
    if !app.is_replay && app.vm.session_name.is_empty() && app.vm.rows.is_empty() {
        empty_state(f, body_area, app);
        racecontrol::ticker(f, ticker_area, app);
        footer(f, footer_area, app);
        return;
    }

    match effective_mode(app, f.area()) {
        ViewMode::Split => {
            // Map is the hero pane on the left; tower + driver panel on the right.
            let [map_area, side_area] =
                Layout::horizontal([Constraint::Min(60), Constraint::Length(46)]).areas(body_area);
            map::draw(f, map_area, app);
            let [tower_area, focus_area] =
                Layout::vertical([Constraint::Min(6), Constraint::Length(8)]).areas(side_area);
            // Narrow side pane: use the compact per-sector strip so quali's
            // live-lap column isn't clipped.
            tower::draw(f, tower_area, app, true);
            focus::draw(f, focus_area, app);
        }
        ViewMode::MapOnly => map::draw(f, body_area, app),
        ViewMode::TowerOnly => {
            // Full-width tower over the driver panel, mirroring Split's right column.
            let [tower_area, focus_area] =
                Layout::vertical([Constraint::Min(6), Constraint::Length(8)]).areas(body_area);
            tower::draw(f, tower_area, app, false);
            focus::draw(f, focus_area, app);
        }
    }

    racecontrol::ticker(f, ticker_area, app);
    footer(f, footer_area, app);

    if app.rc_open {
        racecontrol::overlay(f, app);
    }
    if app.help_open {
        help_overlay(f, app);
    }
}

/// Centered keybindings reference (toggled with `?`), reusing the race-control
/// overlay's Clear + bordered-block pattern.
fn help_overlay(f: &mut Frame, app: &App) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let mut keys: Vec<(&str, &str)> = vec![
        ("↑ / ↓", "select driver (focus panel + map spotlight)"),
        ("PgUp / PgDn", "move selection 5 rows"),
        ("Home / End", "select leader / last row"),
        ("m", "cycle view: split → map → tower → auto"),
        ("r", "race control message log"),
        ("?", "toggle this help"),
    ];
    if app.is_replay {
        keys.extend([
            ("space", "pause / resume playback"),
            ("+ / -", "playback speed"),
            ("f / F", "jump forward 1 / 5 min"),
            ("b / B", "jump back 1 / 5 min"),
            ("← / →", "seek back / forward 1 min"),
        ]);
    }
    keys.push(("q / Esc", "quit"));

    let key_w = keys
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line> = vec![Line::from("")];
    for (k, desc) in &keys {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{k:<key_w$}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(*desc, Style::default().fg(Color::Gray)),
        ]));
    }
    lines.push(Line::from(""));

    let screen = f.area();
    let width = (key_w as u16 + 52).min(screen.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(screen.height.saturating_sub(2));
    let area = Rect {
        x: (screen.width.saturating_sub(width)) / 2,
        y: (screen.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Keybindings (?/Esc close) "),
        ),
        area,
    );
}

/// Centered "waiting for a live session…" body with the next scheduled session
/// once known (3.10).
fn empty_state(f: &mut Frame, area: Rect, app: &App) {
    use ratatui::layout::Alignment;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "waiting for a live session…",
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if let Some(next) = &app.next_session {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("next: ", Style::default().fg(Color::DarkGray)),
            Span::styled(next.clone(), Style::default().fg(Color::Cyan)),
        ]));
    }
    // Vertically center-ish within the body.
    let pad = (area.height.saturating_sub(lines.len() as u16)) / 2;
    let mut padded = vec![Line::from(""); pad as usize];
    padded.extend(lines);
    f.render_widget(Paragraph::new(padded).alignment(Alignment::Center), area);
}

fn footer(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Slimmed to the everyday set (4.3); +/-, f/F/b/B and paging live in `?`.
    let keys = if app.is_replay {
        "q quit · ↑↓ driver · r messages · space pause · ←/→ seek · m view · ? help"
    } else {
        "q quit · ↑↓ driver · r messages · m view · ? help"
    };
    let mut spans = vec![Span::styled(keys, Style::default().fg(Color::DarkGray))];
    if !app.status.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("· {}", app.status),
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.state.dropped > 0 {
        spans.push(Span::styled(
            format!("  · {} corrupt msgs", app.state.dropped),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Line::from(spans), area);
}

#[cfg(test)]
mod tests {
    use super::quantize_256;

    #[test]
    fn quantize_256_known_pairs() {
        // Pure black and white map to the cube corners.
        assert_eq!(quantize_256(0, 0, 0), 16);
        assert_eq!(quantize_256(255, 255, 255), 231);
        // Pure red (level 5,0,0) → 16 + 36*5 = 196.
        assert_eq!(quantize_256(255, 0, 0), 196);
        // Pure green → 16 + 6*5 = 46; pure blue → 16 + 5 = 21.
        assert_eq!(quantize_256(0, 255, 0), 46);
        assert_eq!(quantize_256(0, 0, 255), 21);
        // A mid-gray team-ish color quantizes into the cube, not to black.
        assert!(quantize_256(120, 200, 255) > 16);
    }
}
