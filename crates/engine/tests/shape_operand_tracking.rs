//! Incoming compound geometry is one Boolean operand for Shape and Path modes.
use photocraft_doc::{LayerContent, LayerId, Path};
use photocraft_engine::{Session, vector_cmds::path_json};
use photocraft_geom::Rect;
use serde_json::{Value, json};

fn square(x: i32, y: i32, w: i32, h: i32) -> Value {
    json!({"closed":true,"knots":[[x,y],[x+w,y],[x+w,y+h],[x,y+h]]})
}

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":64,"height":48,"depth":depth,"background":"transparent"})).unwrap();
    s
}

fn incoming(kind: &str) -> Value {
    match kind {
        "ring" => {
            let mut hole = square(24, 20, 16, 8);
            hole["op"] = json!("subtract");
            json!({"subpaths":[square(12,12,40,24),hole]})
        }
        "evenoddRing" => {
            let mut hole = square(24, 20, 16, 8);
            hole["op"] = json!("join");
            json!({"fillRule":"evenodd","subpaths":[square(12,12,40,24),hole]})
        }
        "disconnected" => json!({"subpaths":[square(12,12,8,24),square(44,12,8,24)]}),
        _ => panic!("test operand kind"),
    }
}

fn setup(s: &mut Session, target: &str) -> Option<LayerId> {
    let path = json!({"subpaths":[square(4,16,24,16)]});
    if target == "shape" {
        Some(LayerId(s.execute("shape.create", json!({"kind":"path","path":path})).unwrap()["layer"].as_u64().unwrap()))
    } else {
        s.execute("path.set", json!({"name":target,"path":path})).unwrap();
        None
    }
}

fn apply(s: &mut Session, target: &str, id: Option<LayerId>, path: Value, op: Value) -> photocraft_engine::Result<Value> {
    if let Some(id) = id {
        s.execute("shape.create", json!({"kind":"path","path":path,"op":op,"addTo":id.0}))
    } else {
        s.execute("path.set", json!({"name":target,"path":path,"op":op}))
    }
}

fn result_path(s: &Session, target: &str, id: Option<LayerId>) -> Path {
    let doc = &s.active().unwrap().doc;
    if let Some(id) = id {
        let LayerContent::Shape(shape) = &doc.layer(id).unwrap().content else { panic!("shape") };
        shape.path.clone()
    } else if target == "work" {
        doc.work_path.clone().unwrap()
    } else {
        doc.paths.iter().find(|p| p.name == target).unwrap().path.clone()
    }
}

#[test]
fn rings_and_disconnected_components_are_single_operands_for_all_operations() {
    let samples = [(8, true, false), (16, true, true), (26, true, false), (48, false, true), (34, false, false)];
    for depth in [8, 16, 32] {
        for target in ["shape", "work", "Saved operand path"] {
            for kind in ["ring", "evenoddRing", "disconnected"] {
                for op in ["combine", "subtract", "intersect", "exclude"] {
                    let mut s = session(depth);
                    let id = setup(&mut s, target);
                    let before = s.active().unwrap().doc.clone();
                    let history = s.active().unwrap().history.past_len();
                    apply(&mut s, target, id, incoming(kind), json!(op)).unwrap();
                    assert_eq!(s.active().unwrap().history.past_len(), history + 1);
                    let path = result_path(&s, target, id);
                    for (x, a, b) in samples {
                        let expected = match op {
                            "combine" => a || b,
                            "subtract" => a && !b,
                            "intersect" => a && b,
                            "exclude" => a != b,
                            _ => panic!(),
                        };
                        let coverage = photocraft_vector::path_coverage(&path, Rect::new(x, 24, x + 1, 25))[0];
                        assert_eq!(
                            coverage > 0.9,
                            expected,
                            "depth={depth}, target={target}, kind={kind}, op={op}, x={x}, coverage={coverage}, path={}",
                            path_json(&path)
                        );
                    }
                    let after = s.active().unwrap().doc.clone();
                    s.execute("edit.undo", json!({})).unwrap();
                    assert_eq!(s.active().unwrap().doc, before);
                    s.execute("edit.redo", json!({})).unwrap();
                    assert_eq!(s.active().unwrap().doc, after);
                }
            }
        }
    }
}

#[test]
fn empty_operands_and_empty_accumulators_follow_boolean_semantics() {
    for target in ["shape", "work"] {
        for op in ["combine", "subtract", "intersect", "exclude"] {
            for empty_base in [false, true] {
                let mut s = session(8);
                let id = setup(&mut s, target);
                let empty = json!({"subpaths":[]});
                if empty_base {
                    if let Some(id) = id {
                        s.execute("shape.edit", json!({"layer":id.0,"path":empty})).unwrap();
                    } else {
                        s.execute("path.set", json!({"name":target,"path":empty})).unwrap();
                    }
                }
                let path = if empty_base { incoming("ring") } else { empty };
                apply(&mut s, target, id, path, json!(op)).unwrap();
                let path = result_path(&s, target, id);
                let coverage = photocraft_vector::path_coverage(&path, Rect::new(16, 24, 17, 25))[0];
                let expected = if empty_base { matches!(op, "combine" | "exclude") } else { op != "intersect" };
                assert_eq!(coverage > 0.9, expected, "target={target}, op={op}, empty_base={empty_base}");
            }
        }
    }
}

#[test]
fn invalid_or_excessive_compound_operands_fail_atomically() {
    for target in ["shape", "work", "Saved operand path"] {
        let mut s = session(8);
        let id = setup(&mut s, target);
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        let mut huge_hole = square(100, 100, 1000, 1000);
        huge_hole["op"] = json!("subtract");
        for (path, op) in [
            (incoming("ring"), json!("unknown")),
            (incoming("ring"), json!(1)),
            (json!({"inverted":true,"subpaths":[square(12,12,40,24)]}), json!("combine")),
            (json!({"subpaths":[square(0,0,1200,1200),huge_hole]}), json!("combine")),
            (json!({"subpaths":[{"knots":vec![json!([0,0]);4097]}]}), json!("combine")),
        ] {
            assert!(apply(&mut s, target, id, path, op).is_err());
            assert_eq!(s.active().unwrap().doc, before);
            assert_eq!(s.active().unwrap().history.past_len(), history);
        }
    }
}
