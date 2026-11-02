#![allow(clippy::expect_used)] // Test setup and assertions fail with context when prerequisites are missing.

use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use craftcloud_client::{CloudError, CloudTransport, Request, Response};
use serde_json::json;
use uuid::Uuid;

use crate::{PhotocraftApp, Services};

#[derive(Clone)]
struct SeenRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct RecordingTransport {
    requests: Mutex<Vec<SeenRequest>>,
    file_id: Mutex<Option<Uuid>>,
    create_file_id: Uuid,
    upload_id: Uuid,
    version_id: Uuid,
    fail_upload: bool,
    completed_calls: AtomicUsize,
}

impl CloudTransport for RecordingTransport {
    fn send(&self, request: Request) -> Result<Response, CloudError> {
        let path = request.path.split("127.0.0.1:18095").nth(1).unwrap_or(&request.path).to_string();
        let mut requests = self.requests.lock().map_err(|_| CloudError::Offline)?;
        requests.push(SeenRequest { method: request.method.clone(), path: path.clone(), headers: request.headers.clone(), body: request.body.clone() });
        drop(requests);

        if self.fail_upload {
            self.completed_calls.fetch_add(1, Ordering::Release);
            return Err(CloudError::Unreachable);
        }
        let body = if path.starts_with("/api/v1/projects/") && path.ends_with("/files") && request.method == "POST" {
            let project_id = path.split('/').nth(4).and_then(|value| Uuid::parse_str(value).ok()).ok_or(CloudError::InvalidResponse)?;
            let create: serde_json::Value = serde_json::from_slice(&request.body).map_err(|_| CloudError::InvalidResponse)?;
            let name = create.get("name").and_then(serde_json::Value::as_str).ok_or(CloudError::InvalidResponse)?;
            let app = create.get("app").and_then(serde_json::Value::as_str).ok_or(CloudError::InvalidResponse)?;
            let format = create.get("format").and_then(serde_json::Value::as_str).ok_or(CloudError::InvalidResponse)?;
            *self.file_id.lock().map_err(|_| CloudError::Offline)? = Some(self.create_file_id);
            json!({
                "id": self.create_file_id,
                "organization_id": Uuid::nil(),
                "workspace_id": Uuid::nil(),
                "project_id": project_id,
                "name": name,
                "app": app,
                "format": format,
                "size_bytes": 0,
                "sha256": null,
                "head_version_id": null,
                "version": 0,
                "created_at": "2026-10-30T00:00:00Z",
                "can_share": false,
            })
            .to_string()
            .into_bytes()
        } else if path.ends_with("/uploads") && request.method == "POST" {
            let file_id = path.split('/').nth(4).and_then(|value| Uuid::parse_str(value).ok()).ok_or(CloudError::InvalidResponse)?;
            *self.file_id.lock().map_err(|_| CloudError::Offline)? = Some(file_id);
            json!({"upload_id": self.upload_id, "file_id": file_id, "size_bytes": 0, "sha256": "", "state": "created"}).to_string().into_bytes()
        } else if path.ends_with("/versions") && request.method == "POST" {
            let file_id = *self.file_id.lock().map_err(|_| CloudError::Offline)?.as_ref().ok_or(CloudError::InvalidResponse)?;
            json!({"id": self.version_id, "file_id": file_id, "number": 2, "size_bytes": 0, "sha256": ""}).to_string().into_bytes()
        } else {
            Vec::new()
        };
        self.completed_calls.fetch_add(1, Ordering::Release);
        Ok(Response { status: 200, headers: Vec::new(), body })
    }
}

fn wait_for_link_version(app: &mut PhotocraftApp, ctx: &egui::Context, links: &craftcloud_client_native::LinksStore, path: &Path, version: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        crate::jobs_ui::tick(app, ctx);
        if links.get(path).expect("read uploaded link").is_some_and(|link| link.base_version.as_deref() == Some(version)) {
            return;
        }
        assert!(Instant::now() < deadline, "Cloud version upload timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_new_file_upload(transport: &RecordingTransport, file_id: Uuid, format: &str, bytes: &[u8]) {
    let requests = transport.requests.lock().expect("transport request lock");
    let creates: Vec<_> =
        requests.iter().filter(|request| request.method == "POST" && request.path.contains("/projects/") && request.path.ends_with("/files")).collect();
    let uploads: Vec<_> = requests.iter().filter(|request| request.method == "POST" && request.path.ends_with("/uploads")).collect();
    let contents: Vec<_> = requests.iter().filter(|request| request.method == "PUT" && request.path.ends_with("/content")).collect();
    let commits: Vec<_> = requests.iter().filter(|request| request.method == "POST" && request.path.ends_with("/versions")).collect();
    assert_eq!(creates.len(), 1, "one CraftCloud file created");
    assert_eq!(uploads.len(), 1, "one version upload session created");
    assert_eq!(contents.len(), 1, "one version content upload");
    assert_eq!(commits.len(), 1, "one first version commit");
    assert_eq!(contents[0].body, bytes, "Cloud receives exact locally exported bytes");
    let create: serde_json::Value = serde_json::from_slice(&creates[0].body).expect("parse new-file request");
    assert_eq!(create.get("format").and_then(serde_json::Value::as_str), Some(format), "requested Cloud format");
    assert_eq!(commits[0].path, format!("/api/v1/files/{file_id}/versions"), "first version targets created file");
    assert!(commits[0].headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("If-None-Match") && value == "*"), "first version has no base");
}

fn save_uploads_the_written_snapshot_once_against_its_linked_base(background_jobs: bool, fail_upload: bool) {
    let root = tempfile::tempdir().expect("temporary cloud data directory");
    let local = root.path().join("working.pcraft");
    let expected = if background_jobs { b"exact bytes produced by the save worker".to_vec() } else { b"exact bytes produced by the inline exporter".to_vec() };
    let written = Arc::new(Mutex::new(None::<Vec<u8>>));
    let save_capture = Arc::clone(&written);
    let save_bytes = expected.clone();
    let save_file: crate::SaveFileFn = Arc::new(move |_, path, _, ctl| {
        ctl.commit()?;
        std::fs::write(path, &save_bytes).map_err(|error| error.to_string())?;
        *save_capture.lock().map_err(|_| "save capture lock poisoned".to_string())? = Some(save_bytes.clone());
        ctl.saved_bytes(&save_bytes);
        Ok(Vec::new())
    });
    let export_bytes = expected.clone();
    let write_capture = Arc::clone(&written);
    let file_write: crate::WriteFn = Box::new(move |path, bytes| {
        std::fs::write(path, bytes).map_err(|error| error.to_string())?;
        *write_capture.lock().map_err(|_| "save capture lock poisoned".to_string())? = Some(bytes.to_vec());
        Ok(())
    });
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = session.active().expect("active document").doc.id;
    session.active_mut().expect("active document").path = Some(local.to_string_lossy().into_owned());
    let mut app = PhotocraftApp::new(
        session,
        Services {
            cloud_data_dir: Some(root.path().to_path_buf()),
            export: Some(Box::new(move |_, _, _| Ok((export_bytes.clone(), Vec::new())))),
            write: Some(file_write),
            save_file: Some(save_file),
            ..Services::default()
        },
    );
    app.background_jobs = background_jobs;

    let file_id = Uuid::new_v4();
    let base_version = Uuid::new_v4();
    let transport = Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        file_id: Mutex::new(None),
        create_file_id: Uuid::new_v4(),
        upload_id: Uuid::new_v4(),
        version_id: Uuid::new_v4(),
        fail_upload,
        completed_calls: AtomicUsize::new(0),
    });
    let uploaded_version = transport.version_id.to_string();
    let server = "http://127.0.0.1:18095";
    let links = craftcloud_client_native::LinksStore::new(root.path().join("CraftCloud"));
    app.cloud = crate::cloud::Runtime::with_test_transport(Some(root.path()), server, transport.clone()).expect("test Cloud client");
    app.cloud
        .bind_test_document(
            doc,
            craftcloud_client_native::CloudLink {
                local_path: local.clone(),
                server: server.into(),
                file_id: file_id.to_string(),
                base_version: Some(base_version.to_string()),
                sha256: String::new(),
            },
        )
        .expect("bind Cloud document");

    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "file.save", json!({})).expect("start normal Save command");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        crate::jobs_ui::tick(&mut app, &ctx);
        let requests = transport.requests.lock().expect("transport request lock");
        let committed = requests.iter().any(|request| request.path.ends_with("/versions"));
        let failed = fail_upload && transport.completed_calls.load(Ordering::Acquire) == 1;
        drop(requests);
        if failed {
            break;
        }
        if committed && links.get(&local).expect("read link").is_some_and(|link| link.base_version.as_deref() == Some(uploaded_version.as_str())) {
            break;
        }
        assert!(Instant::now() < deadline, "Cloud version commit timed out");
        std::thread::sleep(Duration::from_millis(5));
    }

    let saved = written.lock().expect("written bytes lock").clone().expect("local write completed");
    assert_eq!(saved, expected, "save worker wrote the expected snapshot");
    assert_eq!(std::fs::read(&local).expect("read local save"), expected, "local file contains saved bytes");
    let requests = transport.requests.lock().expect("transport request lock");
    let uploads: Vec<_> = requests.iter().filter(|request| request.method == "POST" && request.path.ends_with("/uploads")).collect();
    let contents: Vec<_> = requests.iter().filter(|request| request.method == "PUT" && request.path.ends_with("/content")).collect();
    let commits: Vec<_> = requests.iter().filter(|request| request.method == "POST" && request.path.ends_with("/versions")).collect();
    assert_eq!(uploads.len(), 1, "one upload session per normal save");
    if fail_upload {
        assert!(contents.is_empty() && commits.is_empty(), "failed Cloud request stops before content and version commit");
        assert_eq!(links.get(&local).expect("read link").and_then(|link| link.base_version), Some(base_version.to_string()), "failed upload keeps linked base");
    } else {
        assert_eq!(contents.len(), 1, "one content upload per normal save");
        assert_eq!(commits.len(), 1, "one version commit per normal save");
        assert_eq!(contents[0].body, expected, "Cloud receives exact bytes written locally");
        assert_eq!(commits[0].path, format!("/api/v1/files/{file_id}/versions"), "version commit targets linked file");
        assert!(
            commits[0].headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("If-Match") && value == &format!("\"version:{base_version}\"")),
            "version commit uses linked base"
        );
    }
    let saved_doc = app.session.documents().iter().find(|state| state.doc.id == doc).expect("saved document remains open");
    assert_eq!(saved_doc.revision, saved_doc.saved_revision, "Cloud failure does not undo the local saved state");
    assert!(!app.ui.status_error && app.ui.status.starts_with("Saved "), "Cloud failure preserves non-error local save status");
}

#[test]
fn synchronous_and_background_save_entrypoints_upload_once() {
    save_uploads_the_written_snapshot_once_against_its_linked_base(false, false);
    save_uploads_the_written_snapshot_once_against_its_linked_base(true, false);
}

#[test]
fn cloud_failure_keeps_local_save_and_cloud_base_unchanged() {
    save_uploads_the_written_snapshot_once_against_its_linked_base(true, true);
}

#[test]
fn save_as_persists_a_new_pcraft_cloud_file_from_the_local_snapshot() {
    let root = tempfile::tempdir().expect("temporary cloud data directory");
    let local = root.path().join("first-save.pcraft");
    let expected = b"native Cloud first-save bytes".to_vec();
    let save_bytes = expected.clone();
    let save_file: crate::SaveFileFn = Arc::new(move |_, path, _, ctl| {
        ctl.commit()?;
        std::fs::write(path, &save_bytes).map_err(|error| error.to_string())?;
        ctl.saved_bytes(&save_bytes);
        Ok(Vec::new())
    });
    let export_bytes = expected.clone();
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = session.active().expect("active document").doc.id;
    let mut app = PhotocraftApp::new(
        session,
        Services {
            cloud_data_dir: Some(root.path().to_path_buf()),
            export: Some(Box::new(move |_, _, _| Ok((export_bytes.clone(), Vec::new())))),
            write: Some(Box::new(|path, bytes| std::fs::write(path, bytes).map_err(|error| error.to_string()))),
            save_file: Some(save_file),
            ..Services::default()
        },
    );
    let file_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let version_id = Uuid::new_v4();
    let transport = Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        file_id: Mutex::new(None),
        create_file_id: file_id,
        upload_id: Uuid::new_v4(),
        version_id,
        fail_upload: false,
        completed_calls: AtomicUsize::new(0),
    });
    let server = "http://127.0.0.1:18095";
    app.cloud = crate::cloud::Runtime::with_test_transport(Some(root.path()), server, transport.clone()).expect("test Cloud client");
    app.cloud.queue_test_new_file(doc, project_id, "First Save.pcraft".into(), "pcraft".into(), server.into(), true);
    app.background_jobs = true;

    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "file.saveAs", json!({"path": local.to_string_lossy().into_owned()})).expect("save first Cloud file locally");
    let links = craftcloud_client_native::LinksStore::new(root.path().join("CraftCloud"));
    wait_for_link_version(&mut app, &ctx, &links, &local, &version_id.to_string());

    assert_eq!(std::fs::read(&local).expect("read local first save"), expected);
    assert_new_file_upload(&transport, file_id, "pcraft", &expected);
    let saved_doc = app.session.documents().iter().find(|state| state.doc.id == doc).expect("document remains open");
    assert_eq!(saved_doc.path.as_deref(), local.to_str());
    assert_eq!(saved_doc.revision, saved_doc.saved_revision, "local Save As remains clean");
    assert!(app.cloud.wants_local_save(doc, local.to_str().expect("Cloud working path")), "first Cloud save binds this document path");
}

#[test]
fn explicit_psd_cloud_copy_uploads_export_bytes_without_retargeting_origin() {
    let root = tempfile::tempdir().expect("temporary cloud data directory");
    let origin = root.path().join("original.pcraft");
    let target = root.path().join("first-export.psd");
    let expected = b"PSD export bytes".to_vec();
    let export_bytes = expected.clone();
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = session.active().expect("active document").doc.id;
    session.active_mut().expect("active document").path = Some(origin.to_string_lossy().into_owned());
    let initial = session.active().expect("active document");
    let initial_revision = initial.revision;
    let initial_saved_revision = initial.saved_revision;
    let mut app = PhotocraftApp::new(
        session,
        Services {
            cloud_data_dir: Some(root.path().to_path_buf()),
            export: Some(Box::new(move |_, _, _| Ok((export_bytes.clone(), Vec::new())))),
            write: Some(Box::new(|path, bytes| std::fs::write(path, bytes).map_err(|error| error.to_string()))),
            ..Services::default()
        },
    );
    let file_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let version_id = Uuid::new_v4();
    let transport = Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        file_id: Mutex::new(None),
        create_file_id: file_id,
        upload_id: Uuid::new_v4(),
        version_id,
        fail_upload: false,
        completed_calls: AtomicUsize::new(0),
    });
    let server = "http://127.0.0.1:18095";
    app.cloud = crate::cloud::Runtime::with_test_transport(Some(root.path()), server, transport.clone()).expect("test Cloud client");
    app.cloud.queue_test_new_file(doc, project_id, "First PSD.psd".into(), "psd".into(), server.into(), false);

    app.replay_save("file.saveACopy", &json!({"path": target.to_string_lossy().into_owned()})).expect("export PSD copy");
    let links = craftcloud_client_native::LinksStore::new(root.path().join("CraftCloud"));
    let ctx = egui::Context::default();
    wait_for_link_version(&mut app, &ctx, &links, &target, &version_id.to_string());

    assert_eq!(std::fs::read(&target).expect("read local PSD copy"), expected);
    assert_new_file_upload(&transport, file_id, "psd", &expected);
    let saved_doc = app.session.documents().iter().find(|state| state.doc.id == doc).expect("origin remains open");
    assert_eq!(saved_doc.path.as_deref(), origin.to_str(), "PSD copy does not retarget the original");
    assert_eq!(saved_doc.revision, initial_revision, "PSD copy does not change document revision");
    assert_eq!(saved_doc.saved_revision, initial_saved_revision, "PSD copy leaves original saved state intact");
    assert!(!app.cloud.wants_local_save(doc, target.to_str().expect("PSD path")), "PSD Cloud copy does not bind document to the copy");
}

#[test]
fn ordinary_save_as_detaches_existing_cloud_identity() {
    let root = tempfile::tempdir().expect("temporary cloud data directory");
    let origin = root.path().join("linked.pcraft");
    let destination = root.path().join("separate.pcraft");
    let expected = b"Save As output".to_vec();
    let export_bytes = expected.clone();
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": 8, "height": 8})).expect("create document");
    let doc = session.active().expect("active document").doc.id;
    session.active_mut().expect("active document").path = Some(origin.to_string_lossy().into_owned());
    let mut app = PhotocraftApp::new(
        session,
        Services {
            cloud_data_dir: Some(root.path().to_path_buf()),
            export: Some(Box::new(move |_, _, _| Ok((export_bytes.clone(), Vec::new())))),
            write: Some(Box::new(|path, bytes| std::fs::write(path, bytes).map_err(|error| error.to_string()))),
            ..Services::default()
        },
    );
    let server = "http://127.0.0.1:18095";
    let transport = Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        file_id: Mutex::new(None),
        create_file_id: Uuid::new_v4(),
        upload_id: Uuid::new_v4(),
        version_id: Uuid::new_v4(),
        fail_upload: false,
        completed_calls: AtomicUsize::new(0),
    });
    app.cloud = crate::cloud::Runtime::with_test_transport(Some(root.path()), server, transport.clone()).expect("test Cloud client");
    app.cloud
        .bind_test_document(
            doc,
            craftcloud_client_native::CloudLink {
                local_path: origin.clone(),
                server: server.into(),
                file_id: Uuid::new_v4().to_string(),
                base_version: Some(Uuid::new_v4().to_string()),
                sha256: String::new(),
            },
        )
        .expect("bind original Cloud path");
    assert!(app.cloud.wants_local_save(doc, origin.to_str().expect("origin path")));

    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "file.saveAs", json!({"path": destination.to_string_lossy().into_owned()})).expect("Save As to another path");

    assert_eq!(std::fs::read(&destination).expect("read Save As output"), expected);
    assert!(!app.cloud.wants_local_save(doc, origin.to_str().expect("origin path")), "old identity detached");
    assert!(!app.cloud.wants_local_save(doc, destination.to_str().expect("destination path")), "new local path is unlinked");
    assert!(transport.requests.lock().expect("transport requests").is_empty(), "ordinary Save As does not upload");
}
