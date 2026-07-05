use super::{App, Dir};
use crate::state::view::{Row as VRow, Segment, TimeFlag, TimeVal};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

/// A car within this interval of the one ahead is "in the battle" (≈ DRS range).
const BATTLE_GAP_SECS: f64 = 1.0;

/// Per-row context computed once in `draw` and threaded into each row builder,
/// so row functions don't each re-derive whole-field facts.
struct Ctx<'a> {
    app: &'a App,
    selected: bool,
    /// This row (and the one behind it) is within DRS range (3.2).
    battle: bool,
    /// Quali cutoff position (Q1/Q2), and the time to beat (3.5).
    cutoff: Option<u32>,
    cutoff_time: Option<f64>,
    compact: bool,
    /// Practice LAPS column present (3.8).
    show_laps: bool,
}

/// `compact` collapses the quali live-lap strip to one block per sector so it
/// fits the narrow Split side pane instead of being clipped.
pub fn draw(f: &mut Frame, area: Rect, app: &mut App, compact: bool) {
    let is_race = app.vm.is_race();
    let is_practice = app.vm.is_practice();
    let cutoff = app.vm.quali_cutoff();
    let cutoff_time = app.vm.cutoff_time();
    // Practice full-width tower gains a LAPS column; compact has no room for it.
    let show_laps = is_practice && !compact;

    // Battle set (3.2): a row is "in battle" if its own interval ≤ 1.0s or the
    // row behind it is, so both cars of a fight get highlighted.
    let close: Vec<bool> = app
        .vm
        .rows
        .iter()
        .map(|r| r.position > 1 && r.interval_secs.is_some_and(|s| s <= BATTLE_GAP_SECS))
        .collect();

    let header_cells: Vec<&str> = if is_race {
        vec!["P", "DRV", "GAP", "INT", "TIRE", "LAST"]
    } else if show_laps {
        vec!["P", "DRV", "BEST", "GAP", "TIRE", "LAPS", "LIVE LAP"]
    } else {
        vec!["P", "DRV", "BEST", "GAP", "TIRE", "LIVE LAP"]
    };
    let header = Row::new(
        header_cells
            .iter()
            .map(|h| Cell::from(*h).style(Style::default().fg(Color::DarkGray))),
    )
    .height(1);

    // Track where the selected driver's row lands so scroll-follow can point at
    // it (a cutoff divider row shifts everything below it down by one).
    let mut selected_render_idx: Option<usize> = None;
    let mut rendered = 0usize; // rows emitted so far (data + dividers), header excluded
    let mut rows: Vec<Row> = Vec::with_capacity(app.vm.rows.len() + 1);
    for (i, r) in app.vm.rows.iter().enumerate() {
        let selected = i == app.selected;
        if selected {
            selected_render_idx = Some(rendered);
        }
        let ctx = Ctx {
            app,
            selected,
            battle: close[i] || close.get(i + 1).copied().unwrap_or(false),
            cutoff,
            cutoff_time,
            compact,
            show_laps,
        };
        rows.push(if is_race {
            race_row(r, &ctx)
        } else {
            quali_row(r, &ctx)
        });
        rendered += 1;
        // Elimination line after the last safe position in Q1/Q2.
        if cutoff == Some(r.position) {
            rows.push(cutoff_row(show_laps));
            rendered += 1;
        }
    }

    let mut widths: Vec<Constraint> = vec![
        Constraint::Length(3), // P (with selection marker)
        Constraint::Length(4), // DRV
        Constraint::Length(9), // GAP / BEST
        Constraint::Length(9), // INT / GAP
        Constraint::Length(5), // TIRE
    ];
    if show_laps {
        widths.push(Constraint::Length(4)); // LAPS
    }
    widths.push(Constraint::Min(9)); // LAST / live-lap strip

    // Compact (Split side pane) tightens inter-column spacing so the last
    // column keeps enough cells for a full lap time (see plan 2.1).
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(if compact { 1 } else { 2 })
        .block(Block::default().borders(Borders::TOP).title(" Timing "))
        .row_highlight_style(Style::default());
    // Scroll-follow (3.3): keep the selected row visible in short panes.
    app.table_state.select(selected_render_idx);
    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn race_row(r: &VRow, ctx: &Ctx) -> Row<'static> {
    let dim = r.retired || r.stopped;
    let base = base_style(dim);

    // Pit activity replaces the interval — that's where your eye already is.
    let interval_cell = if r.retired || r.stopped {
        Cell::from("OUT").style(Style::default().fg(Color::DarkGray))
    } else if r.in_pit {
        // Show the ticking pit-lane time if the feed has it, else IN PIT.
        let label = match &r.pit_time {
            Some(t) if !t.is_empty() => format!("PIT {t}"),
            _ => "IN PIT".to_string(),
        };
        Cell::from(label).style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
    } else if let Some(t) = ctx.app.recent_pit(&r.tla) {
        // Just left the pits: flash the final pit-lane time briefly (5.2).
        Cell::from(fit(&format!("{t}s"), 9)).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else if r.pit_out {
        Cell::from("OUT LAP").style(Style::default().fg(Color::Cyan))
    } else if r.position == 1 {
        Cell::from("—").style(Style::default().fg(Color::DarkGray))
    } else {
        // Both cars in a fight get the yellow INT (3.2), not just the chaser.
        let style = if ctx.battle {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            base
        };
        Cell::from(fit(&r.interval, 9)).style(style)
    };

    Row::new(vec![
        pos_cell(r, ctx, false),
        drv_cell(r, ctx.selected, base_style(dim)),
        Cell::from(fit(&r.gap, 9)).style(base),
        interval_cell,
        tire_cell(r, dim),
        time_cell(&r.last_lap, dim),
    ])
    .height(1)
}

fn quali_row(r: &VRow, ctx: &Ctx) -> Row<'static> {
    let dim = r.retired || r.stopped || r.knocked_out;
    let base = base_style(dim);
    let in_drop_zone = ctx.cutoff.is_some_and(|c| r.position > c) && !dim;

    // Pit status moves out of GAP into the LIVE LAP slot (3.6) so the banked
    // time's gap stays visible while a car sits in the pits.
    let pit_status: Option<Cell> = if r.knocked_out {
        None // ELIM shown in GAP; no live lap for a knocked-out driver
    } else if r.in_pit {
        Some(Cell::from("IN PIT").style(Style::default().fg(Color::DarkGray)))
    } else if r.pit_out {
        Some(Cell::from("OUT LAP").style(Style::default().fg(Color::Cyan)))
    } else {
        None
    };

    // GAP: drop-zone drivers show the red delta to the cutoff time (3.5);
    // everyone else shows the normal gap. Knocked-out shows ELIM.
    let gap_cell = if r.knocked_out {
        Cell::from("ELIM").style(Style::default().fg(Color::DarkGray))
    } else if let (true, Some(cut), Some(mine)) = (in_drop_zone, ctx.cutoff_time, r.best_lap_secs) {
        let delta = mine - cut;
        if delta > 0.0 {
            Cell::from(fit(&format!("+{delta:.3}"), 9))
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        } else {
            Cell::from(fit(&r.gap, 9)).style(base)
        }
    } else {
        Cell::from(fit(&r.gap, 9)).style(base)
    };

    // LIVE LAP: pit status if pitted, else a just-completed lap flash (3.4),
    // else the live mini-sector strip.
    let live_cell = if let Some(cell) = pit_status {
        cell
    } else if let Some(t) = ctx.app.recent_lap(&r.tla) {
        time_cell(t, dim)
    } else if ctx.compact {
        Cell::from(compact_segments_line(r))
    } else {
        Cell::from(segments_line(r))
    };

    let mut cells = vec![
        pos_cell(r, ctx, in_drop_zone),
        drv_cell(r, ctx.selected, base),
        Cell::from(fit(&r.best_lap, 9)).style(if dim {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }),
        gap_cell,
        tire_cell(r, dim),
    ];
    if ctx.show_laps {
        cells.push(laps_cell(r, dim));
    }
    cells.push(live_cell);
    Row::new(cells).height(1)
}

fn cutoff_row(show_laps: bool) -> Row<'static> {
    let dash = |w: usize| Cell::from("╌".repeat(w)).style(Style::default().fg(Color::Red));
    let mut cells = vec![dash(3), dash(4), dash(9), dash(9), dash(5)];
    if show_laps {
        cells.push(dash(4));
    }
    cells.push(dash(9));
    Row::new(cells).height(1)
}

fn laps_cell(r: &VRow, dim: bool) -> Cell<'static> {
    let text = r.laps.map(|n| n.to_string()).unwrap_or_default();
    let style = if dim {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Gray)
    };
    Cell::from(format!("{text:>3}")).style(style)
}

/// Position cell: the ▸ selection marker, or a green ▲ / red ▼ position-change
/// arrow (3.1) when the driver just gained or lost a place. Selection wins.
fn pos_cell(r: &VRow, ctx: &Ctx, drop_zone: bool) -> Cell<'static> {
    let arrow = ctx.app.pos_arrow(&r.tla);
    let (marker, marker_style) = if ctx.selected {
        ("▸", Style::default())
    } else {
        match arrow {
            Some(Dir::Up) => ("▲", Style::default().fg(Color::Green)),
            Some(Dir::Down) => ("▼", Style::default().fg(Color::Red)),
            None => (" ", Style::default()),
        }
    };
    let num_style = if drop_zone {
        Style::default().fg(Color::Red)
    } else if r.retired || r.stopped || r.knocked_out {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    Cell::from(Line::from(vec![
        Span::styled(marker, marker_style),
        Span::styled(format!("{:>2}", r.position), num_style),
    ]))
}

fn drv_cell(r: &VRow, selected: bool, base: Style) -> Cell<'static> {
    let (tr, tg, tb) = r.team_color;
    let mut style = base.add_modifier(Modifier::BOLD);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Cell::from(Line::from(vec![
        Span::styled("▍", Style::default().fg(super::color(tr, tg, tb))),
        Span::styled(r.tla.clone(), style),
    ]))
}

fn tire_cell(r: &VRow, dim: bool) -> Cell<'static> {
    match r.stint() {
        Some(st) => {
            let color = if dim {
                Color::DarkGray
            } else {
                compound_color(st.compound)
            };
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
        let ch = if color == Color::DarkGray {
            "·"
        } else {
            "▪"
        };
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
