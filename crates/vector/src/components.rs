//! Alignment and stacking of complete path components (including their holes).
use photocraft_doc::Path;
use photocraft_geom::Affine;

fn groups(path: &Path) -> Vec<Path> {
    path.components()
        .into_iter()
        .map(|r| Path { subpaths: path.subpaths.get(r).unwrap_or_default().to_vec(), fill_rule: path.fill_rule, inverted: path.inverted })
        .collect()
}

/// Bake boolean components into plain outlines. Caller supplies its coverage allocation budget.
pub fn merge(path: &Path, tolerance: f64, max_pixels: u64) -> Result<Path, String> {
    if !tolerance.is_finite() || !(0.05..=10.0).contains(&tolerance) {
        return Err("Merge tolerance must be 0.05..10".into());
    }
    let scale = 4.0;
    let up = path.transform(&Affine::scale(scale));
    let raster = crate::fill_rasterizer(&up, crate::DEFAULT_TOLERANCE);
    let Some(bounds) = raster.pixel_bounds() else {
        let mut empty = path.clone();
        empty.subpaths.clear();
        return Ok(empty);
    };
    let bounds = bounds.inflate(2);
    if u64::from(bounds.width()) * u64::from(bounds.height()) > max_pixels {
        return Err("Path too large to merge".into());
    }
    let mask = raster.render(bounds);
    Ok(crate::trace::trace_mask(&mask, bounds, 0.0, tolerance * scale).transform(&Affine::scale(1.0 / scale)))
}

/// Align component bounds to the canvas or the combined selected bounds; distribution uses centers.
pub fn align(path: &mut Path, selected: &[usize], action: &str, canvas: Option<[f64; 4]>) -> Result<(), String> {
    let mut parts = groups(path);
    let mut items = Vec::new();
    for &i in selected {
        let part = parts.get(i).ok_or("No such path component")?;
        let b = part.control_bounds().ok_or("Path component has no bounds")?;
        items.push((i, [b.0, b.1, b.2, b.3]));
    }
    if items.is_empty() {
        return Err("Select path components to align".into());
    }
    let target = canvas.unwrap_or_else(|| {
        items.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |a, (_, b)| {
            [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
        })
    });
    if target.iter().any(|v| !v.is_finite()) {
        return Err("Non-finite alignment bounds".into());
    }
    let distribute = matches!(action, "distributeX" | "distributeY");
    let axis = usize::from(action == "distributeY");
    if distribute {
        if items.len() < 3 {
            return Err("Distribution needs at least three components".into());
        }
        items.sort_by(|a, b| ((a.1[axis] + a.1[axis + 2]) * 0.5).total_cmp(&((b.1[axis] + b.1[axis + 2]) * 0.5)));
    } else if !matches!(action, "left" | "centerX" | "right" | "top" | "centerY" | "bottom") {
        return Err("Unknown alignment".into());
    }
    let center = |b: [f64; 4]| (b[axis] + b[axis + 2]) * 0.5;
    let first = items.first().map_or(0.0, |(_, b)| center(*b));
    let last = items.last().map_or(first, |(_, b)| center(*b));
    let count = items.len();
    for (n, (i, b)) in items.into_iter().enumerate() {
        let (dx, dy) = match action {
            "left" => (target[0] - b[0], 0.0),
            "centerX" => ((target[0] + target[2] - b[0] - b[2]) * 0.5, 0.0),
            "right" => (target[2] - b[2], 0.0),
            "top" => (0.0, target[1] - b[1]),
            "centerY" => (0.0, (target[1] + target[3] - b[1] - b[3]) * 0.5),
            "bottom" => (0.0, target[3] - b[3]),
            _ => {
                let d = first + (last - first) * n as f64 / (count - 1) as f64 - center(b);
                if axis == 0 { (d, 0.0) } else { (0.0, d) }
            }
        };
        if let Some(part) = parts.get_mut(i) {
            *part = part.transform(&Affine::translate(dx, dy));
        }
    }
    path.subpaths = parts.into_iter().flat_map(|p| p.subpaths).collect();
    Ok(())
}

/// Move selected components, retaining their relative order and boolean operations.
pub fn arrange(path: &mut Path, selected: &[usize], action: &str) -> Result<(), String> {
    let mut parts = groups(path);
    if selected.is_empty() || selected.iter().any(|i| *i >= parts.len()) {
        return Err("Select valid path components to arrange".into());
    }
    let mut marked: Vec<_> = (0..parts.len()).map(|i| selected.contains(&i)).collect();
    match action {
        "front" | "back" => {
            let mut pairs: Vec<_> = parts.into_iter().zip(marked).collect();
            pairs.sort_by_key(|(_, selected)| if action == "front" { *selected } else { !*selected });
            parts = pairs.into_iter().map(|(p, _)| p).collect();
        }
        "forward" => {
            for i in (0..parts.len().saturating_sub(1)).rev() {
                if marked.get(i) == Some(&true) && marked.get(i + 1) == Some(&false) {
                    parts.swap(i, i + 1);
                    marked.swap(i, i + 1);
                }
            }
        }
        "backward" => {
            for i in 1..parts.len() {
                if marked.get(i) == Some(&true) && marked.get(i - 1) == Some(&false) {
                    parts.swap(i, i - 1);
                    marked.swap(i, i - 1);
                }
            }
        }
        _ => return Err("Unknown arrangement".into()),
    }
    path.subpaths = parts.into_iter().flat_map(|p| p.subpaths).collect();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{PathOp, Subpath};
    #[test]
    fn holes_stay_with_components_when_arranging_and_aligning() {
        let mut hole = Subpath::polygon(&[(2.0, 2.0), (8.0, 2.0), (8.0, 8.0)]);
        hole.op = PathOp::Join;
        let mut p =
            Path::new(vec![Subpath::polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]), hole, Subpath::polygon(&[(30.0, 20.0), (40.0, 20.0), (40.0, 30.0)])]);
        let original = p.clone();
        arrange(&mut p, &[0], "front").unwrap();
        assert_eq!(p.subpaths[2], original.subpaths[1]);
        assert_eq!(p.components(), vec![0..1, 1..3]);
        align(&mut p, &[1], "right", Some([0.0, 0.0, 100.0, 100.0])).unwrap();
        assert_eq!(p.subpaths[1].knots[1].anchor.x, 100.0);
        assert_eq!(p.subpaths[2].knots[0].anchor.x, 92.0);
        let before = p.clone();
        assert!(arrange(&mut p, &[usize::MAX], "front").is_err());
        assert_eq!(p, before);
    }
}
