mod focus;
mod header;
mod map;
mod racecontrol;
mod tower;

use crate::message::{PlaybackControl, SourceEvent};
use crate::source::archive::Archive;
use crate::state::{view, SessionState};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

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
    circuit_requested: bool,
    /// TLA holding the session-best lap, and when it was last claimed — drives
    /// the ~1s magenta pulse on the map when a new overall best appears.
    last_fastest_tla: Option<String>,
    fastest_since: Option<Instant>,
}

/// How long a new fastest-lap pulse flashes on the map.
const PULSE: Duration = Duration::from_millis(1000);

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
}

pub fn run(
    rx: Receiver<SourceEvent>,
    tx: Sender<SourceEvent>,
    ctrl: Option<UnboundedSender<PlaybackControl>>,
    rt: tokio::runtime::Handle,
    initial_speed: f64,
) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App {
        state: SessionState::default(),
        vm: view::ViewModel::default(),
        is_replay: ctrl.is_some(),
        speed: initial_speed,
        paused: false,
        sim_clock: None,
        status: String::new(),
        view_override: None,
        selected: 0,
        selected_tla: None,
        rc_open: false,
        rc_scroll: 0,
        help_open: false,
        track: None,
        ended: false,
        circuit_requested: false,
        last_fastest_tla: None,
        fastest_since: None,
    };

    let result = loop {
        // Drain pending feed messages (bounded per frame so rendering never starves).
        let mut drained = 0;
        while drained < 20_000 {
            match rx.try_recv() {
                Ok(SourceEvent::Message(msg)) => app.state.apply(msg),
                Ok(SourceEvent::Info(s)) => app.status = s,
                Ok(SourceEvent::Clock(t)) => app.sim_clock = Some(t),
                Ok(SourceEvent::Circuit(v)) => {
                    app.track = map::TrackOutline::parse(&v);
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
            // Keep the focus on the same driver as positions shuffle.
            if let Some(tla) = &app.selected_tla {
                if let Some(i) = app.vm.rows.iter().position(|r| &r.tla == tla) {
                    app.selected = i;
                }
            }
            app.selected = app.selected.min(app.vm.rows.len().saturating_sub(1));
        }

        // Kick off the circuit outline fetch once we know which track this is.
        if !app.circuit_requested {
            if let (Some(key), Some(year)) = (app.vm.circuit_key, app.vm.year) {
                app.circuit_requested = true;
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

        terminal.draw(|f| draw(f, &mut app))?;
        // Size used to resolve `m`'s cycle relative to what's currently shown.
        let size = terminal.size().unwrap_or_default();

        if event::poll(Duration::from_millis(33))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break Ok(()),
                    KeyCode::Esc => {
                        if app.help_open {
                            app.help_open = false;
                        } else if app.rc_open {
                            app.rc_open = false;
                        } else {
                            break Ok(());
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break Ok(())
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
                            app.selected_tla =
                                app.vm.rows.get(app.selected).map(|r| r.tla.clone());
                        }
                    }
                    KeyCode::Down => {
                        if app.rc_open {
                            app.rc_scroll = app.rc_scroll.saturating_sub(1);
                        } else if app.selected + 1 < app.vm.rows.len() {
                            app.selected += 1;
                            app.selected_tla =
                                app.vm.rows.get(app.selected).map(|r| r.tla.clone());
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
    let idx = SPEED_STEPS
        .iter()
        .position(|s| (s - cur).abs() < 0.01)
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

    match effective_mode(app, f.area()) {
        ViewMode::Split => {
            // Map is the hero pane on the left; tower + driver panel on the right.
            let [map_area, side_area] =
                Layout::horizontal([Constraint::Min(60), Constraint::Length(46)])
                    .areas(body_area);
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
        ("m", "cycle view: split → map → tower → auto"),
        ("r", "race control message log"),
        ("?", "toggle this help"),
    ];
    if app.is_replay {
        keys.extend([
            ("space", "pause / resume playback"),
            ("+ / -", "playback speed"),
            ("f / F", "jump forward 1 / 5 min"),
        ]);
    }
    keys.push(("q / Esc", "quit"));

    let key_w = keys.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    let mut lines: Vec<Line> = vec![Line::from("")];
    for (k, desc) in &keys {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{k:<key_w$}"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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

fn footer(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let keys = if app.is_replay {
        "q quit · ↑↓ driver · r messages · space pause · +/- speed · f/F +1/5min · m view · ? help"
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
    f.render_widget(Line::from(spans), area);
}
