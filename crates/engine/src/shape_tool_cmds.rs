//! Shape-tool pixel drawing and path-component alignment/stacking.
use crate::commands::{CommandSpec, blend_from_str, has_paintable};
use crate::vector_cmds::{bad, has_doc, layer_id, parse_path, resolve_path, shape_path};
use crate::{Result, Session};
use photocraft_doc::{LayerId, Path};
use photocraft_geom::{Rect, TILE_SIZE};
use serde_json::{Value, json};

fn draw_pixels(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "shape.drawPixels";
    let path = if p.get("kind").and_then(Value::as_str) == Some("path") {
        parse_path(p.get("path").ok_or_else(|| bad(CMD, "Missing path"))?).map_err(|e| bad(CMD, e))?
    } else {
        shape_path(p)?
    };
    photocraft_vector::flatten::validate_shape(&path, None).map_err(|e| bad(CMD, e))?;
    let opacity = match p.get("opacity") {
        None => 1.0,
        Some(v) => v.as_f64().filter(|v| (0.0..=100.0).contains(v)).ok_or_else(|| bad(CMD, "Opacity must be 0..100"))? as f32 / 100.0,
    };
    let mode = blend_from_str(string(p, "mode", "normal", CMD)?).ok_or_else(|| bad(CMD, "Unknown blend mode"))?;
    if let Some(target) = p.get("target") {
        let valid = target.as_str().is_some_and(|s| matches!(s, "pixels" | "mask" | "quickMask"))
            || target.get("channel").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).is_some();
        if !valid {
            return Err(bad(CMD, "Invalid paint target"));
        }
    }
    let aa = match p.get("antiAlias") {
        None => true,
        Some(v) => v.as_bool().ok_or_else(|| bad(CMD, "antiAlias must be boolean"))?,
    };
    let color = match p.get("color") {
        None => s.tools.foreground,
        Some(v) => {
            let c = crate::vector_cmds::color(v).ok_or_else(|| bad(CMD, "Invalid color"))?;
            let rgb = c.to_rgb();
            [rgb[0], rgb[1], rgb[2], c.alpha]
        }
    };
    if color.iter().any(|c| !c.is_finite()) {
        return Err(bad(CMD, "Color must be finite"));
    }
    let damage = s.edit("Draw Pixel Shape", |doc, active| {
        let raster = photocraft_vector::fill_rasterizer(&path, photocraft_vector::DEFAULT_TOLERANCE);
        let area = raster.pixel_bounds().unwrap_or(Rect::EMPTY).intersect(&doc.bounds());
        let selection = doc.selection.clone();
        let id = match p.get("layer") {
            Some(v) => Some(LayerId(v.as_u64().ok_or_else(|| bad(CMD, "Invalid layer id"))?)),
            None => *active,
        };
        if matches!(crate::channel_cmds::target_of(p), crate::channel_cmds::Target::Pixels | crate::channel_cmds::Target::Mask)
            && id.is_some_and(|id| doc.effective_locks(id).all)
        {
            return Err(bad(CMD, "Layer is locked"));
        }
        let (surface, lock) = crate::channel_cmds::target_surface(doc, id, p)?;
        // Match the resampling memory policy, accounting for complete stored tiles.
        // A sparse document can have huge dimensions without allocating any pixels.
        if !area.is_empty() {
            let tiles_x = ((area.x1 - 1).div_euclid(TILE_SIZE) - area.x0.div_euclid(TILE_SIZE) + 1) as u64;
            let tiles_y = ((area.y1 - 1).div_euclid(TILE_SIZE) - area.y0.div_euclid(TILE_SIZE) + 1) as u64;
            let bytes = tiles_x
                .saturating_mul(tiles_y)
                .saturating_mul(TILE_SIZE as u64)
                .saturating_mul(TILE_SIZE as u64)
                .saturating_mul(surface.format().bytes_per_pixel() as u64);
            let limit = if cfg!(target_pointer_width = "64") { 8 << 30 } else { 2 << 30 };
            if bytes > limit {
                return Err(bad(CMD, "Pixel shape exceeds drawing memory limit"));
            }
        }
        // Bounded tile buffers, including for very large documents.
        let step = TILE_SIZE as usize;
        for y in (area.y0..area.y1).step_by(step) {
            for x in (area.x0..area.x1).step_by(step) {
                let tile = Rect::new(x, y, x.saturating_add(TILE_SIZE).min(area.x1), y.saturating_add(TILE_SIZE).min(area.y1));
                let mut cov = raster.render(tile);
                let width = tile.width() as usize;
                for (i, c) in cov.iter_mut().enumerate() {
                    if !aa {
                        *c = f32::from(*c >= 0.5);
                    }
                    if let Some(sel) = &selection {
                        *c *= sel.sample_channel(tile.x0 + (i % width) as i32, tile.y0 + (i / width) as i32, 0);
                    }
                }
                crate::vector_cmds::paint_coverage(surface, tile, &cov, color, opacity, mode, lock);
            }
        }
        surface.prune();
        Ok(area)
    })?;
    Ok(json!({"damage": damage}))
}

fn components(s: &Session, p: &Value, cmd: &str) -> Result<(Path, Vec<usize>)> {
    string(p, "name", "work", cmd)?;
    if p.get("layer").is_some_and(|v| v.as_u64().is_none()) {
        return Err(bad(cmd, "Invalid layer id"));
    }
    let path = if p.get("name").and_then(Value::as_str) == Some("layer") {
        let id = layer_id(s, p)?;
        let layer = s.active().and_then(|st| st.doc.layer(id)).ok_or(crate::EngineError::NoLayer(id))?;
        match &layer.content {
            photocraft_doc::LayerContent::Shape(sh) => sh.path.clone(),
            _ => layer.vector_mask.as_ref().ok_or_else(|| bad(cmd, "Layer has no path"))?.path.clone(),
        }
    } else {
        resolve_path(s, p.get("name").and_then(Value::as_str))?
    };
    photocraft_vector::flatten::validate_shape(&path, None).map_err(|e| bad(cmd, e))?;
    let count = path.components().len();
    let mut selected = match p.get("components") {
        None => (0..count).collect(),
        Some(v) => v
            .as_array()
            .ok_or_else(|| bad(cmd, "components must be an array"))?
            .iter()
            .map(|v| v.as_u64().and_then(|v| usize::try_from(v).ok()).filter(|i| *i < count).ok_or_else(|| bad(cmd, "Invalid component index")))
            .collect::<Result<Vec<_>>>()?,
    };
    selected.sort_unstable();
    selected.dedup();
    if selected.is_empty() {
        return Err(bad(cmd, "No path components selected"));
    }
    Ok((path, selected))
}

fn check_lock(s: &Session, p: &Value, position: bool) -> Result<()> {
    if p.get("name").and_then(Value::as_str) == Some("layer") {
        let id = p.get("layer").and_then(Value::as_u64).map(LayerId).map_or_else(|| layer_id(s, p), Ok)?;
        let locks = s.active().ok_or(crate::EngineError::NoDocument)?.doc.effective_locks(id);
        if locks.all || locks.pixels || (position && locks.position) {
            return Err(bad("path.components", "Layer is locked"));
        }
    }
    Ok(())
}
fn align(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "path.align";
    check_lock(s, p, true)?;
    let (original, selected) = components(s, p, CMD)?;
    let action = p.get("action").and_then(Value::as_str).ok_or_else(|| bad(CMD, "Missing action"))?;
    let canvas = match string(p, "to", "selection", CMD)? {
        "selection" => None,
        "canvas" => {
            let d = &s.active().ok_or(crate::EngineError::NoDocument)?.doc;
            Some([0.0, 0.0, d.size.width as f64, d.size.height as f64])
        }
        _ => return Err(bad(CMD, "to must be selection or canvas")),
    };
    // A complete live shape moves as a whole; retain its editable parameters.
    // Component edits of compound paths still use the general path editor below.
    if p.get("name").and_then(Value::as_str) == Some("layer") && original.components().len() == 1 {
        let id = layer_id(s, p)?;
        let live =
            s.active().and_then(|st| st.doc.layer(id)).is_some_and(|l| matches!(&l.content, photocraft_doc::LayerContent::Shape(sh) if sh.live.is_some()));
        if live {
            let mut aligned = original.clone();
            photocraft_vector::components::align(&mut aligned, &selected, action, canvas).map_err(|e| bad(CMD, e))?;
            photocraft_vector::flatten::validate_shape(&aligned, None).map_err(|e| bad(CMD, e))?;
            let before = original.control_bounds().ok_or_else(|| bad(CMD, "Path has no bounds"))?;
            let after = aligned.control_bounds().ok_or_else(|| bad(CMD, "Path has no bounds"))?;
            let transform = photocraft_geom::Affine::translate(after.0 - before.0, after.1 - before.1);
            let path = crate::vector_cmds::with_shape(s, id, "Align Path Components", |sh, _| {
                crate::vector_cmds::transform_shape(sh, &transform);
                // Keep the translated source geometry, including imported path precision.
                sh.path = aligned;
                Ok(crate::vector_cmds::path_json(&sh.path))
            })?;
            return Ok(json!({"name":"layer","path":path}));
        }
    }
    crate::path_edit_cmds::edit_path(s, CMD, p, "Align Path Components", |path| {
        photocraft_vector::components::align(path, &selected, action, canvas)?;
        photocraft_vector::flatten::validate_shape(path, None).map_err(String::from)
    })
}

fn string<'a>(p: &'a Value, key: &str, default: &'a str, cmd: &str) -> Result<&'a str> {
    match p.get(key) {
        None => Ok(default),
        Some(v) => v.as_str().ok_or_else(|| bad(cmd, format!("{key} must be a string"))),
    }
}
fn arrange(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "path.arrange";
    check_lock(s, p, false)?;
    let (_, selected) = components(s, p, CMD)?;
    let action = p.get("action").and_then(Value::as_str).ok_or_else(|| bad(CMD, "Missing action"))?;
    crate::path_edit_cmds::edit_path(s, CMD, p, "Arrange Path Components", |path| photocraft_vector::components::arrange(path, &selected, action))
}
fn merge(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "path.merge";
    check_lock(s, p, false)?;
    components(s, p, CMD)?;
    let tolerance = match p.get("tolerance") {
        None => 0.1,
        Some(v) => v.as_f64().filter(|v| (0.05..=10.0).contains(v)).ok_or_else(|| bad(CMD, "Invalid merge tolerance"))?,
    };
    crate::path_edit_cmds::edit_path(s, CMD, p, "Merge Shape Components", |path| {
        *path = photocraft_vector::components::merge(path, tolerance, 16_000_000)?;
        Ok(())
    })
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "path.merge",
            label: "Merge Shape Components",
            menu: &[],
            shortcut: None,
            params: r##"{name:work|layer|saved path,layer:id?,tolerance:0.05..10=0.1} (bakes boolean components into outlines; 16M coverage-pixel budget at 4x)"##,
            enabled: has_doc,
            run: merge,
            journal: true,
        },
        CommandSpec {
            id: "shape.drawPixels",
            label: "Draw Pixel Shape",
            menu: &[],
            shortcut: None,
            params: r##"{kind,rect/from/to/path: shape.create geometry,color: foreground,mode: blend mode,opacity: 0..100=100,antiAlias: bool=true,target: pixels|mask|quickMask|{channel:i},layer:id?} (drawing bounds limited to 8 GiB of tiles on 64-bit, 2 GiB on 32-bit)"##,
            enabled: has_paintable,
            run: draw_pixels,
            journal: true,
        },
        CommandSpec {
            id: "path.align",
            label: "Align Path Components",
            menu: &[],
            shortcut: None,
            params: r##"{name:work|layer|saved path,layer:id?,components:[index,...]?=all,action:left|centerX|right|top|centerY|bottom|distributeX|distributeY,to:selection|canvas=selection}"##,
            enabled: has_doc,
            run: align,
            journal: true,
        },
        CommandSpec {
            id: "path.arrange",
            label: "Arrange Path Components",
            menu: &[],
            shortcut: None,
            params: r##"{name:work|layer|saved path,layer:id?,components:[index,...]?=all,action:front|forward|backward|back}"##,
            enabled: has_doc,
            run: arrange,
            journal: true,
        },
    ]
}
