use super::*;
use egui::{Event, Key, Modifiers, PointerButton};
use photocraft_doc::{LayerContent, LiveShape};
use serde_json::json;

fn frame(app: &mut PhotocraftApp, ctx: &egui::Context, mut events: Vec<Event>, modifiers: Modifiers) -> egui::FullOutput {
    let rect = Rect::from_min_size(Pos2::ZERO, vec2(600.0, 480.0));
    app.last_canvas_rect = rect;
    let view = app.ui.views[0].clone();
    events.insert(0, Event::ModifiersChanged(modifiers));
    let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), events, ..Default::default() }, |ui| {
        crate::shortcuts::handle(app, ctx);
        app.ui.views[0] = canvas_view(app, ui, 0, rect, view.clone(), true);
    });
    out.textures_delta.clear();
    out
}

fn app(tool: Tool) -> (PhotocraftApp, egui::Context) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width":320,"height":240,"background":"transparent"})).unwrap();
    app.run("shape.create", json!({"kind":"roundedRect","rect":[50,40,140,120],"radii":[5,10,15,20],"fill":"#ffe030"})).unwrap();
    app.sync_views();
    app.ui.tool = tool;
    app.ui.extras.rulers = false;
    app.ui.extras.snap = false;
    app.ui.views[0].zoom = 1.0;
    app.ui.views[0].center = [160.0, 120.0];
    app.ui.views[0].fit_pending = false;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
    frame(&mut app, &ctx, vec![], Modifiers::NONE);
    (app, ctx)
}

fn button(pos: Pos2, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers }
}

fn key(app: &mut PhotocraftApp, ctx: &egui::Context, key: Key) {
    let events = [true, false].into_iter().map(|pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }).collect();
    frame(app, ctx, events, Modifiers::NONE);
}

fn drag(app: &mut PhotocraftApp, ctx: &egui::Context, from: Pos2, to: Pos2, modifiers: Modifiers) {
    frame(app, ctx, vec![Event::PointerMoved(from), button(from, true, modifiers)], modifiers);
    for fraction in [0.4, 0.7, 1.0] {
        frame(app, ctx, vec![Event::PointerMoved(from.lerp(to, fraction))], modifiers);
    }
    assert!(app.ui.transform.is_some(), "the native control drag must enter Free Transform");
    frame(app, ctx, vec![button(to, false, modifiers)], modifiers);
}

fn collect(shape: &egui::Shape, squares: &mut Vec<Pos2>, circles: &mut Vec<Pos2>) {
    match shape {
        egui::Shape::Rect(r) if r.fill == Color32::WHITE && (r.rect.size() - vec2(7.0, 7.0)).length() < 0.01 => squares.push(r.rect.center()),
        egui::Shape::Circle(c) if (c.radius - 4.0).abs() < 0.01 => circles.push(c.center),
        egui::Shape::Vec(shapes) => {
            for s in shapes {
                collect(s, squares, circles);
            }
        }
        _ => {}
    }
}

fn painted_controls(out: &egui::FullOutput) -> (Vec<Pos2>, Vec<Pos2>) {
    let (mut squares, mut circles) = (Vec::new(), Vec::new());
    for s in &out.shapes {
        collect(&s.shape, &mut squares, &mut circles);
    }
    (squares, circles)
}

fn shape(app: &PhotocraftApp) -> &photocraft_doc::ShapeLayer {
    let st = app.session.active().unwrap();
    let LayerContent::Shape(shape) = &st.doc.layer(st.active_layer.unwrap()).unwrap().content else { panic!("editable shape must survive") };
    shape
}

#[test]
fn selected_shape_paints_eight_controls_and_four_radius_knobs_for_move_and_shape_tools() {
    for tool in [Tool::Move, Tool::Rectangle, Tool::EllipseShape, Tool::Triangle, Tool::Polygon, Tool::Line, Tool::CustomShape] {
        let (mut a, ctx) = app(tool);
        assert!(a.ui.tool_options.shape.show_transform);
        assert!(!a.ui.tool_options.move_show_transform, "shapes have automatic controls, independent of the raster toggle");
        let before = a.session.active().unwrap().doc.clone();
        let history = a.session.active().unwrap().history.past_len();
        let out = frame(&mut a, &ctx, vec![], Modifiers::NONE);
        let (squares, circles) = painted_controls(&out);
        assert_eq!(squares.len(), 8, "{tool:?}");
        assert_eq!(circles.len(), 4, "{tool:?}");
        let xf = ViewXform::active(&a).unwrap();
        for [x, y] in [[50.0, 40.0], [120.0, 40.0], [190.0, 40.0], [190.0, 100.0], [190.0, 160.0], [120.0, 160.0], [50.0, 160.0], [50.0, 100.0]] {
            let expected = xf.to_screen(x, y);
            assert!(squares.iter().any(|p| p.distance(expected) < 0.1), "{tool:?}: missing control at {expected:?}");
        }
        assert_eq!(a.session.active().unwrap().doc, before);
        assert_eq!(a.session.active().unwrap().history.past_len(), history);
    }
}

#[test]
fn raster_controls_keep_the_move_checkbox_contract() {
    let (mut a, ctx) = app(Tool::Move);
    a.run("layer.new.layer", json!({})).unwrap();
    a.session
        .edit("Raster box", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(50, 40, 190, 160), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
    assert!(painted_controls(&frame(&mut a, &ctx, vec![], Modifiers::NONE)).0.is_empty());
    a.ui.tool_options.move_show_transform = true;
    let (squares, circles) = painted_controls(&frame(&mut a, &ctx, vec![], Modifiers::NONE));
    assert_eq!(squares.len(), 8);
    assert!(circles.is_empty());
    a.ui.tool = Tool::Rectangle;
    assert!(painted_controls(&frame(&mut a, &ctx, vec![], Modifiers::NONE)).0.is_empty());
}

#[test]
fn native_shape_control_scale_is_staged_then_commits_once_and_undoes() {
    for tool in [Tool::Move, Tool::Rectangle] {
        let (mut a, ctx) = app(tool);
        let before = a.session.active().unwrap().doc.clone();
        let old_path = shape(&a).path.clone();
        let history = a.session.active().unwrap().history.past_len();
        let xf = ViewXform::active(&a).unwrap();
        let from = xf.to_screen(190.0, 160.0);
        let hovered = frame(&mut a, &ctx, vec![Event::PointerMoved(from)], Modifiers::NONE);
        assert_eq!(hovered.platform_output.cursor_icon, egui::CursorIcon::ResizeNwSe);
        drag(&mut a, &ctx, from, xf.to_screen(230.0, 180.0), Modifiers::NONE);
        let staged = a.ui.transform.as_ref().unwrap();
        assert_eq!(staged.quad[0], [50.0, 40.0]);
        assert!(staged.quad[2][0] > 220.0 && staged.quad[2][1] > 180.0);
        assert_eq!(a.session.active().unwrap().doc, before);
        assert_eq!(a.session.active().unwrap().history.past_len(), history);
        key(&mut a, &ctx, Key::Enter);
        assert!(a.ui.transform.is_none());
        assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
        assert_ne!(shape(&a).path, old_path);
        let after = a.session.active().unwrap().doc.clone();
        a.run("edit.undo", json!({})).unwrap();
        assert_eq!(a.session.active().unwrap().doc, before);
        a.run("edit.redo", json!({})).unwrap();
        assert_eq!(a.session.active().unwrap().doc, after);
    }
}

#[test]
fn native_ctrl_corner_shape_distort_can_cancel_or_commit_without_rasterizing() {
    for tool in [Tool::Move, Tool::Rectangle] {
        let (mut a, ctx) = app(tool);
        let before = a.session.active().unwrap().doc.clone();
        let old_shape = shape(&a).clone();
        let history = a.session.active().unwrap().history.past_len();
        let mods = Modifiers { command: true, ctrl: true, ..Modifiers::NONE };
        let xf = ViewXform::active(&a).unwrap();
        for commit in [false, true] {
            drag(&mut a, &ctx, xf.to_screen(190.0, 40.0), xf.to_screen(220.0, 55.0), mods);
            let quad = a.ui.transform.as_ref().unwrap().quad;
            assert_eq!(quad[0], [50.0, 40.0]);
            assert!((quad[1][0] - 220.0).abs() < 0.1 && (quad[1][1] - 55.0).abs() < 0.1);
            assert_eq!(quad[2], [190.0, 160.0]);
            assert_eq!(quad[3], [50.0, 160.0]);
            assert_eq!(a.session.active().unwrap().doc, before);
            assert_eq!(a.session.active().unwrap().history.past_len(), history);
            key(&mut a, &ctx, if commit { Key::Enter } else { Key::Escape });
            assert!(a.ui.transform.is_none());
            if commit {
                let new_shape = shape(&a);
                assert_ne!(new_shape.path, old_shape.path);
                assert_eq!(new_shape.fill, old_shape.fill);
                assert_eq!(new_shape.stroke, old_shape.stroke);
                assert!(new_shape.live.is_none(), "a distorted path must not regenerate as the original rectangle");
                assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
                a.run("edit.undo", json!({})).unwrap();
            }
            assert_eq!(a.session.active().unwrap().doc, before);
            assert_eq!(a.session.active().unwrap().history.past_len(), history);
        }
    }
}

#[test]
fn rotated_flipped_native_shape_control_drag_uses_the_drawn_corner() {
    for flip in [false, true] {
        let (mut a, ctx) = app(Tool::Rectangle);
        a.ui.views[0].rotation = 33.0;
        a.ui.views[0].zoom = 1.25;
        a.ui.view.flip_horizontal = flip;
        let out = frame(&mut a, &ctx, vec![], Modifiers::NONE);
        let xf = ViewXform::active(&a).unwrap();
        let (squares, _) = painted_controls(&out);
        assert_eq!(squares.len(), 8);
        for [x, y] in [[50.0, 40.0], [190.0, 40.0], [190.0, 160.0], [50.0, 160.0]] {
            let expected = xf.to_screen(x, y);
            assert!(squares.iter().any(|p| p.distance(expected) < 0.1), "flip={flip}: {expected:?}");
        }
        let before = a.session.active().unwrap().doc.clone();
        let history = a.session.active().unwrap().history.past_len();
        let mods = Modifiers { command: true, ctrl: true, ..Modifiers::NONE };
        drag(&mut a, &ctx, xf.to_screen(190.0, 40.0), xf.to_screen(210.0, 55.0), mods);
        let quad = a.ui.transform.as_ref().unwrap().quad;
        assert!((quad[1][0] - 210.0).abs() < 0.1 && (quad[1][1] - 55.0).abs() < 0.1, "flip={flip}: {quad:?}");
        for (i, expected) in [(0, [50.0, 40.0]), (2, [190.0, 160.0]), (3, [50.0, 160.0])] {
            assert_eq!(quad[i], expected);
        }
        key(&mut a, &ctx, Key::Escape);
        assert_eq!(a.session.active().unwrap().doc, before);
        assert_eq!(a.session.active().unwrap().history.past_len(), history);
    }
}

#[test]
fn native_radius_press_keeps_priority_over_automatic_shape_controls() {
    let (mut a, ctx) = app(Tool::Move);
    let history = a.session.active().unwrap().history.past_len();
    let out = frame(&mut a, &ctx, vec![], Modifiers::NONE);
    let (_, circles) = painted_controls(&out);
    let knob = circles[0];
    let hovered = frame(&mut a, &ctx, vec![Event::PointerMoved(knob)], Modifiers::NONE);
    assert_eq!(hovered.platform_output.cursor_icon, egui::CursorIcon::PointingHand);
    frame(&mut a, &ctx, vec![button(knob, true, Modifiers::NONE)], Modifiers::NONE);
    assert!(crate::shape_canvas::dragging(&a));
    assert!(a.ui.transform.is_none() && a.drag.is_none());
    let end = knob + vec2(3.0, 3.0);
    frame(&mut a, &ctx, vec![Event::PointerMoved(end)], Modifiers::NONE);
    frame(&mut a, &ctx, vec![button(end, false, Modifiers::NONE)], Modifiers::NONE);
    let Some(LiveShape::Rect { radii, .. }) = &shape(&a).live else { panic!("radius keeps live geometry") };
    assert!(radii[0] > 5.0);
    assert!(a.ui.transform.is_none() && a.drag.is_none());
    assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
}

#[test]
fn shape_controls_respect_targets_locks_modes_and_navigation() {
    for context in ["disabled", "mask", "vectorMask", "path", "pixels", "positionLock", "allLock", "hand", "zoom", "rotate"] {
        let (mut a, ctx) = app(Tool::Rectangle);
        match context {
            "disabled" => a.ui.tool_options.shape.show_transform = false,
            "mask" => a.ui.mask_target = true,
            "vectorMask" => a.ui.vector_mask_target = true,
            "path" | "pixels" => a.ui.tool_options.shape.mode = context.into(),
            "positionLock" | "allLock" => {
                a.session
                    .edit("Lock", |doc, active| {
                        let locks = &mut doc.layer_mut(active.unwrap()).unwrap().locks;
                        locks.position = context == "positionLock";
                        locks.all = context == "allLock";
                        Ok(())
                    })
                    .unwrap();
            }
            "hand" => a.ui.tool = Tool::Hand,
            "zoom" => a.ui.tool = Tool::Zoom,
            "rotate" => a.ui.tool = Tool::RotateView,
            _ => panic!("unknown test context: {context}"),
        }
        let before = a.session.active().unwrap().doc.clone();
        let history = a.session.active().unwrap().history.past_len();
        let out = frame(&mut a, &ctx, vec![], Modifiers::NONE);
        assert!(painted_controls(&out).0.is_empty(), "{context}");
        assert!(a.ui.transform.is_none());
        assert_eq!(a.session.active().unwrap().doc, before);
        assert_eq!(a.session.active().unwrap().history.past_len(), history);
    }
    let (mut a, ctx) = app(Tool::Rectangle);
    let before = a.session.active().unwrap().doc.clone();
    let history = a.session.active().unwrap().history.past_len();
    let xf = ViewXform::active(&a).unwrap();
    let from = xf.to_screen(190.0, 160.0);
    let space = |pressed| Event::Key { key: Key::Space, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
    let out = frame(&mut a, &ctx, vec![space(true), Event::PointerMoved(from), button(from, true, Modifiers::NONE)], Modifiers::NONE);
    assert!(painted_controls(&out).0.is_empty());
    let to = from + vec2(35.0, 25.0);
    frame(&mut a, &ctx, vec![Event::PointerMoved(to)], Modifiers::NONE);
    frame(&mut a, &ctx, vec![button(to, false, Modifiers::NONE), space(false)], Modifiers::NONE);
    assert!(a.ui.transform.is_none() && !crate::shape_canvas::dragging(&a));
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
}

#[test]
fn failed_native_shape_control_apply_keeps_the_session_for_correction_or_cancel() {
    let (mut a, ctx) = app(Tool::Move);
    let before = a.session.active().unwrap().doc.clone();
    let history = a.session.active().unwrap().history.past_len();
    let xf = ViewXform::active(&a).unwrap();
    drag(&mut a, &ctx, xf.to_screen(190.0, 160.0), xf.to_screen(220.0, 180.0), Modifiers::NONE);
    a.ui.transform.as_mut().unwrap().quad[1] = [2_000_000_000.0, 40.0];
    let rejected_quad = a.ui.transform.as_ref().unwrap().quad;
    key(&mut a, &ctx, Key::Enter);
    assert_eq!(a.ui.transform.as_ref().unwrap().quad, rejected_quad);
    assert!(a.transform_preview.is_some());
    assert!(!a.ui.status.is_empty());
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
    key(&mut a, &ctx, Key::Escape);
    assert!(a.ui.transform.is_none() && a.transform_preview.is_none());
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
}

#[test]
fn automation_pointer_starts_the_same_visible_shape_grip() {
    let (mut a, ctx) = app(Tool::Rectangle);
    let before = a.session.active().unwrap().doc.clone();
    let history = a.session.active().unwrap().history.past_len();
    let params = json!({"tool":"rectangle","modifiers":{"command":true,"ctrl":true},"events":[
        {"kind":"down","x":190,"y":40},{"kind":"move","x":210,"y":55},{"kind":"up","x":210,"y":55}
    ]});
    let (request, _) = crate::control::ControlRequest::new("ui.pointer", params);
    let crate::control::Outcome::Done(reply) = crate::control::handle(&mut a, &ctx, &request) else { panic!("pointer is synchronous") };
    assert_eq!(reply["ok"], true, "{reply}");
    let quad = a.ui.transform.as_ref().unwrap().quad;
    assert_eq!(quad[1], [210.0, 55.0]);
    assert_eq!(quad[0], [50.0, 40.0]);
    assert_eq!(quad[2], [190.0, 160.0]);
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
    key(&mut a, &ctx, Key::Escape);
    assert!(a.ui.transform.is_none());
    assert_eq!(a.session.active().unwrap().doc, before);
}

#[test]
fn native_shape_distort_preview_caches_the_quad_keeps_text_above_and_matches_commit() {
    let (mut a, ctx) = app(Tool::Move);
    a.session.edit_prefs(|p| p.performance.low_resolution_previews = false);
    let shape_id = a.session.active().unwrap().active_layer.unwrap();
    let text_id =
        photocraft_doc::LayerId(a.run("type.create", json!({"x":80,"y":110,"text":"TEST","size":32,"color":"#000000"})).unwrap()["layer"].as_u64().unwrap());
    a.run("layer.select", json!({"layer":shape_id.0})).unwrap();
    frame(&mut a, &ctx, vec![], Modifiers::NONE);
    let before = a.session.active().unwrap().doc.clone();
    let history = a.session.active().unwrap().history.past_len();
    let order: Vec<_> = before.layers.iter().map(|l| l.id).collect();
    let text = before.layer(text_id).unwrap().surface().unwrap();
    let bounds = text.content_bounds();
    let point = (bounds.y0..bounds.y1)
        .flat_map(|y| (bounds.x0..bounds.x1).map(move |x| (x, y)))
        .filter(|&(x, y)| (65..180).contains(&x) && (60..140).contains(&y))
        .max_by(|&(x, y), &(xx, yy)| text.pixel(x, y).last().unwrap().total_cmp(text.pixel(xx, yy).last().unwrap()))
        .unwrap();
    assert!(text.pixel(point.0, point.1).last().copied().unwrap() > 0.999, "the text oracle uses an opaque glyph interior");
    let xf = ViewXform::active(&a).unwrap();
    let mods = Modifiers { command: true, ctrl: true, ..Modifiers::NONE };
    drag(&mut a, &ctx, xf.to_screen(190.0, 40.0), xf.to_screen(220.0, 55.0), mods);
    crate::transform_tool::follow_edits(&mut a);
    let idx = a.session.active_index().unwrap();
    let (first, first_key) = crate::transform_tool::display_doc(&a, idx).unwrap();
    let LayerContent::Shape(first_shape) = &first.layer(shape_id).unwrap().content else { panic!("vector preview") };
    assert_ne!(first_shape.path, shape(&a).path);
    assert_ne!(first.layer(shape_id).unwrap().surface().unwrap().content_bounds(), before.layer(shape_id).unwrap().surface().unwrap().content_bounds());
    crate::transform_tool::follow_edits(&mut a);
    let (steady, steady_key) = crate::transform_tool::display_doc(&a, idx).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &steady));
    assert_eq!(first_key, steady_key);
    // Both quads are nonaffine: the quad itself must invalidate the cache, rather than None == None.
    drag(&mut a, &ctx, xf.to_screen(220.0, 55.0), xf.to_screen(230.0, 70.0), mods);
    crate::transform_tool::follow_edits(&mut a);
    let (shown, shown_key) = crate::transform_tool::display_doc(&a, idx).unwrap();
    assert_ne!(shown_key, first_key);
    assert!(!std::sync::Arc::ptr_eq(&first, &shown));
    let LayerContent::Shape(shown_shape) = &shown.layer(shape_id).unwrap().content else { panic!("vector preview") };
    assert_ne!(shown_shape.path, first_shape.path);
    assert_eq!(shown.layers.iter().map(|l| l.id).collect::<Vec<_>>(), order);
    assert_eq!(shown.layer(text_id).unwrap(), before.layer(text_id).unwrap());
    assert!(shown.layer(shape_id).unwrap().visible);
    let pixel_rect = photocraft_geom::Rect::new(point.0, point.1, point.0 + 1, point.1 + 1);
    let pixel = photocraft_compose::render(&shown, pixel_rect).px[0];
    assert!(pixel[0] < 0.06 && pixel[1] < 0.06 && pixel[2] < 0.06 && pixel[3] > 0.99, "higher text remains black: {pixel:?}");
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
    let accepted_path = shown_shape.path.clone();
    let accepted_paint = (shown_shape.fill.clone(), shown_shape.stroke.clone());
    key(&mut a, &ctx, Key::Enter);
    assert!(a.ui.transform.is_none());
    assert_eq!(shape(&a).path, accepted_path);
    assert_eq!((shape(&a).fill.clone(), shape(&a).stroke.clone()), accepted_paint);
    assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
    assert_eq!(photocraft_compose::render(&a.session.active().unwrap().doc, pixel_rect).px[0], pixel);
    a.run("edit.undo", json!({})).unwrap();
    assert_eq!(a.session.active().unwrap().doc, before);
}

#[test]
fn rejected_shape_control_apply_releases_capture_when_tool_or_layer_changes() {
    for switch_layer in [false, true] {
        let (mut a, ctx) = app(Tool::Move);
        let first = a.session.active().unwrap().active_layer.unwrap();
        let second =
            photocraft_doc::LayerId(a.run("shape.create", json!({"kind":"rect","rect":[215,140,70,60],"fill":"#0099ff"})).unwrap()["layer"].as_u64().unwrap());
        a.run("layer.select", json!({"layer":first.0})).unwrap();
        let before = a.session.active().unwrap().doc.clone();
        let history = a.session.active().unwrap().history.past_len();
        let xf = ViewXform::active(&a).unwrap();
        drag(&mut a, &ctx, xf.to_screen(190.0, 160.0), xf.to_screen(220.0, 180.0), Modifiers::NONE);
        a.ui.transform.as_mut().unwrap().quad[1] = [2_000_000_000.0, 40.0];
        let (expected_layer, from, to) = if switch_layer {
            a.run("layer.select", json!({"layer":second.0})).unwrap();
            (second, [285.0, 200.0], [300.0, 210.0])
        } else {
            a.ui.tool = Tool::Rectangle;
            (first, [190.0, 160.0], [215.0, 180.0])
        };
        crate::transform_tool::end_if_left(&mut a);
        frame(&mut a, &ctx, vec![], Modifiers::NONE);
        assert!(a.ui.transform.is_none() && a.transform_preview.is_none(), "switch_layer={switch_layer}: old owner must release capture");
        assert_eq!(a.session.active().unwrap().doc, before);
        assert_eq!(a.session.active().unwrap().history.past_len(), history);
        // The new context receives a fresh control drag, rather than the rejected owner's box.
        drag(&mut a, &ctx, xf.to_screen(from[0], from[1]), xf.to_screen(to[0], to[1]), Modifiers::NONE);
        assert_eq!(a.ui.transform.as_ref().unwrap().layer, expected_layer.0);
        key(&mut a, &ctx, Key::Escape);
        assert!(a.ui.transform.is_none() && a.transform_preview.is_none());
        assert_eq!(a.session.active().unwrap().doc, before);
        assert_eq!(a.session.active().unwrap().history.past_len(), history);
    }
}

#[test]
fn native_shape_grips_are_hidden_for_alpha_quick_mask_and_mask_only_targets() {
    for target in ["alpha", "quickMask", "maskView"] {
        let (mut a, ctx) = app(Tool::Move);
        match target {
            "alpha" => {
                a.run("channel.new", json!({"fill":"white"})).unwrap();
                a.run("channel.target", json!({"channel":0})).unwrap();
            }
            "quickMask" => {
                a.run("select.rect", json!({"x":0,"y":0,"width":320,"height":240})).unwrap();
                a.run("select.editInQuickMaskMode", json!({})).unwrap();
            }
            "maskView" => {
                a.run("layer.layerMask.revealAll", json!({})).unwrap();
                a.run("view.layerMask", json!({"mode":"gray"})).unwrap();
            }
            _ => panic!("unknown test target: {target}"),
        }
        // Commands synchronize the thumbnail flag; emulate a UI flag that no longer reflects the engine target.
        a.ui.mask_target = false;
        assert!(!a.ui.mask_target, "the engine's target must be honored even without the UI thumbnail flag");
        assert_ne!(paint_target(&a), json!("pixels"));
        let path = shape(&a).path.clone();
        let out = frame(&mut a, &ctx, vec![], Modifiers::NONE);
        assert!(painted_controls(&out).0.is_empty(), "{target}: no shape resize controls on a channel/mask target");
        let xf = ViewXform::active(&a).unwrap();
        let corner = xf.to_screen(190.0, 40.0);
        frame(&mut a, &ctx, vec![Event::PointerMoved(corner), button(corner, true, Modifiers::NONE)], Modifiers::NONE);
        frame(&mut a, &ctx, vec![button(corner, false, Modifiers::NONE)], Modifiers::NONE);
        assert!(a.ui.transform.is_none() && a.transform_preview.is_none(), "{target}: a hidden grip must not start the wrong transform owner");
        assert_eq!(shape(&a).path, path);
    }
}

#[test]
fn native_nonaffine_controls_follow_the_image_drawn_before_this_frames_pointer_move() {
    let (mut a, ctx) = app(Tool::Move);
    a.session.edit_prefs(|p| p.performance.low_resolution_previews = false);
    let before = a.session.active().unwrap().doc.clone();
    let history = a.session.active().unwrap().history.past_len();
    let id = a.session.active().unwrap().active_layer.unwrap();
    let idx = a.session.active_index().unwrap();
    let xf = ViewXform::active(&a).unwrap();
    let mods = Modifiers { command: true, ctrl: true, ..Modifiers::NONE };
    let from = xf.to_screen(190.0, 40.0);
    let moved_a = xf.to_screen(210.0, 55.0);
    frame(&mut a, &ctx, vec![Event::PointerMoved(from), button(from, true, mods)], mods);
    frame(&mut a, &ctx, vec![Event::PointerMoved(moved_a)], mods);
    let quad_a = a.ui.transform.as_ref().unwrap().quad;
    assert!((quad_a[1][0] - 210.0).abs() < 0.1 && (quad_a[1][1] - 55.0).abs() < 0.1);
    frame(&mut a, &ctx, vec![], mods);
    let (image_a, key_a) = crate::transform_tool::display_doc(&a, idx).unwrap();
    let source_rect = a.ui.transform.as_ref().unwrap().rect;
    let projected_a = photocraft_engine::transform_cmds::preview_shape_quad_transform(&before, id, source_rect, quad_a, "bicubic").unwrap();
    let path = |doc: &photocraft_doc::Document| {
        let LayerContent::Shape(shape) = &doc.layer(id).unwrap().content else { panic!("vector shape") };
        shape.path.clone()
    };
    assert_eq!(path(&image_a), path(&projected_a));
    let moved_b = xf.to_screen(230.0, 65.0);
    let move_frame = frame(&mut a, &ctx, vec![Event::PointerMoved(moved_b)], mods);
    let quad_b = a.ui.transform.as_ref().unwrap().quad;
    assert!((quad_b[1][0] - 230.0).abs() < 0.1 && (quad_b[1][1] - 65.0).abs() < 0.1);
    assert_ne!(quad_a, quad_b);
    let (still_a, still_key_a) = crate::transform_tool::display_doc(&a, idx).unwrap();
    assert_eq!(still_key_a, key_a);
    assert!(std::sync::Arc::ptr_eq(&image_a, &still_a));
    let (squares, _) = painted_controls(&move_frame);
    assert_eq!(squares.len(), 8);
    for p in quad_a {
        let shown_corner = xf.to_screen(p[0] as f32, p[1] as f32);
        assert!(squares.iter().any(|p| p.distance(shown_corner) < 0.1), "controls must match the cached image, not the newer input");
    }
    assert!(!squares.iter().any(|p| p.distance(moved_b) < 0.1), "new pointer geometry is not the image this frame");
    let catch_up = frame(&mut a, &ctx, vec![], mods);
    let (image_b, key_b) = crate::transform_tool::display_doc(&a, idx).unwrap();
    assert_ne!(key_a, key_b);
    assert_ne!(path(&image_a), path(&image_b));
    let projected_b = photocraft_engine::transform_cmds::preview_shape_quad_transform(&before, id, source_rect, quad_b, "bicubic").unwrap();
    assert_eq!(path(&image_b), path(&projected_b));
    let (caught_squares, _) = painted_controls(&catch_up);
    for p in quad_b {
        let expected = xf.to_screen(p[0] as f32, p[1] as f32);
        assert!(caught_squares.iter().any(|p| p.distance(expected) < 0.1));
    }
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
    frame(&mut a, &ctx, vec![button(moved_b, false, mods)], mods);
    key(&mut a, &ctx, Key::Escape);
    assert_eq!(a.session.active().unwrap().doc, before);
    assert_eq!(a.session.active().unwrap().history.past_len(), history);
}

#[test]
fn native_shape_transform_suppresses_the_old_passive_outline_and_restores_the_current_path() {
    fn passive_paths(app: &PhotocraftApp, xf: &ViewXform) -> Vec<Vec<Pos2>> {
        fn collect(shape: &egui::Shape, paths: &mut Vec<Vec<Pos2>>) {
            match shape {
                egui::Shape::Path(path) if path.closed => paths.push(path.points.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, paths);
                    }
                }
                _ => {}
            }
        }
        // A separate painter context must not release the native gesture held in the canvas context.
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(xf.rect), ..Default::default() }, |ui| {
            crate::vector_ui::draw_overlay(app, ui.painter(), xf, &app.session.active().unwrap().doc);
        });
        out.textures_delta.clear();
        let mut paths = Vec::new();
        for clipped in &out.shapes {
            collect(&clipped.shape, &mut paths);
        }
        paths
    }

    for distort in [false, true] {
        for commit in [false, true] {
            let (mut a, ctx) = app(Tool::Rectangle);
            a.ui.tool_options.shape.show_path = true;
            let before = a.session.active().unwrap().doc.clone();
            let history = a.session.active().unwrap().history.past_len();
            let xf = ViewXform::active(&a).unwrap();
            let original_outline = passive_paths(&a, &xf);
            assert_eq!(original_outline.len(), 1, "the selected shape has its passive path at rest");
            let mods = if distort { Modifiers { command: true, ctrl: true, ..Modifiers::NONE } } else { Modifiers::NONE };
            let (from, to) =
                if distort { (xf.to_screen(190.0, 40.0), xf.to_screen(220.0, 55.0)) } else { (xf.to_screen(190.0, 160.0), xf.to_screen(230.0, 180.0)) };
            frame(&mut a, &ctx, vec![Event::PointerMoved(from), button(from, true, mods)], mods);
            for fraction in [0.5, 1.0] {
                frame(&mut a, &ctx, vec![Event::PointerMoved(from.lerp(to, fraction))], mods);
            }
            assert!(a.ui.transform.is_some());
            assert!(passive_paths(&a, &xf).is_empty(), "distort={distort}: the held transform must not show the original passive shape path");
            assert_eq!(a.session.active().unwrap().doc, before);
            assert_eq!(a.session.active().unwrap().history.past_len(), history);
            frame(&mut a, &ctx, vec![button(to, false, mods)], mods);
            assert!(passive_paths(&a, &xf).is_empty(), "releasing the pointer keeps the staged transform open");
            key(&mut a, &ctx, if commit { Key::Enter } else { Key::Escape });
            assert!(a.ui.transform.is_none());
            let restored = passive_paths(&a, &xf);
            assert_eq!(restored.len(), 1, "the passive current path returns after the session ends");
            if commit {
                assert_ne!(restored, original_outline, "the restored outline follows committed geometry");
                assert_eq!(a.session.active().unwrap().history.past_len(), history + 1);
                a.run("edit.undo", json!({})).unwrap();
            }
            assert_eq!(passive_paths(&a, &xf), original_outline);
            assert_eq!(a.session.active().unwrap().doc, before);
            assert_eq!(a.session.active().unwrap().history.past_len(), history);
        }
    }
}
