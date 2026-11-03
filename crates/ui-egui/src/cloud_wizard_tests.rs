#![allow(clippy::expect_used)] // Test setup and assertions fail with context when prerequisites are missing.

//! The guided save/open flow: auto-selection, remembered selection, project creation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use craftcloud_client::{CloudError, CloudTransport, Request, Response};
use craftcloud_egui::{Action, Dialog, Level};
use serde_json::json;
use uuid::Uuid;

use super::dialog::{Mode, View};
use super::{Config, LastSelection, Runtime};
use crate::{PhotocraftApp, Services};

const SERVER: &str = "http://127.0.0.1:18095";

struct Hierarchy {
    organizations: Vec<(Uuid, String)>,
    workspaces: Vec<(Uuid, String)>,
    projects: Mutex<Vec<(Uuid, String)>>,
    create_status: u16,
    /// Files every project lists.
    files: Vec<serde_json::Value>,
    requests: Mutex<Vec<(String, String, String)>>,
}

impl Hierarchy {
    fn new(organizations: usize, workspaces: usize, projects: Vec<(Uuid, String)>) -> Arc<Self> {
        Arc::new(Self {
            organizations: (0..organizations).map(|i| (Uuid::from_u128(100 + i as u128), format!("Org {i}"))).collect(),
            workspaces: (0..workspaces).map(|i| (Uuid::from_u128(200 + i as u128), format!("Workspace {i}"))).collect(),
            projects: Mutex::new(projects),
            create_status: 201,
            files: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self, method: &str) -> Vec<(String, String)> {
        self.requests.lock().expect("request log").iter().filter(|r| r.0 == method).map(|r| (r.1.clone(), r.2.clone())).collect()
    }
}

fn page(items: Vec<serde_json::Value>) -> Vec<u8> {
    json!({"items": items, "next_cursor": null}).to_string().into_bytes()
}

fn respond(status: u16, body: Vec<u8>) -> Result<Response, CloudError> {
    Ok(Response { status, headers: Vec::new(), body })
}

impl CloudTransport for Hierarchy {
    fn send(&self, request: Request) -> Result<Response, CloudError> {
        let path = request.path.split("127.0.0.1:18095").nth(1).unwrap_or(&request.path).to_string();
        self.requests.lock().expect("request log").push((request.method.clone(), path.clone(), String::from_utf8_lossy(&request.body).into_owned()));
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["api", "v1", "organizations"]) => respond(
                200,
                page(
                    self.organizations
                        .iter()
                        .map(|(id, name)| json!({"id": id, "slug": name, "name": name, "role": "owner", "version": 1, "created_at": "now"}))
                        .collect(),
                ),
            ),
            ("GET", ["api", "v1", "organizations", org, "workspaces"]) => respond(
                200,
                page(
                    self.workspaces
                        .iter()
                        .map(|(id, name)| {
                            json!({"id": id, "organization_id": org, "slug": name, "name": name, "workspace_role": "owner", "version": 1, "created_at": "now"})
                        })
                        .collect(),
                ),
            ),
            ("GET", ["api", "v1", "workspaces", workspace, "projects"]) => respond(
                200,
                page(
                    self.projects
                        .lock()
                        .expect("projects")
                        .iter()
                        .map(|(id, name)| project_json(*id, workspace, name))
                        .collect(),
                ),
            ),
            ("GET", ["api", "v1", "projects", _, "files"]) => respond(200, page(self.files.clone())),
            ("GET", ["api", "v1", "organizations", _, "shared-with-me"]) => respond(200, page(Vec::new())),
            ("POST", ["api", "v1", "workspaces", workspace, "projects"]) => {
                if self.create_status != 201 {
                    return respond(self.create_status, br#"{"error":{"code":"FORBIDDEN","message":"no","request_id":null}}"#.to_vec());
                }
                let body: serde_json::Value = serde_json::from_slice(&request.body).map_err(|_| CloudError::InvalidResponse)?;
                let name = body.get("name").and_then(serde_json::Value::as_str).ok_or(CloudError::InvalidResponse)?.to_string();
                let id = Uuid::from_u128(900);
                self.projects.lock().expect("projects").push((id, name.clone()));
                respond(201, project_json(id, workspace, &name).to_string().into_bytes())
            }
            _ => respond(404, br#"{"error":{"code":"NOT_FOUND","message":"x","request_id":null}}"#.to_vec()),
        }
    }
}

fn project_json(id: Uuid, workspace: &str, name: &str) -> serde_json::Value {
    json!({"id": id, "organization_id": Uuid::from_u128(100), "workspace_id": workspace, "name": name, "version": 1, "created_at": "now", "can_share": true})
}

struct Fixture {
    runtime: Runtime,
    app: PhotocraftApp,
    transport: Arc<Hierarchy>,
    root: tempfile::TempDir,
}

fn fixture(transport: Arc<Hierarchy>, config: impl FnOnce(&mut Config)) -> Fixture {
    let root = tempfile::tempdir().expect("temp dir");
    let mut runtime = Runtime::with_test_transport(Some(root.path()), SERVER, transport.clone()).expect("test Cloud client");
    config(&mut runtime.config);
    let app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    Fixture { runtime, app, transport, root }
}

impl Fixture {
    /// Applies worker results until no request is in flight any more.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.runtime.apply_worker_result(&mut self.app);
            if !self.runtime.request_busy {
                // A result may start the next request; give it one more round to prove idle.
                std::thread::sleep(Duration::from_millis(20));
                self.runtime.apply_worker_result(&mut self.app);
                if !self.runtime.request_busy {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "Cloud requests did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn single_organization_and_workspace_are_selected_and_an_empty_project_list_is_shown() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, Some(Uuid::from_u128(100)));
    assert_eq!(f.runtime.selected_workspace, Some(Uuid::from_u128(200)));
    assert_eq!(f.runtime.selected_project, None);
    assert!(f.runtime.picker.projects.is_empty());
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Open));
    assert_eq!(f.runtime.dialog_view(), View::Projects);
    assert!(f.runtime.picker.error.is_none());
}

#[test]
fn several_choices_wait_for_the_user() {
    let mut f = fixture(Hierarchy::new(2, 1, Vec::new()), |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, None);
    assert_eq!(f.runtime.picker.organizations.len(), 2);
}

#[test]
fn creating_a_project_posts_the_name_selects_it_and_loads_its_files() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    f.runtime.picker.new_project_name = "Shots".into();
    f.runtime.handle_action(&mut f.app, Action::CreateProject { name: "  Shots ".into() });
    f.settle();
    let posts = f.transport.seen("POST");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, format!("/api/v1/workspaces/{}/projects", Uuid::from_u128(200)));
    assert_eq!(posts[0].1, r#"{"name":"Shots"}"#);
    assert_eq!(f.runtime.selected_project, Some(Uuid::from_u128(900)));
    assert!(f.runtime.picker.new_project_name.is_empty());
    assert!(f.runtime.picker.projects.iter().any(|(_, name)| name == "Shots"));
    assert!(f.transport.seen("GET").iter().any(|(path, _)| path == &format!("/api/v1/projects/{}/files", Uuid::from_u128(900))));
}

#[test]
fn forbidden_project_creation_shows_a_readable_message() {
    let mut transport = Hierarchy::new(1, 1, Vec::new());
    Arc::get_mut(&mut transport).expect("unshared transport").create_status = 403;
    let mut f = fixture(transport, |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    f.runtime.handle_action(&mut f.app, Action::CreateProject { name: "Shots".into() });
    f.settle();
    assert_eq!(f.runtime.picker.error.as_deref(), Some("You are not allowed to create projects here."));
    assert_eq!(f.runtime.selected_project, None);
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Open), "no raw error dialog");
    assert_eq!(f.runtime.picker.dialog, None);
}

#[test]
fn an_empty_project_name_is_not_sent() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    f.runtime.handle_action(&mut f.app, Action::CreateProject { name: "   ".into() });
    f.settle();
    assert!(f.transport.seen("POST").is_empty());
}

#[test]
fn the_remembered_selection_is_preselected_and_survives_a_restart() {
    let project = Uuid::from_u128(300);
    let transport = Hierarchy::new(2, 2, vec![(project, "Remembered".into()), (Uuid::from_u128(301), "Other".into())]);
    let remembered = LastSelection {
        organization: Some(Uuid::from_u128(101).to_string()),
        workspace: Some(Uuid::from_u128(201).to_string()),
        project: Some(project.to_string()),
    };
    let mut f = fixture(transport, |config| {
        config.last_used.insert(SERVER.into(), remembered.clone());
    });
    f.runtime.show_cloud_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, Some(Uuid::from_u128(101)));
    assert_eq!(f.runtime.selected_workspace, Some(Uuid::from_u128(201)));
    assert_eq!(f.runtime.selected_project, Some(project));
    // Selecting persisted the ids (and nothing secret) next to the trusted servers.
    let saved = Config::load(Some(f.root.path())).expect("saved config");
    assert_eq!(saved.last_used.get(SERVER), Some(&remembered));
    let text = std::fs::read_to_string(f.root.path().join("CraftCloud").join("app.json")).expect("config file");
    assert!(!text.contains("token") && !text.contains("password"));
}

#[test]
fn a_remembered_selection_that_no_longer_exists_falls_back_silently() {
    let gone = LastSelection {
        organization: Some(Uuid::from_u128(999).to_string()),
        workspace: Some(Uuid::from_u128(998).to_string()),
        project: Some(Uuid::from_u128(997).to_string()),
    };
    let mut f = fixture(Hierarchy::new(2, 1, Vec::new()), |config| {
        config.last_used.insert(SERVER.into(), gone);
    });
    f.runtime.show_cloud_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, None);
    assert!(f.runtime.picker.error.is_none());
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Open));
    assert_eq!(f.runtime.dialog_view(), View::Organizations);
}

#[test]
fn a_config_written_before_the_remembered_selection_still_loads() {
    let config: Config =
        serde_json::from_str(r#"{"servers":[{"url":"https://cloud.example","trusted":true}],"selected_server":"https://cloud.example"}"#).expect("old config");
    assert!(config.last_used.is_empty());
}

#[test]
fn going_back_clears_the_selection_from_that_level_inwards_and_does_not_jump_forward() {
    let transport = Hierarchy::new(1, 1, vec![(Uuid::from_u128(300), "Only".into())]);
    let mut f = fixture(transport, |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    assert_eq!(f.runtime.selected_project, Some(Uuid::from_u128(300)));
    f.runtime.handle_action(&mut f.app, Action::GoTo(Level::Projects));
    assert_eq!(f.runtime.selected_project, None);
    assert!(f.runtime.selected_workspace.is_some());
    f.runtime.handle_action(&mut f.app, Action::GoTo(Level::Organizations));
    assert!(f.runtime.selected_org.is_none() && f.runtime.selected_workspace.is_none());
    assert!(f.runtime.picker.workspaces.is_empty() && f.runtime.picker.projects.is_empty());
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Open));
    f.runtime.sync_picker();
    assert!(f.runtime.picker.selected_organization.is_none());
}

/// A context that has run once, so text can be laid out.
fn ready_context() -> egui::Context {
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
    ctx.run_ui(Default::default(), |_| {}).drop_without_applying_deltas();
    ctx
}

/// One frame of the Cloud dialog.
fn draw(f: &mut Fixture, ctx: &egui::Context) {
    ctx.run_ui(Default::default(), |ui| f.runtime.show_cloud_dialog(&mut f.app, &ui.ctx().clone())).drop_without_applying_deltas();
}

#[test]
fn the_save_name_is_prefilled_from_the_document_without_extension_once() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    std::sync::Arc::make_mut(&mut f.app.session.active_mut().expect("active document").doc).name = "Holiday.png".into();
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
    let ctx = ready_context();
    draw(&mut f, &ctx);
    assert_eq!(f.runtime.cloud_dialog_name(), Some("Holiday"));
    // The user's edit is not overwritten on later frames.
    f.runtime.cloud_dialog.as_mut().expect("dialog").name = "Mine".into();
    draw(&mut f, &ctx);
    assert_eq!(f.runtime.cloud_dialog_name(), Some("Mine"));
    f.settle();
    f.runtime.handle_action(&mut f.app, Action::Cancel);
    std::sync::Arc::make_mut(&mut f.app.session.active_mut().expect("active document").doc).name = "Layers.PSD".into();
    f.runtime.begin_save_psd();
    draw(&mut f, &ctx);
    assert_eq!(f.runtime.cloud_dialog_name(), Some("Layers"));
    assert!(f.runtime.cloud_dialog.as_ref().expect("dialog").psd, "Save as PSD opens on the Photoshop format");
}

#[test]
fn the_menu_items_open_the_save_dialog_in_the_chosen_format() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.begin_save();
    assert!(!f.runtime.save_as_psd && !f.runtime.cloud_dialog.as_ref().expect("dialog").psd, "native format is the default");
    f.runtime.handle_action(&mut f.app, Action::Cancel);
    f.runtime.begin_save_psd();
    assert!(f.runtime.save_as_psd && f.runtime.cloud_dialog.as_ref().expect("dialog").psd, "Save as PSD opens on Photoshop");
    // A second request keeps the dialog that is up.
    f.runtime.begin_save();
    assert!(f.runtime.cloud_dialog.as_ref().expect("dialog").psd);
    assert_eq!(super::strip_format_extension("a.PSD"), "a");
    assert_eq!(super::strip_format_extension("a.png"), "a.png");
    assert_eq!(super::strip_format_extension("ä"), "ä");
}

#[test]
fn a_name_handed_over_by_the_caller_wins_and_loses_a_path_and_extension() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, Some(r"C:\Pictures\Poster.psd".into()), crate::FormatChoices::default(), false);
    draw(&mut f, &ready_context());
    assert_eq!(f.runtime.cloud_dialog_name(), Some("Poster"));
    assert_eq!(f.runtime.cloud_dialog.as_ref().expect("dialog").doc, Some(doc));
}

#[test]
fn show_cloud_save_opens_at_the_last_used_project() {
    let project = Uuid::from_u128(300);
    let transport = Hierarchy::new(2, 2, vec![(project, "Remembered".into()), (Uuid::from_u128(301), "Other".into())]);
    let remembered = LastSelection {
        organization: Some(Uuid::from_u128(101).to_string()),
        workspace: Some(Uuid::from_u128(201).to_string()),
        project: Some(project.to_string()),
    };
    let mut f = fixture(transport, |config| {
        config.last_used.insert(SERVER.into(), remembered);
    });
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
    f.settle();
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Save));
    assert_eq!(f.runtime.dialog_view(), View::Files);
    assert_eq!(f.runtime.selected_project, Some(project));
}

/// The fixture's app in a real frame loop. The temp dir stays with the returned guard.
fn frame_loop(f: Fixture) -> (egui_kittest::Harness<'static, PhotocraftApp>, tempfile::TempDir) {
    let Fixture { runtime, mut app, root, .. } = f;
    app.cloud = runtime;
    let harness = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::ALL[0]);
        app
    });
    (harness, root)
}

fn settle_frames(h: &mut egui_kittest::Harness<'static, PhotocraftApp>) {
    for _ in 0..400 {
        h.run_steps(1);
        if !h.state().cloud.request_busy {
            h.run_steps(2);
            if !h.state().cloud.request_busy {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("Cloud requests did not settle");
}

#[test]
fn the_open_dialog_has_a_shared_with_me_row_that_lists_the_shared_files() {
    use egui_kittest::kittest::Queryable;
    let mut f = fixture(Hierarchy::new(1, 2, Vec::new()), |_| {});
    f.runtime.show_cloud_open();
    f.settle();
    assert_eq!(f.runtime.dialog_view(), View::Workspaces, "the single organization is skipped");
    let (mut h, _root) = frame_loop(f);
    h.run_steps(3);
    h.get_by_label("Shared with me").click();
    settle_frames(&mut h);
    assert_eq!(h.state().cloud.dialog_view(), View::Shared);
    assert!(h.state().cloud.picker.error.is_none());
    let _ = h.get_by_label("Open from CraftCloud");
    h.get_by_label("Back").click();
    settle_frames(&mut h);
    assert_eq!(h.state().cloud.dialog_view(), View::Workspaces);
}

#[test]
fn the_save_dialog_has_no_shared_with_me_row() {
    use egui_kittest::kittest::Queryable;
    let mut f = fixture(Hierarchy::new(1, 2, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
    f.settle();
    let (mut h, _root) = frame_loop(f);
    h.run_steps(3);
    assert!(h.query_by_label("Shared with me").is_none());
    let _ = h.get_by_label("Save to CraftCloud");
    let _ = h.get_by_label("Workspace 0");
}

#[test]
fn save_stays_disabled_until_a_project_is_chosen() {
    use egui_kittest::kittest::{NodeT, Queryable};
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, Some("Poster".into()), crate::FormatChoices::default(), false);
    f.settle();
    assert_eq!(f.runtime.dialog_view(), View::Projects);
    let (mut h, _root) = frame_loop(f);
    h.run_steps(3);
    assert!(h.get_by_label("Save").accesskit_node().is_disabled(), "no project yet");
    let mut cloud = std::mem::replace(&mut h.state_mut().cloud, Runtime::new(None));
    cloud.handle_action(h.state_mut(), Action::CreateProject { name: "Shots".into() });
    h.state_mut().cloud = cloud;
    settle_frames(&mut h);
    assert_eq!(h.state().cloud.dialog_view(), View::Files);
    assert!(!h.get_by_label("Save").accesskit_node().is_disabled(), "a project and a valid name");
}

#[test]
fn this_computer_hands_the_save_back_to_the_native_dialog_and_the_open_to_the_native_open() {
    use egui_kittest::kittest::Queryable;
    use std::cell::RefCell;
    use std::rc::Rc;
    let asked = Rc::new(RefCell::new(Vec::<String>::new()));
    let held = asked.clone();
    let services = Services {
        file_dialog: Some(Box::new(move |request, _parent, _reply| {
            held.borrow_mut().push(match request {
                crate::file_dialog::FileDialogRequest::Save { .. } => "save".into(),
                crate::file_dialog::FileDialogRequest::Open { .. } => "open".into(),
                _ => "other".into(),
            });
        })),
        ..Services::default()
    };
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    f.app.run("file.new", json!({"width": 16, "height": 16})).expect("new document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
    let (mut h, _root) = frame_loop(f);
    h.run_steps(3);
    assert_eq!(h.state().cloud.cloud_dialog_mode(), Some(Mode::Save));
    h.get_by_label("This computer").click();
    h.run_steps(4);
    assert_eq!(h.state().cloud.cloud_dialog_mode(), None, "the dialog closes");
    assert_eq!(asked.borrow().as_slice(), ["save"], "exactly one native Save dialog");

    h.state_mut().file_dialog = None;
    h.state_mut().cloud.show_cloud_open();
    h.run_steps(3);
    h.get_by_label("This computer").click();
    h.run_steps(4);
    assert_eq!(h.state().cloud.cloud_dialog_mode(), None);
    assert_eq!(asked.borrow().as_slice(), ["save", "open"]);
}

/// Delivers `event` as the result of a request that is running now.
fn deliver(f: &mut Fixture, event: super::WorkerEvent) {
    let context = f.runtime.begin_request(SERVER).expect("request starts");
    f.runtime.request_tx.send(super::WorkerResult { context, event }).expect("send result");
    f.settle();
}

#[test]
fn only_the_upload_the_save_dialog_started_closes_it() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
    f.settle();
    let background = std::path::PathBuf::from("background.pcraft");
    deliver(&mut f, super::WorkerEvent::Upload(Ok((background, super::SaveOutcome::LocalOnly))));
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Save), "a Ctrl+S upload finishing leaves the dialog open");

    let started = std::path::PathBuf::from("dialog.pcraft");
    f.runtime.dialog_upload = Some(started.clone());
    deliver(&mut f, super::WorkerEvent::Upload(Ok((started, super::SaveOutcome::LocalOnly))));
    assert_eq!(f.runtime.cloud_dialog_mode(), None, "its own upload closes it");

    // A cancelled dialog forgets its upload: the late result does not close the next dialog.
    f.runtime.show_cloud_open();
    f.runtime.dialog_upload = Some(std::path::PathBuf::from("late.pcraft"));
    f.runtime.handle_action(&mut f.app, Action::Cancel);
    f.runtime.show_cloud_open();
    deliver(&mut f, super::WorkerEvent::Upload(Ok((std::path::PathBuf::from("late.pcraft"), super::SaveOutcome::LocalOnly))));
    assert_eq!(f.runtime.cloud_dialog_mode(), Some(Mode::Open));
}

#[test]
fn the_save_dialog_never_opens_on_shared_files() {
    let mut f = fixture(Hierarchy::new(1, 2, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = f.app.session.active().expect("active document").doc.id;
    f.runtime.show_cloud_open();
    f.settle();
    f.runtime.handle_action(&mut f.app, Action::OpenShared);
    f.settle();
    assert_eq!(f.runtime.dialog_view(), View::Shared);
    f.runtime.handle_action(&mut f.app, Action::Cancel);
    assert_ne!(f.runtime.dialog_view(), View::Shared, "cancel leaves the shared list");
    f.runtime.browsing_shared = true;
    assert!(f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false));
    assert_ne!(f.runtime.dialog_view(), View::Shared);
}

#[test]
fn a_second_dialog_request_says_it_was_not_opened() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    assert!(f.runtime.show_cloud_open());
    assert!(!f.runtime.show_cloud_open(), "the caller must tell the user");
    f.runtime.handle_action(&mut f.app, Action::Cancel);
    f.runtime.picker.dialog = Some(Dialog::Conflict);
    assert!(!f.runtime.show_cloud_open(), "a prompt is up");
}

#[test]
fn saving_a_version_for_a_closed_document_says_so() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.show_cloud_save(photocraft_doc::DocId(987_654), None, crate::FormatChoices::default(), false);
    f.settle();
    let file: craftcloud_client::File = serde_json::from_value(file_json("Poster.pcraft", 1, None)).expect("file");
    let id = file.id;
    f.runtime.files.insert(id, file);
    f.runtime.handle_action(&mut f.app, Action::SaveVersion { file_id: id.to_string() });
    assert_eq!(f.runtime.picker.error.as_deref(), Some("Open a document before saving to CraftCloud."));
}

#[test]
fn a_tls_failure_is_reported_as_such_and_not_as_an_offline_save() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.picker.dialog = Some(Dialog::Login);
    f.runtime.show_error(CloudError::TlsHandshake);
    let error = f.runtime.picker.error.clone().expect("error text");
    assert!(error.starts_with("The server does not speak HTTPS at this address"), "{error}");
    assert_eq!(f.runtime.picker.dialog, Some(Dialog::Login), "the offline-save dialog is not shown");
}

fn file_json(name: &str, size: u64, lock: Option<&str>) -> serde_json::Value {
    let mut file = json!({
        "id": Uuid::new_v4(), "organization_id": Uuid::from_u128(100), "workspace_id": Uuid::from_u128(200), "project_id": Uuid::from_u128(300),
        "name": name, "app": "photocraft", "format": "pcraft", "size_bytes": size, "sha256": null, "head_version_id": Uuid::new_v4(),
        "version": 3, "created_at": "2026-11-02T10:11:12Z"
    });
    if let Some(holder) = lock {
        file["lock"] = json!({"holder_display_name": holder, "since": "now", "expires_at": "later", "mine": false, "held_by_current_session": false});
    }
    file
}

/// Renders both dialogs in a dark and a light theme to PNGs when `PHOTOCRAFT_CLOUD_SCREENS` names a
/// directory. Without it the test does nothing (it needs a GPU adapter).
#[test]
fn the_dialogs_render_to_screenshots_when_asked() {
    let Some(dir) = std::env::var_os("PHOTOCRAFT_CLOUD_SCREENS").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).expect("screenshot directory");
    for (theme_name, theme) in [("dark", crate::theme::ThemeKind::ProMedium), ("light", crate::theme::ThemeKind::ProLight)] {
        for mode in ["save", "save-files", "open", "open-files", "signin", "settings"] {
            let mut transport = Hierarchy::new(1, 1, vec![(Uuid::from_u128(300), "Brand campaign".into()), (Uuid::from_u128(301), "Website".into())]);
            Arc::get_mut(&mut transport).expect("unshared transport").files =
                vec![file_json("Poster.pcraft", 2_400_000, None), file_json("Logo sheet.psd", 18_000_000, Some("Anna")), file_json("Banner.pcraft", 640_000, None)];
            let mut f = fixture(transport, |config| {
                config.servers[0].account = Some("anna@studio.example".into());
                config.servers.push(super::TrustedServer { url: "https://cloud.example.org".into(), trusted: true, account: None });
            });
            f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
            std::sync::Arc::make_mut(&mut f.app.session.active_mut().expect("active document").doc).name = "Summer poster.png".into();
            let doc = f.app.session.active().expect("active document").doc.id;
            match mode {
                "save" | "save-files" => {
                    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
                }
                "open" | "open-files" => {
                    f.runtime.show_cloud_open();
                }
                "signin" => {
                    f.runtime.client = None;
                    f.runtime.client_server = None;
                    f.runtime.show_cloud_save(doc, None, crate::FormatChoices::default(), false);
                }
                _ => {}
            }
            f.settle();
            if mode.ends_with("-files") {
                f.runtime.handle_action(&mut f.app, Action::SelectProject(Uuid::from_u128(300).to_string()));
                f.settle();
            }
            if mode == "settings" {
                crate::prefs_ui::open_preferences(&mut f.app, "craftcloud");
            }
            let (mut h, _root) = frame_loop(f);
            let ctx = h.ctx.clone();
            crate::PhotocraftApp::setup_context(&ctx, theme);
            h.run_steps(4);
            let image = h.render().expect("render the frame");
            image.save(dir.join(format!("{mode}-{theme_name}.png"))).expect("save the screenshot");
        }
    }
}
