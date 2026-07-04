use super::App;
use crate::state::view::TrackFlag;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span as RSpan;
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use serde_json::Value;

pub struct TrackOutline {
    pub points: Vec<(f64, f64)>,
    pub corners: Vec<(f64, f64, i64)>,
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
        if xs.len() < 2 || xs.len() != ys.len() {
            return None;
        }
        let raw: Vec<(f64, f64)> = xs
            .iter()
            .zip(ys)
            .filter_map(|(x, y)| Some((x.as_f64()?, y.as_f64()?)))
            .collect();

        let rotation = v
            .get("rotation")
            .and_then(|r| r.as_f64())
            .unwrap_or(0.0)
            .to_radians();
        let (min_x, max_x) = min_max(raw.iter().map(|p| p.0));
        let (min_y, max_y) = min_max(raw.iter().map(|p| p.1));
        let center = ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);

        let mut outline = TrackOutline {
            points: Vec::new(),
            corners: Vec::new(),
            bounds: (0.0, 0.0, 0.0, 0.0),
            rotation,
            center,
        };
        outline.points = densify(raw.iter().map(|&p| outline.transform(p)).collect());
        outline.corners = v
            .get("corners")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let n = c.get("number")?.as_i64()?;
                        let x = c.pointer("/trackPosition/x")?.as_f64()?;
                        let y = c.pointer("/trackPosition/y")?.as_f64()?;
                        let (tx, ty) = outline.transform((x, y));
                        Some((tx, ty, n))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let (min_x, max_x) = min_max(outline.points.iter().map(|p| p.0));
        let (min_y, max_y) = min_max(outline.points.iter().map(|p| p.1));
        let pad_x = (max_x - min_x) * 0.05;
        let pad_y = (max_y - min_y) * 0.05;
        outline.bounds = (min_x - pad_x, max_x + pad_x, min_y - pad_y, max_y + pad_y);
        Some(outline)
    }

    /// Rotate around the circuit center so the map matches the official orientation.
    pub fn transform(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (cx, cy) = self.center;
        let (dx, dy) = (x - cx, y - cy);
        let (sin, cos) = self.rotation.sin_cos();
        (dx * cos - dy * sin, dx * sin + dy * cos)
    }
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
        Some(TrackFlag::Yellow | TrackFlag::SafetyCar | TrackFlag::Vsc) => Color::Rgb(120, 100, 30),
        Some(TrackFlag::Red) => Color::Rgb(120, 40, 40),
        _ => Color::DarkGray,
    };

    // Label density: when the pane is roomy, label every car; when it's tight,
    // only the selected driver (avoids a wall of overlapping text).
    let label_all = area.width >= 44 && app.vm.cars.len() <= 24;
    let is_quali = app.vm.is_qualifying();
    // Practice has no laps and isn't quali: highlight the fastest-lap holder.
    let is_practice = !app.vm.is_race() && !is_quali;
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
            let dim = (c.in_pit || (is_quali && !c.hot_lap && !selected))
                && !(is_practice && fastest);
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

    // Thicken the outline: each point becomes a small cross so the track reads
    // as a wider ribbon rather than a hairline.
    let track_thick = thicken(&track.points, rx, ry);

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
            ctx.draw(&Points {
                coords: &track_thick,
                color: outline_color,
            });
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
                let color = Color::Rgb(cr, cg, cb);

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
                ctx.draw(&Points { coords: &dot, color });
            }
            // Labels on the top layer so they're never occluded by dots.
            ctx.layer();
            for car in &cars {
                if !car.label {
                    continue;
                }
                let (x, y) = car.pos;
                let style = if car.selected {
                    Style::default().fg(Color::White).bg(Color::Rgb(car.color.0, car.color.1, car.color.2))
                } else if car.pulse || car.fastest {
                    Style::default().fg(Color::Rgb(FL_MAGENTA.0, FL_MAGENTA.1, FL_MAGENTA.2))
                } else if car.dim {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Rgb(car.color.0, car.color.1, car.color.2))
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
fn densify(points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    if points.len() < 2 {
        return points;
    }
    let (min_x, max_x) = min_max(points.iter().map(|p| p.0));
    let step = ((max_x - min_x) / 400.0).max(1.0);
    let mut out = Vec::with_capacity(points.len() * 4);
    for w in points.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        out.push((x0, y0));
        let dist = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let n = (dist / step) as usize;
        for i in 1..n {
            let t = i as f64 / n as f64;
            out.push((x0 + (x1 - x0) * t, y0 + (y1 - y0) * t));
        }
    }
    out.push(*points.last().unwrap());
    out
}

fn min_max(iter: impl Iterator<Item = f64>) -> (f64, f64) {
    iter.fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
}
