use super::*;
use photocraft_geom::Rect;

fn shape_below_text(depth: u32) -> (PhotocraftApp, LayerId, LayerId) {
    shape_below_text_size(depth, 18)
}

fn shape_below_text_size(depth: u32, size: u32) -> (PhotocraftApp, LayerId, LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.execute("file.new", json!({"width": 512, "height": 512, "depth": depth})).unwrap();
    app.sync_views();
    let shape =
        LayerId(app.session.execute("shape.create", json!({"kind": "rect", "rect": [8, 8, 40, 40], "fill": "#ffff00"})).unwrap()["layer"].as_u64().unwrap());
    let text = LayerId(
        app.session.execute("type.create", json!({"x": 12, "y": 40, "text": "TEST", "size": size, "color": "#000000"})).unwrap()["layer"].as_u64().unwrap(),
    );
    app.session.execute("layer.select", json!({"layer": shape.0})).unwrap();
    app.ui.tool = Tool::Move;
    app.ui.extras.snap = false;
    (app, shape, text)
}

fn texture_meshes(app: &PhotocraftApp, ctx: &egui::Context) -> usize {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let xf = ViewXform {
            aspect: 1.0,
            rect: egui::Rect::from_min_size(Pos2::ZERO, vec2(256.0, 256.0)),
            zoom: 2.0,
            center: [64.0, 64.0],
            flip: false,
            rotation: 0.0,
        };
        draw_overlay(app, &ui.painter().with_clip_rect(xf.rect), &xf);
    });
    output.textures_delta.clear();
    output.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default())).count()
}

#[test]
fn scaling_a_shape_keeps_higher_text_visible_in_preview_and_commit() {
    for depth in [8, 16, 32] {
        let (mut app, shape, text) = shape_below_text(depth);
        app.session.edit_prefs(|p| p.performance.low_resolution_previews = false);
        let original = app.session.active().unwrap().doc.clone();
        let history = app.session.active().unwrap().history.past_len();
        let layer_order: Vec<_> = original.layers.iter().map(|l| l.id).collect();
        let text_layer = original.layer(text).unwrap();
        let surface = text_layer.surface().unwrap();
        let bounds = surface.content_bounds();
        let p = (bounds.y0..bounds.y1)
            .flat_map(|y| (bounds.x0..bounds.x1).map(move |x| (x, y)))
            .find(|&(x, y)| (8..48).contains(&x) && (8..48).contains(&y) && surface.pixel(x, y).last().copied().unwrap() > 0.95)
            .unwrap();
        let ctx = egui::Context::default();
        begin(&mut app, &ctx).unwrap();
        let corner = app.ui.transform.as_ref().unwrap().quad[2];
        pointer(&mut app, ToolEvent::Down { x: corner[0], y: corner[1], pressure: 1.0 }, egui::Modifiers::NONE);
        pointer(&mut app, ToolEvent::Up { x: corner[0] + 32.0, y: corner[1] + 32.0 }, egui::Modifiers::NONE);
        follow_edits(&mut app);
        let shown = display_doc(&app, app.session.active_index().unwrap()).unwrap().0;
        assert_eq!(texture_meshes(&app, &ctx), 0, "shape must not cover higher layers with an overlay");
        assert!(shown.layer(shape).unwrap().visible);
        assert_eq!(shown.layers.iter().map(|l| l.id).collect::<Vec<_>>(), layer_order);
        let pixel = photocraft_compose::render(&shown, Rect::new(p.0, p.1, p.0 + 1, p.1 + 1)).px[0];
        assert!(pixel[0] < 0.06 && pixel[1] < 0.06 && pixel[2] < 0.06 && pixel[3] > 0.99, "text remains black at {p:?}: {pixel:?}");
        assert_eq!(app.session.active().unwrap().history.past_len(), history, "preview adds no history");
        let before = app.transform_preview.as_ref().unwrap().doc.clone();
        follow_edits(&mut app);
        assert!(Arc::ptr_eq(&before, &app.transform_preview.as_ref().unwrap().doc), "steady frames reuse rendered tiles");
        commit(&mut app);
        let st = app.session.active().unwrap();
        assert_eq!(st.history.past_len(), history + 1);
        assert_eq!(st.doc.layers.iter().map(|l| l.id).collect::<Vec<_>>(), layer_order);
        assert_eq!(st.doc.layer(text).unwrap().surface().unwrap().pixel(p.0, p.1), surface.pixel(p.0, p.1));
        let committed = photocraft_compose::render(&st.doc, Rect::new(0, 0, 128, 128));
        assert_eq!(committed.px, photocraft_compose::render(&shown, committed.rect).px, "preview matches commit at depth {depth}");
        assert!(app.session.undo());
        assert!(Arc::ptr_eq(&original, &app.session.active().unwrap().doc));
        assert!(app.session.redo());
        assert_eq!(photocraft_compose::render(&app.session.active().unwrap().doc, committed.rect).px, committed.px);
    }
}

#[test]
fn shape_transform_preview_damage_and_external_edits_are_cached() {
    let (mut app, shape, _) = shape_below_text(8);
    app.session.edit_prefs(|p| p.performance.low_resolution_previews = false);
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    let idx = app.session.active_index().unwrap();
    let (first, key1) = display_doc(&app, idx).unwrap();
    scale_about_pivot(&mut app, 1.5, 1.5);
    follow_edits(&mut app);
    let (second, key2) = display_doc(&app, idx).unwrap();
    assert_ne!(key1, key2);
    let revision = app.session.active().unwrap().revision;
    let area = damage(&app, first.id, revision, key1, key2).unwrap();
    assert!(area.contains_rect(&first.layer(shape).unwrap().surface().unwrap().content_bounds()));
    assert!(area.contains_rect(&second.layer(shape).unwrap().surface().unwrap().content_bounds()));
    assert!(area.width() < second.size.width || area.height() < second.size.height, "small shape invalidates only local tiles where possible");
    app.session.execute("layer.setProps", json!({"layer": shape.0, "opacity": 0.5})).unwrap();
    follow_edits(&mut app);
    let (edited, key3) = display_doc(&app, idx).unwrap();
    assert_ne!(key2, key3);
    assert_eq!(edited.layer(shape).unwrap().opacity, 0.5);
    let shown = app.transform_preview.as_ref().unwrap().doc.clone();
    follow_edits(&mut app);
    assert!(Arc::ptr_eq(&shown, &app.transform_preview.as_ref().unwrap().doc));
    enter_warp(&mut app, &ctx);
    follow_edits(&mut app);
    assert!(!app.transform_preview.as_ref().unwrap().shape.as_ref().unwrap().active);
    assert!(texture_meshes(&app, &ctx) > 0, "Warp retains its texture preview");
    leave_warp(&mut app);
    follow_edits(&mut app);
    assert!(app.transform_preview.as_ref().unwrap().shape.as_ref().unwrap().active);
    assert_eq!(texture_meshes(&app, &ctx), 0);
    cancel(&mut app);
    assert!(app.transform_preview.is_none());
}

#[test]
fn selection_does_not_crop_shape_transform_frame_or_old_surface_damage() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.edit_prefs(|p| p.performance.low_resolution_previews = false);
    app.session.execute("file.new", json!({"width": 1536, "height": 1536, "background": "transparent"})).unwrap();
    app.sync_views();
    let shape =
        LayerId(app.session.execute("shape.create", json!({"kind": "rect", "rect": [0, 0, 512, 512], "fill": "#ffff00"})).unwrap()["layer"].as_u64().unwrap());
    app.session.execute("select.rect", json!({"x": 448, "y": 448, "width": 64, "height": 64})).unwrap();
    app.ui.tool = Tool::Move;
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    assert_eq!(app.ui.transform.as_ref().unwrap().rect, [0.0, 0.0, 512.0, 512.0], "#2630: the frame covers the whole shape despite the selection");
    let idx = app.session.active_index().unwrap();
    let (before, key1) = display_doc(&app, idx).unwrap();
    let old_bounds = before.layer(shape).unwrap().surface().unwrap().tile_bounds();
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 1000.0, p[1] + 1000.0]);
    follow_edits(&mut app);
    let (after, key2) = display_doc(&app, idx).unwrap();
    assert_eq!(after.layer(shape).unwrap().surface().unwrap().content_bounds(), Rect::new(1000, 1000, 1512, 1512), "the entire shape moves");
    assert_eq!(after.selection, before.selection, "transforming a shape preserves the selection");
    let revision = app.session.active().unwrap().revision;
    for previous_key in [0, key1] {
        let damaged = damage(&app, before.id, revision, previous_key, key2).unwrap();
        assert!(damaged.contains_rect(&old_bounds), "moving a shape invalidates its entire old surface despite the selection");
        let mut cached = photocraft_compose::render(&before, before.bounds());
        let update = photocraft_compose::render_reduced_damage(&after, after.size.width, after.size.height, damaged);
        let width = cached.rect.width() as usize;
        let update_width = update.rect.width() as usize;
        for (row, pixels) in update.px.chunks_exact(update_width).enumerate() {
            let start = (update.rect.y0 as usize + row) * width + update.rect.x0 as usize;
            cached.px[start..start + update_width].copy_from_slice(pixels);
        }
        assert_eq!(cached.px, photocraft_compose::render(&after, after.bounds()).px, "partial canvas refresh must match a full composite");
    }
}

#[test]
fn warp_fallback_refreshes_shape_texture_after_source_edits() {
    let (mut app, shape, _) = shape_below_text(8);
    let ctx = egui::Context::default();
    begin_warp(&mut app, &ctx).unwrap();
    follow_edits(&mut app);
    let original = app.transform_preview.as_ref().unwrap().texture.pick(&ctx, 1.0, 1.0, false).0;
    let uploaded_color = |ctx: &egui::Context, texture| {
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        let color = {
            let upload = output.textures_delta.set.get(&texture).and_then(|updates| updates.last()).unwrap();
            let egui::ImageData::Color(image) = &upload.image;
            image.pixels[image.size[0] * (image.size[1] / 2) + image.size[0] / 2]
        };
        output.textures_delta.clear();
        color
    };
    assert_eq!(uploaded_color(&ctx, original), Color32::YELLOW);
    app.session.execute("shape.edit", json!({"layer": shape.0, "fill": "#0000ff"})).unwrap();
    follow_edits(&mut app);
    let updated = app.transform_preview.as_ref().unwrap().texture.pick(&ctx, 1.0, 1.0, false).0;
    assert_ne!(updated, original, "a source revision replaces the inactive Warp texture");
    assert_eq!(uploaded_color(&ctx, updated), Color32::BLUE);
    follow_edits(&mut app);
    assert_eq!(app.transform_preview.as_ref().unwrap().texture.pick(&ctx, 1.0, 1.0, false).0, updated, "steady Warp frames retain their texture");
}

#[test]
fn shape_transform_preview_preserves_linked_masks_and_rejects_locked_targets() {
    for linked in [false, true] {
        let (mut app, shape, _) = shape_below_text(16);
        app.session.execute("layer.layerMask.revealAll", json!({})).unwrap();
        app.session
            .edit("mask", |doc, _| {
                let mask = doc.layer_mut(shape).unwrap().mask.as_mut().unwrap();
                mask.linked = linked;
                mask.surface.fill_rect(Rect::new(8, 8, 28, 48), &[0.0]);
                Ok(())
            })
            .unwrap();
        let source = app.session.active().unwrap().doc.clone();
        let affine = photocraft_geom::Affine { m: [1.5, 0.0, 0.0, 1.5, 4.0, 5.0] };
        let shown = photocraft_engine::transform_cmds::preview_shape_transform(&source, shape, affine, "bicubic").unwrap();
        app.session.execute("edit.transform", json!({"layer": shape.0, "matrix": affine.m})).unwrap();
        let committed = &app.session.active().unwrap().doc;
        let area = Rect::new(0, 0, 128, 128);
        assert_eq!(
            photocraft_compose::render(&shown, area).px,
            photocraft_compose::render(committed, area).px,
            "linked mask {linked} follows engine semantics"
        );
        assert_eq!(shown.layer(shape).unwrap().mask.as_ref().unwrap().linked, linked);
        app.session.execute("layer.setProps", json!({"layer": shape.0, "locks": {"position": true}})).unwrap();
        let locked = &app.session.active().unwrap().doc;
        assert!(photocraft_engine::transform_cmds::preview_shape_transform(locked, shape, affine, "bicubic").is_err());
        assert!(photocraft_engine::transform_cmds::preview_shape_transform(locked, shape, photocraft_geom::Affine { m: [f64::NAN; 6] }, "bicubic").is_err());
    }
}

#[test]
fn rotated_shape_drag_preview_tracks_committed_path_in_screen_space() {
    for tool in [Tool::Rectangle, Tool::EllipseShape, Tool::Triangle, Tool::Polygon, Tool::Line, Tool::CustomShape] {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width": 128, "height": 128})).unwrap();
        app.sync_views();
        let (start, end) = ([20.0, 30.0], [90.0, 80.0]);
        crate::vector_ui::finish_shape(&mut app, tool, start, end, egui::Modifiers::NONE);
        let st = app.session.active().unwrap();
        let LayerContent::Shape(shape) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("{tool:?} did not create a shape") };
        let path = shape.path.clone();
        for flip in [false, true] {
            for rotation in [-0.7, 0.35, std::f32::consts::FRAC_PI_2] {
                let ctx = egui::Context::default();
                let rect = egui::Rect::from_min_size(pos2(17.0, 29.0), vec2(600.0, 450.0));
                let xf = ViewXform { aspect: 1.0, rect, zoom: 1.6, center: [55.0, 63.0], flip, rotation };
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    let painter = ui.ctx().layer_painter(egui::LayerId::background()).with_clip_rect(rect);
                    crate::vector_ui::draw_shape_preview(&app, &painter, &xf, tool, start, end, egui::Modifiers::NONE);
                });
                output.textures_delta.clear();
                let outlines: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        egui::Shape::Path(p) => Some(p),
                        _ => None,
                    })
                    .collect();
                let expected = crate::vector_ui::path_lines(&path, &|q| xf.to_screen(q[0] as f32, q[1] as f32));
                for (points, closed) in expected {
                    assert!(
                        outlines.iter().any(|p| p.closed == closed && p.points == points),
                        "{tool:?}, flip {flip}, rotation {rotation}: outline follows full canvas transform"
                    );
                }
                assert!(
                    output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default())),
                    "fill preview also rendered for {tool:?}"
                );
            }
        }
    }
}

/// `cargo test --release -p photocraft-ui-egui shape_transform_preview_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn shape_transform_preview_bench() {
    for side in [128.0, 4000.0] {
        for linked_mask in [false, true] {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            app.session.execute("file.new", json!({"width": 7200, "height": 5000, "background": "transparent"})).unwrap();
            app.sync_views();
            let shape = LayerId(
                app.session.execute("shape.create", json!({"kind": "rect", "rect": [100, 100, side, side], "fill": "#ffff00"})).unwrap()["layer"]
                    .as_u64()
                    .unwrap(),
            );
            if linked_mask {
                app.session
                    .edit("linked preview mask", |doc, _| {
                        let mut mask = photocraft_doc::LayerMask::reveal_all();
                        mask.linked = true;
                        mask.surface.fill_rect(Rect::new(100, 100, (100.0 + side / 2.0) as i32, (100.0 + side) as i32), &[0.25]);
                        doc.layer_mut(shape).unwrap().mask = Some(mask);
                        Ok(())
                    })
                    .unwrap();
            }
            app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(1800.0, 1250.0));
            app.ui.views[0].zoom = 0.25;
            app.ui.views[0].center = [3600.0, 2500.0];
            app.ui.tool = Tool::Move;
            let ctx = egui::Context::default();
            let start = std::time::Instant::now();
            begin(&mut app, &ctx).unwrap();
            let begin_ms = start.elapsed().as_secs_f64() * 1000.0;
            scale_about_pivot(&mut app, 1.05, 1.05);
            let start = std::time::Instant::now();
            follow_edits(&mut app);
            let changed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let first = shape_proxy(&app, 0).unwrap();
            let base = app.transform_preview.as_ref().unwrap().shape.as_ref().unwrap().proxy.as_ref().unwrap().base.clone();
            assert!(first.region.width() <= 2048 && first.region.height() <= 2048);
            assert!(first.doc.layer(LayerId(app.ui.transform.as_ref().unwrap().layer)).unwrap().surface().unwrap().content_bounds().width() <= 2048);
            scale_about_pivot(&mut app, 1.01, 1.01);
            let start = std::time::Instant::now();
            follow_edits(&mut app);
            let warm_changed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let actual = shape_proxy(&app, 0).unwrap();
            assert!(
                Arc::ptr_eq(&base, &app.transform_preview.as_ref().unwrap().shape.as_ref().unwrap().proxy.as_ref().unwrap().base),
                "warm changed quad reuses the reduced source"
            );
            let layer = actual.doc.layer(shape).unwrap();
            let moving = layer.surface().unwrap();
            assert!(moving.tiles().count() <= actual.region.tiles().count());
            if let Some(mask) = &layer.mask {
                assert!(mask.surface.tiles().count() <= actual.region.inflate(4).tiles().count(), "changed linked mask stays viewport bounded");
            }
            let start = std::time::Instant::now();
            let composite = photocraft_compose::render(&actual.doc, actual.region);
            let cpu_composite_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert!(composite.px.iter().any(|p| p[3] > 0.0), "timed frame contains actual rendered shape pixels");
            let shown = app.transform_preview.as_ref().unwrap().doc.clone();
            let start = std::time::Instant::now();
            for _ in 0..100 {
                follow_edits(&mut app);
            }
            let steady_ms = start.elapsed().as_secs_f64() * 10.0;
            assert!(Arc::ptr_eq(&shown, &app.transform_preview.as_ref().unwrap().doc));
            eprintln!(
                "shape transform preview 7200x5000, {side}x{side}, linked mask {linked_mask}: begin {begin_ms:.3} ms, cold changed quad {changed_ms:.3} ms (source cache + bounded raster), cached-base changed quad {warm_changed_ms:.3} ms (bounded raster), CPU composite {cpu_composite_ms:.3} ms, steady frame {steady_ms:.6} ms, proxy {}x{} at 1/{}",
                actual.region.width(),
                actual.region.height(),
                actual.k
            );
        }
    }
}

#[test]
fn rotated_and_flipped_shape_preview_texture_samples_document_paint() {
    use photocraft_doc::{Color, ColorMode, Fill, Pattern, PixelFormat};
    let mut surface = photocraft_raster::Surface::new(PixelFormat::RGBA16);
    surface.fill_rect(Rect::new(0, 0, 4, 4), &[0.9, 0.1, 0.2, 1.0]);
    surface.fill_rect(Rect::new(2, 0, 4, 4), &[0.1, 0.3, 0.9, 1.0]);
    let p = Pattern::new("Preview tile", surface, 4, 4);
    let pattern = Fill::Pattern { id: p.id.clone(), name: p.name.clone(), scale: 3.0, angle: 33.0, link: true, phase: (7.0, 9.0) };
    let mut gradient =
        Fill::gradient(vec![(0.0, Color::rgb(0.1, 0.2, 0.9)), (1.0, Color::rgb(0.9, 0.7, 0.1))], 17.0, 1.0, photocraft_doc::GradientStyle::Linear, false);
    if let Fill::Gradient { align, offset, .. } = &mut gradient {
        *align = false;
        *offset = (0.1, -0.2);
    }
    for fill in [pattern, gradient] {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.execute("file.new", json!({"width":128,"height":128,"background":"transparent"})).unwrap();
        app.sync_views();
        // An unembedded library tile must preview before the create command embeds it.
        app.session.patterns.items.push(p.clone());
        app.ui.tool_options.shape_fill_paint = Some(fill.clone());
        app.ui.tool_options.shape_fill = true;
        app.ui.tool_options.shape.mode = "shape".into();
        app.ui.tool_options.stroke_width = 0.0;
        let expected = photocraft_vector::Paint::from_fill_with_patterns(
            &fill,
            (16.0, 16.0, 112.0, 112.0),
            (0.0, 0.0, 128.0, 128.0),
            std::slice::from_ref(&p),
            ColorMode::Rgb,
        );
        for flip in [false, true] {
            for rotation in [-0.7, std::f32::consts::FRAC_PI_2] {
                let ctx = egui::Context::default();
                let rect = egui::Rect::from_min_size(pos2(17.0, 29.0), vec2(400.0, 400.0));
                let xf = ViewXform { aspect: 1.0, rect, zoom: 0.75, center: [64.0, 64.0], flip, rotation };
                let draw = |ui: &mut egui::Ui| {
                    let painter = ui.ctx().layer_painter(egui::LayerId::background()).with_clip_rect(rect);
                    crate::vector_ui::draw_shape_preview(&app, &painter, &xf, Tool::Rectangle, [16.0, 16.0], [112.0, 112.0], egui::Modifiers::NONE);
                };
                let mut output = ctx.run_ui(egui::RawInput::default(), draw);
                let mesh = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() => Some(m),
                        _ => None,
                    })
                    .unwrap();
                let image = output.textures_delta.set.get(&mesh.texture_id).and_then(|updates| updates.last()).unwrap().image.clone();
                output.textures_delta.clear();
                let egui::ImageData::Color(image) = image;
                assert!(image.size[0] <= 2048 && image.size[1] <= 2048);
                let mut checked = 0;
                for y in (3..image.size[1].saturating_sub(3)).step_by(7) {
                    for x in (3..image.size[0].saturating_sub(3)).step_by(7) {
                        let u = (x as f32 + 0.5) / image.size[0] as f32;
                        let v = (y as f32 + 0.5) / image.size[1] as f32;
                        let screen =
                            mesh.vertices[0].pos + (mesh.vertices[1].pos - mesh.vertices[0].pos) * u + (mesh.vertices[3].pos - mesh.vertices[0].pos) * v;
                        let q = xf.to_doc(screen);
                        if !(20.0..108.0).contains(&q[0]) || !(20.0..108.0).contains(&q[1]) {
                            continue;
                        }
                        let want = expected.at(q[0], q[1]);
                        let actual = image.pixels[y * image.size[0] + x].to_srgba_unmultiplied();
                        for c in 0..4 {
                            assert!((f32::from(actual[c]) / 255.0 - want[c]).abs() < 0.008, "flip {flip}, angle {rotation}, doc {q:?}: {actual:?} != {want:?}");
                        }
                        checked += 1;
                    }
                }
                assert!(checked > 20, "sample actual texture pixels across the shape");
                let texture = mesh.texture_id;
                let mut next = ctx.run_ui(egui::RawInput::default(), draw);
                let reuploaded = next.textures_delta.set.contains_key(&texture);
                next.textures_delta.clear();
                assert!(!reuploaded, "steady preview reuses its texture");
            }
        }
    }
}

#[test]
fn changed_shape_proxy_keeps_paint_masks_text_and_bounded_tiles() {
    use photocraft_doc::{Color, Fill, LayerMask, Pattern, PixelFormat, Size};
    for depth in [8, 16, 32] {
        for linked in [false, true] {
            for pattern in [false, true] {
                let (mut app, shape, text) = shape_below_text_size(depth, 32);
                let mut tile = photocraft_raster::Surface::new(PixelFormat::RGBA16);
                tile.fill_rect(Rect::new(0, 0, 8, 8), &[0.9, 0.1, 0.2, 1.0]);
                tile.fill_rect(Rect::new(4, 0, 8, 8), &[0.1, 0.3, 0.9, 1.0]);
                let p = Pattern::new("Proxy test", tile, 8, 8);
                let fill = if pattern {
                    Fill::Pattern { id: p.id.clone(), name: p.name.clone(), scale: 3.0, angle: 33.0, link: linked, phase: (7.0, 9.0) }
                } else {
                    let mut gradient = Fill::gradient(
                        vec![(0.0, Color::rgb(0.1, 0.2, 0.9)), (1.0, Color::rgb(0.9, 0.7, 0.1))],
                        17.0,
                        1.0,
                        photocraft_doc::GradientStyle::Linear,
                        false,
                    );
                    if let Fill::Gradient { align, .. } = &mut gradient {
                        *align = linked;
                    }
                    gradient
                };
                app.session
                    .edit("proxy test", |doc, _| {
                        doc.size = Size::new(7200, 5000);
                        doc.patterns.push(p.clone());
                        let layer = doc.layer_mut(shape).unwrap();
                        let LayerContent::Shape(s) = &mut layer.content else { panic!("shape") };
                        s.fill = Some(fill.clone());
                        s.path = photocraft_vector::shapes::rounded_rect(8.0, 8.0, 160.0, 160.0, [0.0; 4]);
                        let mut mask = LayerMask::reveal_all();
                        mask.linked = linked;
                        mask.surface.fill_rect(Rect::new(8, 8, 28, 160), &[0.0]);
                        layer.mask = Some(mask);
                        Ok(())
                    })
                    .unwrap();
                // Build the source cache once; the changed-preview path must never refresh it.
                let mut params = photocraft_engine::vector_cmds::shape_paint_params(Some(&fill), None);
                params["layer"] = json!(shape.0);
                app.session.execute("shape.edit", params).unwrap();
                app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 320.0));
                app.ui.views[0].zoom = 1.0;
                app.ui.views[0].center = [96.0, 96.0];
                app.ui.views[0].rotation = 31.0;
                app.ui.view.flip_horizontal = true;
                let source = app.session.active().unwrap().doc.clone();
                let history = app.session.active().unwrap().history.past_len();
                let ctx = egui::Context::default();
                begin(&mut app, &ctx).unwrap();
                app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 16.0, p[1] + 8.0]);
                follow_edits(&mut app);
                let frame = shape_proxy(&app, 0).unwrap();
                assert!(frame.k >= 4 && frame.region.width() <= 2048 && frame.region.height() <= 2048);
                assert_eq!(display_doc(&app, 0).unwrap().0.size, source.size, "document geometry stays original");
                assert_eq!(frame.doc.layers.iter().map(|l| l.id).collect::<Vec<_>>(), source.layers.iter().map(|l| l.id).collect::<Vec<_>>());
                let layer = frame.doc.layer(shape).unwrap();
                let cache = layer.surface().unwrap();
                assert!(cache.tiles().count() <= frame.region.tiles().count());
                let affine = rect_to_quad_affine(app.ui.transform.as_ref().unwrap().rect, app.ui.transform.as_ref().unwrap().quad).unwrap();
                let exact = photocraft_engine::transform_cmds::preview_shape_transform(&source, shape, affine, "bicubic").unwrap();
                let LayerContent::Shape(geometry) = &exact.layer(shape).unwrap().content else { panic!("shape") };
                let bounds = geometry.path.control_bounds().unwrap();
                let canvas = source.bounds();
                let paint = photocraft_vector::Paint::from_fill_with_patterns(
                    geometry.fill.as_ref().unwrap(),
                    bounds,
                    (f64::from(canvas.x0), f64::from(canvas.y0), f64::from(canvas.x1), f64::from(canvas.y1)),
                    &source.patterns,
                    source.mode,
                );
                let k = frame.k as i32;
                for (x, y) in [(12, 16), (20, 20), (28, 32)] {
                    let full = paint.at((f64::from(x) + 0.5) * f64::from(k), (f64::from(y) + 0.5) * f64::from(k));
                    let encoded =
                        photocraft_raster::Surface::with_default(source.pixel_format(), &photocraft_raster::from_rgba(&source.pixel_format(), full)).rgba(0, 0);
                    let actual = cache.rgba(x, y);
                    for c in 0..4 {
                        assert!((actual[c] - encoded[c]).abs() < 1e-4, "depth {depth}, pattern {pattern}, linked {linked}: {actual:?} != {encoded:?}");
                    }
                }
                // The mask's interior, including its linked translation, matches the exact command.
                for (x, y) in [(3, 12), (7, 12), (12, 12)] {
                    assert!(
                        (layer.mask.as_ref().unwrap().surface.pixel(x, y)[0]
                            - exact.layer(shape).unwrap().mask.as_ref().unwrap().surface.pixel(x * k, y * k)[0])
                            .abs()
                            < 1e-4
                    );
                }
                let text_surface = frame.doc.layer(text).unwrap().surface().unwrap();
                let black = text_surface
                    .content_bounds()
                    .tiles()
                    .flat_map(|tc| {
                        let r = tc.rect().intersect(&text_surface.content_bounds());
                        (r.y0..r.y1).flat_map(move |y| (r.x0..r.x1).map(move |x| (x, y)))
                    })
                    .filter(|&(x, y)| cache.rgba(x, y)[3] > 0.99 && layer.mask.as_ref().unwrap().surface.pixel(x, y)[0] > 0.99)
                    .max_by(|&(ax, ay), &(bx, by)| text_surface.rgba(ax, ay)[3].total_cmp(&text_surface.rgba(bx, by)[3]))
                    .unwrap();
                let coverage = text_surface.rgba(black.0, black.1)[3];
                assert!(coverage > 0.999, "fixture selects opaque text over visible shape pixels, depth {depth}: {coverage}");
                let pixel = photocraft_compose::render(&frame.doc, Rect::new(black.0, black.1, black.0 + 1, black.1 + 1)).px[0];
                assert!(
                    pixel[0] < 0.06 && pixel[1] < 0.06 && pixel[2] < 0.06 && pixel[3] > 0.99,
                    "higher opaque text stays black, depth {depth}, pattern {pattern}, linked {linked}, point {black:?}: {pixel:?}"
                );
                let base = app.transform_preview.as_ref().unwrap().shape.as_ref().unwrap().proxy.as_ref().unwrap().base.clone();
                app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 4.0, p[1]]);
                follow_edits(&mut app);
                assert!(Arc::ptr_eq(&base, &app.transform_preview.as_ref().unwrap().shape.as_ref().unwrap().proxy.as_ref().unwrap().base));
                assert_eq!(app.session.active().unwrap().history.past_len(), history);
                commit(&mut app);
                assert_eq!(app.session.active().unwrap().history.past_len(), history + 1);
                assert!(app.session.undo());
                assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
                assert!(app.session.redo());
            }
        }
    }
}

#[test]
fn pattern_preview_repeated_tile_uses_exact_shape_transform_after_a_proxy() {
    let (mut app, shape, _) = shape_below_text(8);
    app.session
        .edit("wide canvas", |doc, _| {
            doc.size = photocraft_doc::Size::new(4097, 1000);
            Ok(())
        })
        .unwrap();
    app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 320.0));
    app.ui.views[0].center = [64.0, 64.0];
    app.ui.views[0].zoom = 1.0;
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    scale_about_pivot(&mut app, 1.5, 1.5);
    follow_edits(&mut app);
    assert!(shape_proxy(&app, 0).is_some(), "start with a reduced shape preview");
    let source = app.session.active().unwrap().doc.clone();
    let transform = app.ui.transform.as_ref().unwrap();
    let affine = rect_to_quad_affine(transform.rect, transform.quad).unwrap();
    let expected = photocraft_engine::transform_cmds::preview_shape_transform(&source, shape, affine, &transform.interpolation).unwrap();

    app.ui.view.pattern_preview = true;
    app.ui.views[0].center = [-4097.0 + 64.0, 64.0];
    follow_edits(&mut app);
    assert!(shape_proxy(&app, 0).is_none(), "the original document is offscreen; copies must not reuse its empty proxy region");
    let (shown, _) = display_doc(&app, 0).unwrap();
    let region = Rect::new(0, 0, 96, 64);
    assert_eq!(
        photocraft_compose::render(&shown, region).px,
        photocraft_compose::render(&expected, region).px,
        "the repeated tile retains the shape and higher text"
    );
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc), "preview leaves committed pixels untouched");

    app.ui.view.pattern_preview = false;
    app.ui.views[0].center = [64.0, 64.0];
    follow_edits(&mut app);
    assert!(shape_proxy(&app, 0).is_some(), "normal canvas previews can resume their bounded proxy");
}

#[test]
fn shape_proxy_cpu_texture_follows_rotated_flipped_view() {
    let (mut app, _, _) = shape_below_text(8);
    app.session
        .edit("large canvas", |doc, _| {
            doc.size = photocraft_doc::Size::new(7200, 5000);
            Ok(())
        })
        .unwrap();
    app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 320.0));
    app.ui.views[0].center = [64.0, 64.0];
    app.ui.views[0].zoom = 1.0;
    app.ui.views[0].rotation = 33.0;
    app.ui.view.flip_horizontal = true;
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    scale_about_pivot(&mut app, 1.5, 1.5);
    follow_edits(&mut app);
    let frame = shape_proxy(&app, 0).unwrap();
    let xf = ViewXform::active(&app).unwrap();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        assert!(crate::canvas::paint_shape_proxy(&mut app, 0, &ctx, &ui.painter().with_clip_rect(xf.rect), &xf, None));
    });
    let mesh = out.shapes.iter().find_map(|s| if let egui::Shape::Mesh(m) = &s.shape { Some(m) } else { None }).unwrap();
    let delta = out.textures_delta.set.get(&mesh.texture_id).and_then(|u| u.last()).unwrap();
    let egui::ImageData::Color(image) = &delta.image;
    let image = image.clone();
    let vertices = mesh.vertices.clone();
    out.textures_delta.clear();
    let r = frame.region;
    let k = frame.k as f32;
    let expected = [(r.x0, r.y0), (r.x1, r.y0), (r.x1, r.y1), (r.x0, r.y1)].map(|(x, y)| xf.to_screen(x as f32 * k, y as f32 * k));
    assert_eq!(vertices.iter().map(|v| v.pos).collect::<Vec<_>>(), expected);
    assert_eq!(image.size, [r.width() as usize, r.height() as usize]);
    let actual = photocraft_compose::render(&frame.doc, r);
    let mut count = 0;
    for (got, want) in image.pixels.iter().zip(&actual.px) {
        if want[3] > 0.99 {
            for (c, channel) in want[..3].iter().enumerate() {
                assert!((f32::from(got.to_array()[c]) / 255.0 - channel).abs() < 0.008);
            }
            count += 1;
        }
    }
    assert!(count > 0, "texture contains actual composite pixels");
    let id = vertices.len();
    let mut next = ctx.run_ui(egui::RawInput::default(), |ui| {
        assert!(crate::canvas::paint_shape_proxy(&mut app, 0, &ctx, ui.painter(), &xf, None));
    });
    assert!(next.shapes.iter().any(|s| matches!(&s.shape,egui::Shape::Mesh(m) if m.vertices.len()==id)));
    let uploads = next.textures_delta.set.len();
    next.textures_delta.clear();
    assert_eq!(uploads, 0, "steady frame reuses composite texture");
}

fn proxy_shape_app(depth: u32, large: bool, mode: &str) -> (PhotocraftApp, LayerId) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.execute("file.new", json!({"width":512,"height":512,"depth":depth,"mode":mode,"background":"transparent"})).unwrap();
    app.sync_views();
    let shape = LayerId(app.session.execute("shape.create", json!({"kind":"rect","rect":[8,8,40,40],"fill":"#ffff00"})).unwrap()["layer"].as_u64().unwrap());
    if large {
        app.session
            .edit("large canvas", |doc, _| {
                doc.size = photocraft_doc::Size::new(7200, 5000);
                Ok(())
            })
            .unwrap();
    }
    app.session.edit_prefs(|p| p.performance.low_resolution_previews = true);
    app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 320.0));
    app.ui.views[0].zoom = 1.0;
    app.ui.views[0].center = [64.0, 64.0];
    app.ui.tool = Tool::Move;
    app.ui.extras.snap = false;
    (app, shape)
}

fn exact_shape_preview(app: &PhotocraftApp, shape: LayerId) -> Document {
    let t = app.ui.transform.as_ref().unwrap();
    let affine = rect_to_quad_affine(t.rect, t.quad).unwrap();
    let doc = photocraft_engine::transform_cmds::preview_shape_transform(&app.session.active().unwrap().doc, shape, affine, &t.interpolation).unwrap();
    photocraft_engine::mode_cmds::display_document(&doc).unwrap_or(doc)
}

#[test]
fn pattern_fill_layer_keeps_exact_shape_preview_and_pattern_frequency() {
    use photocraft_doc::{Fill, Layer, Pattern, PixelFormat};
    for depth in [8, 16, 32] {
        let (mut app, shape) = proxy_shape_app(depth, true, "rgb");
        let ctx = egui::Context::default();
        begin(&mut app, &ctx).unwrap();
        follow_edits(&mut app);
        assert!(shape_proxy(&app, 0).is_some());
        let mut tile = photocraft_raster::Surface::new(PixelFormat::RGBA16);
        tile.fill_rect(Rect::new(0, 0, 8, 1), &[0.9, 0.1, 0.2, 1.0]);
        tile.fill_rect(Rect::new(4, 0, 8, 1), &[0.1, 0.3, 0.9, 1.0]);
        let pattern = Pattern::new("Preview stripes", tile, 8, 1);
        app.session
            .edit("pattern background", |doc, _| {
                doc.patterns.push(pattern.clone());
                doc.layers.insert(
                    0,
                    Layer::new(
                        "Pattern",
                        LayerContent::Fill(Fill::Pattern {
                            id: pattern.id.clone(),
                            name: pattern.name.clone(),
                            scale: 1.0,
                            angle: 0.0,
                            link: false,
                            phase: (1.0, 0.0),
                        }),
                    ),
                );
                Ok(())
            })
            .unwrap();
        let source = app.session.active().unwrap().doc.clone();
        let history = app.session.active().unwrap().history.past_len();
        app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 8.0, p[1] + 8.0]);
        follow_edits(&mut app);
        assert!(shape_proxy(&app, 0).is_none(), "uncached pattern fill needs document-coordinate sampling");
        let shown = display_doc(&app, 0).unwrap().0;
        let area = Rect::new(120, 100, 136, 101);
        let pixels = photocraft_compose::render(&shown, area).px;
        assert_eq!(pixels, photocraft_compose::render(&exact_shape_preview(&app, shape), area).px);
        assert!((pixels[2][0] - pixels[6][0]).abs() > 0.5, "both stripe colours survive at the original period");
        let inside = photocraft_compose::render(&shown, Rect::new(28, 20, 29, 21)).px[0];
        assert!(inside[0] > 0.99 && inside[1] > 0.99 && inside[2] < 0.01 && inside[3] > 0.99);
        assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().history.past_len(), history);
        cancel(&mut app);
        assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    }
}

#[test]
fn extra_document_window_sees_shape_outside_main_preview_region() {
    let (mut app, shape) = proxy_shape_app(8, true, "rgb");
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    follow_edits(&mut app);
    let old_region = shape_proxy(&app, 0).unwrap().region;
    let mut extra_view = app.ui.views[0].clone();
    extra_view.center = [4020.0, 24.0];
    app.ui.windows.push(crate::state::DocWindow { id: 99, document: 0, view: extra_view, open: true, position: None });
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 4000.0, p[1]]);
    follow_edits(&mut app);
    assert!(shape_proxy(&app, 0).is_none(), "independently panned windows require complete staged shape pixels");
    assert!(!old_region.contains(4016 / 4, 16 / 4));
    let shown = display_doc(&app, 0).unwrap().0;
    let area = Rect::new(4016, 16, 4017, 17);
    let pixel = photocraft_compose::render(&shown, area).px[0];
    assert_eq!(pixel, photocraft_compose::render(&exact_shape_preview(&app, shape), area).px[0]);
    assert!(pixel[0] > 0.99 && pixel[1] > 0.99 && pixel[2] < 0.01 && pixel[3] > 0.99);
    app.ui.windows[0].open = false;
    follow_edits(&mut app);
    let hidden = shape_proxy(&app, 0).unwrap();
    assert_eq!(hidden.doc.layer(shape).unwrap().surface().unwrap().tiles().count(), 0, "closed extra view permits main-viewport clipping");
    app.ui.windows[0].open = true;
    follow_edits(&mut app);
    assert_eq!(photocraft_compose::render(&display_doc(&app, 0).unwrap().0, area).px[0], pixel, "reopening the view restores shape coverage");
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    assert_eq!(app.session.active().unwrap().history.past_len(), history);
    cancel(&mut app);
}

#[test]
fn shape_proxy_displays_duotone_inks_at_all_depths() {
    use photocraft_doc::{ColorMode, Duotone, DuotoneInk};
    for depth in [8, 16, 32] {
        let (mut app, shape) = proxy_shape_app(depth, true, "grayscale");
        app.session.execute("shape.edit", json!({"layer":shape.0,"fill":"#808080"})).unwrap();
        let inks = Duotone { inks: vec![DuotoneInk::new("Warm ink", [0.8, 0.1, 0.3])], psd_raw: None };
        app.session
            .edit("duotone inks", |doc, _| {
                doc.mode = ColorMode::Duotone;
                doc.duotone = Some(inks.clone());
                Ok(())
            })
            .unwrap();
        let source = app.session.active().unwrap().doc.clone();
        let history = app.session.active().unwrap().history.past_len();
        let ctx = egui::Context::default();
        begin(&mut app, &ctx).unwrap();
        app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 8.0, p[1] + 8.0]);
        follow_edits(&mut app);
        let frame = shape_proxy(&app, 0).unwrap();
        let pixel = photocraft_compose::render(&frame.doc, Rect::new(7, 5, 8, 6)).px[0];
        let k = frame.k as i32;
        let (x, y) = (7 * k + k / 2, 5 * k + k / 2);
        let exact = exact_shape_preview(&app, shape);
        let want = photocraft_compose::render(&exact, Rect::new(x, y, x + 1, y + 1)).px[0];
        for c in 0..4 {
            assert!((pixel[c] - want[c]).abs() < 0.008, "depth {depth}: ink composite {pixel:?} != exact {want:?}");
        }
        assert!(pixel[0] > pixel[1] + 0.2 && pixel[2] > pixel[1] + 0.05 && pixel[3] > 0.99, "preview uses ink colour, not stored gray");
        assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().history.past_len(), history);
        cancel(&mut app);
    }
}

#[test]
fn shape_proxy_invalid_cameras_fall_back_and_finite_extremes_stay_bounded() {
    let (mut app, shape) = proxy_shape_app(8, true, "rgb");
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 8.0, p[1] + 8.0]);
    let area = Rect::new(28, 20, 29, 21);
    let want = photocraft_compose::render(&exact_shape_preview(&app, shape), area).px;
    for case in 0..10 {
        app.ui.views[0].zoom = 1.0;
        app.ui.views[0].center = [64.0, 64.0];
        app.ui.views[0].rotation = 0.0;
        app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 320.0));
        follow_edits(&mut app);
        assert!(shape_proxy(&app, 0).is_some());
        match case {
            0 => app.ui.views[0].zoom = 0.0,
            1 => app.ui.views[0].zoom = -1.0,
            2 => app.ui.views[0].zoom = f32::NAN,
            3 => app.ui.views[0].zoom = f32::INFINITY,
            4 => app.ui.views[0].center[0] = f32::NAN,
            5 => app.ui.views[0].center[1] = f32::NEG_INFINITY,
            6 => app.ui.views[0].rotation = f32::NAN,
            7 => app.ui.views[0].rotation = f32::INFINITY,
            8 => app.last_canvas_rect.max.x = f32::NAN,
            _ => app.last_canvas_rect.max.y = f32::INFINITY,
        }
        follow_edits(&mut app);
        assert!(shape_proxy(&app, 0).is_none(), "invalid camera case {case} keeps an exact preview");
        assert_eq!(photocraft_compose::render(&display_doc(&app, 0).unwrap().0, area).px, want);
    }
    for (zoom, center) in [(f32::MIN_POSITIVE, [64.0, 64.0]), (1.0, [f32::MAX, f32::MAX]), (f32::MAX, [64.0, 64.0])] {
        app.ui.views[0].zoom = zoom;
        app.ui.views[0].center = center;
        app.ui.views[0].rotation = 0.0;
        app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(400.0, 320.0));
        follow_edits(&mut app);
        let frame = shape_proxy(&app, 0).unwrap();
        assert!(frame.doc.bounds().contains_rect(&frame.region));
        assert!(frame.region.width() <= 2048 && frame.region.height() <= 2048);
        assert!(frame.doc.layer(shape).unwrap().surface().unwrap().tiles().count() <= frame.region.tiles().count());
        if zoom == f32::MIN_POSITIVE {
            let pixel = photocraft_compose::render(&frame.doc, Rect::new(7, 5, 8, 6)).px[0];
            assert_eq!(pixel, want[0], "extreme zoom still renders visible shape pixels");
        }
    }
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    assert_eq!(app.session.active().unwrap().history.past_len(), history);
    cancel(&mut app);
}

#[test]
fn factor_one_shape_preview_is_viewport_bounded_and_recovers_after_pan_and_cancel() {
    let (mut app, shape) = proxy_shape_app(16, false, "rgb");
    app.last_canvas_rect = egui::Rect::from_min_size(Pos2::ZERO, vec2(48.0, 48.0));
    app.ui.views[0].center = [48.0, 48.0];
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 16.0, p[1] + 8.0]);
    follow_edits(&mut app);
    let first = shape_proxy(&app, 0).unwrap();
    assert_eq!(first.k, 1);
    assert!(first.region.width() <= 52 && first.region.height() <= 52);
    assert!(first.region.width() < source.size.width);
    let pixels = photocraft_compose::render(&first.doc, first.region).px;
    assert!(pixels.iter().any(|p| p[3] > 0.99), "bounded region contains the moving shape");
    assert_eq!(pixels, photocraft_compose::render(&exact_shape_preview(&app, shape), first.region).px);
    app.ui.views[0].center = [300.0, 300.0];
    follow_edits(&mut app);
    let away = shape_proxy(&app, 0).unwrap();
    assert_ne!(away.key, first.key);
    assert_eq!(away.doc.layer(shape).unwrap().surface().unwrap().tiles().count(), 0);
    app.ui.views[0].center = [48.0, 48.0];
    follow_edits(&mut app);
    let returned = shape_proxy(&app, 0).unwrap();
    assert_ne!(returned.key, away.key);
    assert_eq!(photocraft_compose::render(&returned.doc, first.region).px, pixels, "panning back restores discarded shape coverage");
    follow_edits(&mut app);
    assert!(Arc::ptr_eq(&returned.doc, &shape_proxy(&app, 0).unwrap().doc), "steady camera reuses the preview frame");
    assert_eq!(app.session.active().unwrap().history.past_len(), history);
    cancel(&mut app);
    assert!(shape_proxy(&app, 0).is_none());
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    begin(&mut app, &ctx).unwrap();
    follow_edits(&mut app);
    let reopened = shape_proxy(&app, 0).unwrap();
    assert_ne!(reopened.key, returned.key);
    assert_eq!(photocraft_compose::render(&reopened.doc, reopened.region).px, photocraft_compose::render(&source, reopened.region).px);
    cancel(&mut app);
}

#[test]
fn shape_proxy_cpu_canvas_refreshes_changed_quad_without_manual_follow() {
    let (mut app, _) = proxy_shape_app(8, true, "rgb");
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    let xf = ViewXform::active(&app).unwrap();
    let mut initial = ctx.run_ui(egui::RawInput::default(), |ui| {
        assert!(crate::canvas::paint_shape_proxy(&mut app, 0, &ctx, ui.painter(), &xf, None));
    });
    initial.textures_delta.clear();
    let before = shape_proxy(&app, 0).unwrap();
    let area = Rect::new(22, 5, 23, 6);
    assert_eq!(photocraft_compose::render(&before.doc, area).px[0][3], 0.0);
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 64.0, p[1]]);
    // The canvas hook, rather than the test, must refresh the changed transform.
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        assert!(crate::canvas::paint_shape_proxy(&mut app, 0, &ctx, ui.painter(), &xf, None));
    });
    let frame = shape_proxy(&app, 0).unwrap();
    assert_ne!(before.key, frame.key);
    let mesh = output.shapes.iter().find_map(|s| if let egui::Shape::Mesh(m) = &s.shape { Some(m) } else { None }).unwrap();
    let upload = output.textures_delta.set.get(&mesh.texture_id).and_then(|u| u.last()).unwrap();
    let egui::ImageData::Color(image) = &upload.image;
    let offset = (5 - frame.region.y0) as usize * image.size[0] + (22 - frame.region.x0) as usize;
    assert_eq!(image.pixels[offset], Color32::YELLOW, "the actual newly uploaded canvas image contains the moved shape");
    output.textures_delta.clear();
    let pixel = photocraft_compose::render(&frame.doc, area).px[0];
    assert!(pixel[0] > 0.99 && pixel[1] > 0.99 && pixel[2] < 0.01 && pixel[3] > 0.99);
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    assert_eq!(app.session.active().unwrap().history.past_len(), history);
    cancel(&mut app);
}

#[test]
fn shape_proxy_canvas_refreshes_pattern_pixels_when_source_revision_changes() {
    use photocraft_doc::{Pattern, PixelFormat};
    let (mut app, shape) = proxy_shape_app(8, true, "rgb");
    let mut tile = photocraft_raster::Surface::new(PixelFormat::RGBA16);
    tile.fill_rect(Rect::new(0, 0, 8, 8), &[1.0, 0.0, 0.0, 1.0]);
    let pattern = Pattern::new("Revision tile", tile, 8, 8);
    app.session.patterns.items.push(pattern.clone());
    app.session.execute("shape.edit", json!({"layer":shape.0,"fill":{"pattern":pattern.name,"id":pattern.id,"scale":100}})).unwrap();
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 64.0, p[1]]);
    let xf = ViewXform::active(&app).unwrap();
    let paint = |app: &mut PhotocraftApp| {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(crate::canvas::paint_shape_proxy(app, 0, &ctx, ui.painter(), &xf, None));
        });
        let frame = shape_proxy(app, 0).unwrap();
        let mesh = output.shapes.iter().find_map(|s| if let egui::Shape::Mesh(m) = &s.shape { Some(m) } else { None }).unwrap();
        let upload = output.textures_delta.set.get(&mesh.texture_id).and_then(|u| u.last()).unwrap();
        let egui::ImageData::Color(image) = &upload.image;
        let offset = (5 - frame.region.y0) as usize * image.size[0] + (22 - frame.region.x0) as usize;
        let pixel = image.pixels[offset];
        output.textures_delta.clear();
        (frame, pixel)
    };
    let (red, red_upload) = paint(&mut app);
    assert_eq!(red_upload, Color32::RED);
    app.session
        .edit("replace pattern pixels", |doc, _| {
            let embedded = doc.patterns.iter_mut().find(|p| p.id == pattern.id).unwrap();
            embedded.surface.fill_rect(Rect::new(0, 0, 8, 8), &[0.0, 0.0, 1.0, 1.0]);
            Ok(())
        })
        .unwrap();
    let blue_source = app.session.active().unwrap().doc.clone();
    let (blue, blue_upload) = paint(&mut app);
    assert_ne!(red.key, blue.key);
    assert_eq!(blue_upload, Color32::BLUE, "same pattern id must use the new source revision's pixels");
    let area = Rect::new(22, 5, 23, 6);
    assert_eq!(photocraft_compose::render(&red.doc, area).px[0], [1.0, 0.0, 0.0, 1.0], "old preview remains a COW snapshot");
    assert_eq!(photocraft_compose::render(&blue.doc, area).px[0], [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(photocraft_compose::render(&exact_shape_preview(&app, shape), Rect::new(90, 22, 91, 23)).px[0], [0.0, 0.0, 1.0, 1.0]);
    assert!(Arc::ptr_eq(&blue_source, &app.session.active().unwrap().doc));
    assert_eq!(app.session.active().unwrap().history.past_len(), history + 1, "only the source edit adds history");
    cancel(&mut app);
    assert!(app.session.undo());
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    assert!(app.session.redo());
    assert!(Arc::ptr_eq(&blue_source, &app.session.active().unwrap().doc));
}

#[test]
fn huge_linked_mask_transform_renders_only_visible_preview_pixels() {
    for depth in [8, 16, 32] {
        let (mut app, shape) = proxy_shape_app(depth, true, "rgb");
        app.session
            .edit("sparse linked mask", |doc, _| {
                let mut mask = photocraft_doc::LayerMask::reveal_all();
                mask.linked = true;
                mask.surface.fill_rect(Rect::new(16, 8, 32, 48), &[0.0]);
                doc.layer_mut(shape).unwrap().mask = Some(mask);
                Ok(())
            })
            .unwrap();
        let source = app.session.active().unwrap().doc.clone();
        let history = app.session.active().unwrap().history.past_len();
        let ctx = egui::Context::default();
        begin(&mut app, &ctx).unwrap();
        let starting_quad = app.ui.transform.as_ref().unwrap().quad;
        // The viewport maps wholly inside either the hidden stripe or the clear part.
        // A full destination-mask warp would cover billions of pixels: never use it as an oracle.
        for (source_x, expected_mask) in [(20.0, 0.0), (40.0, 1.0), (20.0, 0.0)] {
            app.ui.transform.as_mut().unwrap().quad = starting_quad.map(|p| [(p[0] - source_x) * 10_000.0 + 64.0, (p[1] - 20.0) * 10_000.0 + 64.0]);
            follow_edits(&mut app);
            let frame = shape_proxy(&app, 0).unwrap();
            let layer = frame.doc.layer(shape).unwrap();
            let mask = layer.mask.as_ref().unwrap();
            assert!(mask.linked);
            assert!(frame.region.width() <= 2048 && frame.region.height() <= 2048);
            assert!(mask.surface.tiles().count() <= frame.region.inflate(4).tiles().count(), "huge affine still bounds linked-mask work to the visible region");
            assert!(layer.surface().unwrap().tiles().count() <= frame.region.tiles().count());
            for (x, y) in [(5, 5), (20, 16), (32, 24)] {
                assert!(frame.region.contains(x, y));
                assert!(
                    (mask.surface.pixel(x, y)[0] - expected_mask).abs() < 1e-4,
                    "depth {depth}, source x {source_x}: inverse sample must stay in the same mask interior"
                );
                let pixel = photocraft_compose::render(&frame.doc, Rect::new(x, y, x + 1, y + 1)).px[0];
                if expected_mask == 0.0 {
                    assert!(pixel[3] < 1e-4, "hidden mask interior remains transparent: {pixel:?}");
                } else {
                    assert!(
                        pixel[0] > 0.99 && pixel[1] > 0.99 && pixel[2] < 0.01 && pixel[3] > 0.99,
                        "clear interior retains the shape's yellow fill: {pixel:?}"
                    );
                }
            }
        }
        assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().history.past_len(), history);
        cancel(&mut app);
    }
}

#[test]
fn shape_proxy_stays_with_its_document_when_layer_ids_overlap() {
    let (mut app, shape) = proxy_shape_app(8, true, "rgb");
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 8.0, p[1] + 8.0]);
    follow_edits(&mut app);
    let frame = shape_proxy(&app, 0).unwrap();
    let preview = app.transform_preview.as_ref().unwrap().doc.clone();
    let quad = app.ui.transform.as_ref().unwrap().quad;
    let xf = ViewXform::active(&app).unwrap();
    let idx = app.session.add_document((*source).clone(), None);
    assert_eq!(idx, 1);
    assert_ne!(app.session.active().unwrap().doc.id, source.id);
    assert!(app.session.active().unwrap().doc.layer(shape).is_some());
    app.session.execute("shape.edit", json!({"layer":shape.0,"fill":"#0000ff"})).unwrap();
    let second = app.session.active().unwrap().doc.clone();
    let second_history = app.session.active().unwrap().history.past_len();
    let view = app.ui.views[0].clone();
    app.ui.views.push(view);
    // Simulate the canvas read between activation and the normal end_if_left cleanup.
    assert!(shape_proxy(&app, idx).is_none(), "a reused layer id cannot expose another document's preview");
    assert!(shape_proxy(&app, 0).is_none());
    follow_edits(&mut app);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        assert!(!crate::canvas::paint_shape_proxy(&mut app, idx, &ctx, ui.painter(), &xf, None));
    });
    assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default())));
    output.textures_delta.clear();
    assert!(Arc::ptr_eq(&preview, &app.transform_preview.as_ref().unwrap().doc));
    assert_eq!(app.ui.transform.as_ref().unwrap().quad, quad);
    assert!(Arc::ptr_eq(&source, &app.session.documents()[0].doc));
    assert!(Arc::ptr_eq(&second, &app.session.documents()[idx].doc));
    assert_eq!(app.session.documents()[0].history.past_len(), history);
    assert_eq!(app.session.documents()[idx].history.past_len(), second_history);
    let blue = photocraft_compose::render(&second, Rect::new(30, 22, 31, 23)).px[0];
    assert_eq!(blue, [0.0, 0.0, 1.0, 1.0]);
    assert!(app.session.set_active(0));
    follow_edits(&mut app);
    let returned = shape_proxy(&app, 0).unwrap();
    assert_eq!(returned.key, frame.key);
    assert!(Arc::ptr_eq(&returned.doc, &frame.doc), "returning to the owner resumes its cached preview");
    let yellow = photocraft_compose::render(&returned.doc, Rect::new(7, 5, 8, 6)).px[0];
    assert_eq!(yellow, [1.0, 1.0, 0.0, 1.0]);
    assert_eq!(app.session.active().unwrap().history.past_len(), history);
    cancel(&mut app);
}

#[test]
fn wide_sparse_stationary_disabled_and_unlinked_masks_keep_exact_preview() {
    use photocraft_doc::{Layer, LayerMask};
    for depth in [8, 16, 32] {
        for case in 0..3 {
            let (mut app, shape) = proxy_shape_app(depth, true, "rgb");
            let moving_unlinked = case == 2;
            let disabled = case == 1;
            let masked = app
                .session
                .edit("wide sparse mask", |doc, _| {
                    let mut mask = LayerMask::reveal_all();
                    mask.linked = !moving_unlinked;
                    mask.enabled = !disabled;
                    let stripe = if moving_unlinked { Rect::new(80, 8, 96, 48) } else { Rect::new(16, 8, 32, 48) };
                    mask.surface.fill_rect(stripe, &[0.0]);
                    mask.surface.write_pixel(32768, 20, &[0.0]);
                    if moving_unlinked {
                        doc.layer_mut(shape).unwrap().mask = Some(mask);
                        Ok(shape)
                    } else {
                        let mut pixels = photocraft_raster::Surface::new(doc.pixel_format());
                        pixels.fill_rect(Rect::new(8, 8, 48, 48), &[1.0, 0.0, 0.0, 1.0]);
                        let mut layer = Layer::new("Stationary red", LayerContent::Raster(pixels));
                        layer.mask = Some(mask);
                        let id = layer.id;
                        doc.layers.insert(0, layer);
                        Ok(id)
                    }
                })
                .unwrap();
            let source = app.session.active().unwrap().doc.clone();
            let history = app.session.active().unwrap().history.past_len();
            let original_mask = &source.layer(masked).unwrap().mask.as_ref().unwrap().surface;
            assert!(original_mask.tiles().count() <= 2, "fixture has no dense distant allocation");
            let bounds = original_mask.content_bounds();
            let reduced_width = bounds.width().div_ceil(4);
            let reduced_height = bounds.height().div_ceil(4);
            assert!(reduced_width > 2048 && reduced_width * reduced_height < 2048 * 2048, "elongated source proves a side limit, independently of area");
            let ctx = egui::Context::default();
            begin(&mut app, &ctx).unwrap();
            app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 64.0, p[1]]);
            follow_edits(&mut app);
            assert!(shape_proxy(&app, 0).is_none(), "case {case}: every mask that proxy_document would read requires bounded source dimensions");
            let shown = display_doc(&app, 0).unwrap().0;
            assert_eq!(shown.size, source.size);
            assert_eq!(
                &shown.layer(masked).unwrap().mask.as_ref().unwrap().surface,
                original_mask,
                "stationary or unlinked mask pixels retain document coordinates"
            );
            assert_eq!(shown.layer(masked).unwrap().mask.as_ref().unwrap().surface.pixel(32768, 20)[0], 0.0);
            let hidden_x = if moving_unlinked { 84 } else { 20 };
            let hidden = photocraft_compose::render(&shown, Rect::new(hidden_x, 20, hidden_x + 1, 21)).px[0];
            if disabled {
                assert_eq!(hidden, [1.0, 0.0, 0.0, 1.0], "disabled mask keeps its stationary layer visible");
            } else {
                assert_eq!(hidden[3], 0.0, "visible hidden stripe stays fixed while the shape moves");
            }
            let clear_x = if moving_unlinked { 104 } else { 36 };
            let clear = photocraft_compose::render(&shown, Rect::new(clear_x, 20, clear_x + 1, 21)).px[0];
            assert_eq!(clear, if moving_unlinked { [1.0, 1.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0, 1.0] });
            assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
            assert_eq!(app.session.active().unwrap().history.past_len(), history);
            cancel(&mut app);
        }
    }
}

#[test]
fn wide_sparse_selection_keeps_exact_shape_preview_without_reduction() {
    for depth in [8, 16, 32] {
        let (mut app, shape) = proxy_shape_app(depth, true, "rgb");
        app.session
            .edit("wide sparse selection", |doc, _| {
                let mut selection = photocraft_raster::Surface::new(photocraft_doc::PixelFormat::GRAY8);
                selection.fill_rect(Rect::new(8, 8, 48, 48), &[1.0]);
                selection.write_pixel(32768, 20, &[1.0]);
                doc.selection = Some(selection);
                Ok(())
            })
            .unwrap();
        let source = app.session.active().unwrap().doc.clone();
        let history = app.session.active().unwrap().history.past_len();
        let selection = source.selection.as_ref().unwrap();
        assert!(selection.tiles().count() <= 2);
        let bounds = selection.content_bounds();
        assert!(bounds.width().div_ceil(4) > 2048 && bounds.width().div_ceil(4) * bounds.height().div_ceil(4) < 2048 * 2048);
        let ctx = egui::Context::default();
        begin(&mut app, &ctx).unwrap();
        app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 64.0, p[1]]);
        follow_edits(&mut app);
        assert!(shape_proxy(&app, 0).is_none(), "selections are also proxy_document downsample sources");
        let shown = display_doc(&app, 0).unwrap().0;
        assert_eq!(shown.size, source.size);
        assert_eq!(shown.selection.as_ref().unwrap(), selection);
        let area = Rect::new(90, 20, 91, 21);
        let pixel = photocraft_compose::render(&shown, area).px[0];
        assert_eq!(pixel, [1.0, 1.0, 0.0, 1.0]);
        assert_eq!(pixel, photocraft_compose::render(&exact_shape_preview(&app, shape), area).px[0]);
        assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().history.past_len(), history);
        cancel(&mut app);
    }
}

#[test]
fn shape_proxy_scales_effective_feather_before_reduction_and_keeps_mask_flags() {
    use photocraft_doc::{Layer, LayerMask};
    let (mut app, shape) = proxy_shape_app(8, true, "rgb");
    let (group, child) = app
        .session
        .edit("imported mask feather", |doc, _| {
            let mut moving = LayerMask::reveal_all();
            moving.feather = 2000.0;
            moving.density = 0.75;
            doc.layer_mut(shape).unwrap().mask = Some(moving);
            let mut leaf = Layer::raster("Stationary leaf", doc.pixel_format());
            let mut normal = LayerMask::reveal_all();
            normal.feather = 2.0;
            normal.density = 0.5;
            normal.linked = false;
            leaf.mask = Some(normal);
            let child_id = leaf.id;
            let mut inner = Layer::group("Stationary inner", vec![leaf]);
            let mut imported = LayerMask::reveal_all();
            imported.feather = 2000.0;
            imported.density = 0.4;
            imported.enabled = false;
            imported.linked = false;
            inner.mask = Some(imported);
            let group_id = inner.id;
            doc.layers.insert(0, Layer::group("Outer", vec![inner]));
            Ok((group_id, child_id))
        })
        .unwrap();
    let source = app.session.active().unwrap().doc.clone();
    let history = app.session.active().unwrap().history.past_len();
    let ctx = egui::Context::default();
    begin(&mut app, &ctx).unwrap();
    app.ui.transform.as_mut().unwrap().quad = app.ui.transform.as_ref().unwrap().quad.map(|p| [p[0] + 8.0, p[1] + 8.0]);
    follow_edits(&mut app);
    let frame = shape_proxy(&app, 0).unwrap();
    assert_eq!(frame.k, 4);
    for (id, raw, expected) in [(shape, 2000.0, 250.0), (group, 2000.0, 250.0), (child, 2.0, 0.5)] {
        let before = source.layer(id).unwrap().mask.as_ref().unwrap();
        let shown = frame.doc.layer(id).unwrap().mask.as_ref().unwrap();
        assert_eq!(before.feather, raw, "imported source metadata remains intact");
        assert_eq!(shown.feather, expected, "the committed compositor's effective radius is reduced, not the imported raw value");
        assert_eq!(photocraft_compose::masks::feather_sigma(shown.feather), expected);
        assert_eq!((shown.enabled, shown.linked, shown.density), (before.enabled, before.linked, before.density));
        assert_eq!(shown.surface, before.surface);
        assert_eq!(shown.surface.tiles().count(), 0, "constant masks avoid an expensive full-size feather oracle");
    }
    let pixel = frame.doc.layer(shape).unwrap().surface().unwrap().rgba(7, 5);
    assert_eq!(pixel, [1.0, 1.0, 0.0, 1.0], "actual proxy cache contains the moved shape");
    assert!(Arc::ptr_eq(&source, &app.session.active().unwrap().doc));
    assert_eq!(app.session.active().unwrap().history.past_len(), history);
    cancel(&mut app);
}
