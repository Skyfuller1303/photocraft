use photocraft_doc::{ColorMode, LayerContent, LayerMask, PixelFormat, SampleType};
use photocraft_engine::Session;
use photocraft_engine::transform_cmds::{PreparedShapeMask, preview_shape_transform, preview_shape_transform_geometry_clipped};
use photocraft_geom::{Affine, Rect};
use photocraft_raster::Surface;
use serde_json::json;

#[test]
fn prepared_shape_mask_matches_exact_preview_formats_defaults_and_downscales() {
    for (depth, sample) in [(8, SampleType::U8), (16, SampleType::U16), (32, SampleType::F32)] {
        for default in [0.0, 0.35, 1.0] {
            let mut session = Session::new();
            session.execute("file.new", json!({"width":128,"height":128,"depth":depth})).unwrap();
            session.execute("shape.create", json!({"kind":"rect","rect":[8,8,64,64],"fill":"#ff0000"})).unwrap();
            let id = session.active().unwrap().active_layer.unwrap();
            session
                .edit("mask", |doc, _| {
                    let mut surface = Surface::with_default(PixelFormat::new(ColorMode::Grayscale, sample, false), &[default]);
                    surface.fill_rect(Rect::new(12, 16, 52, 56), &[0.2]);
                    surface.fill_rect(Rect::new(28, 24, 40, 44), &[0.8]);
                    doc.layer_mut(id).unwrap().mask = Some(LayerMask { surface, enabled: true, linked: true, density: 0.47, feather: 5.5 });
                    Ok(())
                })
                .unwrap();
            let source = session.active().unwrap().doc.clone();
            let mask = source.layer(id).unwrap().mask.as_ref().unwrap();
            let prepared = PreparedShapeMask::new(&mask.surface).unwrap();
            assert!(preview_shape_transform_geometry_clipped(&source, id, Affine::IDENTITY, "bicubic", Rect::new(0, 0, 2049, 2049), Some(&prepared)).is_err());
            let clip = Rect::new(0, 0, 96, 96);
            for interpolation in ["nearest", "bilinear", "bicubic"] {
                for affine in [
                    Affine { m: [1.2, 0.3, -0.3, 1.2, 7.0, -5.0] },
                    Affine { m: [0.125, 0.0, 0.0, 0.125, 24.0, 30.0] },
                    Affine { m: [0.125, 0.0, 0.0, 0.125, 30.0, 32.0] },
                    Affine { m: [0.3, 0.0, 0.0, 0.3, 24.0, 30.0] },
                    Affine { m: [0.75, 0.0, 0.0, 0.75, 7.0, -5.0] },
                ] {
                    let exact = preview_shape_transform(&source, id, affine, interpolation).unwrap();
                    let shown = preview_shape_transform_geometry_clipped(&source, id, affine, interpolation, clip, Some(&prepared)).unwrap();
                    let actual = shown.layer(id).unwrap().mask.as_ref().unwrap();
                    let expected = exact.layer(id).unwrap().mask.as_ref().unwrap();
                    assert_eq!(actual.surface.format(), mask.surface.format());
                    assert_eq!((actual.enabled, actual.linked, actual.density, actual.feather), (mask.enabled, mask.linked, mask.density, mask.feather));
                    assert_eq!(actual.surface.default_pixel(), mask.surface.default_pixel());
                    assert_eq!(
                        actual.surface.read_region(clip),
                        expected.surface.read_region(clip),
                        "depth {depth}, default {default}, {interpolation}, {affine:?}"
                    );
                    assert!(clip.contains_rect(&actual.surface.content_bounds()));
                    assert!(actual.surface.tiles().count() <= clip.tiles().count());
                    assert!(matches!(&shown.layer(id).unwrap().content,LayerContent::Shape(shape) if shape.cache.is_none()));
                }
            }
        }
    }
}

#[test]
fn prepared_mask_source_is_immutable_and_capped() {
    let mut mask = Surface::with_default(PixelFormat::new(ColorMode::Grayscale, SampleType::U16, false), &[1.0]);
    mask.fill_rect(Rect::new(8, 8, 12, 12), &[0.0]);
    let prepared = PreparedShapeMask::new(&mask).unwrap();
    mask.fill_rect(Rect::new(8, 8, 12, 12), &[1.0]);
    let clip = Rect::new(0, 0, 64, 64);
    let affine = Affine { m: [10_000.0, 0.0, 0.0, 10_000.0, -99_968.0, -99_968.0] };
    let output = prepared.warp_clipped(affine, "bicubic", clip).unwrap();
    assert_eq!(output.pixel(32, 32), vec![0.0]);
    assert_eq!(output.default_pixel(), vec![1.0]);
    assert!(clip.contains_rect(&output.content_bounds()));
    assert!(output.tiles().count() <= clip.tiles().count());
    let mut sparse = Surface::new(PixelFormat::GRAY8);
    sparse.write_pixel(0, 0, &[1.0]);
    sparse.write_pixel(10_000, 10_000, &[1.0]);
    assert!(PreparedShapeMask::new(&sparse).is_err(), "reject before reading an enormous sparse bounding box");
    assert!(PreparedShapeMask::new(&Surface::new(PixelFormat::RGBA8)).is_err());
    let rejected = prepared.warp_clipped(Affine::IDENTITY, "bicubic", Rect::new(0, 0, 2049, 2049)).unwrap();
    assert!(rejected.tiles().next().is_none());
    assert_eq!(rejected.default_pixel(), vec![1.0]);
}
