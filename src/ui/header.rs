use super::App;
use crate::state::view::TrackFlag;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// One-line status bar: flag chip · lap/segment/clock · fastest lap · weather · transport.
pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let vm = &app.vm;
    let sep = Span::styled(" │ ", Style::default().fg(Color::DarkGray));
    let mut spans: Vec<Span> = Vec::new();

    // Track flag chip — the single most important signal, always first.
    let (label, bg) = match vm.track_flag {
        Some(TrackFlag::Green) => (" GREEN ", Color::Green),
        Some(TrackFlag::Yellow) => (" YELLOW ", Color::Yellow),
        Some(TrackFlag::SafetyCar) => (" SAFETY CAR ", Color::Yellow),
        Some(TrackFlag::Vsc) => (" VSC ", Color::Yellow),
        Some(TrackFlag::Red) => (" RED FLAG ", Color::Red),
        _ => ("  —  ", Color::DarkGray),
    };
    spans.push(Span::styled(
        label,
        Style::default()
            .bg(bg)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    ));

    // Session-type accent so race / quali / practice is readable at a glance.
    // The chip is derived from the session name where possible (3.9): FP1/FP2/
    // FP3, SPRINT, SQ.
    let (kind, kind_color) = session_chip(vm);
    if !kind.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            kind,
            Style::default().fg(kind_color).add_modifier(Modifier::BOLD),
        ));
    }

    // Progress: lap counter (race) or Q-segment + remaining clock.
    spans.push(sep.clone());
    if let Some((cur, total)) = vm.lap {
        spans.push(Span::styled(
            format!("Lap {cur}/{total}"),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    } else {
        let seg = match vm.part_label() {
            Some(l) => format!("{l} "),
            None => String::new(),
        };
        // Between segments (3.7): the real transition is Started → Finished →
        // Inactive → Started, so the gap between Q1/Q2/Q3 sits in Finished or
        // Inactive (Aborted = red-flag stop). Any of these with a part set shows
        // "Q{p} ended" rather than a stale/frozen clock. Verified against the
        // 2024 Austin quali SessionStatus stream.
        let ended = matches!(
            vm.session_status.as_str(),
            "Finished" | "Aborted" | "Inactive"
        ) && vm.part_label().is_some();
        if ended {
            spans.push(Span::styled(
                format!("{seg}ended"),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        } else {
            // Prefer the locally-ticking clock (3.11); fall back to the feed value.
            let clock = app
                .ticking_clock()
                .or_else(|| vm.clock_remaining.clone())
                .unwrap_or_else(|| "--:--".into());
            spans.push(Span::styled(
                format!("{seg}⏱ "),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(clock, countdown_style(app)));
        }
    }

    // Session-best lap chip.
    if let Some((time, tla)) = &vm.fastest {
        spans.push(sep.clone());
        spans.push(Span::styled(
            format!("FL {time} {tla}"),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Compact weather.
    if let Some(w) = &vm.weather {
        spans.push(sep.clone());
        // Self-describing: A(ir) / T(rack) temps, wind, humidity (7.2).
        spans.push(Span::styled(
            format!(
                "A {}° · T {}° · {}m/s · {}%",
                w.air, w.track, w.wind, w.humidity
            ),
            Style::default().fg(Color::Gray),
        ));
        if w.raining {
            spans.push(Span::styled(
                " RAIN",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    // Replay transport: glyph, speed, and elapsed / total timeline (1.5).
    // Ended shows ⏹, paused ⏸, otherwise ⏵ (2.2).
    if app.is_replay {
        spans.push(sep.clone());
        let glyph = if app.ended {
            "⏹"
        } else if app.paused {
            "⏸"
        } else {
            "⏵"
        };
        let elapsed = app.sim_clock.map(fmt_hms).unwrap_or_default();
        let timeline = match app.total {
            Some(total) => format!("{elapsed} / {}", fmt_hms(total)),
            None => elapsed,
        };
        spans.push(Span::styled(
            format!("{glyph} {}× {timeline}", trim_speed(app.speed)),
            Style::default().fg(Color::Cyan),
        ));
    }

    // Meeting name + year + session, dim, at the end (2.1). Year omitted cleanly
    // when unknown: "Bahrain Grand Prix 2024 · Race".
    if !vm.meeting.is_empty() {
        spans.push(sep);
        let meeting = match vm.year {
            Some(y) => format!("{} {y}", vm.meeting),
            None => vm.meeting.clone(),
        };
        spans.push(Span::styled(
            format!("{meeting} · {}", vm.session_name),
            Style::default().fg(Color::DarkGray),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Session-type chip label + color, derived from the session name where it
/// carries more detail than the coarse type (FP1/FP2/FP3, SPRINT, SQ). Empty
/// when the session type hasn't arrived yet.
fn session_chip(vm: &crate::state::view::ViewModel) -> (&'static str, Color) {
    let name = vm.session_name.as_str();
    let race = super::color(120, 200, 255);
    let quali = super::color(255, 170, 90);
    let practice = super::color(150, 210, 150);
    if name.contains("Sprint Qualifying") || name.contains("Sprint Shootout") {
        ("SQ", quali)
    } else if name.contains("Sprint") {
        ("SPRINT", race)
    } else if name.contains("Practice 1") {
        ("FP1", practice)
    } else if name.contains("Practice 2") {
        ("FP2", practice)
    } else if name.contains("Practice 3") {
        ("FP3", practice)
    } else if vm.is_race() {
        ("RACE", race)
    } else if vm.is_qualifying() {
        ("QUALI", quali)
    } else if vm.is_practice() {
        ("PRACTICE", practice)
    } else {
        ("", Color::DarkGray)
    }
}

/// Countdown urgency (3.7): yellow under 2:00, red under 0:30, while running.
fn countdown_style(app: &App) -> Style {
    let base = Style::default().add_modifier(Modifier::BOLD);
    if app.vm.session_status != "Started" {
        return base;
    }
    match app.ticking_clock().and_then(|c| parse_hms(&c)) {
        Some(s) if s < 30 => base.fg(Color::Red),
        Some(s) if s < 120 => base.fg(Color::Yellow),
        _ => base,
    }
}

/// A `Duration` as `H:MM:SS` (transport timeline, 1.5).
fn fmt_hms(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// `H:MM:SS` → total seconds.
fn parse_hms(s: &str) -> Option<u64> {
    let mut p = s.split(':');
    let h: u64 = p.next()?.parse().ok()?;
    let m: u64 = p.next()?.parse().ok()?;
    let sec: u64 = p.next()?.parse().ok()?;
    Some(h * 3600 + m * 60 + sec)
}

fn trim_speed(s: f64) -> String {
    if (s - s.round()).abs() < 0.01 {
        format!("{}", s.round() as i64)
    } else {
        format!("{s:.1}")
    }
}
