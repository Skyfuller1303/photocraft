#![allow(clippy::expect_used, clippy::panic)] // Ignored integration test reports failed prerequisites clearly.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use craftcloud_client::{CloudClient, CloudError, File};
use craftcloud_client_native::{KeychainTokenStore, UreqTransport};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;
use crate::PhotocraftApp;

#[derive(Deserialize)]
struct FixtureState {
    server: String,
    organization_id: String,
    workspace_id: String,
    project_id: String,
    file_id: String,
    initial_version_id: String,
    fixture: PathBuf,
}

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct Accounts {
    owner: Credentials,
    editor: Credentials,
}

#[derive(Serialize)]
struct E2eEvidence {
    server: String,
    organization_id: String,
    workspace_id: String,
    project_id: String,
    file_id: String,
    fixture_initial_version_id: String,
    before_version_id: String,
    after_version_id: String,
    content_sha256: Option<String>,
    checks: E2eChecks,
}

#[derive(Serialize)]
struct E2eChecks {
    app_runtime_login: bool,
    cloud_psd_opened_as_document: bool,
    local_document_changed: bool,
    normal_save_menu_hook_completed: bool,
    server_head_advanced: bool,
    server_bytes_equal_local_bytes: bool,
    second_session_saw_owner_lock: bool,
    second_session_lock_attempt_was_423: bool,
    closed_document_released_owner_lock: bool,
}

struct LogoutCleanup {
    server: String,
    owner_email: String,
    lock_dir: PathBuf,
    app_client: Option<NativeClient>,
    cli_client: Option<Arc<CloudClient<UreqTransport>>>,
    keychain: KeychainTokenStore,
}

impl LogoutCleanup {
    fn finish(&mut self) {
        if let Some(client) = self.app_client.take() {
            client.logout().expect("revoke the synthetic PhotoCraft device session");
        }
        if let Some(client) = self.cli_client.take() {
            client.logout().expect("revoke the synthetic second device session");
        }
        self.keychain.delete(&self.server, &self.owner_email, "photocraft").expect("remove the synthetic PhotoCraft keychain entry");
        assert!(self.keychain.load(&self.server, &self.owner_email, "photocraft").expect("verify synthetic keychain cleanup").is_none());
    }
}

impl Drop for LogoutCleanup {
    fn drop(&mut self) {
        if let Some(client) = &self.app_client {
            let _ = client.logout();
        }
        if let Some(client) = &self.cli_client {
            let _ = client.logout();
        }
        if let Ok(Some(refresh)) = self.keychain.load(&self.server, &self.owner_email, "photocraft")
            && let Ok(client) = CloudClient::new(self.server.clone(), UreqTransport::default())
        {
            let store = self.keychain.bound(self.server.clone(), self.owner_email.clone(), "photocraft").with_lock_dir(self.lock_dir.clone());
            if client.set_refresh_token_store(Arc::new(store)).is_ok() {
                client.restore_refresh_token(refresh);
                let _ = client.current_user();
                let _ = client.logout();
            }
        }
        let _ = self.keychain.delete(&self.server, &self.owner_email, "photocraft");
    }
}

struct CliLockCleanup {
    client: Arc<CloudClient<UreqTransport>>,
    file_id: Uuid,
    armed: bool,
}

impl CliLockCleanup {
    fn release(mut self) -> Result<(), CloudError> {
        let result = self.client.release_lock(self.file_id);
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for CliLockCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.client.release_lock(self.file_id);
        }
    }
}

#[test]
#[ignore = "requires the local AC-05 CraftCloud server, fixture-state.json, and private/accounts.json"]
fn real_server_photo_runtime_opens_edits_saves_versions_and_releases_lock() {
    let root = std::env::var_os("AC05_E2E_ROOT").map(PathBuf::from).expect("set AC05_E2E_ROOT to the AC-05 output directory");
    let private = root.join("private");
    let fixture_state: FixtureState = read_json(&root.join("fixture-state.json"), "fixture state");
    let accounts: Accounts = read_json(&private.join("accounts.json"), "private account credentials");
    let server_url = url::Url::parse(&fixture_state.server).expect("fixture server URL");
    let loopback = match server_url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    assert!(loopback && server_url.scheme() == "http", "the real-server test accepts only an HTTP loopback fixture server");
    validate_server_url(&fixture_state.server).expect("fixture server must pass the app URL validator");

    let organization_id = Uuid::parse_str(&fixture_state.organization_id).expect("fixture organization id");
    let workspace_id = Uuid::parse_str(&fixture_state.workspace_id).expect("fixture workspace id");
    let project_id = Uuid::parse_str(&fixture_state.project_id).expect("fixture project id");
    let file_id = Uuid::parse_str(&fixture_state.file_id).expect("fixture file id");
    let initial_version_id = Uuid::parse_str(&fixture_state.initial_version_id).expect("fixture initial version id");
    let fixture_path = fixture_state.fixture.canonicalize().expect("synthetic PSD fixture exists");
    let fixtures_dir = root.join("fixtures").canonicalize().expect("fixture directory exists");
    assert!(fixture_path.starts_with(&fixtures_dir), "fixture PSD must remain below the AC-05 fixtures directory");

    let keychain = KeychainTokenStore::new("CraftCloud");
    let app_data = tempfile::Builder::new().prefix("photocraft-ac05-e2e-").tempdir_in(&private).expect("private temporary app-data directory");
    let mut cleanup = LogoutCleanup {
        server: fixture_state.server.clone(),
        owner_email: accounts.owner.email.clone(),
        lock_dir: Paths::under(app_data.path()).keychain_locks,
        app_client: None,
        cli_client: None,
        keychain,
    };
    cleanup.keychain.delete(&fixture_state.server, &accounts.owner.email, "photocraft").expect("clear stale PhotoCraft test keychain entry");

    let mut app = PhotocraftApp::new(
        photocraft_engine::Session::new(),
        crate::Services {
            cloud_data_dir: Some(app_data.path().to_path_buf()),
            import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
                photocraft_io::import_with_options(name, bytes, opts).map(|result| (result.document, result.warnings)).map_err(|error| error.to_string())
            })),
            export: Some(Box::new(|doc, path, settings| {
                photocraft_io::export(doc, path, &settings.io_options()).map(|result| (result.bytes, result.warnings)).map_err(|error| error.to_string())
            })),
            write: Some(Box::new(|path, bytes| photocraft_format::atomic_write(Path::new(path), bytes).map_err(|error| error.to_string()))),
            ..crate::Services::default()
        },
    );
    app.session
        .execute("edit.colorSettings", serde_json::json!({"askOnMismatch": false, "askOnMissing": false}))
        .expect("disable profile prompts for the test fixture");

    let cli_client = Arc::new(CloudClient::new(fixture_state.server.clone(), UreqTransport::default()).expect("construct real SDK CLI client"));
    cli_client
        .login(&accounts.editor.email, &accounts.editor.password, "cli", "craftcloud-e2e-cli", "PhotoCraft E2E second session")
        .expect("real SDK CLI login");
    cleanup.cli_client = Some(Arc::clone(&cli_client));
    let before: File = cli_client.get(&format!("/api/v1/files/{file_id}")).expect("second account can read the fixture file");
    assert_eq!(before.organization_id, organization_id);
    assert_eq!(before.workspace_id, workspace_id);
    assert_eq!(before.project_id, project_id);
    assert_eq!(before.app, "photocraft");
    assert!(before.lock.is_none(), "fixture starts without a stale lock");
    let before_version = before.head_version_id.expect("fixture has a committed version");

    let mut runtime = std::mem::replace(&mut app.cloud, Runtime::new(None));
    runtime.config.trust(fixture_state.server.clone()).expect("trust loopback fixture server");
    runtime.config.save(Some(app_data.path())).expect("persist test server trust in private app data");
    let open_link =
        format!("craftcloud://open?server={}&file={file_id}", url::form_urlencoded::byte_serialize(fixture_state.server.as_bytes()).collect::<String>());
    let request = runtime.prepare_open_link(&open_link).expect("prepare Cloud open link").expect("fixture server is trusted");
    assert_eq!(request.file_id, file_id);
    runtime.handle_action(
        &mut app,
        Action::Login {
            server: fixture_state.server.clone(),
            email: accounts.owner.email.clone(),
            password: accounts.owner.password.clone(),
            second_factor: None,
        },
    );

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        runtime.apply_worker_result(&mut app);
        if let Some(client) = runtime.client.as_ref() {
            cleanup.app_client = Some(Arc::clone(client));
        }
        if !runtime.local_open_requests.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "PhotoCraft Runtime did not finish real Cloud login and download");
        assert!(runtime.picker.error.is_none(), "PhotoCraft Runtime reported a Cloud login or download error");
        thread::sleep(Duration::from_millis(20));
    }

    let paths = runtime.take_local_open_requests();
    assert_eq!(paths.len(), 1, "Cloud download queues one local PSD open");
    assert!(runtime.has_pending_admission(&paths[0]), "download remains pending until the real document is admitted");
    app.cloud = runtime;
    assert_eq!(app.open_paths(&paths), 1, "the native PhotoCraft file-open path admits the downloaded PSD");
    assert!(app.held_opens.is_empty(), "profile prompt is disabled for the synthetic fixture");
    let document = app.session.active().expect("opened PSD document").doc.id;
    let local_path = app.session.active().and_then(|state| state.path.clone()).expect("PhotoCraft records the Cloud working path");
    assert!(app.cloud.documents.contains_key(&document), "Runtime binds the actual opened DocId");
    assert!(app.cloud.leases.contains_key(&document), "PhotoCraft owns the server file lease while open");

    let ctx = egui::Context::default();
    app.run("layer.new.layer", serde_json::json!({})).expect("add a layer to the downloaded PSD");
    app.run("edit.fill", serde_json::json!({"color": "#D13A68"})).expect("change the local PSD document");
    crate::menus::invoke(&mut app, &ctx, "file.save", serde_json::json!({})).expect("normal File > Save completes locally");
    let local_bytes = fs::read(&local_path).expect("read the atomically saved PSD working file");
    assert!(!local_bytes.is_empty(), "PhotoCraft wrote PSD bytes before uploading");

    let after = wait_for_committed_version(&mut app, &cli_client, file_id, before_version, Path::new(&local_path));
    let after_version = after.head_version_id.expect("new version has a head id");
    let server_bytes = cli_client.download(file_id, Some(after_version)).expect("download the new version through the real SDK");
    assert_eq!(server_bytes, local_bytes, "server content must equal the bytes PhotoCraft wrote locally");
    assert_eq!(
        app.cloud.links.as_ref().and_then(|links| links.get(Path::new(&local_path)).ok().flatten()).and_then(|link| link.base_version),
        Some(after_version.to_string()),
        "local link advances to the committed version"
    );

    let locked_file: File = cli_client.get(&format!("/api/v1/files/{file_id}")).expect("editor can inspect the owner's open file");
    let lock = locked_file.lock.expect("server exposes the active owner lock to the second account");
    assert!(!lock.mine && !lock.held_by_current_session, "the editor CLI sees a lock held by the PhotoCraft owner session");
    assert!(
        matches!(cli_client.acquire_lock(file_id, "craftcloud-e2e-cli"), Err(CloudError::FileLocked(_))),
        "the second real SDK session must receive 423 while PhotoCraft holds the lock"
    );

    crate::menus::invoke(&mut app, &ctx, "file.close", serde_json::json!({})).expect("close the clean saved PSD");
    assert!(app.session.documents().is_empty(), "document closed through the app command path");
    let live_documents: HashSet<_> = app.session.documents().iter().map(|state| state.doc.id).collect();
    app.cloud.sync_live_documents(&live_documents);
    assert!(!app.cloud.leases.contains_key(&document), "closed document drops its local heartbeat lease");

    wait_for_lock_release(&cli_client, file_id);
    let probe = cli_client.acquire_lock(file_id, "craftcloud-e2e-cli").expect("second account can acquire the released lock");
    let lock_cleanup = CliLockCleanup { client: Arc::clone(&cli_client), file_id, armed: true };
    assert!(probe.held_by_current_session, "the post-close lock belongs to the CLI session");
    lock_cleanup.release().expect("release the CLI lock probe");
    cleanup.finish();

    let evidence = E2eEvidence {
        server: fixture_state.server,
        organization_id: organization_id.to_string(),
        workspace_id: workspace_id.to_string(),
        project_id: project_id.to_string(),
        file_id: file_id.to_string(),
        fixture_initial_version_id: initial_version_id.to_string(),
        before_version_id: before_version.to_string(),
        after_version_id: after_version.to_string(),
        content_sha256: after.sha256,
        checks: E2eChecks {
            app_runtime_login: true,
            cloud_psd_opened_as_document: true,
            local_document_changed: true,
            normal_save_menu_hook_completed: true,
            server_head_advanced: true,
            server_bytes_equal_local_bytes: true,
            second_session_saw_owner_lock: true,
            second_session_lock_attempt_was_423: true,
            closed_document_released_owner_lock: true,
        },
    };
    fs::write(root.join("photo-real-server-e2e.json"), serde_json::to_vec_pretty(&evidence).expect("serialize credential-free E2E evidence"))
        .expect("write credential-free E2E evidence");
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> T {
    let bytes = fs::read(path).unwrap_or_else(|_| panic!("could not read {label}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("could not parse {label}"))
}

fn pump_runtime(app: &mut crate::PhotocraftApp) {
    let mut runtime = std::mem::replace(&mut app.cloud, Runtime::new(None));
    runtime.apply_worker_result(app);
    app.cloud = runtime;
}

fn wait_for_lock_release(client: &CloudClient<UreqTransport>, file_id: Uuid) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let file: File = client.get(&format!("/api/v1/files/{file_id}")).expect("second account checks lock release");
        if file.lock.is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "closing the PhotoCraft document did not release the server lock");
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_committed_version(app: &mut PhotocraftApp, client: &CloudClient<UreqTransport>, file_id: Uuid, before: Uuid, path: &Path) -> File {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        pump_runtime(app);
        let current: File = client.get(&format!("/api/v1/files/{file_id}")).expect("second account reads the updated file head");
        if let Some(version) = current.head_version_id {
            let version_id = version.to_string();
            let linked_base = app.cloud.links.as_ref().and_then(|links| links.get(path).ok().flatten()).and_then(|link| link.base_version);
            if version != before && linked_base.as_deref() == Some(version_id.as_str()) {
                return current;
            }
        }
        assert!(Instant::now() < deadline, "PhotoCraft's local save hook did not create a server version");
        thread::sleep(Duration::from_millis(30));
    }
}
