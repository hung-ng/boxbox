use super::{App, tower};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

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
        Span::styled("▍", Style::default().fg(super::color(tr, tg, tb))),
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
        Span::styled(
            pad(&r.last_lap.text, 9),
            tower::time_style(&r.last_lap, false),
        ),
        Span::styled("  BEST ", label),
        Span::styled(
            pad(&r.best_lap, 9),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));

    // In quali the per-segment bests (5.1) are more useful than the current-lap
    // sector split, and the panel has a fixed height — so they take that slot:
    // "SEG  Q1 1:30.1 · Q2 1:29.8 · Q3 —". Elsewhere show the sector times.
    if app.vm.is_qualifying() && !r.segment_bests.is_empty() {
        let mut spans = vec![Span::styled("SEG ", label)];
        for (i, t) in r.segment_bests.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", label));
            }
            spans.push(Span::styled(format!("Q{} ", i + 1), label));
            spans.push(Span::raw(if t.is_empty() { "—" } else { t }));
        }
        lines.push(Line::from(spans));
    } else {
        let mut sector_spans = Vec::new();
        for (i, s) in r.sectors.iter().enumerate() {
            sector_spans.push(Span::styled(format!("S{} ", i + 1), label));
            sector_spans.push(Span::styled(
                pad(if s.text.is_empty() { "—" } else { &s.text }, 7),
                tower::time_style(s, false),
            ));
        }
        lines.push(Line::from(sector_spans));
    }

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
        // Last pit-lane time when the feed has reported one (5.2). Duration is
        // pit-lane time (~22s), not the stationary stop — label it as such.
        if let Some(t) = &r.pit_time {
            if !t.is_empty() {
                stint_spans.push(Span::styled(format!("  · pit lane {t}s"), label));
            }
        }
    } else if app.vm.is_practice() {
        // Practice: lap count matters more than stops (3.8).
        if let Some(n) = r.laps {
            stint_spans.push(Span::styled(format!("  · LAPS {n}"), label));
        }
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

    // Speed trap (5.3): "SPD  I1 312  I2 289  FL 301  ST 318" — dashes when empty.
    let spd_labels = ["I1", "I2", "FL", "ST"];
    let mut spd_spans = vec![Span::styled("SPD ", label)];
    for (name, t) in spd_labels.iter().zip(r.speeds.iter()) {
        spd_spans.push(Span::styled(format!(" {name} "), label));
        let text = if t.text.is_empty() { "—" } else { &t.text };
        spd_spans.push(Span::styled(text.to_string(), tower::time_style(t, false)));
    }
    lines.push(Line::from(spd_spans));

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}
