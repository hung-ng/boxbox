use super::{tower, App};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Detail panel for the ↑↓-selected driver: full lap breakdown and stint history.
pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP).title(" Driver ");
    let Some(r) = app.vm.rows.get(app.selected) else {
        f.render_widget(block, area);
        return;
    };
    let (tr, tg, tb) = r.team_color;
    let label = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("▍", Style::default().fg(Color::Rgb(tr, tg, tb))),
        Span::styled(
            format!("{:>2} ", r.position),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if r.full_name.is_empty() {
                r.tla.clone()
            } else {
                r.full_name.clone()
            },
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  #{}", r.number), label),
    ]));

    lines.push(Line::from(vec![
        Span::styled("LAST ", label),
        Span::styled(pad(&r.last_lap.text, 9), tower::time_style(&r.last_lap, false)),
        Span::styled("  BEST ", label),
        Span::styled(pad(&r.best_lap, 9), Style::default().add_modifier(Modifier::BOLD)),
    ]));

    let mut sector_spans = Vec::new();
    for (i, s) in r.sectors.iter().enumerate() {
        sector_spans.push(Span::styled(format!("S{} ", i + 1), label));
        sector_spans.push(Span::styled(
            pad(if s.text.is_empty() { "—" } else { &s.text }, 7),
            tower::time_style(s, false),
        ));
    }
    lines.push(Line::from(sector_spans));

    lines.push(tower::segments_line(r));

    // Stint history: "M 11 → S 14 · 1 stop"
    let mut stint_spans = vec![Span::styled("TIRES ", label)];
    if r.stints.is_empty() {
        stint_spans.push(Span::styled("—", label));
    }
    for (i, st) in r.stints.iter().enumerate() {
        if i > 0 {
            stint_spans.push(Span::styled(" → ", label));
        }
        stint_spans.push(Span::styled(
            st.compound.to_string(),
            Style::default()
                .fg(tower::compound_color(st.compound))
                .add_modifier(Modifier::BOLD),
        ));
        stint_spans.push(Span::raw(format!(" {}", st.age_laps)));
    }
    if app.vm.is_race() {
        stint_spans.push(Span::styled(
            format!(
                "  · {} stop{}",
                r.pit_count,
                if r.pit_count == 1 { "" } else { "s" }
            ),
            label,
        ));
    }
    lines.push(Line::from(stint_spans));

    let interval = if r.position == 1 || r.interval.is_empty() {
        "—"
    } else {
        &r.interval
    };
    lines.push(Line::from(vec![
        Span::styled("GAP ", label),
        Span::raw(pad(if r.gap.is_empty() { "—" } else { &r.gap }, 10)),
        Span::styled(" INT ", label),
        Span::raw(pad(interval, 10)),
    ]));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}
