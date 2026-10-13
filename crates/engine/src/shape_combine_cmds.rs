//! Combine selected shape layers while retaining the frontmost layer's position and style.
use photocraft_doc::{FillRule, LayerContent, LayerId, Path, PathOp};
use serde_json::Value;

use crate::{EngineError, Result, Session};

/// `None` preserves the existing single-layer/component operation. An explicit component
/// or layer never implicitly merges the rest of the layer selection.
pub(crate) fn selected(s: &mut Session, p: &Value, op: PathOp, bake: bool, label: &str) -> Result<Option<LayerId>> {
    if !bake && p.get("subpath").is_some() {
        return Ok(None);
    }
    let ids = crate::layer_multi_cmds::targets(s, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let ids = if p.get("layer").is_some() {
        ids
    } else {
        crate::layer_multi_cmds::top_level(&st.doc, &ids)
            .into_iter()
            .filter(|id| st.doc.layer(*id).is_some_and(|layer| matches!(layer.content, LayerContent::Shape(_))))
            .collect()
    };
    if ids.len() < 2 {
        return Ok(None);
    }
    let top = ids.last().copied().ok_or(EngineError::NoDocument)?;
    let mut combined = Path::default();
    for (operand_index, &id) in ids.iter().enumerate() {
        let layer = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
        let locks = st.doc.effective_locks(id);
        if locks.all || locks.pixels || locks.position {
            return Err(EngineError::Other("A selected shape layer is locked".into()));
        }
        let LayerContent::Shape(shape) = &layer.content else {
            return Err(EngineError::Other("Select only shape layers to combine".into()));
        };
        if shape.path.inverted {
            return Err(EngineError::Other("Cannot combine inverted shape paths".into()));
        }
        // One selected layer is one boolean operand. In particular, intersecting a
        // compound front layer means A ∩ (B ∪ C), not (A ∩ B) ∩ C. Canonical contours
        // also let operands with different winding rules share a NonZero result.
        let mut operand = if shape.path.components().len() > 1 || shape.path.fill_rule != FillRule::NonZero {
            photocraft_vector::components::merge(&shape.path, 0.1, 16_000_000).map_err(EngineError::Other)?
        } else {
            shape.path.clone()
        };
        if operand.subpaths.is_empty() {
            if operand_index > 0 && op == PathOp::Intersect {
                combined.subpaths.clear();
            }
            continue;
        }
        let first = combined.subpaths.is_empty();
        if first && operand_index > 0 && matches!(op, PathOp::Subtract | PathOp::Intersect) {
            continue;
        }
        for (index, subpath) in operand.subpaths.iter_mut().enumerate() {
            subpath.op = if index == 0 { if first { PathOp::Combine } else { op } } else { PathOp::Join };
        }
        combined.subpaths.extend(operand.subpaths);
        photocraft_vector::flatten::validate_shape(&combined, None).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    if bake {
        let tolerance = p.get("tolerance").and_then(Value::as_f64).unwrap_or(0.1).clamp(0.05, 10.0);
        combined = photocraft_vector::components::merge(&combined, tolerance, 16_000_000).map_err(EngineError::Other)?;
    }
    let layer = st.doc.layer(top).ok_or(EngineError::NoLayer(top))?;
    let LayerContent::Shape(mut merged) = layer.content.clone() else {
        return Err(EngineError::Other("Select only shape layers to combine".into()));
    };
    merged.path = combined;
    merged.live = None;
    merged.psd_raw = None;
    merged.cache = None;
    photocraft_vector::flatten::validate_shape(&merged.path, merged.stroke.as_ref()).map_err(|e| EngineError::Other(e.to_string()))?;
    s.edit(label, |doc, active| {
        crate::vector_cmds::refresh_shape(doc, &mut merged);
        for &id in &ids {
            if id != top {
                doc.remove(id).ok_or(EngineError::NoLayer(id))?;
            }
        }
        doc.layer_mut(top).ok_or(EngineError::NoLayer(top))?.content = LayerContent::Shape(merged);
        *active = Some(top);
        Ok(())
    })?;
    crate::layer_multi_cmds::reselect(s, vec![top], Some(top));
    Ok(Some(top))
}
