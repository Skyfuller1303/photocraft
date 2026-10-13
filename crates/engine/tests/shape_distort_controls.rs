use photocraft_algo::transform::Homography;
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Fill, FillRule, GradientStyle, LayerContent, LayerId, LayerMask, Path, PathOp, ShapeLayer, ShapeStroke, Subpath, VectorMask};
use photocraft_engine::{Session, transform_cmds::preview_shape_quad_transform, vector_cmds::shape_paint_params};
use photocraft_geom::{Point, Rect};
use photocraft_raster::Surface;
use serde_json::json;

const FRAME: [f64; 4] = [20.0, 20.0, 100.0, 80.0];
const QUAD: [[f64; 2]; 4] = [[30.0, 18.0], [110.0, 28.0], [94.0, 96.0], [12.0, 80.0]];

fn session(depth: u32, kind: &str) -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":140,"height":120,"depth":depth,"background":"transparent"})).unwrap();
    let id = LayerId(s.execute("shape.create", json!({"kind":kind,"rect":[20,20,80,60],"fill":"#fdca22"})).unwrap()["layer"].as_u64().unwrap());
    (s, id)
}

fn shape(s: &Session, id: LayerId) -> &ShapeLayer {
    let LayerContent::Shape(shape) = &s.active().unwrap().doc.layer(id).unwrap().content else { panic!("shape stays vector") };
    shape
}

fn distance_to_segment(p: (f64, f64), a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let denominator = dx * dx + dy * dy;
    let t = if denominator == 0.0 { 0.0 } else { ((p.0 - a.x) * dx + (p.1 - a.y) * dy) / denominator }.clamp(0.0, 1.0);
    (p.0 - a.x - t * dx).hypot(p.1 - a.y - t * dy)
}

#[test]
fn distorted_rectangle_keeps_exact_corners_paint_and_one_undo_at_every_depth() {
    for depth in [8, 16, 32] {
        let (mut s, id) = session(depth, "rect");
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        let preview = preview_shape_quad_transform(&before, id, FRAME, QUAD, "bicubic").unwrap();
        assert_eq!(s.active().unwrap().doc, before, "preview never edits the session");
        assert_eq!(s.active().unwrap().history.past_len(), history);
        s.execute("edit.transform", json!({"layer":id.0,"rect":FRAME,"quad":QUAD})).unwrap();
        let after = shape(&s, id);
        assert!(after.live.is_none());
        assert_eq!(after.path.subpaths[0].knots.len(), 4);
        let h = Homography::rect_to_quad(FRAME, QUAD).unwrap();
        let LayerContent::Shape(original) = &before.layer(id).unwrap().content else { panic!() };
        for (a, b) in original.path.subpaths[0].knots.iter().zip(&after.path.subpaths[0].knots) {
            let expected = h.apply(a.anchor.x, a.anchor.y);
            assert!((b.anchor.x - expected.0).hypot(b.anchor.y - expected.1) < 1e-10);
            assert_eq!(b.anchor, b.in_ctrl);
            assert_eq!(b.anchor, b.out_ctrl);
        }
        assert_eq!(after.fill, original.fill);
        assert!(after.cache.as_ref().unwrap().rgba(60, 50)[3] > 0.99);
        assert_eq!(s.active().unwrap().doc, std::sync::Arc::new(preview));
        assert_eq!(s.active().unwrap().history.past_len(), history + 1);
        let committed = s.active().unwrap().doc.clone();
        assert!(s.undo());
        assert_eq!(s.active().unwrap().doc, before);
        assert!(s.redo());
        assert_eq!(s.active().unwrap().doc, committed);
    }
}

#[test]
fn projected_curves_meet_destination_tolerance_and_keep_paint_and_path_operations() {
    for depth in [8, 16, 32] {
        let (mut s, id) = session(depth, "ellipse");
        let pattern = s.patterns.items[0].clone();
        let fill = Fill::Gradient {
            stops: vec![(0.0, Color::rgba(0.1, 0.2, 0.9, 1.0)), (1.0, Color::rgba(0.9, 0.7, 0.1, 1.0))],
            angle: 37.0,
            scale: 0.8,
            style: GradientStyle::Linear,
            reverse: true,
            opacity_stops: vec![],
            midpoints: vec![0.3],
            offset: (-0.1, 0.2),
            dither: true,
            align: false,
        };
        let stroke = ShapeStroke {
            paint: Fill::Pattern { name: pattern.name.clone(), id: pattern.id.clone(), scale: 1.3, angle: 22.0, link: false, phase: (3.0, -4.0) },
            opacity: 0.7,
            width: 2.0,
            ..Default::default()
        };
        let mut paint = shape_paint_params(Some(&fill), Some(&stroke));
        paint["layer"] = json!(id.0);
        s.execute("shape.edit", paint).unwrap();
        s.edit("compound path", |doc, _| {
            let LayerContent::Shape(sh) = &mut doc.layer_mut(id).unwrap().content else { panic!() };
            sh.path.fill_rule = FillRule::EvenOdd;
            sh.path.inverted = true;
            sh.path.subpaths.push(Subpath::polygon(&[(45.0, 40.0), (65.0, 40.0), (65.0, 60.0), (45.0, 60.0)]).with_op(PathOp::Subtract));
            Ok(())
        })
        .unwrap();
        let source = shape(&s, id).clone();
        s.execute("edit.transform", json!({"rect":FRAME,"quad":QUAD})).unwrap();
        let projected = shape(&s, id);
        assert_eq!(projected.fill, Some(fill));
        assert_eq!(projected.stroke, Some(stroke));
        assert_eq!(projected.path.fill_rule, source.path.fill_rule);
        assert_eq!(projected.path.inverted, source.path.inverted);
        assert_eq!(projected.path.subpaths.len(), source.path.subpaths.len());
        assert!(projected.live.is_none());
        assert!(projected.path.subpaths.iter().map(|p| p.knots.len()).sum::<usize>() <= 4096);
        let h = Homography::rect_to_quad(FRAME, QUAD).unwrap();
        for (original, actual) in source.path.subpaths.iter().zip(&projected.path.subpaths) {
            assert_eq!((actual.closed, actual.op), (original.closed, original.op));
            let edges: Vec<_> = actual.segments().into_iter().map(|s| (s[0], s[3])).collect();
            // Sample the original cubic independently of the production flattener.
            for [p0, c1, c2, p1] in original.segments() {
                for i in 0..=1024 {
                    let t = f64::from(i) / 1024.0;
                    let u = 1.0 - t;
                    let x = u.powi(3) * p0.x + 3.0 * u.powi(2) * t * c1.x + 3.0 * u * t.powi(2) * c2.x + t.powi(3) * p1.x;
                    let y = u.powi(3) * p0.y + 3.0 * u.powi(2) * t * c1.y + 3.0 * u * t.powi(2) * c2.y + t.powi(3) * p1.y;
                    let q = h.apply(x, y);
                    let error = edges.iter().map(|&(a, b)| distance_to_segment(q, a, b)).fold(f64::INFINITY, f64::min);
                    assert!(error <= 0.250001, "depth {depth}, projected curve error {error}");
                }
            }
        }
        assert!(projected.cache.as_ref().unwrap().tiles().next().is_some());
        let preserved_path = projected.path.clone();
        s.execute("shape.edit", json!({"layer":id.0,"fill":null})).unwrap();
        assert_eq!(shape(&s, id).path, preserved_path, "paint editing never regenerates the former live ellipse");
    }
}

#[test]
fn linked_and_unlinked_pixel_and_vector_masks_follow_the_shape_controls() {
    for (depth, sample) in [(8, SampleType::U8), (16, SampleType::U16), (32, SampleType::F32)] {
        for linked in [false, true] {
            let (mut s, id) = session(depth, "rect");
            s.edit("masks", |doc, _| {
                let l = doc.layer_mut(id).unwrap();
                let mut surface = Surface::with_default(PixelFormat::new(ColorMode::Grayscale, sample, false), &[1.0]);
                surface.fill_rect(Rect::new(30, 30, 50, 60), &[0.0]);
                l.mask = Some(LayerMask { surface, enabled: true, linked, density: 0.6, feather: 2.0 });
                let mut vector = VectorMask::new(Path::new(vec![Subpath::polygon(&[(20.0, 20.0), (65.0, 20.0), (65.0, 80.0), (20.0, 80.0)])]));
                vector.linked = linked;
                vector.density = 0.7;
                vector.feather = 1.0;
                l.vector_mask = Some(vector);
                Ok(())
            })
            .unwrap();
            let before = s.active().unwrap().doc.clone();
            let preview = preview_shape_quad_transform(&before, id, FRAME, QUAD, "nearest").unwrap();
            s.execute("edit.transform", json!({"rect":FRAME,"quad":QUAD,"interpolation":"nearest"})).unwrap();
            assert_eq!(s.active().unwrap().doc.as_ref(), &preview);
            let original = before.layer(id).unwrap();
            let actual = s.active().unwrap().doc.layer(id).unwrap();
            let old_mask = original.mask.as_ref().unwrap();
            let mask = actual.mask.as_ref().unwrap();
            assert_eq!((mask.enabled, mask.linked, mask.density, mask.feather), (old_mask.enabled, linked, old_mask.density, old_mask.feather));
            assert_eq!(mask.surface.format(), old_mask.surface.format());
            assert_eq!(mask.surface.default_pixel(), old_mask.surface.default_pixel());
            let old_vector = original.vector_mask.as_ref().unwrap();
            let vector = actual.vector_mask.as_ref().unwrap();
            assert_eq!((vector.enabled, vector.linked, vector.density, vector.feather), (old_vector.enabled, linked, old_vector.density, old_vector.feather));
            if linked {
                let h = Homography::rect_to_quad(FRAME, QUAD).unwrap();
                let q = h.apply(40.5, 45.5);
                assert!(mask.surface.sample_channel(q.0.floor() as i32, q.1.floor() as i32, 0) < 0.01);
                for (a, b) in old_vector.path.subpaths[0].knots.iter().zip(&vector.path.subpaths[0].knots) {
                    let q = h.apply(a.anchor.x, a.anchor.y);
                    assert!((b.anchor.x - q.0).hypot(b.anchor.y - q.1) < 1e-10);
                }
                assert_ne!(vector.path, old_vector.path);
            } else {
                assert_eq!(mask, old_mask);
                assert_eq!(vector, old_vector);
            }
            assert!(s.undo());
            assert_eq!(s.active().unwrap().doc, before);
        }
    }
}

fn unchanged_after_error(s: &mut Session, params: serde_json::Value) -> String {
    let before = s.active().unwrap().doc.clone();
    let revision = s.active().unwrap().revision;
    let history = s.active().unwrap().history.past_len();
    let error = s.execute("edit.transform", params).unwrap_err().to_string();
    assert_eq!(s.active().unwrap().doc, before);
    assert_eq!(s.active().unwrap().revision, revision);
    assert_eq!(s.active().unwrap().history.past_len(), history);
    error
}

#[test]
fn projection_horizon_precision_budget_and_locks_fail_atomically() {
    let (mut empty, id) = session(8, "rect");
    empty
        .edit("empty path", |doc, _| {
            let LayerContent::Shape(shape) = &mut doc.layer_mut(id).unwrap().content else { panic!() };
            shape.path = Path::default();
            shape.live = None;
            shape.cache = None;
            Ok(())
        })
        .unwrap();
    let original = empty.active().unwrap().doc.clone();
    let mut nonfinite = QUAD;
    nonfinite[0][0] = f64::NAN;
    assert!(preview_shape_quad_transform(&original, id, FRAME, nonfinite, "bicubic").is_err());
    assert_eq!(empty.active().unwrap().doc, original);
    assert!(
        unchanged_after_error(&mut empty, json!({"rect":FRAME,"quad":[[0,0],[1e308,0],[9e307,1e308],[0,1e308]]})).contains("finite"),
        "empty paths still reject nonfinite homographies produced by extreme JSON coordinates"
    );
    let (mut s, _) = session(8, "ellipse");
    for quad in [
        json!([[20, 20], [100, 80], [100, 20], [20, 80]]),
        json!([[0, 0], [1e100, 0], [9e99, 1e100], [0, 1e100]]),
        json!([[0, 0], [600000, 0], [500000, 600000], [0, 600000]]),
    ] {
        unchanged_after_error(&mut s, json!({"rect":FRAME,"quad":quad}));
    }
    s.execute("layer.setProps", json!({"locks":{"position":true}})).unwrap();
    assert!(unchanged_after_error(&mut s, json!({"rect":FRAME,"quad":QUAD})).contains("locked"));
    s.execute("layer.setProps", json!({"locks":{"position":false}})).unwrap();
    s.edit("many curves", |doc, active| {
        let LayerContent::Shape(sh) = &mut doc.layer_mut(active.unwrap()).unwrap().content else { panic!() };
        sh.path.subpaths = vec![sh.path.subpaths[0].clone(); 128];
        Ok(())
    })
    .unwrap();
    assert!(
        unchanged_after_error(&mut s, json!({"rect":FRAME,"quad":[[20,20],[1500,20],[1400,1200],[20,1100]]})).contains("4096"),
        "reject projected knot growth before raster allocation"
    );
    let (mut s, id) = session(8, "rect");
    s.edit("sparse mask", |doc, _| {
        let mut surface = Surface::new(PixelFormat::GRAY8);
        surface.write_pixel(0, 0, &[1.0]);
        surface.write_pixel(100_000, 100_000, &[1.0]);
        doc.layer_mut(id).unwrap().mask = Some(LayerMask { surface, enabled: true, linked: true, density: 1.0, feather: 0.0 });
        Ok(())
    })
    .unwrap();
    assert!(unchanged_after_error(&mut s, json!({"rect":FRAME,"quad":QUAD})).contains("mask source"));
    s.edit("small mask", |doc, _| {
        let mut surface = Surface::with_default(PixelFormat::GRAY8, &[1.0]);
        surface.fill_rect(Rect::new(30, 30, 50, 60), &[0.0]);
        doc.layer_mut(id).unwrap().mask.as_mut().unwrap().surface = surface;
        Ok(())
    })
    .unwrap();
    assert!(
        unchanged_after_error(&mut s, json!({"rect":FRAME,"quad":[[20,20],[40000,20],[35000,35000],[20,35000]]})).contains("projected mask"),
        "reject a huge warped mask before allocating destination pixels"
    );
    s.edit("mask across horizon", |doc, _| {
        let mut surface = Surface::with_default(PixelFormat::GRAY8, &[1.0]);
        surface.write_pixel(0, 0, &[0.0]);
        surface.write_pixel(200, 200, &[0.0]);
        doc.layer_mut(id).unwrap().mask.as_mut().unwrap().surface = surface;
        Ok(())
    })
    .unwrap();
    assert!(
        unchanged_after_error(&mut s, json!({"rect":FRAME,"quad":[[20,20],[100,20],[140,80],[-20,80]]})).contains("horizon"),
        "a valid shape quad must still reject a linked mask that crosses its horizon"
    );
}

#[test]
#[ignore = "release measurement; 36MP full-resolution projective shape preview, no GPU or presentation"]
fn shape_distort_preview_36mp_bench() {
    use std::time::Instant;
    let mut s = Session::new();
    s.execute("file.new", json!({"width":7200,"height":5000,"background":"transparent"})).unwrap();
    let id = LayerId(s.execute("shape.create", json!({"rect":[800,400,4000,4000],"fill":"#fdca22"})).unwrap()["layer"].as_u64().unwrap());
    let source = s.active().unwrap().doc.clone();
    let frame = [800.0, 400.0, 4800.0, 4400.0];
    for (n, quad) in
        [[[820.0, 400.0], [4810.0, 440.0], [4790.0, 4450.0], [760.0, 4380.0]], [[880.0, 380.0], [4860.0, 470.0], [4760.0, 4490.0], [710.0, 4330.0]]]
            .into_iter()
            .enumerate()
    {
        let started = Instant::now();
        let preview = preview_shape_quad_transform(&source, id, frame, quad, "bicubic").unwrap();
        let preview_ms = started.elapsed().as_secs_f64() * 1000.0;
        let LayerContent::Shape(shape) = &preview.layer(id).unwrap().content else { panic!() };
        assert!(shape.live.is_none());
        assert_eq!(shape.path.subpaths[0].knots.len(), 4);
        assert!((shape.path.subpaths[0].knots[0].anchor.x - quad[0][0]).abs() < 1e-9);
        assert!((shape.path.subpaths[0].knots[0].anchor.y - quad[0][1]).abs() < 1e-9);
        assert!(shape.cache.as_ref().unwrap().tiles().next().is_some());
        let started = Instant::now();
        let composite = photocraft_compose::render(&preview, preview.bounds());
        let compose_ms = started.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(composite);
        println!(
            "shape_distort36mp changed_frame={n} project_and_raster_ms={preview_ms:.3} cpu_compose_36mp_ms={compose_ms:.3}; GPU/upload/presentation excluded"
        );
        assert_eq!(s.active().unwrap().doc, source);
    }
}
