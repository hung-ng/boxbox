use super::App;
use crate::state::view::{Row as VRow, Segment, TimeFlag, TimeVal};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use ratatui::Frame;

/// A car within this interval of the one ahead is "in the battle" (≈ DRS range).
const BATTLE_GAP_SECS: f64 = 1.0;

/// `compact` collapses the quali live-lap strip to one block per sector so it
/// fits the narrow Split side pane instead of being clipped.
pub fn draw(f: &mut Frame, area: Rect, app: &App, compact: bool) {
    let is_race = app.vm.is_race();
    let cutoff = app.vm.quali_cutoff();

    let header_cells: Vec<&str> = if is_race {
        vec!["P", "DRV", "GAP", "INT", "TIRE", "LAST"]
    } else {
        vec!["P", "DRV", "BEST", "GAP", "TIRE", "LIVE LAP"]
    };
    let header = Row::new(
        header_cells
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().fg(Color::DarkGray))),
    )
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(app.vm.rows.len() + 1);
    for (i, r) in app.vm.rows.iter().enumerate() {
        let selected = i == app.selected;
        rows.push(if is_race {
            race_row(r, selected)
        } else {
            quali_row(r, selected, cutoff, compact)
        });
        // Elimination line after the last safe position in Q1/Q2.
        if cutoff == Some(r.position) {
            rows.push(cutoff_row());
        }
    }

    let widths: Vec<Constraint> = vec![
        Constraint::Length(3),  // P (with selection marker)
        Constraint::Length(4),  // DRV
        Constraint::Length(9),  // GAP / BEST
        Constraint::Length(9),  // INT / GAP
        Constraint::Length(5),  // TIRE
        Constraint::Min(9),     // LAST / live-lap strip
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .block(Block::default().borders(Borders::TOP).title(" Timing "));
    f.render_widget(table, area);
}

fn race_row(r: &VRow, selected: bool) -> Row<'static> {
    let dim = r.retired || r.stopped;
    let base = base_style(dim);

    // Pit activity replaces the interval — that's where your eye already is.
    let interval_cell = if r.retired || r.stopped {
        Cell::from("OUT").style(Style::default().fg(Color::DarkGray))
    } else if r.in_pit {
        Cell::from("IN PIT").style(
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    } else if r.pit_out {
        Cell::from("OUT LAP").style(Style::default().fg(Color::Cyan))
    } else if r.position == 1 {
        Cell::from("—").style(Style::default().fg(Color::DarkGray))
    } else {
        let in_battle = r
            .interval_secs
            .is_some_and(|s| s <= BATTLE_GAP_SECS && r.position > 1);
        let style = if in_battle {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            base
        };
        Cell::from(fit(&r.interval, 9)).style(style)
    };

    Row::new(vec![
        pos_cell(r, selected, false),
        drv_cell(r, selected, base_style(dim)),
        Cell::from(fit(&r.gap, 9)).style(base),
        interval_cell,
        tire_cell(r, dim),
        time_cell(&r.last_lap, dim),
    ])
    .height(1)
}

fn quali_row(r: &VRow, selected: bool, cutoff: Option<u32>, compact: bool) -> Row<'static> {
    let dim = r.retired || r.stopped || r.knocked_out;
    let base = base_style(dim);
    let in_drop_zone = cutoff.is_some_and(|c| r.position > c) && !dim;

    let gap_cell = if r.knocked_out {
        Cell::from("ELIM").style(Style::default().fg(Color::DarkGray))
    } else if r.in_pit {
        Cell::from("IN PIT").style(Style::default().fg(Color::DarkGray))
    } else if r.pit_out {
        Cell::from("OUT LAP").style(Style::default().fg(Color::Cyan))
    } else {
        Cell::from(fit(&r.gap, 9)).style(base)
    };

    Row::new(vec![
        pos_cell(r, selected, in_drop_zone),
        drv_cell(r, selected, base),
        Cell::from(fit(&r.best_lap, 9)).style(if dim {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }),
        gap_cell,
        tire_cell(r, dim),
        Cell::from(if compact {
            compact_segments_line(r)
        } else {
            segments_line(r)
        }),
    ])
    .height(1)
}

fn cutoff_row() -> Row<'static> {
    let dash = |w: usize| {
        Cell::from("╌".repeat(w)).style(Style::default().fg(Color::Red))
    };
    Row::new(vec![dash(3), dash(4), dash(9), dash(9), dash(5), dash(9)]).height(1)
}

fn pos_cell(r: &VRow, selected: bool, drop_zone: bool) -> Cell<'static> {
    let marker = if selected { "▸" } else { " " };
    let style = if drop_zone {
        Style::default().fg(Color::Red)
    } else if r.retired || r.stopped || r.knocked_out {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    Cell::from(format!("{marker}{:>2}", r.position)).style(style)
}

fn drv_cell(r: &VRow, selected: bool, base: Style) -> Cell<'static> {
    let (tr, tg, tb) = r.team_color;
    let mut style = base.add_modifier(Modifier::BOLD);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Cell::from(Line::from(vec![
        Span::styled("▍", Style::default().fg(Color::Rgb(tr, tg, tb))),
        Span::styled(r.tla.clone(), style),
    ]))
}

fn tire_cell(r: &VRow, dim: bool) -> Cell<'static> {
    match r.stint() {
        Some(st) => {
            let color = if dim { Color::DarkGray } else { compound_color(st.compound) };
            Cell::from(Line::from(vec![
                Span::styled(
                    st.compound.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>3}", st.age_laps),
                    Style::default().fg(Color::Gray),
                ),
            ]))
        }
        None => Cell::from(""),
    }
}

fn base_style(dim: bool) -> Style {
    if dim {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    }
}

pub fn compound_color(c: char) -> Color {
    match c {
        'S' => Color::Red,
        'M' => Color::Yellow,
        'H' => Color::White,
        'I' => Color::Green,
        'W' => Color::Blue,
        _ => Color::DarkGray,
    }
}

pub fn time_style(t: &TimeVal, dim: bool) -> Style {
    if dim {
        return Style::default().fg(Color::DarkGray);
    }
    match t.flag {
        Some(TimeFlag::OverallBest) => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        Some(TimeFlag::PersonalBest) => Style::default().fg(Color::Green),
        None => Style::default().fg(Color::White),
    }
}

fn time_cell(t: &TimeVal, dim: bool) -> Cell<'static> {
    Cell::from(fit(&t.text, 9)).style(time_style(t, dim))
}

pub fn segments_line(r: &VRow) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for (i, sector) in r.segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("|", Style::default().fg(Color::DarkGray)));
        }
        for seg in sector {
            let (ch, color) = match seg {
                Segment::OverallBest => ("▪", Color::Magenta),
                Segment::PersonalBest => ("▪", Color::Green),
                Segment::Set => ("▪", Color::Yellow),
                Segment::Pit => ("▪", Color::Cyan),
                Segment::NotSet => ("·", Color::DarkGray),
            };
            spans.push(Span::styled(ch, Style::default().fg(color)));
        }
    }
    Line::from(spans)
}

/// One block per sector, colored by the sector's strongest mini-sector status.
/// Fixed 5 cells wide (`▪|▪|▪`) so it never clips the narrow Split pane.
pub fn compact_segments_line(r: &VRow) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for (i, sector) in r.segments.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("|", Style::default().fg(Color::DarkGray)));
        }
        // Rank statuses so the block reflects the best segment reached so far.
        let color = sector
            .iter()
            .map(|seg| match seg {
                Segment::OverallBest => (4, Color::Magenta),
                Segment::PersonalBest => (3, Color::Green),
                Segment::Set => (2, Color::Yellow),
                Segment::Pit => (1, Color::Cyan),
                Segment::NotSet => (0, Color::DarkGray),
            })
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, c)| c)
            .unwrap_or(Color::DarkGray);
        let ch = if color == Color::DarkGray { "·" } else { "▪" };
        spans.push(Span::styled(ch, Style::default().fg(color)));
    }
    Line::from(spans)
}

fn fit(s: &str, w: usize) -> String {
    if s.chars().count() > w {
        s.chars().take(w).collect()
    } else {
        s.to_string()
    }
}
