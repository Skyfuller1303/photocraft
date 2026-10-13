use super::*;
use egui::{Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_engine::Session;

fn app() -> (PhotocraftApp, LayerId, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 120, "height": 80})).unwrap();
    let a = LayerId(s.execute("layer.new.layer", json!({"name": "Source"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("edit.fill", json!({"contents": "color", "color": "#ec6f32"})).unwrap();
    s.execute("layer.layerMask.hideAll", json!({})).unwrap();
    s.edit("fixture", |doc, _| {
        doc.layer_mut(a).unwrap().mask.as_mut().unwrap().surface.write_region(photocraft_geom::Rect::new(15, 10, 70, 55), &vec![1.0; 55 * 45]);
        Ok(())
    })
    .unwrap();
    let b = LayerId(s.execute("layer.new.layer", json!({"name": "Target"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("edit.fill", json!({"contents": "color", "color": "#2277cc"})).unwrap();
    s.execute("layer.select", json!({"layer": a.0})).unwrap();
    (PhotocraftApp::new(s, crate::Services::default()), a, b)
}

fn harness() -> (Harness<'static, PhotocraftApp>, LayerId, LayerId) {
    let (app, a, b) = app();
    let mut h = Harness::builder().with_size(vec2(1440.0, 1000.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new(
        "ui.set",
        json!({"theme":"promedium", "dockWidth": 310.0, "dock": {"collapsed": ["color", "properties", "history", "navigator"]}}),
    );
    crate::control::handle(h.state_mut(), &ctx, &req);
    h.run_steps(8);
    (h, a, b)
}

fn mask(app: &PhotocraftApp, id: LayerId) -> Option<LayerMask> {
    app.session.active().unwrap().doc.layer(id).unwrap().mask.clone()
}

fn drag(h: &mut Harness<'_, PhotocraftApp>, from: Pos2, to: Pos2, modifiers: Modifiers) {
    h.event(egui::Event::ModifiersChanged(modifiers));
    h.hover_at(from);
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers });
    h.run_steps(1);
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers });
    h.run_steps(1);
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
}

#[test]
fn mask_thumbnail_drags_copy_with_alt_and_move_without_reordering_layers() {
    for modifiers in [Modifiers::ALT, Modifiers::NONE] {
        let (mut h, a, b) = harness();
        let original = mask(h.state(), a).unwrap();
        let order: Vec<_> = h.state().session.active().unwrap().doc.layers.iter().map(|l| l.id).collect();
        let from = mask_thumbs_ui::recorded(&h.ctx, a.0).unwrap().0.into_iter().find(|(kind, _)| *kind == mask_thumbs_ui::MaskKind::Pixel).unwrap().1.center();
        let to = crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == b.0).unwrap().row.center();
        let steps = h.state().session.active().unwrap().history.past_len();
        drag(&mut h, from, to, modifiers);
        assert_eq!(mask(h.state(), b), Some(original.clone()));
        assert_eq!(mask(h.state(), a), modifiers.alt.then_some(original));
        assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);
        assert_eq!(h.state().session.active().unwrap().doc.layers.iter().map(|l| l.id).collect::<Vec<_>>(), order);
        assert!(!dragging(&h.ctx));
    }
}

#[test]
fn replacement_dialog_cancel_and_confirm_are_atomic() {
    let (mut h, a, b) = harness();
    let evidence = std::env::var_os("PHOTOCRAFT_MASK_EVIDENCE").map(std::path::PathBuf::from);
    if let Some(dir) = &evidence {
        std::fs::create_dir_all(dir).unwrap();
        h.render().unwrap().save(dir.join("mask-transfer-before.png")).unwrap();
    }
    h.state_mut().run(COPY, json!({"layer": a.0})).unwrap();
    h.state_mut().run("layer.layerMask.revealAll", json!({"layer": b.0})).unwrap();
    let before = mask(h.state(), b);
    let steps = h.state().session.active().unwrap().history.past_len();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, PASTE, json!({"layer": b.0})).unwrap();
    h.run_steps(3);
    assert!(h.state().mask_transfer.is_some());
    if let Some(dir) = &evidence {
        h.render().unwrap().save(dir.join("mask-transfer-replace.png")).unwrap();
    }
    assert_eq!(mask(h.state(), b), before);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().mask_transfer.is_none());
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps);
    crate::menus::invoke(h.state_mut(), &ctx, PASTE, json!({"layer": b.0})).unwrap();
    h.run_steps(3);
    h.get_by_label("Replace").click();
    h.run_steps(3);
    assert_eq!(mask(h.state(), b), mask(h.state(), a));
    if let Some(dir) = &evidence {
        h.render().unwrap().save(dir.join("mask-transfer-after.png")).unwrap();
    }
    assert_eq!(h.state().session.active().unwrap().history.past_len(), steps + 1);
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(mask(h.state(), b), before);
}

#[test]
fn changed_target_source_clipboard_or_document_cannot_receive_stale_confirmation() {
    for change in ["target", "source", "clipboard", "tab"] {
        let (mut app, a, b) = app();
        app.run(COPY, json!({"layer": a.0})).unwrap();
        app.run("layer.layerMask.revealAll", json!({"layer": b.0})).unwrap();
        let cmd = if change == "source" { TRANSFER } else { PASTE };
        app.run(cmd, json!({"from": a.0, "layer": b.0})).unwrap();
        assert!(app.mask_transfer.is_some());
        match change {
            "target" => {
                app.run("layer.layerMask.enabled", json!({"layer": b.0, "enabled": false})).unwrap();
            }
            "source" => {
                app.run("layer.layerMask.delete", json!({"layer": a.0})).unwrap();
            }
            "clipboard" => {
                app.session.mask_clipboard = None;
            }
            "tab" => {
                app.run("file.new", json!({"width": 12, "height": 8})).unwrap();
            }
            _ => panic!("unexpected fixture: {change}"),
        }
        let before = app.session.active().unwrap().doc.clone();
        assert!(confirm(&mut app).is_err(), "{change}");
        assert_eq!(app.session.active().unwrap().doc, before);
    }
}

#[test]
fn copy_paste_menus_are_available_and_automation_requires_explicit_replacement() {
    let (mut app, a, b) = app();
    let ctx = egui::Context::default();
    assert!(crate::menus::is_enabled(&app, COPY));
    assert!(!crate::menus::is_enabled(&app, PASTE));
    crate::menus::invoke(&mut app, &ctx, COPY, json!({})).unwrap();
    let items = crate::menus::menu_items(&app);
    assert!(items.iter().any(|m| m.id == PASTE && m.path == ["Edit"] && m.enabled));
    app.run("layer.layerMask.revealAll", json!({"layer": b.0})).unwrap();
    app.automation_input = true;
    assert!(app.run(PASTE, json!({"layer": b.0})).is_err());
    assert!(app.mask_transfer.is_none());
    app.run(PASTE, json!({"layer": b.0, "replace": true})).unwrap();
    assert_eq!(mask(&app, b), mask(&app, a));
}

#[test]
fn standard_dialog_answers_clear_the_prompt_and_validate_the_snapshot() {
    let (mut app, a, b) = app();
    app.run(COPY, json!({"layer": a.0})).unwrap();
    app.run("layer.layerMask.revealAll", json!({"layer": b.0})).unwrap();
    let before = mask(&app, b);
    let steps = app.session.active().unwrap().history.past_len();
    let pending = app.run(PASTE, json!({"layer": b.0})).unwrap();
    let dialog = pending["dialog"].as_u64().unwrap();
    assert_eq!(crate::dialogs::title(app.ui.dialogs.last().unwrap()), "Replace Layer Mask?");
    crate::dialogs::cancel(&mut app, dialog).unwrap();
    assert!(app.mask_transfer.is_none() && app.ui.dialogs.is_empty());
    assert_eq!(mask(&app, b), before);
    assert_eq!(app.session.active().unwrap().history.past_len(), steps);

    let pending = app.run(PASTE, json!({"layer": b.0})).unwrap();
    crate::dialogs::confirm(&mut app, pending["dialog"].as_u64().unwrap()).unwrap();
    assert!(app.mask_transfer.is_none() && app.ui.dialogs.is_empty());
    assert_eq!(mask(&app, b), mask(&app, a));
    assert_eq!(app.session.active().unwrap().history.past_len(), steps + 1);
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(mask(&app, b), before);

    let pending = app.run(PASTE, json!({"layer": b.0})).unwrap();
    app.run("layer.layerMask.enabled", json!({"layer": b.0, "enabled": false})).unwrap();
    let changed = mask(&app, b);
    assert!(crate::dialogs::confirm(&mut app, pending["dialog"].as_u64().unwrap()).is_err());
    assert!(app.mask_transfer.is_none() && app.ui.dialogs.is_empty());
    assert_eq!(mask(&app, b), changed);
}

#[test]
fn context_menu_transfers_to_the_clicked_layer() {
    let (mut h, a, b) = harness();
    let row = |h: &Harness<'_, PhotocraftApp>, id: LayerId| crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == id.0).unwrap().row.center();
    h.hover_at(row(&h, a));
    h.event(egui::Event::PointerButton { pos: row(&h, a), button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: row(&h, a), button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    if let Some(dir) = std::env::var_os("PHOTOCRAFT_MASK_EVIDENCE").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(dir.join("mask-transfer-menu.png")).unwrap();
    }
    h.get_by_label("Copy Mask").click();
    h.run_steps(3);
    assert!(h.state().session.mask_clipboard.is_some());
    h.hover_at(row(&h, b));
    h.event(egui::Event::PointerButton { pos: row(&h, b), button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: row(&h, b), button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    h.get_by_label("Paste Mask").click();
    h.run_steps(3);
    assert_eq!(mask(h.state(), b), mask(h.state(), a));
}
