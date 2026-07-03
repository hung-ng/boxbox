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
use ratatui::layout::{Constraint, Layout};
use ratatui::Frame;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub const SPEED_STEPS: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0];

pub struct App {
    pub state: SessionState,
    pub vm: view::ViewModel,
    pub is_replay: bool,
    pub speed: f64,
    pub paused: bool,
    pub sim_clock: Option<Duration>,
    pub status: String,
    pub show_map: bool,
    /// Index into vm.rows of the driver shown in the focus panel.
    pub selected: usize,
    /// TLA of the selected driver, so selection follows them through position changes.
    selected_tla: Option<String>,
    pub rc_open: bool,
    pub rc_scroll: usize,
    pub track: Option<map::TrackOutline>,
    pub ended: bool,
    circuit_requested: bool,
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
        show_map: true,
        selected: 0,
        selected_tla: None,
        rc_open: false,
        rc_scroll: 0,
        track: None,
        ended: false,
        circuit_requested: false,
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

        if event::poll(Duration::from_millis(33))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break Ok(()),
                    KeyCode::Esc => {
                        if app.rc_open {
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
                    KeyCode::Char('m') => app.show_map = !app.show_map,
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

fn draw(f: &mut Frame, app: &mut App) {
    let show_side = f.area().width >= 100;
    let [status_area, body_area, ticker_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    header::draw(f, status_area, app);

    if show_side {
        let [tower_area, side_area] =
            Layout::horizontal([Constraint::Min(48), Constraint::Length(46)]).areas(body_area);
        tower::draw(f, tower_area, app);

        let map_visible = app.show_map && app.track.is_some();
        if map_visible {
            let [map_area, focus_area] =
                Layout::vertical([Constraint::Min(8), Constraint::Length(8)]).areas(side_area);
            map::draw(f, map_area, app);
            focus::draw(f, focus_area, app);
        } else {
            focus::draw(f, side_area, app);
        }
    } else {
        tower::draw(f, body_area, app);
    }

    racecontrol::ticker(f, ticker_area, app);
    footer(f, footer_area, app);

    if app.rc_open {
        racecontrol::overlay(f, app);
    }
}

fn footer(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    let keys = if app.is_replay {
        "q quit · ↑↓ driver · r messages · space pause · +/- speed · f/F +1/5min · m map"
    } else {
        "q quit · ↑↓ driver · r messages · m map"
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
