use super::*;
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_raster::Surface;

fn session(depth: u32, mode: &str) -> (Session, LayerId, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 20, "height": 12, "depth": depth, "mode": mode})).unwrap();
    let a = LayerId(s.execute("layer.new.layer", json!({"name": "source"})).unwrap()["layer"].as_u64().unwrap());
    let b = LayerId(s.execute("layer.new.layer", json!({"name": "target"})).unwrap()["layer"].as_u64().unwrap());
    s.edit("fixture", |doc, active| {
        let mut mask = LayerMask::reveal_all();
        mask.surface = Surface::with_default(
            PixelFormat::new(
                ColorMode::Grayscale,
                match depth {
                    16 => SampleType::U16,
                    32 => SampleType::F32,
                    _ => SampleType::U8,
                },
                false,
            ),
            &[1.0],
        );
        mask.surface.write_region(Rect::new(2, 3, 4, 5), &[0.25; 4]);
        mask.density = 0.6;
        mask.feather = 2.5;
        mask.enabled = false;
        mask.linked = false;
        doc.layer_mut(a).unwrap().mask = Some(mask);
        *active = Some(a);
        Ok(())
    })
    .unwrap();
    (s, a, b)
}

fn mask(s: &Session, id: LayerId) -> LayerMask {
    s.active().unwrap().doc.layer(id).unwrap().mask.clone().unwrap()
}

#[test]
fn copying_and_pasting_preserve_full_masks_independently_with_one_undo_step() {
    for depth in [8, 16, 32] {
        for mode in ["rgb", "gray", "cmyk", "lab"] {
            let (mut s, a, b) = session(depth, mode);
            let original = mask(&s, a);
            let steps = s.active().unwrap().history.past_len();
            s.execute(COPY, json!({})).unwrap();
            assert_eq!(s.active().unwrap().history.past_len(), steps, "Copy is read-only");
            s.execute(PASTE, json!({"layer": b.0})).unwrap();
            assert_eq!(mask(&s, b), original);
            assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
            s.execute("edit.undo", json!({})).unwrap();
            assert!(s.active().unwrap().doc.layer(b).unwrap().mask.is_none());
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(mask(&s, b), original);
            s.edit("paint target", |doc, _| {
                doc.layer_mut(b).unwrap().mask.as_mut().unwrap().surface.write_region(Rect::new(2, 3, 3, 4), &[0.9]);
                Ok(())
            })
            .unwrap();
            assert_eq!(mask(&s, a), original, "COW mask edits leave source intact");
            assert_eq!(s.mask_clipboard.as_ref().unwrap().mask, original);
        }
    }
}

#[test]
fn clipboard_survives_source_edits_and_transfers_between_different_document_sizes() {
    let (mut s, a, _) = session(16, "rgb");
    let original = mask(&s, a);
    s.execute(COPY, json!({})).unwrap();
    s.execute("layer.layerMask.delete", json!({"layer": a.0})).unwrap();
    s.execute("file.new", json!({"width": 30, "height": 22, "depth": 32, "mode": "lab"})).unwrap();
    let b = LayerId(s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap());
    let r = s.execute(PASTE, json!({})).unwrap();
    assert_eq!(r["offset"], json!([5, 5]));
    let pasted = mask(&s, b);
    assert_eq!(pasted.surface.pixel(7, 8), original.surface.pixel(2, 3));
    assert_eq!(pasted.surface.pixel(0, 0)[0], 1.0, "sparse white default survives outside source canvas");
    assert_eq!((pasted.enabled, pasted.linked, pasted.density, pasted.feather), (false, false, 0.6, 2.5));
    assert_eq!(pasted.surface.format(), original.surface.format(), "mask values have no document color conversion");
}

#[test]
fn smaller_destination_and_all_white_or_black_masks_keep_their_defaults() {
    for white in [false, true] {
        let (mut s, a, _) = session(8, "gray");
        s.edit("uniform", |doc, _| {
            doc.layer_mut(a).unwrap().mask = Some(if white { LayerMask::reveal_all() } else { LayerMask::hide_all() });
            Ok(())
        })
        .unwrap();
        s.execute(COPY, json!({})).unwrap();
        s.execute("file.new", json!({"width": 10, "height": 6})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let r = s.execute(PASTE, json!({})).unwrap();
        assert_eq!(r["offset"], json!([-5, -3]));
        let st = s.active().unwrap();
        let pasted = st.doc.layer(st.active_layer.unwrap()).unwrap().mask.as_ref().unwrap();
        assert_eq!(pasted.surface.pixel(4, 4)[0], if white { 1.0 } else { 0.0 });
        assert_eq!(pasted.surface.tile_count(), 0);
    }
}

#[test]
fn replacement_requires_confirmation_and_undo_restores_both_masks_for_moves() {
    for copy in [false, true] {
        let (mut s, a, b) = session(8, "rgb");
        s.edit("target mask", |doc, _| {
            doc.layer_mut(b).unwrap().mask = Some(LayerMask::hide_all());
            Ok(())
        })
        .unwrap();
        let (old_a, old_b) = (mask(&s, a), mask(&s, b));
        let before = s.active().unwrap().doc.clone();
        let steps = s.active().unwrap().history.past_len();
        assert!(s.execute(TRANSFER, json!({"from": a.0, "layer": b.0, "copy": copy})).is_err());
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().history.past_len(), steps);
        s.execute(TRANSFER, json!({"from": a.0, "layer": b.0, "copy": copy, "replace": true})).unwrap();
        assert_eq!(mask(&s, b), old_a);
        assert_eq!(s.active().unwrap().doc.layer(a).unwrap().mask.is_some(), copy);
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(mask(&s, a), old_a);
        assert_eq!(mask(&s, b), old_b);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(mask(&s, b), old_a);
        assert!(s.mask_clipboard.is_none(), "dragging never replaces either clipboard");
    }
}

#[test]
fn paste_replacement_and_locks_refuse_without_mutation() {
    let (mut s, a, b) = session(8, "rgb");
    s.execute(COPY, json!({})).unwrap();
    assert!(s.execute(PASTE, json!({"layer": a.0})).is_err());
    s.execute("layer.setProps", json!({"layer": b.0, "locks": {"all": true}})).unwrap();
    let before = s.active().unwrap().doc.clone();
    assert!(s.execute(PASTE, json!({"layer": b.0})).is_err());
    assert!(s.execute(TRANSFER, json!({"from": a.0, "layer": b.0})).is_err());
    assert_eq!(s.active().unwrap().doc, before);
}

#[test]
fn missing_and_malformed_parameters_never_mutate_documents() {
    let (mut s, a, b) = session(8, "rgb");
    assert!(!s.is_enabled(PASTE));
    assert!(s.execute(PASTE, json!({})).is_err());
    s.execute(COPY, json!({})).unwrap();
    for id in [COPY, PASTE, TRANSFER] {
        for p in [Value::Null, json!([]), json!({"layer": -1}), json!({"layer": "bad"}), json!({"layer": u64::MAX}), json!({"document": 0})] {
            let before = s.active().unwrap().doc.clone();
            assert!(s.execute(id, p).is_err(), "{id}");
            assert_eq!(s.active().unwrap().doc, before);
        }
    }
    for p in [
        json!({"from": a.0, "layer": a.0}),
        json!({"from": b.0, "layer": a.0}),
        json!({"from": a.0, "layer": b.0, "copy": "bad"}),
        json!({"from": a.0, "layer": b.0, "replace": 1}),
    ] {
        let before = s.active().unwrap().doc.clone();
        assert!(s.execute(TRANSFER, p).is_err());
        assert_eq!(s.active().unwrap().doc, before);
    }
    assert!(s.execute(PASTE, json!({"layer": b.0, "replace": "bad"})).is_err());
}

#[test]
fn document_guard_rejects_tab_switches() {
    let (mut s, _, _) = session(8, "rgb");
    let doc = s.active().unwrap().doc.id.0;
    s.execute(COPY, json!({})).unwrap();
    s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    let before = s.active().unwrap().doc.clone();
    assert!(s.execute(PASTE, json!({"document": doc})).is_err());
    assert_eq!(s.active().unwrap().doc, before);
}

#[test]
fn explicit_source_background_targets_and_locked_source_moves() {
    let (mut s, a, b) = session(8, "rgb");
    s.execute("layer.select", json!({"layer": b.0})).unwrap();
    s.execute(COPY, json!({"layer": a.0})).unwrap();
    let background = s.active().unwrap().doc.layers.first().unwrap().id;
    s.execute(PASTE, json!({"layer": background.0})).unwrap();
    assert_eq!(mask(&s, background), mask(&s, a));
    assert!(!s.active().unwrap().doc.is_background(background));
    s.execute("layer.setProps", json!({"layer": a.0, "locks": {"all": true}})).unwrap();
    let before = s.active().unwrap().doc.clone();
    assert!(s.execute(TRANSFER, json!({"from": a.0, "layer": b.0, "copy": false})).is_err());
    assert_eq!(s.active().unwrap().doc, before);
    s.execute(TRANSFER, json!({"from": a.0, "layer": b.0, "copy": true})).unwrap();
    assert_eq!(mask(&s, b), mask(&s, a), "read-only copy is allowed from a locked source");
}
