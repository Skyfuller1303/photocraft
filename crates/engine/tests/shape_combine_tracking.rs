//! Selected-layer Boolean operations preserve holes, history and hierarchy (#2496).
use photocraft_doc::{LayerContent, LayerId, PathOp};
use photocraft_engine::Session;
use serde_json::{Value, json};

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":64,"height":48,"depth":depth,"background":"transparent"})).unwrap();
    s
}

fn shape(s: &mut Session, params: Value) -> LayerId {
    LayerId(s.execute("shape.create", params).unwrap()["layer"].as_u64().unwrap())
}

fn select(s: &mut Session, ids: &[LayerId]) {
    for (i, id) in ids.iter().enumerate() {
        s.execute("layer.select", json!({"layer":id.0,"mode":if i == 0 {"replace"} else {"add"}})).unwrap();
    }
}

fn rgba(s: &mut Session, x: i32, y: i32) -> [f32; 4] {
    serde_json::from_value(s.execute("document.pixel", json!({"x":x,"y":y})).unwrap()).unwrap()
}

#[test]
fn compound_and_evenodd_holes_survive_as_either_boolean_operand() {
    for depth in [8, 16, 32] {
        for outer_reverse in [false, true] {
            for (rule, hole_op, opposite_winding) in [
                ("nonzero", "subtract", false),
                ("nonzero", "subtract", true),
                ("nonzero", "join", true),
                ("evenodd", "join", false),
                ("evenodd", "join", true),
            ] {
                for hole_in_front in [false, true] {
                    let mut s = session(depth);
                    let mut outer = vec![[8, 8], [56, 8], [56, 40], [8, 40]];
                    let mut inner = vec![[20, 16], [44, 16], [44, 32], [20, 32]];
                    if outer_reverse {
                        outer.reverse();
                    }
                    if outer_reverse != opposite_winding {
                        inner.reverse();
                    }
                    let hole = json!({"kind":"path","fill":"#0000ff","path":{"fillRule":rule,"subpaths":[
                        {"closed":true,"knots":outer}, {"closed":true,"op":hole_op,"knots":inner}
                    ]}});
                    let solid = json!({"rect":[4,4,56,40],"fill":"#0000ff"});
                    let back = shape(&mut s, if hole_in_front { solid.clone() } else { hole.clone() });
                    let front = shape(&mut s, if hole_in_front { hole } else { solid });
                    select(&mut s, &[front, back]);
                    s.execute("layer.combineShapes.intersectShapeAreas", json!({})).unwrap();
                    for (x, filled) in [(12, true), (32, false), (52, true), (60, false)] {
                        let pixel = rgba(&mut s, x, 24);
                        assert_eq!(
                            pixel[3] > 0.9,
                            filled,
                            "depth={depth}, rule={rule}, hole_op={hole_op}, outer_reverse={outer_reverse}, opposite_winding={opposite_winding}, hole_in_front={hole_in_front}, x={x}: {pixel:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn merge_shape_components_merges_selected_layers_and_bakes_the_union_in_one_step() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        let back = shape(&mut s, json!({"rect":[4,4,24,24],"fill":"#ff0000"}));
        let front = shape(&mut s, json!({"rect":[18,4,24,24],"fill":"#0000ff"}));
        select(&mut s, &[front, back]);
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        s.execute("layer.combineShapes.mergeShapeComponents", json!({})).unwrap();
        let st = s.active().unwrap();
        assert_eq!(st.history.past_len(), history + 1);
        assert_eq!(st.selected_layers(), [front]);
        assert!(st.doc.layer(back).is_none());
        let LayerContent::Shape(shape) = &st.doc.layer(front).unwrap().content else { panic!("merged shape") };
        assert_eq!(shape.path.components().len(), 1);
        assert_eq!(shape.path.subpaths[0].op, PathOp::Combine);
        assert!(shape.live.is_none());
        for x in [8, 20, 36] {
            let pixel = rgba(&mut s, x, 12);
            assert!(pixel[2] > 0.99 && pixel[3] > 0.99 && pixel[0] < 0.01, "x={x}: {pixel:?}");
        }
        assert_eq!(rgba(&mut s, 50, 12)[3], 0.0);
        let after = s.active().unwrap().doc.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().selected_layers(), [back, front]);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, after);
        assert_eq!(s.active().unwrap().selected_layers(), [front]);
    }
}

#[test]
fn three_layer_intersection_stays_empty_after_any_empty_operand() {
    for empty_index in [None, Some(0), Some(1), Some(2)] {
        let mut s = session(8);
        let ids: Vec<_> = [4, 12, 20].into_iter().map(|x| shape(&mut s, json!({"rect":[x,4,32,24],"fill":"#0000ff"}))).collect();
        if let Some(i) = empty_index {
            s.execute("shape.edit", json!({"layer":ids[i].0,"path":{"subpaths":[]}})).unwrap();
        }
        select(&mut s, &ids);
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        s.execute("layer.combineShapes.intersectShapeAreas", json!({})).unwrap();
        assert_eq!(s.active().unwrap().history.past_len(), history + 1);
        assert_eq!(s.active().unwrap().selected_layers(), [ids[2]]);
        for (x, covered) in [(12, false), (24, empty_index.is_none()), (40, false)] {
            let pixel = rgba(&mut s, x, 12);
            assert_eq!(pixel[3] > 0.99, covered, "empty_index={empty_index:?}, x={x}: {pixel:?}");
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().selected_layers(), ids);
    }
}

#[test]
fn merging_shapes_from_separate_groups_keeps_front_parent_slot_and_layer_style() {
    let mut s = session(16);
    let back = shape(&mut s, json!({"rect":[4,4,24,24],"fill":"#ff0000"}));
    let back_group = LayerId(s.execute("layer.groupLayers", json!({"layer":back.0,"name":"Back"})).unwrap()["layer"].as_u64().unwrap());
    let front = shape(&mut s, json!({"rect":[18,4,24,24],"fill":"#0000ff","stroke":{"width":2,"color":"#ffffff","dashes":[2,1]}}));
    let front_group = LayerId(s.execute("layer.groupLayers", json!({"layer":front.0,"name":"Front"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("layer.setProps", json!({"layer":front.0,"name":"Retained shape","opacity":0.4,"fill":0.7,"blend":"multiply"})).unwrap();
    let before_layer = s.active().unwrap().doc.layer(front).unwrap().clone();
    let before_slot = s.active().unwrap().doc.path_of(front).unwrap();
    let before_root: Vec<_> = s.active().unwrap().doc.layers.iter().map(|l| l.id).collect();
    let LayerContent::Shape(before_shape) = &before_layer.content else { panic!("shape") };
    let before_paint = (before_shape.fill.clone(), before_shape.stroke.clone());
    select(&mut s, &[back, front]);
    let before = s.active().unwrap().doc.clone();
    s.execute("layer.combineShapes.unite", json!({})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.path_of(front), Some(before_slot));
    assert_eq!(doc.layers.iter().map(|l| l.id).collect::<Vec<_>>(), before_root);
    assert!(doc.layer(back_group).unwrap().children().unwrap().is_empty());
    assert_eq!(doc.layer(front_group).unwrap().children().unwrap().iter().map(|l| l.id).collect::<Vec<_>>(), [front]);
    let after_layer = doc.layer(front).unwrap();
    let LayerContent::Shape(after_shape) = &after_layer.content else { panic!("merged shape") };
    assert_eq!((after_shape.fill.clone(), after_shape.stroke.clone()), before_paint);
    let mut expected_layer = before_layer;
    expected_layer.content = after_layer.content.clone();
    assert_eq!(*after_layer, expected_layer, "all frontmost layer metadata must stay unchanged");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc, before);
}

#[test]
fn combining_inverted_operands_rejects_atomically_for_every_operation() {
    for inverted_front in [false, true] {
        let mut s = session(32);
        let back = shape(&mut s, json!({"rect":[4,4,24,24]}));
        let front = shape(&mut s, json!({"rect":[18,4,24,24]}));
        s.execute(
            "shape.edit",
            json!({"layer":if inverted_front {front.0} else {back.0},"path":{
                "inverted":true,"subpaths":[{"closed":true,"knots":[[8,8],[40,8],[40,32],[8,32]]}]
            }}),
        )
        .unwrap();
        select(&mut s, &[back, front]);
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        for command in [
            "layer.combineShapes.unite",
            "layer.combineShapes.subtractFrontShape",
            "layer.combineShapes.intersectShapeAreas",
            "layer.combineShapes.excludeOverlappingShapes",
            "layer.combineShapes.mergeShapeComponents",
        ] {
            let error = s.execute(command, json!({})).unwrap_err();
            assert!(error.to_string().contains("inverted"), "{command}: {error}");
            assert_eq!(s.active().unwrap().doc, before);
            assert_eq!(s.active().unwrap().history.past_len(), history);
            assert_eq!(s.active().unwrap().selected_layers(), [back, front]);
        }
    }
}

#[test]
fn combining_shapes_clears_stale_psd_path_data_and_restores_it_on_undo() {
    let mut s = session(16);
    let back = shape(&mut s, json!({"rect":[4,4,24,24]}));
    let front = shape(&mut s, json!({"rect":[18,4,24,24]}));
    let doc = std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc);
    let LayerContent::Shape(front_shape) = &mut doc.layer_mut(front).unwrap().content else { panic!("shape") };
    front_shape.psd_raw = Some(std::sync::Arc::new(vec![1, 2, 3, 4]));
    select(&mut s, &[back, front]);
    let before = s.active().unwrap().doc.clone();
    let history = s.active().unwrap().history.past_len();
    s.execute("layer.combineShapes.unite", json!({})).unwrap();
    let LayerContent::Shape(merged) = &s.active().unwrap().doc.layer(front).unwrap().content else { panic!("merged shape") };
    assert!(merged.psd_raw.is_none(), "the original PSD geometry cannot describe the combined path");
    assert_eq!(s.active().unwrap().history.past_len(), history + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc, before);
    s.execute("edit.redo", json!({})).unwrap();
    let LayerContent::Shape(merged) = &s.active().unwrap().doc.layer(front).unwrap().content else { panic!("merged shape") };
    assert!(merged.psd_raw.is_none());
}
