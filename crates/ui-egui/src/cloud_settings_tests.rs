#![allow(clippy::expect_used)] // Test setup and assertions fail with context when prerequisites are missing.

//! Preferences › CraftCloud: the server list, switching servers and the account block.

use std::sync::Arc;

use craftcloud_client::{CloudError, CloudTransport, Request, Response};
use uuid::Uuid;

use super::{Config, LastSelection, Runtime};

const SERVER: &str = "http://127.0.0.1:18095";

struct NoCalls;

impl CloudTransport for NoCalls {
    fn send(&self, _request: Request) -> Result<Response, CloudError> {
        Ok(Response { status: 200, headers: Vec::new(), body: br#"{"items":[],"next_cursor":null}"#.to_vec() })
    }
}

fn saved(root: &std::path::Path) -> Config {
    Config::load(Some(root)).expect("saved settings")
}

#[test]
fn adding_selecting_and_removing_servers_persists_and_forgets_what_was_remembered() {
    let root = tempfile::tempdir().expect("data dir");
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.add_server("cloud.example.org").expect("a bare host gets https");
    assert_eq!(runtime.config.selected().map(|server| server.url.as_str()), Some("https://cloud.example.org"));
    runtime.add_server("localhost:8080").expect("a loopback host gets http");
    assert_eq!(runtime.config.selected().map(|server| server.url.as_str()), Some("http://localhost:8080"));
    assert_eq!(saved(root.path()).servers.len(), 2);
    assert_eq!(saved(root.path()).selected_server.as_deref(), Some("http://localhost:8080"));

    runtime.select_server("https://cloud.example.org").expect("select a trusted server");
    assert_eq!(saved(root.path()).selected_server.as_deref(), Some("https://cloud.example.org"));
    assert!(runtime.select_server("https://unknown.example").is_err(), "only trusted servers can be selected");

    let remembered = LastSelection { organization: Some(Uuid::new_v4().to_string()), workspace: None, project: None };
    runtime.config.last_used.insert("http://localhost:8080".into(), remembered.clone());
    runtime.config.last_used.insert("https://cloud.example.org".into(), remembered);
    runtime.remove_server("http://localhost:8080").expect("remove a server");
    let config = saved(root.path());
    assert_eq!(config.servers.iter().map(|server| server.url.as_str()).collect::<Vec<_>>(), ["https://cloud.example.org"]);
    assert_eq!(config.selected_server.as_deref(), Some("https://cloud.example.org"), "removing another server keeps the selection");
    assert!(!runtime.config.last_used.contains_key("http://localhost:8080"), "its remembered folders go with it");

    runtime.remove_server("https://cloud.example.org").expect("remove the selected server");
    let config = saved(root.path());
    assert!(config.servers.is_empty() && config.selected_server.is_none() && config.last_used.is_empty());
    assert!(runtime.config.selected().is_none());
}

#[test]
fn invalid_addresses_are_refused_and_nothing_changes_while_a_request_runs() {
    let root = tempfile::tempdir().expect("data dir");
    let mut runtime = Runtime::new(Some(root.path()));
    assert!(runtime.add_server("   ").is_err());
    assert!(runtime.add_server("http://cloud.example").is_err(), "plain http is only for loopback hosts");
    assert!(runtime.add_server("https://cloud.example/path").is_err());
    assert!(runtime.config.servers.is_empty());

    runtime.add_server("https://cloud.example").expect("add");
    let context = runtime.begin_request("https://cloud.example").expect("request starts");
    assert!(runtime.add_server("https://other.example").is_err());
    assert!(runtime.select_server("https://other.example").is_err());
    assert!(runtime.remove_server("https://cloud.example").is_err());
    assert_eq!(runtime.config.servers.len(), 1);
    drop(context);
}

#[test]
fn the_settings_file_never_holds_credentials() {
    let root = tempfile::tempdir().expect("data dir");
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.add_server("https://cloud.example").expect("add");
    runtime.picker.password = "hunter2".into();
    runtime.picker.email = "me@example.com".into();
    runtime.add_server("https://other.example").expect("add");
    let text = std::fs::read_to_string(root.path().join("CraftCloud").join("app.json")).expect("settings file");
    assert!(!text.contains("hunter2") && !text.contains("token") && !text.contains("password"), "{text}");
}

#[test]
fn switching_servers_drops_the_session_and_the_loaded_hierarchy() {
    let root = tempfile::tempdir().expect("data dir");
    let mut runtime = Runtime::with_test_transport(Some(root.path()), SERVER, Arc::new(NoCalls)).expect("test Cloud client");
    runtime.selected_org = Some(Uuid::new_v4());
    runtime.selected_workspace = Some(Uuid::new_v4());
    runtime.selected_project = Some(Uuid::new_v4());
    runtime.picker.organizations.push((Uuid::new_v4().to_string(), "Org".into()));
    runtime.picker.selected_file = Some(Uuid::new_v4().to_string());
    runtime.browsing_shared = true;
    assert!(runtime.signed_in_account().is_some());

    runtime.add_server("https://cloud.example.org").expect("add and switch");
    assert!(runtime.client.is_none() && runtime.client_server.is_none(), "the other server's session is left");
    assert!(runtime.selected_org.is_none() && runtime.selected_workspace.is_none() && runtime.selected_project.is_none());
    assert!(runtime.picker.organizations.is_empty() && runtime.picker.selected_file.is_none());
    assert!(!runtime.browsing_shared);
    assert_eq!(runtime.picker.server_url, "https://cloud.example.org");
    assert!(runtime.signed_in_account().is_none(), "signed out until the stored session or a sign-in");

    runtime.select_server(SERVER).expect("back to the first server");
    assert_eq!(runtime.picker.server_url, SERVER);
    assert!(runtime.client.is_none(), "no stored account, so no session is restored");
}

#[test]
fn removing_the_signed_in_server_signs_out_locally() {
    let root = tempfile::tempdir().expect("data dir");
    let mut runtime = Runtime::with_test_transport(Some(root.path()), SERVER, Arc::new(NoCalls)).expect("test Cloud client");
    runtime.remove_server(SERVER).expect("remove");
    assert!(runtime.client.is_none() && runtime.config.selected().is_none());
    assert_eq!(runtime.dialog_view(), super::dialog::View::NoServer);
}

#[test]
fn the_page_shows_the_account_and_a_sign_out_button_when_signed_in() {
    let root = tempfile::tempdir().expect("data dir");
    let mut runtime = Runtime::with_test_transport(Some(root.path()), SERVER, Arc::new(NoCalls)).expect("test Cloud client");
    runtime.config.servers[0].account = Some("me@example.com".into());
    let signed_in = page_text(&mut runtime);
    assert!(signed_in.contains("Signed in as me@example.com"), "{signed_in}");
    assert!(signed_in.contains("Sign out") && signed_in.contains(SERVER) && signed_in.contains("Add server"), "{signed_in}");
    runtime.client = None;
    runtime.client_server = None;
    let signed_out = page_text(&mut runtime);
    assert!(signed_out.contains("Not signed in") && signed_out.contains("Password"), "{signed_out}");
    assert!(!signed_out.contains("Signed in as"), "{signed_out}");
}

fn page_text(runtime: &mut Runtime) -> String {
    fn collect(shape: &egui::epaint::Shape, into: &mut String) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                into.push_str(text.galley.text());
                into.push('\n');
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, into)),
            _ => {}
        }
    }
    let ctx = egui::Context::default();
    crate::PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
    let mut text = String::new();
    for _ in 0..2 {
        text.clear();
        let output = ctx.run_ui(Default::default(), |ui| {
            let mut ask = true;
            runtime.prefs_page(ui, Some(&mut ask));
        });
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut text);
        }
        output.drop_without_applying_deltas();
    }
    text
}

#[test]
fn the_page_in_preferences_offers_the_ask_toggle_everywhere_but_on_windows() {
    use egui_kittest::kittest::Queryable;
    let data = tempfile::tempdir().expect("data dir");
    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services { cloud_data_dir: Some(data.path().to_path_buf()), ..crate::Services::default() });
    crate::prefs_ui::open_preferences(&mut app, "craftcloud");
    let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_max_steps(64).build_eframe(move |cc| {
        crate::PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::ALL[0]);
        app
    });
    harness.run_steps(5);
    let _ = harness.get_by_label("Add server");
    assert_eq!(harness.query_by_label("Ask where to save").is_some(), !cfg!(windows));
}
