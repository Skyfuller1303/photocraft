use photocraft_doc::{LayerContent, LayerId, LiveShape};
use photocraft_engine::Session;
use serde_json::json;

fn session(depth: u32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":64,"height":48,"depth":depth,"background":"transparent"})).unwrap();
    s
}
fn rgba(s: &Session, id: LayerId, x: i32, y: i32) -> [f32; 4] {
    let surface = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap();
    let fmt = surface.format();
    photocraft_raster::to_rgba(&fmt, &surface.read_region(photocraft_geom::Rect::new(x, y, x + 1, y + 1)))
}

#[test]
fn pixel_shapes_respect_selection_opacity_blend_and_undo_at_every_depth() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        let id = s.active().unwrap().active_layer.unwrap();
        let original = s.active().unwrap().doc.clone();
        s.execute("select.rect", json!({"x":10,"y":5,"width":12,"height":20})).unwrap();
        let history = s.active().unwrap().history.past_len();
        s.execute("shape.drawPixels", json!({"rect":[5,5,25,20],"color":"#ff0000","opacity":50})).unwrap();
        assert_eq!(s.active().unwrap().history.past_len(), history + 1);
        assert!(rgba(&s, id, 15, 10)[0] > 0.99);
        assert!((rgba(&s, id, 15, 10)[3] - 0.5).abs() < 0.005);
        assert_eq!(rgba(&s, id, 6, 10)[3], 0.0);
        let painted = s.active().unwrap().doc.clone();
        s.execute("shape.drawPixels", json!({"rect":[5,5,25,20],"color":"#00ff00","mode":"multiply"})).unwrap();
        assert!(rgba(&s, id, 15, 10)[0] < 0.01);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, painted);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(rgba(&s, id, 15, 10)[3], 0.0);
        assert_eq!(s.active().unwrap().doc.layers.len(), original.layers.len());
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, painted);
    }
}

#[test]
fn pixel_antialias_and_transparency_lock_are_observable() {
    let mut s = session(32);
    let id = s.active().unwrap().active_layer.unwrap();
    s.execute("shape.drawPixels", json!({"kind":"ellipse","rect":[5.2,5.4,20.3,20.6],"antiAlias":true})).unwrap();
    let surface = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap();
    assert!(surface.read_region(photocraft_geom::Rect::new(0, 0, 32, 32)).chunks_exact(surface.format().channels()).any(|p| p[3] > 0.0 && p[3] < 1.0));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("shape.drawPixels", json!({"kind":"ellipse","rect":[5.2,5.4,20.3,20.6],"antiAlias":false})).unwrap();
    let surface = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap();
    assert!(surface.read_region(photocraft_geom::Rect::new(0, 0, 32, 32)).chunks_exact(surface.format().channels()).all(|p| p[3] == 0.0 || p[3] == 1.0));
    s.execute("layer.setProps", json!({"locks":{"transparency":true}})).unwrap();
    s.execute("shape.drawPixels", json!({"rect":[0,0,40,40],"color":"#ff0000"})).unwrap();
    assert_eq!(rgba(&s, id, 1, 1)[3], 0.0);
}

#[test]
fn pixel_mask_target_and_bad_params_preserve_document_on_failure() {
    let mut s = session(16);
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    s.execute("shape.drawPixels", json!({"rect":[3,3,20,20],"target":"mask","color":"#000000"})).unwrap();
    let layer = s.active().unwrap().doc.layer(s.active().unwrap().active_layer.unwrap()).unwrap();
    assert!(layer.mask.as_ref().unwrap().surface.sample_channel(10, 10, 0) < 0.01);
    s.execute("layer.setProps", json!({"locks":{"all":true}})).unwrap();
    let before = s.active().unwrap().doc.clone();
    let history = s.active().unwrap().history.past_len();
    for params in [
        json!({}),
        json!({"rect":[0,0,10,10],"target":"mask"}),
        json!({"rect":[0,0,10,10],"opacity":101}),
        json!({"rect":[0,0,10,10],"mode":"unknown"}),
        json!({"rect":[0,0,10,10],"antiAlias":"true"}),
        json!({"rect":[1e100,0,10,10]}),
        json!({"kind":"path","path":null}),
    ] {
        assert!(s.execute("shape.drawPixels", params.clone()).is_err(), "{params}");
        assert_eq!(s.active().unwrap().doc, before);
        assert_eq!(s.active().unwrap().history.past_len(), history);
    }
}

fn compound(s: &mut Session) {
    s.execute(
        "path.set",
        json!({"path":{"subpaths":[
            {"closed":true,"knots":[[5,5],[15,5],[15,15],[5,15]]},
            {"closed":true,"op":"subtract","knots":[[20,10],[30,10],[30,20],[20,20]]},
            {"closed":true,"knots":[[40,25],[50,25],[50,35],[40,35]]}
        ]}}),
    )
    .unwrap();
}
#[test]
fn align_distribute_arrange_work_saved_and_explicit_layer_targets() {
    let mut s = session(8);
    compound(&mut s);
    let original = s.active().unwrap().doc.work_path.clone().unwrap();
    s.execute("path.align", json!({"action":"top"})).unwrap();
    assert!(s.active().unwrap().doc.work_path.as_ref().unwrap().subpaths.iter().all(|sp| sp.knots[0].anchor.y == 5.0));
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("path.align", json!({"action":"distributeX"})).unwrap();
    assert_eq!(s.active().unwrap().doc.work_path.as_ref().unwrap().subpaths[1].knots[0].anchor.x, 22.5);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("path.arrange", json!({"components":[1],"action":"front"})).unwrap();
    let path = s.active().unwrap().doc.work_path.as_ref().unwrap();
    assert_eq!(path.subpaths[2], original.subpaths[1]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.work_path.as_ref(), Some(&original));
    s.execute("path.set", json!({"name":"saved","path":photocraft_engine::vector_cmds::path_json(&original)})).unwrap();
    s.execute("path.align", json!({"name":"saved","action":"right","to":"canvas","components":[0]})).unwrap();
    assert_eq!(s.active().unwrap().doc.paths[0].path.subpaths[0].knots[1].anchor.x, 64.0);
    let first =
        s.execute("shape.create", json!({"kind":"path","path":photocraft_engine::vector_cmds::path_json(&original)})).unwrap()["layer"].as_u64().unwrap();
    let other = s.execute("shape.create", json!({"rect":[2,2,10,10]})).unwrap()["layer"].as_u64().unwrap();
    s.execute("path.align", json!({"name":"layer","layer":first,"action":"bottom","to":"canvas","components":[2]})).unwrap();
    let LayerContent::Shape(sh) = &s.active().unwrap().doc.layer(LayerId(first)).unwrap().content else { panic!() };
    assert_eq!(sh.path.subpaths[2].knots[2].anchor.y, 48.0);
    let LayerContent::Shape(sh) = &s.active().unwrap().doc.layer(LayerId(other)).unwrap().content else { panic!() };
    assert!(sh.live.is_some());
}

#[test]
fn component_actions_reject_invalid_indices_locks_and_insufficient_distribution() {
    let mut s = session(8);
    compound(&mut s);
    let before = s.active().unwrap().doc.clone();
    for (cmd, p) in [
        ("path.align", json!({"action":"bad"})),
        ("path.align", json!({"action":"top","components":[999]})),
        ("path.align", json!({"action":"distributeX","components":[0,1]})),
        ("path.arrange", json!({"action":"front","components":[]})),
        ("path.arrange", json!({"action":"bad"})),
        ("path.arrange", json!({"action":"front","components":[1e100]})),
    ] {
        assert!(s.execute(cmd, p).is_err());
        assert_eq!(s.active().unwrap().doc, before);
    }
    let id = s.execute("shape.create", json!({"rect":[4,4,10,10]})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.setProps", json!({"locks":{"position":true}})).unwrap();
    assert!(s.execute("path.align", json!({"name":"layer","layer":id,"action":"top","to":"canvas"})).is_err());
}

#[test]
fn aligning_a_whole_live_shape_keeps_geometry_editable_and_undoable() {
    let mut s = session(8);
    for params in [
        json!({"kind":"roundedRect","rect":[4,5,20,15],"radii":3}),
        json!({"kind":"polygon","rect":[4,5,20,15],"sides":5,"cornerRadius":2}),
        json!({"kind":"line","from":[4,5],"to":[24,20],"weight":2,"arrowheads":{"end":true,"width":8,"length":6}}),
    ] {
        let id = s.execute("shape.create", params).unwrap()["layer"].as_u64().unwrap();
        // Imported paths may have slightly different precision from their live metadata.
        let doc = std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc);
        let LayerContent::Shape(sh) = &mut doc.layer_mut(LayerId(id)).unwrap().content else { panic!() };
        sh.path = sh.path.transform(&photocraft_geom::Affine::translate(0.001, 0.003));
        let before = s.active().unwrap().doc.clone();
        let history = s.active().unwrap().history.past_len();
        s.execute("path.align", json!({"name":"layer","layer":id,"action":"right","to":"canvas"})).unwrap();
        let LayerContent::Shape(sh) = &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content else { panic!() };
        assert!(sh.live.is_some());
        assert!((sh.path.control_bounds().unwrap().2 - 64.0).abs() < 1e-8);
        assert_eq!(s.active().unwrap().history.past_len(), history + 1);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);
        s.execute("edit.redo", json!({})).unwrap();
        let patch = match s.execute("shape.info", json!({"layer":id})).unwrap()["live"]["kind"].as_str().unwrap() {
            "rect" => json!({"layer":id,"radii":5}),
            "polygon" => json!({"layer":id,"cornerRadius":5}),
            _ => json!({"layer":id,"weight":5}),
        };
        s.execute("shape.edit", patch).unwrap();
        let LayerContent::Shape(sh) = &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content else { panic!() };
        match &sh.live {
            Some(LiveShape::Rect { radii, .. }) => assert_eq!(*radii, [5.0; 4]),
            Some(LiveShape::Polygon { corner_radius, .. }) => assert_eq!(*corner_radius, 5.0),
            Some(LiveShape::Line { weight, .. }) => assert_eq!(*weight, 5.0),
            _ => panic!("aligned shape lost its live parameters"),
        }
    }
}

#[test]
fn merging_path_components_preserves_coverage_and_is_one_undo_step() {
    let mut s = session(8);
    compound(&mut s);
    let path = s.active().unwrap().doc.work_path.clone().unwrap();
    let history = s.active().unwrap().history.past_len();
    s.execute("path.merge", json!({})).unwrap();
    let merged = s.active().unwrap().doc.work_path.clone().unwrap();
    let area = photocraft_geom::Rect::new(0, 0, 64, 48);
    let before = photocraft_vector::fill_rasterizer(&path, 0.01).render(area);
    let after = photocraft_vector::fill_rasterizer(&merged, 0.01).render(area);
    assert!(before.iter().zip(after).map(|(a, b)| (a - b).abs()).sum::<f32>() < 3.0);
    assert_eq!(s.active().unwrap().history.past_len(), history + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc.work_path.as_ref(), Some(&path));
    assert!(s.execute("path.merge", json!({"tolerance":1e100})).is_err());
}

#[test]
fn rounded_polygons_and_arrowheads_keep_live_properties_and_undo() {
    for depth in [8, 16, 32] {
        let mut s = session(depth);
        let id =
            s.execute("shape.create", json!({"kind":"star","sides":5,"rect":[5,5,40,30],"cornerRadius":2,"starRatio":0.5})).unwrap()["layer"].as_u64().unwrap();
        s.execute("shape.edit", json!({"layer":id,"sides":6})).unwrap();
        let info = s.execute("shape.info", json!({"layer":id})).unwrap();
        assert_eq!(info["live"]["star_ratio"], 0.5);
        assert_eq!(info["live"]["corner_radius"], 2.0);
        s.execute("shape.edit", json!({"layer":id,"cornerRadius":0})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.execute("shape.edit", json!({"layer":id,"cornerRadius":-1})).is_err());
        let id = s
            .execute("shape.create", json!({"kind":"line","from":[5,20],"to":[55,20],"weight":2,"arrowheads":{"start":true,"end":true,"width":10,"length":12}}))
            .unwrap()["layer"]
            .as_u64()
            .unwrap();
        let LayerContent::Shape(sh) = &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content else { panic!() };
        assert_eq!(sh.path.subpaths.len(), 1);
        assert_eq!(sh.path.subpaths[0].knots.len(), 10);
        assert!(matches!(sh.live, Some(LiveShape::Line { arrowheads: Some(_), .. })));
        s.execute("shape.edit", json!({"layer":id,"weight":4})).unwrap();
        assert!(s.execute("shape.edit", json!({"layer":id,"arrowheads":{"width":1e100}})).is_err());
    }
}

#[test]
fn pixels_draw_in_grayscale_without_format_assumptions() {
    let mut s = session(16);
    s.execute("image.mode.grayscale", json!({})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    s.execute("shape.drawPixels", json!({"rect":[4,4,12,12],"color":"#ff0000"})).unwrap();
    let p = rgba(&s, id, 8, 8);
    assert!((p[0] - p[1]).abs() < 1e-6 && (p[1] - p[2]).abs() < 1e-6 && p[3] > 0.99);
}

#[test]
#[ignore = "release performance measurement on a 36 MP canvas"]
fn pixel_shape_36mp() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":7200,"height":5000,"background":"transparent"})).unwrap();
    let start = std::time::Instant::now();
    s.execute("shape.drawPixels", json!({"rect":[0,0,7200,5000],"color":"#ff0000"})).unwrap();
    eprintln!("36 MP pixel shape: {:?}", start.elapsed());
    let id = s.active().unwrap().active_layer.unwrap();
    assert!(rgba(&s, id, 7199, 4999)[0] > 0.99);
}

#[test]
fn huge_pixel_shape_rejects_before_allocation_without_changing_document_or_history() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":300000,"height":300000,"depth":32,"background":"transparent"})).unwrap();
    let before = s.active().unwrap().doc.clone();
    let history = s.active().unwrap().history.past_len();
    let err = s.execute("shape.drawPixels", json!({"rect":[0,0,300000,300000]})).unwrap_err();
    assert!(err.to_string().contains("drawing memory limit"));
    assert_eq!(s.active().unwrap().doc, before);
    assert_eq!(s.active().unwrap().history.past_len(), history);
    // Large sparse canvases still support small local shapes.
    s.execute("shape.drawPixels", json!({"rect":[2,2,8,8],"color":"#ff0000"})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    assert!(rgba(&s, id, 4, 4)[0] > 0.99);
}
