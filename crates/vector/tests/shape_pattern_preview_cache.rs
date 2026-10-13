use std::sync::Arc;

use photocraft_doc::{ColorMode, Fill, Pattern, PixelFormat, SampleType, ShapeLayer, ShapeStroke};
use photocraft_geom::{Affine, Rect};
use photocraft_raster::Surface;
use photocraft_vector::{CompiledShape, DEFAULT_TOLERANCE, pattern::ShapePatterns};

fn pattern(id: &str, name: &str, width: u32, height: u32, rgba: [f32; 4]) -> Pattern {
    Pattern { id: id.into(), name: name.into(), width, height, surface: Surface::with_default(PixelFormat::RGBA32F, &rgba) }
}

fn paint(p: &Pattern) -> Fill {
    Fill::Pattern { id: p.id.clone(), name: p.name.clone(), scale: 1.0, angle: 0.0, phase: (0.0, 0.0), link: true }
}

fn shape(p: &Pattern) -> ShapeLayer {
    ShapeLayer { path: photocraft_vector::shapes::rounded_rect(16.0, 16.0, 64.0, 64.0, [0.0; 4]), fill: Some(paint(p)), ..Default::default() }
}

fn close(actual: [f32; 4], expected: [f32; 4]) {
    for c in 0..4 {
        assert!((actual[c] - expected[c]).abs() < 0.0001, "{actual:?} != {expected:?}");
    }
}

fn preview(shape: &ShapeLayer, source: &[Pattern], mode: ColorMode, prepared: &ShapePatterns, transform: Affine) -> CompiledShape {
    CompiledShape::preview_prepared(shape, DEFAULT_TOLERANCE, Rect::new(0, 0, 256, 256), source, mode, transform, prepared).unwrap()
}

#[test]
fn changed_frames_reuse_one_large_prepared_tile_for_fill_and_stroke() {
    // A sparse source still needs a 16 MiB dense prepared tile. Each changed frame must
    // reference that allocation, rather than merely returning equivalent pixels.
    let p = pattern("large", "Large", 1024, 1024, [0.2, 0.6, 0.9, 0.5]);
    let source = std::slice::from_ref(&p);
    let mut sh = shape(&p);
    sh.stroke = Some(ShapeStroke { width: 4.0, paint: paint(&p), ..Default::default() });
    let prepared = ShapePatterns::new(&sh, source, ColorMode::Rgb);
    let tile = prepared.get(&p.id, &p.name).unwrap();
    assert!(Arc::ptr_eq(&tile, &prepared.get("missing-id", "large").unwrap()));
    let owners = Arc::strong_count(&tile);
    let transform = Affine { m: [0.5, 0.0, 0.0, 0.5, 0.0, 0.0] };
    let mut frames = Vec::new();
    for origin in [16, 24, 32] {
        sh.path = photocraft_vector::shapes::rounded_rect(f64::from(origin), 16.0, 64.0, 64.0, [0.0; 4]);
        frames.push(preview(&sh, source, ColorMode::Rgb, &prepared, transform));
        assert_eq!(Arc::strong_count(&tile), owners + 2 * frames.len());
        assert!(Arc::ptr_eq(&tile, &prepared.get(&p.id, &p.name).unwrap()));
        let rect = Rect::new(origin / 2 + 8, 16, origin / 2 + 9, 17);
        close(frames.last().unwrap().render_rgba(rect)[0], p.surface.rgba(0, 0));
    }
    drop(frames);
    assert_eq!(Arc::strong_count(&tile), owners);
}

fn painted(rgba: [f32; 4], mode: ColorMode) -> [f32; 4] {
    if matches!(mode, ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone | ColorMode::Cmyk) {
        let format = PixelFormat::new(mode, SampleType::F32, true);
        photocraft_raster::to_rgba(&format, &photocraft_raster::from_rgba(&format, rgba))
    } else {
        rgba
    }
}

// The oracle reads source pixels directly, independently of Tile, Paint and Placement.
fn bilinear(p: &Pattern, u: f64, v: f64, mode: ColorMode) -> [f32; 4] {
    let (x, y) = (u - 0.5, v - 0.5);
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = ((x - x.floor()) as f32, (y - y.floor()) as f32);
    let mut premultiplied = [0.0; 4];
    for (dx, dy, weight) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
        let rgba = painted(p.surface.rgba((ix + dx).rem_euclid(p.width as i32), (iy + dy).rem_euclid(p.height as i32)), mode);
        for c in 0..3 {
            premultiplied[c] += rgba[c] * rgba[3] * weight;
        }
        premultiplied[3] += rgba[3] * weight;
    }
    if premultiplied[3] == 0.0 {
        return [0.0; 4];
    }
    [premultiplied[0] / premultiplied[3], premultiplied[1] / premultiplied[3], premultiplied[2] / premultiplied[3], premultiplied[3]]
}

#[test]
fn prepared_previews_rebuild_linked_placement_and_keep_original_pixels_at_every_depth_and_model() {
    let mut p = pattern("colours", "Colours", 2, 2, [0.9, 0.2, 0.1, 1.0]);
    p.surface.fill_rect(Rect::new(1, 0, 2, 1), &[0.1, 0.3, 0.9, 0.5]);
    p.surface.fill_rect(Rect::new(0, 1, 1, 2), &[0.2, 0.8, 0.3, 0.25]);
    p.surface.fill_rect(Rect::new(1, 1, 2, 2), &[0.8, 0.5, 0.2, 0.75]);
    let source = std::slice::from_ref(&p);
    let transform = Affine { m: [0.25, 0.0, 0.0, 0.25, -2.0, -3.0] };
    for mode in [ColorMode::Rgb, ColorMode::Grayscale, ColorMode::Cmyk, ColorMode::Lab] {
        for link in [false, true] {
            let mut sh = shape(&p);
            sh.fill = Some(Fill::Pattern { id: p.id.clone(), name: p.name.clone(), scale: 2.0, angle: 90.0, phase: (3.5, 5.5), link });
            let prepared = ShapePatterns::new(&sh, source, mode);
            for origin in [16, 22] {
                sh.path = photocraft_vector::shapes::rounded_rect(f64::from(origin), 16.0, 64.0, 64.0, [0.0; 4]);
                let compiled = preview(&sh, source, mode, &prepared, transform);
                let region = Rect::new(origin / 4 + 2, 5, origin / 4 + 8, 11);
                for sample in [SampleType::U8, SampleType::U16, SampleType::F32] {
                    let format = PixelFormat::new(mode, sample, true);
                    let actual = compiled.render(format, region);
                    for y in region.y0..region.y1 {
                        for x in region.x0..region.x1 {
                            let (doc_x, doc_y) = ((f64::from(x) + 0.5 + 2.0) * 4.0, (f64::from(y) + 0.5 + 3.0) * 4.0);
                            let (anchor_x, anchor_y) = if link { (f64::from(origin), 16.0) } else { (0.0, 0.0) };
                            // Undo a quarter turn and 2x scale after the original-document anchor.
                            let expected = bilinear(&p, -(doc_y - anchor_y - 5.5) / 2.0, (doc_x - anchor_x - 3.5) / 2.0, mode);
                            let encoded = Surface::with_default(format, &photocraft_raster::from_rgba(&format, expected));
                            close(actual.rgba(x, y), encoded.rgba(0, 0));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn rebuilding_after_same_id_source_or_mode_change_replaces_pixels_and_keeps_old_snapshot() {
    let mut p = pattern("same-id", "Same", 2, 2, [0.9, 0.2, 0.1, 0.5]);
    let sh = shape(&p);
    let old = ShapePatterns::new(&sh, std::slice::from_ref(&p), ColorMode::Rgb);
    let old_tile = old.get(&p.id, &p.name).unwrap();
    p.surface.fill_rect(Rect::new(0, 0, 2, 2), &[0.1, 0.3, 0.9, 0.75]);
    let changed = ShapePatterns::new(&sh, std::slice::from_ref(&p), ColorMode::Rgb);
    let changed_tile = changed.get(&p.id, &p.name).unwrap();
    assert!(!Arc::ptr_eq(&old_tile, &changed_tile));
    close(old_tile.sample(0.5, 0.5), [0.9, 0.2, 0.1, 0.5]);
    close(changed_tile.sample(0.5, 0.5), p.surface.rgba(0, 0));
    let gray = ShapePatterns::new(&sh, std::slice::from_ref(&p), ColorMode::Grayscale);
    let gray_tile = gray.get(&p.id, &p.name).unwrap();
    assert!(!Arc::ptr_eq(&changed_tile, &gray_tile));
    close(gray_tile.sample(0.5, 0.5), painted(p.surface.rgba(0, 0), ColorMode::Grayscale));
    let transform = Affine { m: [0.5, 0.0, 0.0, 0.5, 0.0, 0.0] };
    let region = Rect::new(16, 16, 17, 17);
    close(preview(&sh, std::slice::from_ref(&p), ColorMode::Rgb, &changed, transform).render_rgba(region)[0], p.surface.rgba(0, 0));
    // Passing a cache of another mode must use the requested document mode, not stale RGB.
    close(
        preview(&sh, std::slice::from_ref(&p), ColorMode::Grayscale, &changed, transform).render_rgba(region)[0],
        painted(p.surface.rgba(0, 0), ColorMode::Grayscale),
    );
}

#[test]
fn name_resolved_patterns_with_empty_ids_keep_distinct_reusable_tiles() {
    let first = pattern("", "First", 2, 2, [0.9, 0.1, 0.2, 1.0]);
    let second = pattern("", "Second", 2, 2, [0.1, 0.2, 0.9, 1.0]);
    let mut sh = shape(&first);
    sh.stroke = Some(ShapeStroke { width: 4.0, paint: paint(&second), ..Default::default() });
    let source = [first, second];
    let prepared = ShapePatterns::new(&sh, &source, ColorMode::Rgb);
    let fill_tile = prepared.get("", "First").unwrap();
    let stroke_tile = prepared.get("", "Second").unwrap();
    assert!(!Arc::ptr_eq(&fill_tile, &stroke_tile));
    close(fill_tile.sample(0.5, 0.5), source[0].surface.rgba(0, 0));
    close(stroke_tile.sample(0.5, 0.5), source[1].surface.rgba(0, 0));
    let owners = (Arc::strong_count(&fill_tile), Arc::strong_count(&stroke_tile));
    let compiled = preview(&sh, &source, ColorMode::Rgb, &prepared, Affine { m: [0.5, 0.0, 0.0, 0.5, 0.0, 0.0] });
    assert_eq!(Arc::strong_count(&fill_tile), owners.0 + 1);
    assert_eq!(Arc::strong_count(&stroke_tile), owners.1 + 1);
    close(compiled.render_rgba(Rect::new(16, 16, 17, 17))[0], source[0].surface.rgba(0, 0));
    close(compiled.render_rgba(Rect::new(8, 16, 9, 17))[0], source[1].surface.rgba(0, 0));
}

#[test]
fn duplicate_ids_keep_document_source_order_when_fill_resolves_the_later_name_first() {
    let first = pattern("duplicate", "First", 2, 2, [0.9, 0.1, 0.2, 1.0]);
    let second = pattern("duplicate", "Second", 2, 2, [0.1, 0.2, 0.9, 1.0]);
    let mut sh = shape(&second);
    sh.fill = Some(Fill::Pattern { id: "missing-id".into(), name: second.name.clone(), scale: 1.0, angle: 0.0, phase: (0.0, 0.0), link: true });
    sh.stroke = Some(ShapeStroke { width: 4.0, paint: paint(&first), ..Default::default() });
    let source = [first, second];
    let prepared = ShapePatterns::new(&sh, &source, ColorMode::Rgb);
    let fill_tile = prepared.get("missing-id", "Second").unwrap();
    let stroke_tile = prepared.get("duplicate", "First").unwrap();
    assert!(!Arc::ptr_eq(&fill_tile, &stroke_tile));
    close(fill_tile.sample(0.5, 0.5), source[1].surface.rgba(0, 0));
    close(stroke_tile.sample(0.5, 0.5), source[0].surface.rgba(0, 0));
    let compiled = preview(&sh, &source, ColorMode::Rgb, &prepared, Affine { m: [0.5, 0.0, 0.0, 0.5, 0.0, 0.0] });
    close(compiled.render_rgba(Rect::new(16, 16, 17, 17))[0], source[1].surface.rgba(0, 0));
    close(compiled.render_rgba(Rect::new(8, 16, 9, 17))[0], source[0].surface.rgba(0, 0));
}

#[test]
fn oversized_prepared_patterns_share_cow_pixels_and_missing_patterns_are_transparent() {
    let mut p = pattern("oversized", "Oversized", 4096, 4096, [0.2, 0.6, 0.9, 0.5]);
    let sh = shape(&p);
    let prepared = ShapePatterns::new(&sh, std::slice::from_ref(&p), ColorMode::Rgb);
    let tile = prepared.get(&p.id, &p.name).unwrap();
    assert!(Arc::ptr_eq(&tile, &prepared.get(&p.id, &p.name).unwrap()));
    p.surface.fill_rect(Rect::new(4000, 2000, 4001, 2001), &[0.9, 0.1, 0.2, 1.0]);
    close(tile.sample(4000.5, 2000.5), [0.2, 0.6, 0.9, 0.5]);
    let changed = ShapePatterns::new(&sh, std::slice::from_ref(&p), ColorMode::Rgb);
    close(changed.get(&p.id, &p.name).unwrap().sample(4000.5, 2000.5), p.surface.rgba(4000, 2000));
    let missing = ShapePatterns::new(&sh, &[], ColorMode::Rgb);
    assert!(missing.get(&p.id, &p.name).is_none());
    let compiled = preview(&sh, &[], ColorMode::Rgb, &missing, Affine { m: [0.5, 0.0, 0.0, 0.5, 0.0, 0.0] });
    assert_eq!(compiled.render_rgba(Rect::new(16, 16, 17, 17)), vec![[0.0; 4]]);
}
