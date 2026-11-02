#![allow(clippy::expect_used)] // Test setup and assertions fail with context when prerequisites are missing.

//! The guided save/open flow: auto-selection, remembered selection, project creation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use craftcloud_client::{CloudError, CloudTransport, Request, Response};
use craftcloud_egui::{Action, Dialog, Level};
use serde_json::json;
use uuid::Uuid;

use super::{Config, LastSelection, Runtime};
use crate::{PhotocraftApp, Services};

const SERVER: &str = "http://127.0.0.1:18095";

struct Hierarchy {
    organizations: Vec<(Uuid, String)>,
    workspaces: Vec<(Uuid, String)>,
    projects: Mutex<Vec<(Uuid, String)>>,
    create_status: u16,
    requests: Mutex<Vec<(String, String, String)>>,
}

impl Hierarchy {
    fn new(organizations: usize, workspaces: usize, projects: Vec<(Uuid, String)>) -> Arc<Self> {
        Arc::new(Self {
            organizations: (0..organizations).map(|i| (Uuid::from_u128(100 + i as u128), format!("Org {i}"))).collect(),
            workspaces: (0..workspaces).map(|i| (Uuid::from_u128(200 + i as u128), format!("Workspace {i}"))).collect(),
            projects: Mutex::new(projects),
            create_status: 201,
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
            ("GET", ["api", "v1", "projects", _, "files"]) => respond(200, page(Vec::new())),
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
    f.runtime.begin_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, Some(Uuid::from_u128(100)));
    assert_eq!(f.runtime.selected_workspace, Some(Uuid::from_u128(200)));
    assert_eq!(f.runtime.selected_project, None);
    assert!(f.runtime.picker.projects.is_empty());
    assert_eq!(f.runtime.picker.dialog, Some(Dialog::Browse));
    assert!(f.runtime.picker.error.is_none());
}

#[test]
fn several_choices_wait_for_the_user() {
    let mut f = fixture(Hierarchy::new(2, 1, Vec::new()), |_| {});
    f.runtime.begin_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, None);
    assert_eq!(f.runtime.picker.organizations.len(), 2);
}

#[test]
fn creating_a_project_posts_the_name_selects_it_and_loads_its_files() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.begin_open();
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
    f.runtime.begin_open();
    f.settle();
    f.runtime.handle_action(&mut f.app, Action::CreateProject { name: "Shots".into() });
    f.settle();
    assert_eq!(f.runtime.picker.error.as_deref(), Some("You are not allowed to create projects here."));
    assert_eq!(f.runtime.selected_project, None);
    assert_eq!(f.runtime.picker.dialog, Some(Dialog::Browse), "no raw error dialog");
}

#[test]
fn an_empty_project_name_is_not_sent() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.runtime.begin_open();
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
    f.runtime.begin_open();
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
    f.runtime.begin_open();
    f.settle();
    assert_eq!(f.runtime.selected_org, None);
    assert!(f.runtime.picker.error.is_none());
    assert_eq!(f.runtime.picker.dialog, Some(Dialog::Browse));
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
    f.runtime.begin_open();
    f.settle();
    assert_eq!(f.runtime.selected_project, Some(Uuid::from_u128(300)));
    f.runtime.handle_action(&mut f.app, Action::GoTo(Level::Projects));
    assert_eq!(f.runtime.selected_project, None);
    assert!(f.runtime.selected_workspace.is_some());
    f.runtime.handle_action(&mut f.app, Action::GoTo(Level::Organizations));
    assert!(f.runtime.selected_org.is_none() && f.runtime.selected_workspace.is_none());
    assert!(f.runtime.picker.workspaces.is_empty() && f.runtime.picker.projects.is_empty());
    assert_eq!(f.runtime.picker.dialog, Some(Dialog::Browse));
    f.runtime.sync_picker(&f.app);
    assert!(f.runtime.picker.selected_organization.is_none());
}

#[test]
fn the_save_name_is_prefilled_from_the_document_with_the_app_extension_once() {
    let mut f = fixture(Hierarchy::new(1, 1, Vec::new()), |_| {});
    f.app.session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    std::sync::Arc::make_mut(&mut f.app.session.active_mut().expect("active document").doc).name = "Holiday.png".into();
    f.runtime.begin_save();
    f.runtime.sync_picker(&f.app);
    assert_eq!(f.runtime.picker.file_name, "Holiday.pcraft");
    // The user's edit is not overwritten on later frames.
    f.runtime.picker.file_name = "Mine.pcraft".into();
    f.runtime.sync_picker(&f.app);
    assert_eq!(f.runtime.picker.file_name, "Mine.pcraft");
    f.settle();
    std::sync::Arc::make_mut(&mut f.app.session.active_mut().expect("active document").doc).name = "Layers.PSD".into();
    f.runtime.begin_save_psd();
    f.runtime.sync_picker(&f.app);
    assert_eq!(f.runtime.picker.file_name, "Layers.psd");
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
