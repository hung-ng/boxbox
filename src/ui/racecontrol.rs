use super::App;
use crate::state::view::RcMessage;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

/// One-line ticker showing the latest non-noise race control message.
pub fn ticker(f: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();
    if let Some(m) = app.vm.race_control.iter().rev().find(|m| m.important) {
        let color = message_color(m);
        spans.push(Span::styled(
            format!(" {} ", m.time),
            Style::default().fg(Color::DarkGray),
        ));
        if let Some(lap) = m.lap {
            spans.push(Span::styled(
                format!("L{lap} "),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            m.text.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    let hint = "[r] messages";
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(used + hint.len() + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    f.render_widget(Line::from(spans), area);
}

/// Full message log as a centered overlay (toggled with `r`).
pub fn overlay(f: &mut Frame, app: &mut App) {
    let screen = f.area();
    let width = screen.width.saturating_sub(8).min(100);
    let height = screen.height.saturating_sub(4);
    let area = Rect {
        x: (screen.width - width) / 2,
        y: (screen.height - height) / 2,
        width,
        height,
    };

    let msgs = &app.vm.race_control;
    let visible = area.height.saturating_sub(2) as usize;
    let max_scroll = msgs.len().saturating_sub(visible);
    if app.rc_scroll > max_scroll {
        app.rc_scroll = max_scroll;
    }
    let end = msgs.len() - app.rc_scroll;
    let start = end.saturating_sub(visible);

    let text_width = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = msgs[start..end]
        .iter()
        .map(|m| {
            let color = message_color(m);
            let style = if m.important {
                Style::default().fg(color)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let mut text = String::new();
            if let Some(lap) = m.lap {
                text.push_str(&format!("L{lap} "));
            }
            text.push_str(&m.text);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", m.time), Style::default().fg(Color::DarkGray)),
                Span::styled(fit(&text, text_width.saturating_sub(6)), style),
            ]))
        })
        .collect();

    let title = if app.rc_scroll > 0 {
        format!(" Race Control (↓ {} newer · r/Esc close) ", app.rc_scroll)
    } else {
        " Race Control (↑↓ scroll · r/Esc close) ".to_string()
    };
    f.render_widget(Clear, area);
    f.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn message_color(m: &RcMessage) -> Color {
    if m.text.contains("PENALTY") || m.text.contains("DELETED") {
        return Color::Red;
    }
    match m.flag.as_deref() {
        Some("GREEN") | Some("CLEAR") => Color::Green,
        Some("YELLOW") | Some("DOUBLE YELLOW") => Color::Yellow,
        Some("RED") => Color::Red,
        Some("BLUE") => Color::Blue,
        Some("CHEQUERED") => Color::White,
        _ => match m.category.as_str() {
            "SafetyCar" => Color::Yellow,
            "Drs" => Color::Cyan,
            _ => Color::Gray,
        },
    }
}

fn fit(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if s.chars().count() > w {
        let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}
