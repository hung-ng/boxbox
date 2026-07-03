use super::App;
use crate::state::view::TrackFlag;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

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

    // Progress: lap counter (race) or Q-segment + remaining clock.
    spans.push(sep.clone());
    let progress = if let Some((cur, total)) = vm.lap {
        format!("Lap {cur}/{total}")
    } else {
        let seg = match (vm.is_qualifying(), vm.session_part) {
            (true, Some(p)) => format!("Q{p} "),
            _ => String::new(),
        };
        format!("{seg}⏱ {}", vm.clock_remaining.as_deref().unwrap_or("--:--"))
    };
    spans.push(Span::styled(
        progress,
        Style::default().add_modifier(Modifier::BOLD),
    ));

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
        spans.push(Span::styled(
            format!("{}°/{}° {}m/s {}%", w.air, w.track, w.wind, w.humidity),
            Style::default().fg(Color::Gray),
        ));
        if w.raining {
            spans.push(Span::styled(
                " RAIN",
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            ));
        }
    }

    // Replay transport.
    if app.is_replay {
        spans.push(sep.clone());
        let clock = app
            .sim_clock
            .map(|d| {
                let s = d.as_secs();
                format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
            })
            .unwrap_or_default();
        spans.push(Span::styled(
            format!(
                "{} {}× {clock}",
                if app.paused { "⏸" } else { "⏵" },
                trim_speed(app.speed)
            ),
            Style::default().fg(Color::Cyan),
        ));
    }

    // Meeting/session name, dim, at the end.
    if !vm.meeting.is_empty() {
        spans.push(sep);
        spans.push(Span::styled(
            format!("{} · {}", vm.meeting, vm.session_name),
            Style::default().fg(Color::DarkGray),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn trim_speed(s: f64) -> String {
    if (s - s.round()).abs() < 0.01 {
        format!("{}", s.round() as i64)
    } else {
        format!("{s:.1}")
    }
}
