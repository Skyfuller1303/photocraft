//! Copy/Paste Mask and thumbnail drags, with explicit replacement confirmation (#2928).

use egui::{Rect, Stroke, StrokeKind};
use photocraft_doc::{Document, Layer, LayerId, LayerMask};
use photocraft_engine::mask_transfer_cmds::{COPY, MaskClip, PASTE, TRANSFER};
use serde_json::{Value, json};

use crate::{PhotocraftApp, mask_thumbs_ui, theme::Tokens};

pub(crate) struct Prompt {
    dialog: u64,
    command: String,
    params: Value,
    document: u64,
    canvas: photocraft_geom::Rect,
    layer: LayerId,
    previous: LayerMask,
    source: Option<(LayerId, LayerMask)>,
    clipboard: Option<MaskClip>,
}

/// Headless/automation calls need `replace: true`; interactive calls park behind a modal.
pub(crate) fn intercept(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, PASTE | TRANSFER)
        || app.automation_input
        || !params.is_object()
        || params.get("replace").is_some_and(|v| v.as_bool() != Some(false))
        || params.get("layer").is_some_and(|v| v.as_u64().is_none())
        || (id == TRANSFER && params.get("copy").is_some_and(|v| v.as_bool().is_none()))
    {
        return None;
    }
    let st = app.session.active()?;
    let layer = params.get("layer").and_then(Value::as_u64).map(LayerId).or(st.active_layer)?;
    let target = st.doc.layer(layer)?;
    let previous = target.mask.clone()?;
    Some((|| {
        if app.mask_transfer.is_some() {
            return Err("Answer the mask replacement prompt first".into());
        }
        if let Some(document) = params.get("document")
            && document.as_u64() != Some(st.doc.id.0)
        {
            return Err("The target document is no longer active".into());
        }
        if st.doc.effective_locks(layer).all {
            return Err("The target layer is locked".into());
        }
        let source = if id == TRANSFER {
            let from = params.get("from").and_then(Value::as_u64).map(LayerId).ok_or("No source layer")?;
            if from == layer {
                return Err("Source and target must be different layers".into());
            }
            let mask = st.doc.layer(from).and_then(|l| l.mask.clone()).ok_or("The source layer has no pixel mask")?;
            Some((from, mask))
        } else {
            if app.session.mask_clipboard.is_none() {
                return Err("No mask has been copied".into());
            }
            None
        };
        let mut params = params.as_object().cloned().ok_or("Params must be an object")?;
        params.insert("layer".into(), json!(layer.0));
        params.insert("document".into(), json!(st.doc.id.0));
        let fields = json!({"__label": "Replace Layer Mask?", "layerName": target.name});
        let dialog = app.ui.open_dialog(crate::state::DialogKind::Command, fields.as_object().cloned().unwrap_or_default());
        app.mask_transfer = Some(Prompt {
            dialog,
            command: id.into(),
            params: Value::Object(params),
            document: st.doc.id.0,
            canvas: st.doc.bounds(),
            layer,
            previous,
            source,
            clipboard: (id == PASTE).then(|| app.session.mask_clipboard.clone()).flatten(),
        });
        Ok(json!({"pending": true, "dialog": dialog}))
    })())
}

pub(crate) fn owns(app: &PhotocraftApp, dialog: u64) -> bool {
    app.mask_transfer.as_ref().is_some_and(|p| p.dialog == dialog)
}

pub(crate) fn cancel(app: &mut PhotocraftApp, dialog: u64) {
    if owns(app, dialog) {
        app.mask_transfer = None;
    }
}

pub(crate) fn confirm(app: &mut PhotocraftApp) -> Result<Value, String> {
    let result = confirm_snapshot(app);
    if let Err(error) = &result {
        app.ui.status = error.clone();
        app.ui.status_error = true;
    }
    result
}

fn confirm_snapshot(app: &mut PhotocraftApp) -> Result<Value, String> {
    let mut p = app.mask_transfer.take().ok_or("No mask replacement pending")?;
    app.ui.close_dialog(p.dialog);
    let st = app.session.active().ok_or("The target document was closed")?;
    if st.doc.id.0 != p.document || st.doc.bounds() != p.canvas || st.doc.layer(p.layer).and_then(|l| l.mask.as_ref()) != Some(&p.previous) {
        return Err("The target document or mask changed; paste again".into());
    }
    if let Some((id, mask)) = &p.source {
        if st.doc.layer(*id).and_then(|l| l.mask.as_ref()) != Some(mask) {
            return Err("The source mask changed; drag again".into());
        }
    } else if app.session.mask_clipboard != p.clipboard {
        return Err("The copied mask changed; paste again".into());
    }
    if let Some(params) = p.params.as_object_mut() {
        params.insert("replace".into(), json!(true));
    }
    app.run(&p.command, p.params)
}

pub(crate) fn body(ui: &mut egui::Ui, fields: &serde_json::Map<String, Value>) {
    let name = fields.get("layerName").and_then(Value::as_str).unwrap_or_default();
    ui.label(crate::i18n::fmt(tl!("Replace the existing pixel mask on “{layer}”?"), &[("layer", name)]));
}

type Drag = (u64, LayerId);
fn drag_key() -> egui::Id {
    egui::Id::new("pixel-mask-drag")
}

pub(crate) fn dragging(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Drag>(drag_key()).is_some())
}

pub(crate) fn clear_drag(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Drag>(drag_key()));
}

/// Paint once after the rows, including when the source was scrolled out of view.
pub(crate) fn drag_feedback(ctx: &egui::Context, doc: &Document) {
    let Some((document, source)) = ctx.data(|d| d.get_temp::<Drag>(drag_key())) else { return };
    if document != doc.id.0 || doc.layer(source).is_none_or(|layer| layer.mask.is_none()) {
        clear_drag(ctx);
        return;
    }
    if let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) {
        crate::layer_transfer::ghost(ctx, pointer, tl!("Layer Mask"));
        if ctx.input(|i| i.modifiers.alt) {
            ctx.set_cursor_icon(egui::CursorIcon::Copy);
        }
    }
}

/// Starts only when the press originated on a pixel-mask thumbnail. A row/name drag remains a layer drag.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drag_row(
    ctx: &egui::Context,
    ui: &egui::Ui,
    doc: &Document,
    layer: &Layer,
    response: &egui::Response,
    row: Rect,
    masks: &mask_thumbs_ui::MaskRects,
    actions: &mut Vec<(String, Value)>,
) {
    if response.drag_started()
        && let Some(origin) = ctx.input(|i| i.pointer.press_origin())
        && masks.thumb(mask_thumbs_ui::MaskKind::Pixel).is_some_and(|r| r.contains(origin))
    {
        ctx.data_mut(|d| d.insert_temp(drag_key(), (doc.id.0, layer.id)));
    }
    let Some((document, source)) = ctx.data(|d| d.get_temp::<Drag>(drag_key())) else { return };
    if document != doc.id.0 || (source == layer.id && layer.mask.is_none()) {
        clear_drag(ctx);
        return;
    }
    let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else { return };
    let copy = ctx.input(|i| i.modifiers.alt);
    if source != layer.id && row.contains(pointer) && !doc.effective_locks(layer.id).all {
        let t = Tokens::get(ctx);
        ui.painter().rect_stroke(row.shrink(1.0), t.radius_sm, Stroke::new(2.0, t.accent), StrokeKind::Inside);
        if ctx.input(|i| i.pointer.any_released()) {
            actions.push((TRANSFER.into(), json!({"document": document, "from": source.0, "layer": layer.id.0, "copy": copy})));
        }
    }
}

pub(crate) fn context_menu(app: &PhotocraftApp, ui: &mut egui::Ui, layer: &Layer, multi: bool, actions: &mut Vec<(String, Value)>) {
    for (id, label, enabled) in [
        (COPY, tl!("Copy Mask"), !multi && layer.mask.is_some()),
        (
            PASTE,
            tl!("Paste Mask"),
            !multi && app.session.mask_clipboard.is_some() && app.session.active().is_some_and(|st| !st.doc.effective_locks(layer.id).all),
        ),
    ] {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            actions.push(("layer.select".into(), json!({"layer": layer.id.0})));
            actions.push((id.into(), json!({"layer": layer.id.0})));
            ui.close();
        }
    }
    ui.separator();
}

#[cfg(test)]
mod tests;
