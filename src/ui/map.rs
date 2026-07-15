use super::App;
use crate::state::view::TrackFlag;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span as RSpan;
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Block, Borders};
use serde_json::Value;

const MAX_INPUT_POINTS: usize = 100_000;
const MAX_DENSIFIED_POINTS: usize = 200_000;

pub struct TrackOutline {
    pub points: Vec<(f64, f64)>,
    pub corners: Vec<(f64, f64, i64)>,
    /// Marshal sector `n` mapped to the range of outline-point indices it covers
    /// (3.1). Sector n runs [its projected index, the next sector's index),
    /// wrapping for the last. Used to paint active yellow sectors (3.3).
    pub sector_ranges: Vec<(i64, std::ops::Range<usize>)>,
    bounds: (f64, f64, f64, f64), // min_x, max_x, min_y, max_y
    rotation: f64,                // radians
    center: (f64, f64),
}

impl TrackOutline {
    /// Parse the MultiViewer circuit response: `{"x": [...], "y": [...],
    /// "corners": [{"number": n, "trackPosition": {"x":..,"y":..}}], "rotation": deg}`.
    pub fn parse(v: &Value) -> Option<Self> {
        let xs = v.get("x")?.as_array()?;
        let ys = v.get("y")?.as_array()?;
        if xs.len() < 2 || xs.len() != ys.len() || xs.len() > MAX_INPUT_POINTS {
            return None;
        }
        let raw: Vec<(f64, f64)> = xs
            .iter()
            .zip(ys)
            .map(|(x, y)| {
                let pair = (x.as_f64()?, y.as_f64()?);
                (pair.0.is_finite() && pair.1.is_finite()).then_some(pair)
            })
            .collect::<Option<_>>()?;

        let rotation = match v.get("rotation") {
            Some(rotation) => rotation.as_f64()?,
            None => 0.0,
        };
        if !rotation.is_finite() {
            return None;
        }
        let rotation = rotation.to_radians();
        let (min_x, max_x) = min_max(raw.iter().map(|p| p.0));
        let (min_y, max_y) = min_max(raw.iter().map(|p| p.1));
        if min_x >= max_x || min_y >= max_y {
            return None;
        }
        let center = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);

        let mut outline = TrackOutline {
            points: Vec::new(),
            corners: Vec::new(),
            sector_ranges: Vec::new(),
            bounds: (0.0, 0.0, 0.0, 0.0),
            rotation,
            center,
        };
        outline.points = densify(raw.iter().map(|&p| outline.transform(p)).collect())?;
        outline.corners = parse_markers(v, "corners", &outline)?;

        // Marshal sectors (3.1): project each sector's marker to the nearest
        // outline-point index, then give sector n the span up to sector n+1's
        // index (wrapping for the last). Same JSON shape as `corners`.
        let mut sectors: Vec<(i64, usize)> = parse_markers(v, "marshalSectors", &outline)?
            .into_iter()
            .map(|(x, y, number)| (number, outline.nearest_index((x, y))))
            .collect();
        // Order by position along the outline so consecutive sectors form
        // contiguous, non-overlapping ranges regardless of source ordering.
        sectors.sort_by_key(|(_, idx)| *idx);
        let n_pts = outline.points.len();
        let mut ranges: Vec<(i64, std::ops::Range<usize>)> = sectors
            .iter()
            .enumerate()
            .map(|(i, &(num, start))| {
                let end = sectors.get(i + 1).map(|&(_, s)| s).unwrap_or(n_pts);
                (num, start..end.max(start))
            })
            .collect();
        // The outline is a loop, so the head [0, first sector's start) sits
        // between the last sector's marker and the first's — it belongs to the
        // last sector. Give it that extra wrap slice (a sector may hold two).
        if let (Some(&(_, first_start)), Some((last_num, _))) = (sectors.first(), sectors.last())
            && first_start > 0
        {
            ranges.push((*last_num, 0..first_start));
        }
        outline.sector_ranges = ranges;

        let (min_x, max_x) = min_max(outline.points.iter().map(|p| p.0));
        let (min_y, max_y) = min_max(outline.points.iter().map(|p| p.1));
        let pad_x = (max_x - min_x) * 0.05;
        let pad_y = (max_y - min_y) * 0.05;
        outline.bounds = (min_x - pad_x, max_x + pad_x, min_y - pad_y, max_y + pad_y);
        Some(outline)
    }

    /// Index of the densified outline point nearest a (already transformed)
    /// position — used to anchor marshal sectors to the drawn outline (3.1).
    fn nearest_index(&self, (x, y): (f64, f64)) -> usize {
        self.points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a.0 - x).powi(2) + (a.1 - y).powi(2);
                let db = (b.0 - x).powi(2) + (b.1 - y).powi(2);
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    /// Rotate around the circuit center so the map matches the official orientation.
    pub fn transform(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (cx, cy) = self.center;
        let (dx, dy) = (x - cx, y - cy);
        let (sin, cos) = self.rotation.sin_cos();
        (dx * cos - dy * sin, dx * sin + dy * cos)
    }
}

fn parse_markers(value: &Value, key: &str, outline: &TrackOutline) -> Option<Vec<(f64, f64, i64)>> {
    let Some(markers) = value.get(key) else {
        return Some(Vec::new());
    };
    markers
        .as_array()?
        .iter()
        .map(|marker| {
            let number = marker.get("number")?.as_i64()?;
            let x = marker.pointer("/trackPosition/x")?.as_f64()?;
            let y = marker.pointer("/trackPosition/y")?.as_f64()?;
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            let (x, y) = outline.transform((x, y));
            (x.is_finite() && y.is_finite()).then_some((x, y, number))
        })
        .collect()
}

/// A car resolved into canvas space, ready to paint.
struct MapCar {
    pos: (f64, f64),
    color: (u8, u8, u8),
    tla: String,
    /// Whether this car should carry a text label this frame.
    label: bool,
    /// Dim to gray (in pit, or off a hot lap during quali).
    dim: bool,
    selected: bool,
    /// Holds the session-best lap (emphasized in practice; pulses on a new best).
    fastest: bool,
    /// Flash magenta this frame — a new fastest lap was just set.
    pulse: bool,
}

/// Magenta used for the fastest-lap accent/pulse, matching the header FL chip.
const FL_MAGENTA: (u8, u8, u8) = (255, 0, 255);

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(track) = &app.track else { return };

    // Equalize data-units-per-braille-dot on both axes so the track keeps its shape.
    let (bx0, bx1, by0, by1) = track.bounds;
    let dots_x = (area.width.max(1) as f64) * 2.0;
    let dots_y = (area.height.max(1) as f64) * 4.0;
    let (x_bounds, y_bounds) = fit_aspect((bx0, bx1), (by0, by1), dots_x, dots_y);

    // Outline color reflects the current track state — the map doubles as a flag.
    let outline_color = match app.vm.track_flag {
        Some(TrackFlag::Yellow | TrackFlag::SafetyCar | TrackFlag::Vsc) => {
            super::color(120, 100, 30)
        }
        Some(TrackFlag::Red) => super::color(120, 40, 40),
        _ => Color::DarkGray,
    };

    // Label density: when the pane is roomy, label every car; when it's tight,
    // only the selected driver (avoids a wall of overlapping text).
    let label_all = area.width >= 44 && app.vm.cars.len() <= 24;
    let is_quali = app.vm.is_qualifying();
    // Only spotlight the fastest-lap holder once we know it's actually a
    // Practice session — `session_type` is empty until SessionInfo arrives, so
    // the old `!race && !quali` heuristic misfired on connect (plan 2.5).
    let is_practice = app.vm.is_practice();
    let selected_tla = app.selected_tla();
    let fastest_tla = app.vm.fastest.as_ref().map(|(_, tla)| tla.as_str());
    let pulsing_tla = app.pulsing_tla();

    let cars: Vec<MapCar> = app
        .vm
        .cars
        .iter()
        .map(|c| {
            let selected = selected_tla.as_deref() == Some(c.tla.as_str());
            let fastest = fastest_tla == Some(c.tla.as_str());
            let pulse = pulsing_tla == Some(c.tla.as_str());
            // In qualifying, spotlight who's actually on a lap; dim the rest.
            // Never dim the practice fastest-lap holder.
            let dim =
                (c.in_pit || (is_quali && !c.hot_lap && !selected)) && !(is_practice && fastest);
            MapCar {
                pos: track.transform((c.x, c.y)),
                color: c.color,
                tla: c.tla.clone(),
                label: label_all || selected || (is_practice && fastest) || pulse,
                dim,
                selected,
                fastest: is_practice && fastest,
                pulse,
            }
        })
        .collect();

    // One braille-dot step in data units, used to thicken lines into ribbons.
    let rx = (x_bounds.1 - x_bounds.0) / dots_x;
    let ry = (y_bounds.1 - y_bounds.0) / dots_y;

    // Split the outline into base points and the points inside an active yellow
    // marshal sector (3.3), so each slice can be thickened and colored on its
    // own (thickening the whole outline at once would bleed colors across the
    // boundary). Under SC/VSC/red, yellow_sectors is empty, so the whole-track
    // tint in `outline_color` wins by construction.
    let mut base_pts: Vec<(f64, f64)> = Vec::with_capacity(track.points.len());
    let mut yellow_pts: Vec<(f64, f64)> = Vec::new();
    if app.vm.yellow_sectors.is_empty() {
        base_pts.extend_from_slice(&track.points);
    } else {
        for (i, &p) in track.points.iter().enumerate() {
            let in_yellow = track
                .sector_ranges
                .iter()
                .any(|(n, r)| app.vm.yellow_sectors.contains(n) && r.contains(&i));
            if in_yellow {
                yellow_pts.push(p);
            } else {
                base_pts.push(p);
            }
        }
    }
    // Thicken each slice independently so colors don't bleed at the seam.
    let track_thick = thicken(&base_pts, rx, ry);
    let yellow_thick = thicken(&yellow_pts, rx, ry);

    // Derived pit-lane trace (3.4): dim, unthickened dots, subordinate to the
    // ribbon. Transform the raw samples at draw time like car dots.
    let pit_lane: Vec<(f64, f64)> = app.pit_lane.iter().map(|&p| track.transform(p)).collect();

    // A short mark at the start of the outline (the S/F line), recolored so it
    // stands out from the thick track ribbon.
    let sf_len = (track.points.len() / 80).clamp(2, 6);
    let sf_segment: Vec<(f64, f64)> = track.points.iter().take(sf_len).copied().collect();
    let sf_thick = thicken(&sf_segment, rx, ry);

    let canvas = Canvas::default()
        .block(Block::default().borders(Borders::TOP).title(map_title(app)))
        .marker(Marker::Braille)
        .x_bounds([x_bounds.0, x_bounds.1])
        .y_bounds([y_bounds.0, y_bounds.1])
        .paint(move |ctx| {
            // Pit lane underneath everything: dim, so it reads as subordinate.
            if !pit_lane.is_empty() {
                ctx.draw(&Points {
                    coords: &pit_lane,
                    color: Color::DarkGray,
                });
            }
            ctx.draw(&Points {
                coords: &track_thick,
                color: outline_color,
            });
            // Active yellow marshal sectors paint over the base outline (3.3).
            if !yellow_thick.is_empty() {
                ctx.draw(&Points {
                    coords: &yellow_thick,
                    color: Color::Yellow,
                });
            }
            // Recolor the start/finish stretch on top of the outline.
            ctx.draw(&Points {
                coords: &sf_thick,
                color: Color::White,
            });
            ctx.layer();

            for car in &cars {
                let (x, y) = car.pos;
                let (cr, cg, cb) = if car.pulse || car.fastest {
                    FL_MAGENTA
                } else if car.dim {
                    (90, 90, 90)
                } else {
                    car.color
                };
                let color = super::color(cr, cg, cb);

                // A filled blob wider than the track ribbon so cars stand out.
                let mut dot = Vec::with_capacity(13);
                for (mx, my) in [
                    (0.0, 0.0),
                    (1.0, 0.0),
                    (-1.0, 0.0),
                    (0.0, 1.0),
                    (0.0, -1.0),
                    (1.0, 1.0),
                    (-1.0, 1.0),
                    (1.0, -1.0),
                    (-1.0, -1.0),
                    (2.0, 0.0),
                    (-2.0, 0.0),
                    (0.0, 2.0),
                    (0.0, -2.0),
                ] {
                    dot.push((x + rx * mx, y + ry * my));
                }
                ctx.draw(&Points {
                    coords: &dot,
                    color,
                });
            }
            // Labels on the top layer so they're never occluded by dots.
            ctx.layer();
            for car in &cars {
                if !car.label {
                    continue;
                }
                let (x, y) = car.pos;
                let style = if car.selected {
                    Style::default().fg(Color::White).bg(super::color(
                        car.color.0,
                        car.color.1,
                        car.color.2,
                    ))
                } else if car.pulse || car.fastest {
                    Style::default().fg(super::color(FL_MAGENTA.0, FL_MAGENTA.1, FL_MAGENTA.2))
                } else if car.dim {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(super::color(car.color.0, car.color.1, car.color.2))
                };
                ctx.print(x + rx * 2.5, y + ry, RSpan::styled(car.tla.clone(), style));
            }
        });
    f.render_widget(canvas, area);
}

/// Widen a polyline into a ribbon by adding a one-dot cross around each point.
fn thicken(points: &[(f64, f64)], rx: f64, ry: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(points.len() * 5);
    for &(x, y) in points {
        out.push((x, y));
        out.push((x + rx, y));
        out.push((x - rx, y));
        out.push((x, y + ry));
        out.push((x, y - ry));
    }
    out
}

/// Map pane title reflects the session type so it's obvious what you're watching.
fn map_title(app: &App) -> String {
    let kind = if app.vm.is_race() {
        "Race"
    } else if app.vm.is_qualifying() {
        "Qualifying"
    } else if !app.vm.session_type.is_empty() {
        &app.vm.session_type
    } else {
        "Track"
    };
    format!(" {kind} · Track ")
}

/// Expand the smaller range so data units per dot are equal on both axes.
fn fit_aspect(
    (x0, x1): (f64, f64),
    (y0, y1): (f64, f64),
    dots_x: f64,
    dots_y: f64,
) -> ((f64, f64), (f64, f64)) {
    let per_dot_x = (x1 - x0) / dots_x;
    let per_dot_y = (y1 - y0) / dots_y;
    if per_dot_x > per_dot_y {
        let new_range = per_dot_x * dots_y;
        let cy = (y0 + y1) / 2.0;
        ((x0, x1), (cy - new_range / 2.0, cy + new_range / 2.0))
    } else {
        let new_range = per_dot_y * dots_x;
        let cx = (x0 + x1) / 2.0;
        ((cx - new_range / 2.0, cx + new_range / 2.0), (y0, y1))
    }
}

/// Insert interpolated points between outline vertices so the drawn line is continuous.
fn densify(points: Vec<(f64, f64)>) -> Option<Vec<(f64, f64)>> {
    if points.len() < 2 {
        return Some(points);
    }
    let (min_x, max_x) = min_max(points.iter().map(|p| p.0));
    let (min_y, max_y) = min_max(points.iter().map(|p| p.1));
    let step = ((max_x - min_x).max(max_y - min_y) / 400.0).max(1.0);
    let mut out = Vec::with_capacity(points.len().saturating_mul(4).min(MAX_DENSIFIED_POINTS));
    for w in points.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        if !dist.is_finite() {
            return None;
        }
        let n = (dist / step).ceil().max(1.0) as usize;
        if out.len().checked_add(n)?.checked_add(1)? > MAX_DENSIFIED_POINTS {
            return None;
        }
        out.push((x0, y0));
        for i in 1..n {
            let t = i as f64 / n as f64;
            out.push((x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
        }
    }
    out.push(*points.last().unwrap());
    Some(out)
}

fn min_max(iter: impl Iterator<Item = f64>) -> (f64, f64) {
    iter.fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_coordinate_does_not_leave_a_partial_outline() {
        assert!(TrackOutline::parse(&json!({"x": [0.0, "bad"], "y": [0.0, 1.0]})).is_none());
    }

    #[test]
    fn degenerate_outline_is_rejected() {
        assert!(TrackOutline::parse(&json!({"x": [1.0, 1.0], "y": [0.0, 2.0]})).is_none());
    }

    #[test]
    fn malformed_rotation_and_markers_are_rejected() {
        assert!(
            TrackOutline::parse(&json!({
                "x": [0.0, 1.0, 2.0],
                "y": [0.0, 1.0, 0.0],
                "rotation": "NaN"
            }))
            .is_none()
        );
        assert!(
            TrackOutline::parse(&json!({
                "x": [0.0, 1.0, 2.0],
                "y": [0.0, 1.0, 0.0],
                "corners": [{"number": 1, "trackPosition": {"x": "bad", "y": 1.0}}]
            }))
            .is_none()
        );
    }

    #[test]
    fn excessive_densification_is_rejected() {
        let points = (0..502)
            .map(|i| {
                if i % 2 == 0 {
                    (0.0, 0.0)
                } else {
                    (1_000_000_000.0, 1.0)
                }
            })
            .collect();
        assert!(densify(points).is_none());
    }
}
