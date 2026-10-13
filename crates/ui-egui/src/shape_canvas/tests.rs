use super::*;
use crate::canvas::tool_event;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width":200,"height":160,"background":"transparent"})).unwrap();
    app.sync_views();
    app.run("shape.create", json!({"kind":"roundedRect","rect":[20,20,120,100],"radii":[5,10,15,20]})).unwrap();
    app.ui.tool = Tool::Rectangle;
    app
}

fn radius(app: &PhotocraftApp) -> [f64; 4] {
    rectangle(app).unwrap().radii
}

fn knob(app: &PhotocraftApp, corner: usize) -> [f64; 2] {
    let r = rectangle(app).unwrap();
    handles(r.rect, r.radii, f64::from(app.point_zoom()))[corner]
}

fn press(app: &mut PhotocraftApp, p: [f64; 2], mods: Modifiers) {
    tool_event(app, ToolEvent::Down { x: p[0], y: p[1], pressure: 1.0 }, mods);
    assert!(app.shape_canvas.drag.is_some());
    assert!(app.drag.is_none(), "a radius knob must not also start a shape/move drag");
}

fn move_to(app: &mut PhotocraftApp, p: [f64; 2], mods: Modifiers) {
    tool_event(app, ToolEvent::Move { x: p[0], y: p[1], pressure: 1.0 }, mods);
}

fn release(app: &mut PhotocraftApp, p: [f64; 2], mods: Modifiers) {
    tool_event(app, ToolEvent::Up { x: p[0], y: p[1] }, mods);
    assert!(app.shape_canvas.drag.is_none());
}

#[test]
fn linked_radius_is_live_and_one_undo_per_gesture() {
    let mut a = app();
    let before = a.session.active().unwrap().doc.clone();
    let n = a.session.active().unwrap().history.past_len();
    let p = knob(&a, 0);
    press(&mut a, p, Modifiers::NONE);
    release(&mut a, p, Modifiers::NONE);
    assert_eq!(a.session.active().unwrap().doc, before, "clicking a knob preserves unlinked radii");
    press(&mut a, p, Modifiers::NONE);
    for d in [2.0, 4.0, 8.0] {
        move_to(&mut a, [p[0] + d, p[1] + d], Modifiers::NONE);
    }
    let live = radius(&a);
    assert!(live[0] > 5.0 && live.iter().all(|r| *r == live[0]));
    release(&mut a, [p[0] + 8.0, p[1] + 8.0], Modifiers::NONE);
    assert_eq!(a.session.active().unwrap().history.past_len(), n + 1);
    let after = a.session.active().unwrap().doc.clone();
    a.run("edit.undo", json!({})).unwrap();
    assert_eq!(a.session.active().unwrap().doc, before);
    a.run("edit.redo", json!({})).unwrap();
    assert_eq!(a.session.active().unwrap().doc, after);
    let p = knob(&a, 0);
    press(&mut a, p, Modifiers::NONE);
    release(&mut a, [p[0] - 2.0, p[1] - 2.0], Modifiers::NONE);
    assert_eq!(a.session.active().unwrap().history.past_len(), n + 2);
}

#[test]
fn alt_edits_only_grabbed_corner_and_limits_radius() {
    let mut a = app();
    a.ui.tool = Tool::Move;
    let count = a.session.active().unwrap().doc.layers.len();
    let p = knob(&a, 1);
    press(&mut a, p, Modifiers::ALT);
    release(&mut a, [p[0] - 200.0, p[1] + 200.0], Modifiers::ALT);
    assert_eq!(radius(&a), [5.0, 50.0, 15.0, 20.0]);
    assert_eq!(a.session.active().unwrap().doc.layers.len(), count, "Alt on a radius knob must not duplicate a layer");
}

#[test]
fn radius_handles_follow_rotated_flipped_scaled_canvas() {
    let mut a = app();
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
    for rotation in [0.0, 43.0, -90.0, 180.0] {
        for flip in [false, true] {
            for zoom in [0.25, 1.0, 4.0] {
                a.ui.views[0].zoom = zoom;
                let xf = ViewXform {
                    aspect: 1.0,
                    rect: egui::Rect::from_min_size(egui::pos2(70.0, 30.0), egui::vec2(600.0, 450.0)),
                    zoom,
                    center: [80.0, 70.0],
                    flip,
                    rotation,
                };
                let expected = handles([20.0, 20.0, 120.0, 100.0], [5.0, 10.0, 15.0, 20.0], f64::from(zoom));
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    draw_overlay(&a, &ui.ctx().layer_painter(egui::LayerId::background()), &xf);
                });
                output.textures_delta.clear();
                let circles: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        egui::Shape::Circle(c) if c.radius == 4.0 => Some(c.center),
                        _ => None,
                    })
                    .collect();
                assert_eq!(circles.len(), 4);
                for (i, p) in expected.into_iter().enumerate() {
                    let screen = xf.to_screen(p[0] as f32, p[1] as f32);
                    assert!(circles[i].distance(screen) < 0.001);
                    let doc = xf.to_doc(screen);
                    assert_eq!(hit([20.0, 20.0, 120.0, 100.0], [5.0, 10.0, 15.0, 20.0], doc, f64::from(zoom), f64::from(xf.aspect)), Some(i));
                    assert_eq!(cursor(&a, doc), Some(egui::CursorIcon::PointingHand));
                }
            }
        }
    }
}

#[test]
fn radius_hit_area_is_six_screen_points_with_pixel_aspect_correction() {
    let rect = [20.0, 20.0, 120.0, 100.0];
    let radii = [5.0, 10.0, 15.0, 20.0];
    for aspect in [0.5, 1.0, 2.0] {
        for rotation in [0.0, 43.0, -90.0] {
            for flip in [false, true] {
                let xf = ViewXform {
                    rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 450.0)),
                    zoom: 2.0,
                    center: [80.0, 70.0],
                    flip,
                    rotation,
                    aspect,
                };
                let p = handles(rect, radii, f64::from(xf.zoom))[0];
                let screen = xf.to_screen(p[0] as f32, p[1] as f32);
                for direction in [egui::vec2(1.0, 0.0), egui::vec2(0.0, 1.0)] {
                    for (distance, expected) in [(5.0, Some(0)), (7.0, None)] {
                        let doc = xf.to_doc(screen + direction * distance);
                        assert_eq!(
                            hit(rect, radii, doc, f64::from(xf.zoom), f64::from(xf.aspect)),
                            expected,
                            "aspect {aspect}, rotation {rotation}, flip {flip}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn native_anamorphic_radius_press_matches_its_hover_area() {
    let (mut a, ctx) = canvas_app();
    crate::menus::invoke(&mut a, &ctx, "view.pixelAspectRatio.anamorphic2To1", json!({})).unwrap();
    for rotation in [0.0, 43.0, -90.0] {
        a.ui.views[0].rotation = rotation;
        for flip in [false, true] {
            a.ui.view.flip_horizontal = flip;
            canvas_frame(&mut a, &ctx, vec![]);
            let before = a.session.active().unwrap().doc.clone();
            let history = a.session.active().unwrap().history.past_len();
            let xf = ViewXform::active(&a).unwrap();
            let p = knob(&a, 0);
            let screen = xf.to_screen(p[0] as f32, p[1] as f32);
            let inside = screen + egui::vec2(5.0, 0.0);
            let outside = screen + egui::vec2(7.0, 0.0);
            assert_eq!(cursor(&a, xf.to_doc(inside)), Some(egui::CursorIcon::PointingHand));
            assert_eq!(cursor(&a, xf.to_doc(outside)), None);
            canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(inside), button(inside, true, Modifiers::NONE)]);
            assert!(dragging(&a), "a visible radius grip owns the press, rotation {rotation}, flip {flip}");
            assert!(a.drag.is_none() && a.ui.transform.is_none());
            canvas_frame(&mut a, &ctx, vec![button(inside, false, Modifiers::NONE)]);
            assert!(!dragging(&a));
            assert_eq!(a.session.active().unwrap().doc, before);
            assert_eq!(a.session.active().unwrap().history.past_len(), history);
        }
    }
}

#[test]
fn locks_and_mask_targets_prevent_radius_gestures() {
    for lock in ["all", "pixels", "position"] {
        let mut a = app();
        let p = knob(&a, 0);
        let id = a.session.active().unwrap().active_layer.unwrap();
        a.run("layer.setProps", json!({"layer":id.0,"locks":{lock:true}})).unwrap();
        let before = a.session.active().unwrap().doc.clone();
        assert!(!pointer(&mut a, ToolEvent::Down { x: p[0], y: p[1], pressure: 1.0 }, Modifiers::NONE));
        assert_eq!(a.session.active().unwrap().doc, before);
    }
    for vector_mask in [false, true] {
        let mut a = app();
        let p = knob(&a, 0);
        a.ui.mask_target = !vector_mask;
        a.ui.vector_mask_target = vector_mask;
        assert!(!pointer(&mut a, ToolEvent::Down { x: p[0], y: p[1], pressure: 1.0 }, Modifiers::NONE));
    }
}

#[test]
fn escape_restores_radius_and_context_changes_never_edit_another_document() {
    let mut a = app();
    let before = a.session.active().unwrap().doc.clone();
    let p = knob(&a, 0);
    press(&mut a, p, Modifiers::NONE);
    move_to(&mut a, [p[0] + 4.0, p[1] + 4.0], Modifiers::NONE);
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }],
            ..Default::default()
        },
        |ui| {
            assert!(keys(&mut a, ui.ctx()));
        },
    );
    output.textures_delta.clear();
    assert_eq!(a.session.active().unwrap().doc, before);
    assert!(a.shape_canvas.drag.is_none());

    press(&mut a, p, Modifiers::NONE);
    move_to(&mut a, [p[0] + 4.0, p[1] + 4.0], Modifiers::NONE);
    a.run("file.new", json!({"width":100,"height":100})).unwrap();
    let other = a.session.active().unwrap().doc.clone();
    cancel_if_left(&mut a);
    assert!(a.shape_canvas.drag.is_none());
    assert!(!pointer(&mut a, ToolEvent::Up { x: p[0] + 40.0, y: p[1] + 40.0 }, Modifiers::NONE));
    assert_eq!(a.session.active().unwrap().doc, other);
}

#[test]
fn tool_switch_and_unrelated_edits_end_radius_gesture() {
    let mut a = app();
    let p = knob(&a, 0);
    press(&mut a, p, Modifiers::NONE);
    a.ui.tool = Tool::Brush;
    cancel_if_left(&mut a);
    assert!(a.shape_canvas.drag.is_none());
    a.ui.tool = Tool::Rectangle;
    press(&mut a, p, Modifiers::NONE);
    move_to(&mut a, [p[0] + 4.0, p[1] + 4.0], Modifiers::NONE);
    a.run("layer.setProps", json!({"name":"Unrelated edit"})).unwrap();
    let edited = a.session.active().unwrap().doc.clone();
    cancel(&mut a);
    assert_eq!(a.session.active().unwrap().doc, edited, "a stale gesture cannot undo another action");
}

fn canvas_frame(app: &mut PhotocraftApp, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<egui::epaint::ClippedShape> {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 400.0));
    app.last_canvas_rect = rect;
    let view = app.ui.views[0].clone();
    let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), events, ..Default::default() }, |ui| {
        app.ui.views[0] = crate::canvas::canvas_view(app, ui, 0, rect, view.clone(), true);
    });
    out.textures_delta.clear();
    out.shapes
}

fn canvas_app() -> (PhotocraftApp, egui::Context) {
    let mut app = app();
    app.ui.views[0].zoom = 1.0;
    app.ui.views[0].center = [100.0, 80.0];
    app.ui.views[0].fit_pending = false;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
    canvas_frame(&mut app, &ctx, vec![]);
    (app, ctx)
}

fn button(pos: egui::Pos2, pressed: bool, modifiers: Modifiers) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers }
}

fn space(pressed: bool) -> egui::Event {
    egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
}

#[test]
fn native_space_navigation_never_grabs_radius_or_leaves_a_hover_edit() {
    let (mut a, ctx) = canvas_app();
    let before = a.session.active().unwrap().doc.clone();
    let n = a.session.active().unwrap().history.past_len();
    let xf = ViewXform::active(&a).unwrap();
    let p = knob(&a, 0);
    let screen = xf.to_screen(p[0] as f32, p[1] as f32);
    let moved = screen + egui::vec2(35.0, 24.0);
    canvas_frame(&mut a, &ctx, vec![space(true), egui::Event::PointerMoved(screen), button(screen, true, Modifiers::NONE)]);
    assert!(!dragging(&a), "Space+press belongs to navigation even over a radius knob");
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(moved)]);
    canvas_frame(&mut a, &ctx, vec![button(moved, false, Modifiers::NONE)]);
    canvas_frame(&mut a, &ctx, vec![space(false)]);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(moved + egui::vec2(40.0, 25.0))]);
    assert!(!dragging(&a));
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), n);
}

#[test]
fn native_space_interrupts_radius_drag_before_navigation_takes_release() {
    let (mut a, ctx) = canvas_app();
    let xf = ViewXform::active(&a).unwrap();
    let p = knob(&a, 0);
    let screen = xf.to_screen(p[0] as f32, p[1] as f32);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(screen), button(screen, true, Modifiers::NONE)]);
    assert!(dragging(&a));
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(screen + egui::vec2(2.0, 2.0))]);
    let changed = a.session.active().unwrap().doc.clone();
    let n = a.session.active().unwrap().history.past_len();
    canvas_frame(&mut a, &ctx, vec![space(true)]);
    assert!(!dragging(&a), "navigation ends a radius gesture without waiting for its consumed release");
    let moved = screen + egui::vec2(30.0, 25.0);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(moved), button(moved, false, Modifiers::NONE)]);
    canvas_frame(&mut a, &ctx, vec![space(false), egui::Event::PointerMoved(moved + egui::vec2(25.0, 25.0))]);
    assert_eq!(a.session.active().unwrap().doc, changed);
    assert_eq!(a.session.active().unwrap().history.past_len(), n);
}

#[test]
fn navigation_tool_override_cannot_start_or_keep_radius_gesture() {
    for tool in [Tool::Hand, Tool::Zoom, Tool::RotateView] {
        let mut a = app();
        let p = knob(&a, 0);
        press(&mut a, p, Modifiers::NONE);
        a.tool_override = Some(tool);
        cancel_if_left(&mut a);
        assert!(!dragging(&a));
        assert!(!pointer(&mut a, ToolEvent::Down { x: p[0], y: p[1], pressure: 1.0 }, Modifiers::NONE));
        assert_eq!(cursor(&a, p), None);
    }
}

#[test]
fn native_radius_press_precedes_move_transform_and_egui_drag_threshold() {
    let (mut a, ctx) = canvas_app();
    a.ui.tool = Tool::Move;
    a.ui.tool_options.move_show_transform = true;
    let n = a.session.active().unwrap().history.past_len();
    let xf = ViewXform::active(&a).unwrap();
    let p = knob(&a, 0);
    let screen = xf.to_screen(p[0] as f32, p[1] as f32);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(screen), button(screen, true, Modifiers::NONE)]);
    assert!(dragging(&a), "radius gesture starts on the actual press");
    assert!(a.ui.transform.is_none());
    let moved = screen + egui::vec2(1.0, 1.0);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(moved)]);
    assert!(radius(&a)[0] > 5.0, "live radius changes before egui's ordinary drag threshold");
    canvas_frame(&mut a, &ctx, vec![button(moved, false, Modifiers::NONE)]);
    assert!(!dragging(&a));
    assert!(a.ui.transform.is_none() && a.drag.is_none());
    assert_eq!(a.session.active().unwrap().history.past_len(), n + 1);
}

#[test]
fn native_shape_boolean_operation_uses_press_modifiers_released_before_drag() {
    let (mut a, ctx) = canvas_app();
    let id = a.session.active().unwrap().active_layer.unwrap();
    let xf = ViewXform::active(&a).unwrap();
    let start = xf.to_screen(150.0, 125.0);
    let end = xf.to_screen(190.0, 145.0);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(start), button(start, true, Modifiers::SHIFT)]);
    canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(end)]);
    canvas_frame(&mut a, &ctx, vec![button(end, false, Modifiers::NONE)]);
    assert_eq!(a.session.active().unwrap().active_layer, Some(id));
    let LayerContent::Shape(sh) = &a.session.active().unwrap().doc.layer(id).unwrap().content else { panic!("shape") };
    assert_eq!(sh.path.subpaths.len(), 2, "Shift at press adds to the existing layer even when released before the drag threshold");
    assert!(sh.live.is_none());
}

fn radius_centers(shapes: &[egui::epaint::ClippedShape]) -> Vec<egui::Pos2> {
    shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Circle(c) if c.radius == 4.0 => Some(c.center),
            _ => None,
        })
        .collect()
}

fn assert_moved_handles(shapes: &[egui::epaint::ClippedShape], original: [[f64; 2]; 4], delta: [f64; 2], xf: &ViewXform) {
    let actual = radius_centers(shapes);
    assert_eq!(actual.len(), 4);
    for (actual, p) in actual.into_iter().zip(original) {
        let expected = xf.to_screen((p[0] + delta[0]) as f32, (p[1] + delta[1]) as f32);
        assert!(actual.distance(expected) < 0.001, "radius handle at {actual:?}, expected {expected:?}");
    }
}

#[test]
fn native_move_radius_handles_follow_held_pixels_release_and_undo() {
    for (rotation, flip, zoom) in [(0.0, false, 1.0), (37.0, true, 1.6), (-90.0, false, 0.75)] {
        let (mut a, ctx) = canvas_app();
        a.ui.tool = Tool::Move;
        a.ui.tool_options.move_auto_select = false;
        a.ui.extras.snap = false;
        a.ui.view.show.smart_guides = false;
        a.ui.view.flip_horizontal = flip;
        a.ui.views[0].rotation = rotation;
        a.ui.views[0].zoom = zoom;
        canvas_frame(&mut a, &ctx, vec![]);
        let xf = ViewXform::active(&a).unwrap();
        let original = handles([20.0, 20.0, 120.0, 100.0], [5.0, 10.0, 15.0, 20.0], f64::from(xf.zoom));
        let before = a.session.active().unwrap().doc.clone();
        let revision = a.session.active().unwrap().revision;
        let history = a.session.active().unwrap().history.past_len();
        let start = xf.to_screen(70.0, 60.0);
        canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(start), button(start, true, Modifiers::NONE)]);
        for (end, delta) in [([87.4, 69.6], [17.0, 10.0]), ([99.2, 52.9], [29.0, -7.0])] {
            let screen = xf.to_screen(end[0], end[1]);
            canvas_frame(&mut a, &ctx, vec![egui::Event::PointerMoved(screen)]);
            // The image is selected before this frame's pointer events; a held frame shows the new image.
            let shapes = canvas_frame(&mut a, &ctx, vec![]);
            assert!(a.drag.as_ref().is_some_and(|d| d.tool == Tool::Move));
            assert!(a.shape_canvas.drag.is_none());
            assert_moved_handles(&shapes, original, delta, &xf);
            let st = a.session.active().unwrap();
            assert_eq!(st.doc, before);
            assert_eq!(st.revision, revision);
            assert_eq!(st.history.past_len(), history);
        }
        let end = xf.to_screen(99.2, 52.9);
        canvas_frame(&mut a, &ctx, vec![button(end, false, Modifiers::NONE)]);
        let shapes = canvas_frame(&mut a, &ctx, vec![]);
        assert_moved_handles(&shapes, original, [29.0, -7.0], &xf);
        assert!(a.drag.is_none());
        assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
        a.run("edit.undo", json!({})).unwrap();
        let shapes = canvas_frame(&mut a, &ctx, vec![]);
        assert_moved_handles(&shapes, original, [0.0, 0.0], &xf);
        assert_eq!(a.session.active().unwrap().doc, before);
    }
}

fn vector_and_radius_shapes(app: &PhotocraftApp, ctx: &egui::Context, xf: &ViewXform) -> Vec<egui::epaint::ClippedShape> {
    let doc = app.session.active().unwrap().doc.clone();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.ctx().layer_painter(egui::LayerId::background()).with_clip_rect(xf.rect);
        crate::vector_ui::draw_overlay(app, &painter, xf, &doc);
        draw_overlay(app, &painter, xf);
    });
    output.textures_delta.clear();
    output.shapes
}

fn assert_moved_path(shapes: &[egui::epaint::ClippedShape], original: &photocraft_doc::Path, delta: [f64; 2], xf: &ViewXform) {
    let expected = crate::vector_ui::path_lines(original, &|p| xf.to_screen((p[0] + delta[0]) as f32, (p[1] + delta[1]) as f32));
    for (points, closed) in expected {
        assert!(shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Path(p) if p.closed == closed && p.points.len() == points.len() && p.points.iter().zip(&points).all(|(a,b)| a.distance(*b) < 0.001))), "shape outline follows the image's offset {delta:?}");
    }
}

#[test]
fn move_path_and_handles_follow_the_image_shown_before_later_pointer_input() {
    let mut a = app();
    a.run("shape.edit", json!({"fill":"#ff0000","stroke":{"color":"#0000ff","width":6}})).unwrap();
    a.ui.tool_options.move_auto_select = false;
    a.ui.extras.snap = false;
    a.ui.view.show.smart_guides = false;
    a.tool_override = Some(Tool::Move);
    let st = a.session.active().unwrap();
    let id = st.active_layer.unwrap();
    let before = st.doc.clone();
    let LayerContent::Shape(original) = &before.layer(id).unwrap().content else { panic!("shape") };
    let history = st.history.past_len();
    let revision = st.revision;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
    let xf = ViewXform {
        aspect: 1.0,
        rect: egui::Rect::from_min_size(egui::pos2(17.0, 29.0), egui::vec2(600.0, 450.0)),
        zoom: 1.6,
        center: [100.0, 80.0],
        flip: true,
        rotation: 37.0,
    };
    let knobs = handles([20.0, 20.0, 120.0, 100.0], [5.0, 10.0, 15.0, 20.0], f64::from(xf.zoom));
    tool_event(&mut a, ToolEvent::Down { x: 70.0, y: 60.0, pressure: 1.0 }, Modifiers::NONE);
    move_to(&mut a, [120.0, 85.0], Modifiers::NONE);
    let shown = crate::move_ui::display_doc(&mut a, 0).unwrap().0;
    let LayerContent::Shape(moved) = &shown.layer(id).unwrap().content else { panic!("shape") };
    assert_eq!(moved.fill, original.fill);
    assert_eq!(moved.stroke, original.stroke);
    let pixel = |doc: &photocraft_doc::Document, x, y| photocraft_compose::render(doc, photocraft_geom::Rect::new(x, y, x + 1, y + 1)).px[0];
    let old_stroke = pixel(&before, 20, 70);
    assert!(old_stroke[2] > 0.99 && old_stroke[0] < 0.01 && old_stroke[3] > 0.99);
    assert_eq!(pixel(&shown, 70, 95), old_stroke, "the rasterized blue stroke moves with the image");
    assert_eq!(pixel(&shown, 20, 70)[3], 0.0, "the original stroke location is clear");
    assert!(pixel(&shown, 100, 85)[0] > 0.99, "the separate red fill is preserved");
    // canvas_view already chose `shown`; input arriving later must not move just the overlays ahead.
    move_to(&mut a, [130.0, 95.0], Modifiers::NONE);
    let shapes = vector_and_radius_shapes(&a, &ctx, &xf);
    assert_moved_handles(&shapes, knobs, [50.0, 25.0], &xf);
    assert_moved_path(&shapes, &original.path, [50.0, 25.0], &xf);
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().revision, revision);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
    crate::move_ui::display_doc(&mut a, 0).unwrap();
    let shapes = vector_and_radius_shapes(&a, &ctx, &xf);
    assert_moved_handles(&shapes, knobs, [60.0, 35.0], &xf);
    assert_moved_path(&shapes, &original.path, [60.0, 35.0], &xf);
    release(&mut a, [130.0, 95.0], Modifiers::NONE);
    let shapes = vector_and_radius_shapes(&a, &ctx, &xf);
    assert_moved_path(&shapes, &original.path, [60.0, 35.0], &xf);
    assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
    a.run("edit.undo", json!({})).unwrap();
    let shapes = vector_and_radius_shapes(&a, &ctx, &xf);
    assert_moved_path(&shapes, &original.path, [0.0, 0.0], &xf);
    assert_eq!(a.session.active().unwrap().doc, before);
}

#[test]
fn stale_move_images_do_not_supply_shape_overlay_geometry() {
    for context in ["cancel", "revision", "preference", "document", "different-drag"] {
        let mut a = app();
        a.tool_override = Some(Tool::Move);
        a.ui.tool_options.move_auto_select = false;
        a.ui.extras.snap = false;
        a.ui.view.show.smart_guides = false;
        tool_event(&mut a, ToolEvent::Down { x: 70.0, y: 60.0, pressure: 1.0 }, Modifiers::NONE);
        move_to(&mut a, [90.0, 70.0], Modifiers::NONE);
        crate::move_ui::display_doc(&mut a, 0).unwrap();
        match context {
            "cancel" => a.drag = None,
            "revision" => {
                a.run("layer.setProps", json!({"name":"Updated"})).unwrap();
            }
            "preference" => {
                a.session.edit_prefs(|p| p.interface.show_bounding_box_when_dragging_layer = true);
            }
            "document" => {
                a.run("file.new", json!({"width":200,"height":160,"background":"transparent"})).unwrap();
                a.sync_views();
                a.run("shape.create", json!({"kind":"roundedRect","rect":[20,20,120,100],"radii":[5,10,15,20]})).unwrap();
            }
            "different-drag" => a.drag.as_mut().unwrap().tool = Tool::Brush,
            _ => panic!("unknown test context: {context}"),
        }
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
        let xf = ViewXform {
            aspect: 1.0,
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 400.0)),
            zoom: 1.0,
            center: [100.0, 80.0],
            flip: false,
            rotation: 0.0,
        };
        let st = a.session.active().unwrap();
        let LayerContent::Shape(shape) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("shape") };
        let path = shape.path.clone();
        let shapes = vector_and_radius_shapes(&a, &ctx, &xf);
        assert_moved_handles(&shapes, handles([20.0, 20.0, 120.0, 100.0], [5.0, 10.0, 15.0, 20.0], 1.0), [0.0, 0.0], &xf);
        assert_moved_path(&shapes, &path, [0.0, 0.0], &xf);
    }
}

#[test]
fn move_bounds_use_four_canvas_transformed_corners_of_the_last_shown_image() {
    for (rotation, flip, zoom) in [(0.0, false, 1.0), (37.0, true, 1.6), (-90.0, false, 0.75)] {
        let mut a = app();
        a.run("shape.edit", json!({"fill":"#ff0000","stroke":{"color":"#0000ff","width":6}})).unwrap();
        a.ui.tool = Tool::Move;
        a.ui.tool_options.move_auto_select = false;
        a.ui.extras.snap = false;
        a.ui.view.show.smart_guides = false;
        let st = a.session.active().unwrap();
        let before = st.doc.clone();
        let revision = st.revision;
        let history = st.history.past_len();
        let layer = before.layer(st.active_layer.unwrap()).unwrap();
        // The painted bounds include the stroke, rather than only the live rectangle's geometry.
        let bounds = layer.surface().unwrap().content_bounds();
        assert!(bounds.x0 < 20 && bounds.y0 < 20 && bounds.x1 > 140 && bounds.y1 > 120);
        let corners = [[bounds.x0, bounds.y0], [bounds.x1, bounds.y0], [bounds.x1, bounds.y1], [bounds.x0, bounds.y1]];
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
        let xf = ViewXform {
            aspect: 1.0,
            rect: egui::Rect::from_min_size(egui::pos2(17.0, 29.0), egui::vec2(600.0, 450.0)),
            zoom,
            center: [100.0, 80.0],
            flip,
            rotation,
        };
        tool_event(&mut a, ToolEvent::Down { x: 70.0, y: 60.0, pressure: 1.0 }, Modifiers::NONE);
        move_to(&mut a, [120.4, 85.6], Modifiers::NONE);
        crate::move_ui::display_doc(&mut a, 0).unwrap();
        // This input lands after the canvas image was selected for the current frame.
        move_to(&mut a, [130.0, 95.0], Modifiers::NONE);
        for (refresh_image, delta) in [(false, [50, 26]), (true, [60, 35])] {
            if refresh_image {
                crate::move_ui::display_doc(&mut a, 0).unwrap();
            }
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let painter = ui.ctx().layer_painter(egui::LayerId::background()).with_clip_rect(xf.rect);
                crate::snap_ui::draw(&a, &painter, &xf);
            });
            output.textures_delta.clear();
            let paths: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Path(p) => Some(p),
                    _ => None,
                })
                .collect();
            assert_eq!(paths.len(), 1, "the Move bounds are one closed path");
            assert!(paths[0].closed);
            assert_eq!(paths[0].points.len(), 4);
            for (actual, p) in paths[0].points.iter().zip(corners) {
                let expected = xf.to_screen((p[0] + delta[0]) as f32, (p[1] + delta[1]) as f32);
                assert!(actual.distance(expected) < 0.001, "rotation {rotation}, flip {flip}: bounds at {actual:?}, expected {expected:?}");
            }
            let st = a.session.active().unwrap();
            assert_eq!(st.doc, before);
            assert_eq!(st.revision, revision);
            assert_eq!(st.history.past_len(), history);
        }
    }
}
