//! Live rectangle corner-radius controls. Document edits remain engine commands.

use egui::{Modifiers, Stroke};
use photocraft_doc::{DocId, LayerContent, LayerId, LiveShape};
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::{ToolEvent, ViewXform};
use crate::state::Tool;
use crate::theme::Tokens;

const HANDLE_INSET: f64 = 14.0;
const GRAB: f64 = 6.0;
const ARC_INSET: f64 = 1.0 - std::f64::consts::FRAC_1_SQRT_2;

#[derive(Default)]
pub(crate) struct State {
    serial: u64,
    drag: Option<Drag>,
}

pub(crate) fn dragging(app: &PhotocraftApp) -> bool {
    app.shape_canvas.drag.is_some()
}

pub(crate) fn supports_tool(tool: Tool) -> bool {
    matches!(tool, Tool::Move | Tool::DirectSelection | Tool::PathSelection) || crate::vector_ui::is_shape_tool(tool)
}

/// Navigation handles its own release, so it must also end an interrupted radius gesture.
pub(crate) fn end_if_navigation(app: &mut PhotocraftApp, tool: Tool) {
    if matches!(tool, Tool::Hand | Tool::Zoom | Tool::RotateView) {
        app.shape_canvas.drag = None;
    }
}

struct Drag {
    doc: DocId,
    layer: LayerId,
    tool: Tool,
    revision: u64,
    corner: usize,
    start: [f64; 2],
    rect: [f64; 4],
    radii: [f64; 4],
    edited: bool,
    key: String,
}

struct Rectangle {
    doc: DocId,
    layer: LayerId,
    revision: u64,
    rect: [f64; 4],
    radii: [f64; 4],
}

fn rectangle(app: &PhotocraftApp) -> Option<Rectangle> {
    let tool = app.active_tool();
    if app.ui.mask_target
        || app.ui.vector_mask_target
        || app.ui.transform.is_some()
        || !supports_tool(tool)
        || (crate::vector_ui::is_shape_tool(tool) && app.ui.tool_options.shape.mode != "shape")
    {
        return None;
    }
    let st = app.session.active()?;
    let layer = st.active_layer?;
    let locks = st.doc.effective_locks(layer);
    if locks.all || locks.pixels || locks.position {
        return None;
    }
    let LayerContent::Shape(sh) = &st.doc.layer(layer)?.content else { return None };
    let Some(LiveShape::Rect { rect, radii }) = &sh.live else { return None };
    if rect.iter().chain(radii.iter()).any(|n| !n.is_finite()) || rect[2] <= 0.0 || rect[3] <= 0.0 {
        return None;
    }
    Some(Rectangle { doc: st.doc.id, layer, revision: st.revision, rect: *rect, radii: *radii })
}

fn direction(corner: usize) -> [f64; 2] {
    match corner {
        0 => [1.0, 1.0],
        1 => [-1.0, 1.0],
        2 => [-1.0, -1.0],
        _ => [1.0, -1.0],
    }
}

fn handles(rect: [f64; 4], radii: [f64; 4], zoom: f64) -> [[f64; 2]; 4] {
    let [x, y, w, h] = rect;
    let radii = photocraft_vector::shapes::clamp_radii(w, h, radii);
    let inset = (HANDLE_INSET / zoom.max(0.01)).min(w * 0.25).min(h * 0.25);
    let corners = [[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
    std::array::from_fn(|i| {
        let d = direction(i);
        let distance = inset + radii[i] * ARC_INSET;
        [corners[i][0] + d[0] * distance, corners[i][1] + d[1] * distance]
    })
}

fn hit(rect: [f64; 4], radii: [f64; 4], p: [f64; 2], zoom: f64, aspect: f64) -> Option<usize> {
    let zoom = zoom.max(0.01);
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
    handles(rect, radii, zoom)
        .into_iter()
        .enumerate()
        .map(|(i, q)| (i, ((q[0] - p[0]) * zoom * aspect).hypot((q[1] - p[1]) * zoom)))
        .filter(|(_, distance)| *distance <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// A changed document, target or tool ends the gesture before another context can receive it.
pub(crate) fn cancel_if_left(app: &mut PhotocraftApp) {
    let keep = app
        .shape_canvas
        .drag
        .as_ref()
        .is_none_or(|d| rectangle(app).is_some_and(|r| r.doc == d.doc && r.layer == d.layer && r.revision == d.revision && app.active_tool() == d.tool));
    if !keep {
        app.shape_canvas.drag = None;
    }
}

/// Escape restores only this gesture; unrelated edits can never be undone by a stale handle.
pub(crate) fn keys(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    cancel_if_left(app);
    if app.shape_canvas.drag.is_none() || !ctx.input_mut(|i| i.consume_key(Modifiers::NONE, egui::Key::Escape)) {
        return false;
    }
    cancel(app);
    true
}

fn cancel(app: &mut PhotocraftApp) {
    cancel_if_left(app);
    if app.shape_canvas.drag.take().is_some_and(|d| d.edited)
        && let Err(e) = app.run("edit.undo", json!({}))
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Runs before snapping and Move/Free Transform so a knob press grabs its actual position.
pub(crate) fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, mods: Modifiers) -> bool {
    cancel_if_left(app);
    match ev {
        ToolEvent::Down { x, y, .. } => {
            let Some(r) = rectangle(app) else { return false };
            let Some(corner) = hit(r.rect, r.radii, [x, y], f64::from(app.point_zoom()), f64::from(app.ui.view.display_aspect())) else { return false };
            app.shape_canvas.serial = app.shape_canvas.serial.wrapping_add(1);
            app.shape_canvas.drag = Some(Drag {
                doc: r.doc,
                layer: r.layer,
                tool: app.active_tool(),
                revision: r.revision,
                corner,
                start: [x, y],
                rect: r.rect,
                radii: r.radii,
                edited: false,
                key: format!("shape-radius-{}-{}-{}", r.doc.0, r.layer.0, app.shape_canvas.serial),
            });
            true
        }
        ToolEvent::Move { x, y, .. } | ToolEvent::Up { x, y } => {
            let Some(mut d) = app.shape_canvas.drag.take() else { return false };
            if x.is_finite() && y.is_finite() && (d.edited || (x - d.start[0]).hypot(y - d.start[1]) * f64::from(app.point_zoom()) > 0.5) {
                let sign = direction(d.corner);
                let delta = ((x - d.start[0]) * sign[0] + (y - d.start[1]) * sign[1]) / (2.0 * ARC_INSET);
                let old = photocraft_vector::shapes::clamp_radii(d.rect[2], d.rect[3], d.radii);
                let radius = (old.get(d.corner).copied().unwrap_or(0.0) + delta).clamp(0.0, d.rect[2].min(d.rect[3]) / 2.0);
                let mut radii = if mods.alt { d.radii } else { [radius; 4] };
                if mods.alt
                    && let Some(r) = radii.get_mut(d.corner)
                {
                    *r = radius;
                }
                let changed = rectangle(app).is_some_and(|r| r.radii != radii);
                if changed {
                    match app.run("shape.edit", json!({"layer":d.layer.0,"radii":radii,"coalesce":d.key})) {
                        Ok(_) => {
                            d.edited = true;
                            if let Some(st) = app.session.active() {
                                d.revision = st.revision;
                            }
                        }
                        Err(e) => {
                            app.ui.status = e;
                            app.ui.status_error = true;
                            return true;
                        }
                    }
                }
            }
            if !matches!(ev, ToolEvent::Up { .. }) {
                app.shape_canvas.drag = Some(d);
            }
            true
        }
    }
}

pub(crate) fn draw_overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    let Some(mut r) = rectangle(app) else { return };
    if let Some(doc) = crate::move_ui::shown_doc(app) {
        let Some(layer) = doc.layer(r.layer) else { return };
        let LayerContent::Shape(shape) = &layer.content else { return };
        let Some(LiveShape::Rect { rect, radii }) = &shape.live else { return };
        r.rect = *rect;
        r.radii = *radii;
    }
    let t = Tokens::get(painter.ctx());
    let grabbed = app.shape_canvas.drag.as_ref().map(|d| d.corner);
    for (i, p) in handles(r.rect, r.radii, f64::from(xf.zoom)).into_iter().enumerate() {
        let p = xf.to_screen(p[0] as f32, p[1] as f32);
        painter.circle(p, 4.0, if grabbed == Some(i) { t.accent_soft } else { t.field }, Stroke::new(1.0, t.accent));
        painter.circle_filled(p, 1.5, t.accent);
    }
}

pub(crate) fn cursor(app: &PhotocraftApp, p: [f64; 2]) -> Option<egui::CursorIcon> {
    let r = rectangle(app)?;
    (app.shape_canvas.drag.is_some() || hit(r.rect, r.radii, p, f64::from(app.point_zoom()), f64::from(app.ui.view.display_aspect())).is_some())
        .then_some(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests;
