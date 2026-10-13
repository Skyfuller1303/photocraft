//! Shape stroke geometry controls shared by the tool defaults and Properties.
use photocraft_doc::{LineCap, LineJoin, ShapeStroke, StrokeAlign};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Colour and width stay in their existing controls. These options never replace a shape's paint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StrokeOptions {
    pub align: StrokeAlign,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    pub dashes: Vec<f32>,
    pub dash_offset: f32,
    pub opacity: f32,
}

impl Default for StrokeOptions {
    fn default() -> Self {
        Self::from(&ShapeStroke::default())
    }
}

impl From<&ShapeStroke> for StrokeOptions {
    fn from(s: &ShapeStroke) -> Self {
        Self { align: s.align, cap: s.cap, join: s.join, miter_limit: s.miter_limit, dashes: s.dashes.clone(), dash_offset: s.dash_offset, opacity: s.opacity }
    }
}

impl StrokeOptions {
    pub fn params(&self) -> Value {
        json!({"align": format!("{:?}", self.align).to_ascii_lowercase(),
            "cap": format!("{:?}", self.cap).to_ascii_lowercase(), "join": format!("{:?}", self.join).to_ascii_lowercase(),
            "miterLimit": self.miter_limit, "dashes": self.dashes, "dashOffset": self.dash_offset, "opacity": self.opacity * 100.0})
    }

    pub fn stroke(&self, width: f32, paint: photocraft_doc::Fill) -> ShapeStroke {
        ShapeStroke {
            width,
            paint,
            align: self.align,
            cap: self.cap,
            join: self.join,
            miter_limit: self.miter_limit,
            dashes: self.dashes.clone(),
            dash_offset: self.dash_offset,
            opacity: self.opacity,
        }
    }

    fn label(&self) -> &'static str {
        if self.dashes.is_empty() {
            "Solid"
        } else if self.dashes == [4.0, 2.0] {
            "Dashed"
        } else if self.dashes == [0.0, 2.0] && self.cap == LineCap::Round {
            "Dotted"
        } else {
            "Custom"
        }
    }

    fn preset(&mut self, name: &str) {
        self.dash_offset = 0.0;
        self.dashes = match name {
            "Dashed" => vec![4.0, 2.0],
            "Dotted" => {
                self.cap = LineCap::Round;
                vec![0.0, 2.0]
            }
            _ => Vec::new(),
        };
    }

    fn valid(&self) -> bool {
        self.stroke(3.0, photocraft_doc::Fill::Solid(photocraft_doc::Color::BLACK)).validate().is_ok()
    }
}

#[derive(Clone)]
struct DragCache {
    shape: photocraft_doc::ShapeLayer,
    canvas: photocraft_geom::Rect,
    viewport: photocraft_geom::Rect,
    scale: f64,
    mode: photocraft_doc::ColorMode,
    patterns: Vec<photocraft_doc::Pattern>,
    image_rect: egui::Rect,
    texture: egui::TextureHandle,
}

/// Rasterize only the visible document region, then map its textured quad through the full view
/// transform. Gradient and pattern placement therefore follows rotated and flipped canvases.
pub(crate) fn paint_document_shape_preview(
    painter: &egui::Painter,
    shape: &photocraft_doc::ShapeLayer,
    canvas: photocraft_geom::Rect,
    patterns: &[photocraft_doc::Pattern],
    mode: photocraft_doc::ColorMode,
    xf: &crate::canvas::ViewXform,
) {
    let clip = painter.clip_rect();
    if !clip.is_finite() || clip.is_negative() || !xf.zoom.is_finite() || xf.zoom <= 0.0 {
        return;
    }
    let corners = [clip.left_top(), clip.right_top(), clip.right_bottom(), clip.left_bottom()].map(|p| xf.to_doc(p));
    if !corners.iter().flatten().all(|v| v.is_finite()) {
        return;
    }
    let min_x = corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let min_y = corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let max_x = corners.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
    let max_y = corners.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
    let viewport = photocraft_geom::Rect::new(min_x.floor() as i32, min_y.floor() as i32, max_x.ceil() as i32, max_y.ceil() as i32).intersect(&canvas);
    let zoom = f64::from(xf.zoom * painter.ctx().pixels_per_point());
    let Some(cache) = preview_texture(painter, shape, canvas, viewport, zoom, patterns, mode) else { return };
    let corners = [cache.image_rect.left_top(), cache.image_rect.right_top(), cache.image_rect.right_bottom(), cache.image_rect.left_bottom()];
    let mut mesh = egui::Mesh::with_texture(cache.texture.id());
    for (p, uv) in corners.into_iter().zip([egui::pos2(0.0, 0.0), egui::pos2(1.0, 0.0), egui::pos2(1.0, 1.0), egui::pos2(0.0, 1.0)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos: xf.to_screen(p.x, p.y), uv, color: egui::Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

fn preview_texture(
    painter: &egui::Painter,
    shape: &photocraft_doc::ShapeLayer,
    canvas: photocraft_geom::Rect,
    viewport: photocraft_geom::Rect,
    zoom: f64,
    patterns: &[photocraft_doc::Pattern],
    mode: photocraft_doc::ColorMode,
) -> Option<DragCache> {
    if viewport.is_empty() || !zoom.is_finite() || zoom <= 0.0 {
        return None;
    }
    let scale = zoom.min(2048.0 / f64::from(viewport.width().max(viewport.height()).max(1)));
    let used_patterns: Vec<_> = shape
        .fill
        .iter()
        .chain(shape.stroke.iter().map(|s| &s.paint))
        .filter_map(|fill| match fill {
            photocraft_doc::Fill::Pattern { id, name, .. } => photocraft_doc::pattern::find(patterns, id, name).cloned(),
            _ => None,
        })
        .collect();
    let id = egui::Id::new("shape-drag-render");
    let cached = painter.ctx().data(|d| d.get_temp::<DragCache>(id));
    let cache = cached
        .filter(|c| c.shape == *shape && c.canvas == canvas && c.viewport == viewport && c.scale == scale && c.mode == mode && c.patterns == used_patterns)
        .or_else(|| {
            let tr = photocraft_geom::Affine { m: [scale, 0.0, 0.0, scale, -f64::from(viewport.x0) * scale, -f64::from(viewport.y0) * scale] };
            let compiled = photocraft_vector::CompiledShape::preview(shape, photocraft_vector::DEFAULT_TOLERANCE, canvas, &used_patterns, mode, tr)?;
            let proxy_canvas = photocraft_geom::Rect::from_size(photocraft_geom::Size::new(
                (f64::from(viewport.width()) * scale).ceil().min(2048.0) as u32,
                (f64::from(viewport.height()) * scale).ceil().min(2048.0) as u32,
            ));
            let bounds = compiled.bounds()?.intersect(&proxy_canvas);
            if bounds.is_empty() {
                return None;
            }
            let rgba = compiled.render_rgba(bounds);
            let bytes: Vec<u8> = rgba.iter().flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect();
            let texture = painter.ctx().load_texture(
                "shape-drag",
                egui::ColorImage::from_rgba_unmultiplied([bounds.width() as usize, bounds.height() as usize], &bytes),
                egui::TextureOptions::LINEAR,
            );
            let document = |x, y| egui::pos2((f64::from(viewport.x0) + f64::from(x) / scale) as f32, (f64::from(viewport.y0) + f64::from(y) / scale) as f32);
            Some(DragCache {
                shape: shape.clone(),
                canvas,
                viewport,
                scale,
                mode,
                patterns: used_patterns,
                image_rect: egui::Rect::from_min_max(document(bounds.x0, bounds.y0), document(bounds.x1, bounds.y1)),
                texture,
            })
        })?;
    painter.ctx().data_mut(|d| d.insert_temp(id, cache.clone()));
    Some(cache)
}
mod editor;

pub use editor::{StrokeEditor, button};

fn open_frame_id() -> egui::Id {
    egui::Id::new("shape-stroke-open-frame")
}

pub(super) fn mark_rendered(ctx: &egui::Context, key: &str) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("shape-stroke-rendered", key)), pass));
}

/// A hidden options row or collapsed Appearance section no longer owns input.
/// Called after all options bars and Properties windows have had a chance to draw.
pub(crate) fn finish_frame(app: &mut crate::PhotocraftApp, ctx: &egui::Context) {
    let Some(e) = &app.ui.stroke_editor else { return };
    let shown = ctx.data(|d| d.get_temp::<u64>(egui::Id::new(("shape-stroke-rendered", e.key.as_str()))));
    if shown != Some(ctx.cumulative_pass_nr()) {
        editor::close_children(ctx, &e.key);
        app.ui.stroke_editor = None;
    }
}

#[derive(Clone, Default)]
struct CanvasDismissal {
    held: bool,
    consumed_frame: Option<u64>,
}

/// Pressing the canvas dismisses this popup and consumes that entire gesture. In particular,
/// its release must not open a shape-size dialog or commit a shape dragged behind the popup.
pub(crate) fn dismiss_from_canvas(app: &mut crate::PhotocraftApp, response: &egui::Response) -> bool {
    let ctx = &response.ctx;
    let frame = ctx.cumulative_frame_nr();
    let was_open =
        app.ui.stroke_editor.is_some() || ctx.data(|d| d.get_temp::<u64>(open_frame_id())) == Some(frame) || crate::shape_paint_ui::paint_popup_open(ctx);
    let (pressed, down) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.any_down()));
    let dismiss = was_open && pressed && response.contains_pointer();
    if dismiss && let Some(e) = app.ui.stroke_editor.take() {
        editor::close_children(ctx, &e.key);
        ctx.request_repaint();
    }
    if dismiss {
        crate::shape_paint_ui::close_popup(ctx);
    }
    ctx.data_mut(|d| {
        let state = d.get_temp_mut_or_default::<CanvasDismissal>(egui::Id::new("shape-stroke-canvas-dismissal"));
        state.held |= dismiss;
        let consumed = state.held || state.consumed_frame == Some(frame);
        if consumed {
            state.held = down;
            state.consumed_frame = Some(frame);
        }
        consumed
    })
}

/// Drop a popup whose source disappeared or changed outside this editor (including Undo).
pub(crate) fn sync_editor(app: &mut crate::PhotocraftApp, ctx: &egui::Context) {
    let Some(e) = &app.ui.stroke_editor else { return };
    let valid = if let Some((doc, layer)) = e.target {
        app.session.active().is_some_and(|st| {
            let locks = st.doc.effective_locks(layer);
            st.doc.id == doc
                && !app.ui.mask_target
                && !app.ui.vector_mask_target
                && st.active_layer == Some(layer)
                && !locks.all
                && !locks.pixels
                && st.doc.layer(layer).is_some_and(|l| match &l.content {
                    photocraft_doc::LayerContent::Shape(shape) => shape.stroke.as_ref().map(StrokeOptions::from).unwrap_or_default() == e.original,
                    _ => false,
                })
        })
    } else {
        let tool = app.ui.tool;
        ((e.key == "tool-stroke" && crate::vector_ui::is_shape_tool(tool) && app.ui.tool_options.shape.mode == "shape")
            || (e.key == "pen-stroke"
                && matches!(tool, crate::state::Tool::Pen | crate::state::Tool::FreeformPen | crate::state::Tool::CurvaturePen)
                && app.ui.tool_options.vector_mode == "shape"))
            && app.ui.tool_options.shape_stroke == e.original
    };
    if !valid {
        editor::close_children(ctx, &e.key);
        app.ui.stroke_editor = None;
    } else {
        // The options row can process a batched press+release before the canvas is drawn.
        // Keep the opening state for this frame so that click is still dismissal-only.
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| d.insert_temp(open_frame_id(), frame));
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod shortcut_tests;
