#![allow(clippy::expect_used)] // Test setup and assertions fail with context when prerequisites are missing.

//! The Save As chooser: both cards, "Don't show again", the settings toggle, keyboard use, and
//! Save on linked and new documents.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use craftcloud_client::{CloudError, CloudTransport, Request, Response};
use super::dialog::{Mode, View};
use egui::{Event, Key, Modifiers};
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use serde_json::json;
use uuid::Uuid;

use super::Config;
use crate::file_dialog::FileDialogRequest;
use crate::{PhotocraftApp, Services};

type Requests = Rc<RefCell<Vec<String>>>;

struct Offline;

impl CloudTransport for Offline {
    fn send(&self, _request: Request) -> Result<Response, CloudError> {
        Err(CloudError::Unreachable)
    }
}

fn app(data: &Path, requests: &Requests) -> PhotocraftApp {
    let held = requests.clone();
    let services = Services {
        cloud_data_dir: Some(data.to_path_buf()),
        file_dialog: Some(Box::new(move |request, _parent, _reply| {
            held.borrow_mut().push(match request {
                FileDialogRequest::Save { suggested } => suggested,
                _ => "other".into(),
            });
        })),
        ..Services::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    app.run("file.new", json!({"width": 16, "height": 16})).expect("new document");
    app
}

fn harness(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut harness = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::ALL[0]);
        app
    });
    harness.run_steps(5);
    harness
}

fn invoke(app: &mut PhotocraftApp, id: &str) -> Result<serde_json::Value, String> {
    crate::menus::invoke(app, &egui::Context::default(), id, json!({}))
}

fn press(harness: &mut Harness<'static, PhotocraftApp>, key: Key) {
    for pressed in [true, false] {
        harness.event(Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE });
        harness.run_steps(2);
    }
}

fn open_chooser(harness: &mut Harness<'static, PhotocraftApp>, command: &str) {
    let result = invoke(harness.state_mut(), command).expect("command starts");
    assert_eq!(result.get("chooser"), Some(&json!(true)), "{command} must ask where to save: {result}");
    harness.run_steps(3);
    assert!(harness.state().cloud.save_chooser_open());
}

#[test]
fn the_chooser_shows_both_choices_and_the_checkbox() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    for label in ["Save As", "Save to CraftCloud", "Access from any device, share with your team", "Save on this computer", "Don't show again", "Cancel"] {
        let _ = h.get_by_label(label);
    }
    assert!(h.state().cloud.blocks_input());
    assert!(requests.borrow().is_empty(), "no native dialog before the answer");
}

#[test]
fn choosing_cloud_opens_the_server_setup_when_cloud_is_not_configured() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    assert!(!h.state().cloud.configured());
    open_chooser(&mut h, "file.saveAs");
    h.get_by_label("Save to CraftCloud").click();
    h.run_steps(3);
    assert!(!h.state().cloud.save_chooser_open());
    assert_eq!(h.state().cloud.cloud_dialog_mode(), Some(Mode::Save));
    assert_eq!(h.state().cloud.dialog_view(), View::NoServer, "the dialog points to the settings");
    assert!(requests.borrow().is_empty(), "choosing CraftCloud never opens the native dialog");
}

#[test]
fn choosing_cloud_with_a_configured_server_goes_to_sign_in_with_the_save_format_choice() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut app = app(data.path(), &requests);
    app.cloud.config.trust("http://127.0.0.1:18095".into()).expect("trust test server");
    let mut h = harness(app);
    open_chooser(&mut h, "file.saveAs");
    h.get_by_label("Save to CraftCloud").click();
    h.run_steps(3);
    assert_eq!(h.state().cloud.cloud_dialog_mode(), Some(Mode::Save));
    assert_eq!(h.state().cloud.dialog_view(), View::SignIn);
    assert!(!h.state().cloud.save_as_psd, "native format is the default");
}

#[test]
fn choosing_the_computer_continues_with_the_native_save_as() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    h.get_by_label("Save on this computer").click();
    h.run_steps(4);
    assert!(!h.state().cloud.save_chooser_open());
    assert_eq!(h.state().cloud.picker.dialog, None, "the CraftCloud wizard stays closed");
    assert_eq!(requests.borrow().len(), 1, "exactly one native Save dialog");
    assert!(requests.borrow()[0].ends_with(".psd"), "the unchanged suggestion: {:?}", requests.borrow());
    assert!(h.state().cloud.asks_where_to_save(), "without the checkbox the chooser comes back next time");
}

#[test]
fn dont_show_again_persists_skips_the_chooser_and_the_setting_restores_it() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    h.get_by_label("Don't show again").click();
    h.run_steps(2);
    h.get_by_label("Save on this computer").click();
    h.run_steps(4);
    assert_eq!(requests.borrow().len(), 1);
    assert!(!h.state().cloud.asks_where_to_save());
    assert!(Config::load(Some(data.path())).expect("saved settings").skip_save_chooser, "stored in the CraftCloud settings");
    let text = std::fs::read_to_string(data.path().join("CraftCloud").join("app.json")).expect("settings file");
    assert!(!text.contains("token") && !text.contains("password"), "non-secret preference");

    // The next Save As goes straight to the native dialog (the first one is still open, so cancel it).
    h.state_mut().file_dialog = None;
    let result = invoke(h.state_mut(), "file.saveAs").expect("Save As");
    assert_eq!(result.get("fileDialog"), Some(&json!("save")), "native dialog without the chooser: {result}");
    assert!(!h.state().cloud.save_chooser_open());

    // A restarted app still skips it, and the setting brings it back.
    let mut restarted = app(data.path(), &requests);
    assert!(!restarted.cloud.asks_where_to_save());
    restarted.cloud.set_asks_where_to_save(true);
    assert!(!Config::load(Some(data.path())).expect("saved settings").skip_save_chooser);
    let result = invoke(&mut restarted, "file.saveAs").expect("Save As");
    assert_eq!(result.get("chooser"), Some(&json!(true)));
}

#[test]
fn the_preferences_toggle_turns_the_chooser_back_on() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut app = app(data.path(), &requests);
    app.cloud.set_asks_where_to_save(false);
    let id = crate::prefs_ui::open_preferences(&mut app, "craftcloud");
    let mut h = harness(app);
    h.run_steps(3);
    h.get_by_label("Ask where to save").click();
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(h.state().cloud.asks_where_to_save(), "dialog {id} applied the toggle");
    assert!(!Config::load(Some(data.path())).expect("saved settings").skip_save_chooser);
    let result = invoke(h.state_mut(), "file.saveAs").expect("Save As");
    assert_eq!(result.get("chooser"), Some(&json!(true)));
}

#[test]
fn escape_cancels_without_saving_or_remembering() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    h.get_by_label("Don't show again").click();
    h.run_steps(2);
    press(&mut h, Key::Escape);
    assert!(!h.state().cloud.save_chooser_open());
    assert!(!h.state().cloud.blocks_input());
    assert_eq!(h.state().cloud.picker.dialog, None);
    assert!(requests.borrow().is_empty());
    assert!(h.state().cloud.asks_where_to_save(), "cancelling does not remember anything");
}

#[test]
fn tab_walks_cloud_computer_checkbox_cancel() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    let focused = |h: &Harness<'static, PhotocraftApp>, label: &str| h.get_by_label(label).accesskit_node().is_focused();
    assert!(focused(&h, "Save to CraftCloud"), "the cloud card starts focused");
    for label in ["Save on this computer", "Don't show again", "Cancel"] {
        press(&mut h, Key::Tab);
        assert!(focused(&h, label), "Tab moves to {label}");
    }
}

#[test]
fn enter_chooses_cloud() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    press(&mut h, Key::Enter);
    assert!(!h.state().cloud.save_chooser_open());
    assert_eq!(h.state().cloud.cloud_dialog_mode(), Some(Mode::Save), "Enter means CraftCloud");
    assert!(requests.borrow().is_empty());
}

#[test]
fn enter_on_the_computer_card_chooses_the_computer() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.saveAs");
    press(&mut h, Key::Tab);
    press(&mut h, Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().cloud.picker.dialog, None);
    assert_eq!(requests.borrow().len(), 1);
}

#[test]
fn save_on_a_new_document_asks_where_to_save() {
    let data = tempfile::tempdir().expect("data dir");
    let requests = Requests::default();
    let mut h = harness(app(data.path(), &requests));
    open_chooser(&mut h, "file.save");
    h.get_by_label("Save on this computer").click();
    h.run_steps(4);
    assert_eq!(requests.borrow().len(), 1, "Save on a new document continues like Save As");
}

#[test]
fn save_on_a_cloud_linked_document_saves_a_version_without_the_chooser() {
    let root = tempfile::tempdir().expect("data dir");
    let local = root.path().join("working.pcraft");
    let written = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = written.clone();
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = session.active().expect("active document").doc.id;
    session.active_mut().expect("active document").path = Some(local.to_string_lossy().into_owned());
    let mut app = PhotocraftApp::new(
        session,
        Services {
            cloud_data_dir: Some(root.path().to_path_buf()),
            export: Some(Box::new(|_, _, _| Ok((b"snapshot".to_vec(), Vec::new())))),
            write: Some(Box::new(move |path, bytes| {
                std::fs::write(path, bytes).map_err(|error| error.to_string())?;
                seen.borrow_mut().push(path.to_string());
                Ok(())
            })),
            ..Services::default()
        },
    );
    let server = "http://127.0.0.1:18095";
    app.cloud = super::Runtime::with_test_transport(Some(root.path()), server, std::sync::Arc::new(Offline)).expect("test Cloud client");
    app.cloud
        .bind_test_document(
            doc,
            craftcloud_client_native::CloudLink {
                local_path: local.clone(),
                server: server.into(),
                file_id: Uuid::new_v4().to_string(),
                base_version: Some(Uuid::new_v4().to_string()),
                sha256: String::new(),
            },
        )
        .expect("bind Cloud document");
    assert!(app.cloud.asks_where_to_save());
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "file.save", json!({})).expect("Save a linked document");
    assert!(!app.cloud.save_chooser_open(), "Save on a linked document never asks");
    assert_eq!(written.borrow().as_slice(), [local.to_string_lossy().into_owned()], "it writes the working file, which uploads the version");
}
