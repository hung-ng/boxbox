use super::App;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span as RSpan;
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Points};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use serde_json::Value;

pub struct TrackOutline {
    pub points: Vec<(f64, f64)>,
    pub corners: Vec<(f64, f64, i64)>,
    /// Start/finish line endpoints, perpendicular to the track at outline point 0.
    pub start_line: Option<((f64, f64), (f64, f64))>,
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
            start_line: None,
            bounds: (0.0, 0.0, 0.0, 0.0),
            rotation,
            center,
        };
        outline.points = densify(raw.iter().map(|&p| outline.transform(p)).collect());

        // The outline starts at the start/finish line; draw a tick across the
        // track there, perpendicular to the racing direction.
        if raw.len() > 4 {
            let p0 = outline.transform(raw[0]);
            let p1 = outline.transform(raw[3]);
            let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len > 0.0 {
                let half = ((max_x - min_x).max(max_y - min_y)) * 0.02;
                let (nx, ny) = (-dy / len * half, dx / len * half);
                outline.start_line =
                    Some(((p0.0 - nx, p0.1 - ny), (p0.0 + nx, p0.1 + ny)));
            }
        }
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

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(track) = &app.track else { return };

    // Equalize data-units-per-braille-dot on both axes so the track keeps its shape.
    let (bx0, bx1, by0, by1) = track.bounds;
    let dots_x = (area.width.max(1) as f64) * 2.0;
    let dots_y = (area.height.max(1) as f64) * 4.0;
    let (x_bounds, y_bounds) = fit_aspect((bx0, bx1), (by0, by1), dots_x, dots_y);

    let cars: Vec<((f64, f64), (u8, u8, u8))> = app
        .vm
        .cars
        .iter()
        .map(|c| {
            let color = if c.in_pit { (90, 90, 90) } else { c.color };
            (track.transform((c.x, c.y)), color)
        })
        .collect();

    let canvas = Canvas::default()
        .block(Block::default().borders(Borders::TOP).title(" Track "))
        .marker(Marker::Braille)
        .x_bounds([x_bounds.0, x_bounds.1])
        .y_bounds([y_bounds.0, y_bounds.1])
        .paint(move |ctx| {
            ctx.draw(&Points {
                coords: &track.points,
                color: Color::DarkGray,
            });
            if let Some(((x1, y1), (x2, y2))) = track.start_line {
                ctx.draw(&CanvasLine {
                    x1,
                    y1,
                    x2,
                    y2,
                    color: Color::White,
                });
                ctx.print(
                    x2,
                    y2,
                    RSpan::styled("S/F", Style::default().fg(Color::DarkGray)),
                );
            }
            ctx.layer();
            for ((x, y), (r, g, b)) in &cars {
                // A small cluster of points so each car is visible.
                let rx = (x_bounds.1 - x_bounds.0) / dots_x;
                let ry = (y_bounds.1 - y_bounds.0) / dots_y;
                let dot = [
                    (*x, *y),
                    (*x + rx, *y),
                    (*x - rx, *y),
                    (*x, *y + ry),
                    (*x, *y - ry),
                ];
                ctx.draw(&Points {
                    coords: &dot,
                    color: Color::Rgb(*r, *g, *b),
                });
            }
        });
    f.render_widget(canvas, area);
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
