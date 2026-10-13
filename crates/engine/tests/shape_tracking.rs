//! Behavioral regressions from the expanded Shape-tools tracking checklist (#2496).
use photocraft_doc::{LayerContent, LayerId, PathOp};
use photocraft_engine::Session;
use serde_json::json;

fn shape(s: &mut Session, rect: [i32; 4], color: &str) -> u64 {
    s.execute("shape.create", json!({"rect":rect,"fill":color})).unwrap()["layer"].as_u64().unwrap()
}

fn select_pair(s: &mut Session, back: u64, front: u64) {
    s.execute("layer.select", json!({"layer":back})).unwrap();
    s.execute("layer.select", json!({"layer":front,"mode":"toggle"})).unwrap();
}

#[test]
fn combine_selected_shape_layers_is_atomic_and_preserves_front_style() {
    for (command, samples) in [
        ("layer.combineShapes.unite", [true, true, true]),
        ("layer.combineShapes.subtractFrontShape", [true, false, false]),
        ("layer.combineShapes.intersectShapeAreas", [false, true, false]),
        ("layer.combineShapes.excludeOverlappingShapes", [true, false, true]),
    ] {
        for depth in [8, 16, 32] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width":64,"height":48,"depth":depth,"background":"transparent"})).unwrap();
            let back = shape(&mut s, [4, 4, 20, 20], "#ff0000");
            let front = shape(&mut s, [14, 4, 20, 20], "#0000ff");
            select_pair(&mut s, back, front);
            let before = s.active().unwrap().doc.clone();
            let history = s.active().unwrap().history.past_len();
            s.execute(command, json!({})).unwrap();
            let st = s.active().unwrap();
            assert!(st.doc.layer(photocraft_doc::LayerId(back)).is_none(), "{command}: selected layers merge");
            assert!(matches!(st.doc.layer(photocraft_doc::LayerId(front)).unwrap().content, photocraft_doc::LayerContent::Shape(_)));
            assert_eq!(st.selected_layers(), [photocraft_doc::LayerId(front)]);
            assert_eq!(st.history.past_len(), history + 1);
            for (x, filled) in [8, 18, 28].into_iter().zip(samples) {
                let p = s.execute("document.pixel", json!({"x":x,"y":10})).unwrap();
                let rgba: [f64; 4] = serde_json::from_value(p.clone()).unwrap();
                assert_eq!(rgba[3] > 0.9, filled, "{command}, depth {depth}, x={x}: {p}");
                if filled {
                    assert!(rgba[2] > 0.99 && rgba[0] < 0.01, "frontmost fill retained: {p}");
                }
            }
            let after = s.active().unwrap().doc.clone();
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc, before);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.active().unwrap().doc, after);
        }
    }
}

#[test]
fn intersection_treats_a_compound_front_layer_as_one_operand() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":64,"height":48,"background":"transparent"})).unwrap();
    let back = shape(&mut s, [4, 4, 56, 20], "#ff0000");
    let front = shape(&mut s, [14, 4, 10, 20], "#0000ff");
    s.execute("shape.create", json!({"rect":[40,4,10,20],"addTo":front,"op":"combine"})).unwrap();
    select_pair(&mut s, back, front);
    s.execute("layer.combineShapes.intersectShapeAreas", json!({})).unwrap();
    for (x, covered) in [(8, false), (18, true), (30, false), (45, true), (55, false)] {
        let p: [f32; 4] = serde_json::from_value(s.execute("document.pixel", json!({"x":x,"y":10})).unwrap()).unwrap();
        assert_eq!(p[3] > 0.9, covered, "x={x}: {p:?}");
    }
}

#[test]
fn combining_layers_handles_empty_operands() {
    for (command, empty_back, covered) in [
        ("layer.combineShapes.unite", true, true),
        ("layer.combineShapes.subtractFrontShape", true, false),
        ("layer.combineShapes.intersectShapeAreas", true, false),
        ("layer.combineShapes.intersectShapeAreas", false, false),
        ("layer.combineShapes.subtractFrontShape", false, true),
        ("layer.combineShapes.excludeOverlappingShapes", true, true),
    ] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":64,"height":48,"background":"transparent"})).unwrap();
        let back = shape(&mut s, [4, 4, 20, 20], "#ff0000");
        let front = shape(&mut s, [4, 4, 20, 20], "#0000ff");
        let empty = if empty_back { back } else { front };
        s.execute("shape.edit", json!({"layer":empty,"path":{"subpaths":[]}})).unwrap();
        select_pair(&mut s, back, front);
        s.execute(command, json!({})).unwrap();
        let p: [f32; 4] = serde_json::from_value(s.execute("document.pixel", json!({"x":10,"y":10})).unwrap()).unwrap();
        assert_eq!(p[3] > 0.9, covered, "{command}, empty_back={empty_back}: {p:?}");
    }
}

#[test]
fn combining_locked_layers_fails_without_changes() {
    for lock in ["all", "pixels", "position"] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":64,"height":48,"background":"transparent"})).unwrap();
        let back = shape(&mut s, [4, 4, 20, 20], "#ff0000");
        let front = shape(&mut s, [14, 4, 20, 20], "#0000ff");
        s.execute("layer.setProps", json!({"layer":back,"locks":{lock:true}})).unwrap();
        select_pair(&mut s, back, front);
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        assert!(s.execute("layer.combineShapes.unite", json!({})).is_err(), "{lock}");
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().history.past_len(), history);
    }
}

#[test]
fn mixed_selection_edits_single_shape_components_without_changing_raster() {
    for depth in [8, 16, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":64,"height":48,"depth":depth,"background":"transparent"})).unwrap();
        let raster = s.active().unwrap().active_layer.unwrap();
        s.execute("edit.fill", json!({"color":"#00ff00"})).unwrap();
        let front = LayerId(shape(&mut s, [4, 4, 20, 20], "#0000ff"));
        s.execute("shape.create", json!({"rect":[14,4,20,20],"addTo":front.0,"op":"subtract"})).unwrap();
        select_pair(&mut s, raster.0, front.0);
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        let LayerContent::Shape(original) = &before.layer(front).unwrap().content else { panic!("shape") };
        assert_eq!(original.path.subpaths[1].op, PathOp::Subtract);
        let pixel: [f32; 4] = serde_json::from_value(s.execute("document.pixel", json!({"x":18,"y":10})).unwrap()).unwrap();
        assert!(pixel[1] > 0.99 && pixel[2] < 0.01, "subtracted area reveals the raster: {pixel:?}");

        s.execute("layer.combineShapes.unite", json!({})).unwrap();
        let st = s.active().unwrap();
        assert_eq!(st.doc.layer(raster), before.layer(raster), "raster pixels and metadata must stay unchanged");
        assert_eq!(st.doc.layers.len(), before.layers.len());
        assert_eq!(st.active_layer, Some(front));
        assert_eq!(st.selected_layers(), [raster, front]);
        assert_eq!(st.history.past_len(), history + 1);
        let LayerContent::Shape(edited) = &st.doc.layer(front).unwrap().content else { panic!("shape") };
        assert_eq!(edited.path.subpaths.len(), 2);
        assert_eq!(edited.path.subpaths[0].op, PathOp::Combine);
        assert_eq!(edited.path.subpaths[1].op, PathOp::Combine);
        assert_eq!((&edited.fill, &edited.stroke), (&original.fill, &original.stroke));
        for x in [8, 18, 28] {
            let pixel: [f32; 4] = serde_json::from_value(s.execute("document.pixel", json!({"x":x,"y":10})).unwrap()).unwrap();
            assert!(pixel[2] > 0.99 && pixel[1] < 0.01, "united shape, depth={depth}, x={x}: {pixel:?}");
        }
        let after = s.active().unwrap().doc.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().selected_layers(), [raster, front]);
        assert_eq!(s.active().unwrap().active_layer, Some(front));
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, after);
    }
}

#[test]
fn mixed_selection_merges_only_shape_layers_and_undo_restores_selection() {
    for depth in [8, 16, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":64,"height":48,"depth":depth,"background":"transparent"})).unwrap();
        let raster = s.active().unwrap().active_layer.unwrap();
        s.execute("edit.fill", json!({"color":"#00ff00"})).unwrap();
        let back = LayerId(shape(&mut s, [4, 4, 20, 20], "#ff0000"));
        let front = LayerId(shape(&mut s, [14, 4, 20, 20], "#0000ff"));
        select_pair(&mut s, raster.0, back.0);
        s.execute("layer.select", json!({"layer":front.0,"mode":"toggle"})).unwrap();
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();

        // An explicit layer keeps the component-edit contract even with multiple shapes selected.
        s.execute("layer.combineShapes.unite", json!({"layer":front.0})).unwrap();
        assert_eq!(s.active().unwrap().doc.layer(raster), before.layer(raster));
        assert_eq!(s.active().unwrap().doc.layer(back), before.layer(back));
        assert_eq!(s.active().unwrap().selected_layers(), [raster, back, front]);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);

        s.execute("layer.combineShapes.unite", json!({})).unwrap();
        let st = s.active().unwrap();
        assert_eq!(st.doc.layer(raster), before.layer(raster));
        assert!(st.doc.layer(back).is_none());
        assert!(matches!(st.doc.layer(front).unwrap().content, LayerContent::Shape(_)));
        assert_eq!(st.doc.layers.len(), before.layers.len() - 1);
        assert_eq!(st.active_layer, Some(front));
        assert_eq!(st.selected_layers(), [front]);
        assert_eq!(st.history.past_len(), history + 1);
        for x in [8, 18, 28] {
            let pixel: [f32; 4] = serde_json::from_value(s.execute("document.pixel", json!({"x":x,"y":10})).unwrap()).unwrap();
            assert!(pixel[2] > 0.99 && pixel[0] < 0.01 && pixel[1] < 0.01, "front style, depth={depth}, x={x}: {pixel:?}");
        }
        let after = s.active().unwrap().doc.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().selected_layers(), [raster, back, front]);
        assert_eq!(s.active().unwrap().active_layer, Some(front));
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, after);
        assert_eq!(s.active().unwrap().selected_layers(), [front]);
    }
}

#[test]
fn curved_shape_bounds_do_not_include_coverage_residue() {
    for depth in [8, 16, 32] {
        for (rect, bounds) in [
            ([0.0, 0.0, 100.0, 100.0], [0, 0, 100, 100]),
            ([660.0, 240.0, 600.0, 600.0], [660, 240, 600, 600]),
            ([10.5, 10.0, 100.0, 100.0], [10, 10, 101, 100]),
        ] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width":1400,"height":900,"depth":depth,"background":"transparent"})).unwrap();
            let result = s.execute("shape.create", json!({"kind":"ellipse","rect":rect,"fill":"#0000ff"})).unwrap();
            assert_eq!(result["bounds"], json!(bounds), "depth {depth}, ellipse {rect:?}");
        }
    }
}
