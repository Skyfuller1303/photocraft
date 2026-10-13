//! Transfer an editable pixel mask, independently of the layer/pixel clipboard (#2928).

use photocraft_doc::{LayerId, LayerMask};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const COPY: &str = "layer.layerMask.copy";
pub const PASTE: &str = "layer.layerMask.paste";
pub const TRANSFER: &str = "layer.layerMask.transfer";

/// Session-only snapshot: the full mask and source canvas, including sparse default pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskClip {
    pub mask: LayerMask,
    pub canvas: Rect,
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn check_document(s: &Session, p: &Value, cmd: &str) -> Result<()> {
    if !p.is_object() {
        return Err(bad(cmd, "params must be an object"));
    }
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if let Some(id) = p.get("document")
        && id.as_u64() != Some(st.doc.id.0)
    {
        return Err(bad(cmd, "the target document is no longer active"));
    }
    Ok(())
}

fn layer_id(s: &Session, p: &Value, field: &str, cmd: &str) -> Result<LayerId> {
    match p.get(field) {
        Some(id) => id.as_u64().map(LayerId).ok_or_else(|| bad(cmd, format!("`{field}` must be an unsigned layer id"))),
        None if field == "layer" => s.active().and_then(|st| st.active_layer).ok_or_else(|| bad(cmd, "no active layer")),
        None => Err(bad(cmd, format!("`{field}` is required"))),
    }
}

fn flag(p: &Value, field: &str, default: bool, cmd: &str) -> Result<bool> {
    p.get(field).map_or(Ok(default), |v| v.as_bool().ok_or_else(|| bad(cmd, format!("`{field}` must be a boolean"))))
}

fn snapshot(s: &Session, id: LayerId) -> Result<MaskClip> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let mask = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?.mask.clone().ok_or_else(|| EngineError::Other("the source layer has no pixel mask".into()))?;
    Ok(MaskClip { mask, canvas: st.doc.bounds() })
}

fn check_unlocked(s: &Session, id: LayerId) -> Result<()> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let layer = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    if st.doc.effective_locks(id).all {
        return Err(EngineError::Other(format!("the layer \"{}\" is locked", layer.name)));
    }
    Ok(())
}

fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    check_document(s, p, COPY)?;
    let id = layer_id(s, p, "layer", COPY)?;
    s.mask_clipboard = Some(snapshot(s, id)?);
    Ok(json!({"layer": id.0, "copied": true}))
}

fn apply(s: &mut Session, p: &Value, cmd: &str, clip: MaskClip, target: LayerId, source: Option<LayerId>) -> Result<Value> {
    let replace = flag(p, "replace", false, cmd)?;
    check_unlocked(s, target)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.layer(target).is_some_and(|l| l.mask.is_some()) && !replace {
        return Err(bad(cmd, "the target already has a pixel mask; confirm replacement with `replace: true`"));
    }
    if let Some(source) = source {
        check_unlocked(s, source)?;
    }
    let canvas = st.doc.bounds();
    let offset = |a: i32, b: i32, c: i32, d: i32| -> Result<i32> {
        i32::try_from((i64::from(a) + i64::from(b) - i64::from(c) - i64::from(d)).div_euclid(2))
            .map_err(|_| bad(cmd, "canvas-centering offset is out of range"))
    };
    let (dx, dy) = (offset(canvas.x0, canvas.x1, clip.canvas.x0, clip.canvas.x1)?, offset(canvas.y0, canvas.y1, clip.canvas.y0, clip.canvas.y1)?);
    let mut mask = clip.mask;
    if dx != 0 || dy != 0 {
        mask.surface = photocraft_algo::resample::translate_surface(&mask.surface, dx, dy);
    }
    s.edit(if source.is_some() { "Move Layer Mask" } else { "Paste Layer Mask" }, |doc, _| {
        crate::extra_cmds::background_to_layer_for_mask(doc, target);
        doc.layer_mut(target).ok_or(EngineError::NoLayer(target))?.mask = Some(mask);
        if let Some(source) = source {
            doc.layer_mut(source).ok_or(EngineError::NoLayer(source))?.mask = None;
        }
        Ok(())
    })?;
    Ok(json!({"layer": target.0, "offset": [dx, dy], "moved": source.is_some()}))
}

fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    check_document(s, p, PASTE)?;
    let target = layer_id(s, p, "layer", PASTE)?;
    let clip = s.mask_clipboard.clone().ok_or_else(|| bad(PASTE, "no mask has been copied"))?;
    apply(s, p, PASTE, clip, target, None)
}

fn transfer(s: &mut Session, p: &Value) -> Result<Value> {
    check_document(s, p, TRANSFER)?;
    let source = layer_id(s, p, "from", TRANSFER)?;
    let target = layer_id(s, p, "layer", TRANSFER)?;
    if source == target {
        return Err(bad(TRANSFER, "source and target must be different layers"));
    }
    let copy = flag(p, "copy", true, TRANSFER)?;
    let clip = snapshot(s, source)?;
    apply(s, p, TRANSFER, clip, target, (!copy).then_some(source))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: COPY,
            label: "Copy Mask",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"layer":id?=active,"document":id?} → {layer,copied}; copies the full pixel mask with density, feather, enabled and linked state; session-only clipboard shared by documents"#,
            enabled: |s| crate::active_layer_of(s).map(|_| ()),
            run: copy,
            journal: true,
        },
        CommandSpec {
            id: PASTE,
            label: "Paste Mask",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"layer":id?=active,"document":id?,"replace":bool=false} → {layer,offset,moved}; attaches the copied pixel mask, centered by source/destination canvas dimensions; existing masks require explicit replacement; one Undo step"#,
            enabled: |s| {
                crate::active_layer_of(s)?;
                s.mask_clipboard.as_ref().map(|_| ()).ok_or_else(|| "no mask has been copied".into())
            },
            run: paste,
            journal: true,
        },
        CommandSpec {
            id: TRANSFER,
            label: "Transfer Layer Mask",
            menu: &[],
            shortcut: None,
            params: r#"{"from":id,"layer":id?=active,"document":id?,"copy":bool=true,"replace":bool=false} → {layer,offset,moved}; copies or moves a pixel mask within the active document without changing either clipboard; one Undo step"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
            run: transfer,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests;
