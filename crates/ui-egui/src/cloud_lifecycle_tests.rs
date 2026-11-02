use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use craftcloud_client::{CloudError, CloudTransport, Request, Response};
use craftcloud_client_native::{CloudLink, FileLeaseHeartbeat};
use photocraft_doc::DocId;
use serde_json::json;
use uuid::Uuid;

use super::*;

#[derive(Clone)]
struct SeenRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct CommitGate {
    ready: Arc<(Mutex<bool>, Condvar)>,
    entered: Arc<AtomicBool>,
}

impl CommitGate {
    fn new() -> Self {
        Self { ready: Arc::new((Mutex::new(false), Condvar::new())), entered: Arc::new(AtomicBool::new(false)) }
    }

    fn wait_until_entered(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "first Cloud commit did not reach the test gate");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn release(&self) {
        let (lock, changed) = &*self.ready;
        *lock.lock().expect("commit gate lock") = true;
        changed.notify_all();
    }
}

struct RecordingTransport {
    requests: Mutex<Vec<SeenRequest>>,
    file_id: Uuid,
    version_ids: [Uuid; 2],
    commits: AtomicUsize,
    gate: Option<CommitGate>,
}

impl RecordingTransport {
    fn new(file_id: Uuid, gate: Option<CommitGate>) -> Self {
        Self { requests: Mutex::new(Vec::new()), file_id, version_ids: [Uuid::new_v4(), Uuid::new_v4()], commits: AtomicUsize::new(0), gate }
    }

    fn requests(&self) -> Vec<SeenRequest> {
        self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl CloudTransport for RecordingTransport {
    fn send(&self, request: Request) -> Result<Response, CloudError> {
        let path = request.path.split("127.0.0.1:18095").nth(1).unwrap_or(&request.path).to_string();
        self.requests.lock().map_err(|_| CloudError::Offline)?.push(SeenRequest {
            method: request.method.clone(),
            path: path.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
        });

        let body = if path.ends_with("/uploads") && request.method == "POST" {
            let upload_id = Uuid::new_v4();
            json!({"upload_id": upload_id, "file_id": self.file_id, "size_bytes": 0, "sha256": "", "state": "created"}).to_string().into_bytes()
        } else if path.ends_with("/versions") && request.method == "POST" {
            let index = self.commits.fetch_add(1, Ordering::AcqRel);
            if index == 0
                && let Some(gate) = &self.gate
            {
                gate.entered.store(true, Ordering::Release);
                let (lock, changed) = &*gate.ready;
                let mut ready = lock.lock().map_err(|_| CloudError::Offline)?;
                while !*ready {
                    ready = changed.wait(ready).map_err(|_| CloudError::Offline)?;
                }
            }
            let version_id = self.version_ids.get(index).copied().unwrap_or_else(Uuid::new_v4);
            json!({"id": version_id, "file_id": self.file_id, "number": index + 2, "size_bytes": 0, "sha256": format!("hash-{index}")}).to_string().into_bytes()
        } else {
            Vec::new()
        };
        Ok(Response { status: 200, headers: Vec::new(), body })
    }
}

fn local_link(path: &Path, server: &str, file_id: Uuid, base: Uuid) -> CloudLink {
    CloudLink {
        local_path: path.to_path_buf(),
        server: server.into(),
        file_id: file_id.to_string(),
        base_version: Some(base.to_string()),
        sha256: String::new(),
    }
}

fn wait_until(deadline: Instant, label: &str, mut ready: impl FnMut() -> bool) {
    while !ready() {
        assert!(Instant::now() < deadline, "timed out: {label}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_client_signed_into_server_b_never_uploads_a_server_a_link() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = root.path().join("server-a.pcraft");
    let doc = DocId(701);
    let server_a = "http://127.0.0.1:18096";
    let server_b = "http://127.0.0.1:18095";
    let transport = Arc::new(RecordingTransport::new(Uuid::new_v4(), None));
    let mut runtime = Runtime::with_test_transport(Some(root.path()), server_b, transport.clone()).expect("client B");
    runtime.bind_test_document(doc, local_link(&path, server_a, Uuid::new_v4(), Uuid::new_v4())).expect("link to server A");

    let result = runtime.on_local_save(doc, path.to_str().expect("local path"), b"local bytes".to_vec());

    assert!(result.is_err(), "server mismatch must be visible");
    assert!(transport.requests().is_empty(), "client B receives no request or document bytes");
    assert_eq!(
        runtime.pending_uploads.get(&path).map(|pending| pending.bytes.as_slice()),
        Some(b"local bytes".as_slice()),
        "local snapshot remains available for the correct server"
    );
}

#[test]
fn second_local_snapshot_waits_for_first_upload_then_uses_refreshed_base() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = root.path().join("linked.pcraft");
    let path_text = path.to_str().expect("local path").to_string();
    let doc = DocId(702);
    let file_id = Uuid::new_v4();
    let base = Uuid::new_v4();
    let server = "http://127.0.0.1:18095";
    let gate = CommitGate::new();
    let transport = Arc::new(RecordingTransport::new(file_id, Some(gate.clone())));
    let mut runtime = Runtime::with_test_transport(Some(root.path()), server, transport.clone()).expect("test Cloud client");
    runtime.bind_test_document(doc, local_link(&path, server, file_id, base)).expect("linked file");

    assert!(runtime.on_local_save(doc, &path_text, b"snapshot A".to_vec()).expect("queue first snapshot"));
    gate.wait_until_entered();
    assert!(runtime.on_local_save(doc, &path_text, b"snapshot B".to_vec()).expect("queue second snapshot while busy"));
    let while_blocked = transport.requests();
    assert_eq!(
        while_blocked.iter().filter(|request| request.method == "PUT" && request.path.ends_with("/content")).count(),
        1,
        "second snapshot waits behind first commit"
    );
    gate.release();

    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let links = runtime.links.as_ref().expect("links store").clone();
    let final_version = transport.version_ids[1].to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    wait_until(deadline, "second upload and refreshed base", || {
        runtime.apply_worker_result(&mut app);
        let commits = transport.requests().iter().filter(|request| request.method == "POST" && request.path.ends_with("/versions")).count();
        let base_advanced = links.get(&path).ok().flatten().and_then(|link| link.base_version).as_deref() == Some(final_version.as_str());
        commits == 2 && base_advanced
    });
    wait_until(deadline, "second upload completion", || {
        runtime.apply_worker_result(&mut app);
        !runtime.request_busy
    });

    let requests = transport.requests();
    let content: Vec<_> = requests.iter().filter(|request| request.method == "PUT" && request.path.ends_with("/content")).collect();
    let commits: Vec<_> = requests.iter().filter(|request| request.method == "POST" && request.path.ends_with("/versions")).collect();
    assert_eq!(content.len(), 2, "both local snapshots upload");
    assert_eq!(content[0].body, b"snapshot A");
    assert_eq!(content[1].body, b"snapshot B");
    assert_eq!(commits.len(), 2, "two version commits complete");
    assert!(
        commits[0].headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("If-Match") && value == &format!("\"version:{base}\"")),
        "first upload uses original base"
    );
    assert!(
        commits[1].headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("If-Match") && value == &format!("\"version:{}\"", transport.version_ids[0])),
        "queued upload uses first commit's refreshed base"
    );
}

#[test]
fn syncing_documents_preserves_live_lease_and_releases_only_closed_document() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let mut runtime = Runtime::new(Some(root.path()));
    let live = DocId(703);
    let closed = DocId(704);
    let live_release = Arc::new(AtomicUsize::new(0));
    let closed_release = Arc::new(AtomicUsize::new(0));
    let live_counter = Arc::clone(&live_release);
    let closed_counter = Arc::clone(&closed_release);
    let live_key = format!("lifecycle-live-{}", Uuid::new_v4());
    let closed_key = format!("lifecycle-closed-{}", Uuid::new_v4());
    runtime.documents.insert(live, PathBuf::from("live.pcraft"));
    runtime.documents.insert(closed, PathBuf::from("closed.pcraft"));
    runtime.leases.insert(
        live,
        FileLeaseHeartbeat::start_keyed(
            live_key.clone(),
            || {},
            move || {
                live_counter.fetch_add(1, Ordering::AcqRel);
            },
        ),
    );
    runtime.leases.insert(
        closed,
        FileLeaseHeartbeat::start_keyed(
            closed_key.clone(),
            || {},
            move || {
                closed_counter.fetch_add(1, Ordering::AcqRel);
            },
        ),
    );

    runtime.sync_live_documents(&HashSet::from([live]));

    assert!(runtime.documents.contains_key(&live) && runtime.leases.contains_key(&live), "live document keeps its lease");
    assert!(!runtime.documents.contains_key(&closed) && !runtime.leases.contains_key(&closed), "closed document and lease are removed");
    wait_until(Instant::now() + Duration::from_secs(2), "closed document lease release", || closed_release.load(Ordering::Acquire) == 1);
    assert_eq!(live_release.load(Ordering::Acquire), 0, "live lease was not released");

    drop(runtime);
    assert!(craftcloud_client_native::wait_for_release(&live_key, Duration::from_secs(2)), "drop eventually releases live lease");
    assert_eq!(live_release.load(Ordering::Acquire), 1, "live lease releases once when its document is dropped");
    assert_eq!(closed_release.load(Ordering::Acquire), 1, "closed lease released exactly once");
    assert!(craftcloud_client_native::wait_for_release(&closed_key, Duration::from_millis(5)), "closed lease has no pending release");
}

struct UnreachableTransport;

impl CloudTransport for UnreachableTransport {
    fn send(&self, _request: Request) -> Result<Response, CloudError> {
        Err(CloudError::Unreachable)
    }
}

/// A working copy under the Cloud working directory, as a download or first Cloud save creates it.
fn working_copy(root: &Path, name: &str) -> PathBuf {
    let dir = root.join("CraftCloud").join("working").join("new");
    std::fs::create_dir_all(&dir).expect("create working directory");
    let path = dir.join(name);
    std::fs::write(&path, b"working copy").expect("write working copy");
    path
}

fn pump_until_idle(runtime: &mut Runtime, app: &mut crate::PhotocraftApp, live: &HashSet<DocId>) {
    wait_until(Instant::now() + Duration::from_secs(10), "upload completion", || {
        runtime.apply_worker_result(app);
        runtime.sync_live_documents(live);
        !runtime.request_busy && runtime.queued_uploads.is_empty()
    });
}

#[test]
fn closing_a_document_during_an_upload_deletes_its_working_copy_only_after_success() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "linked.pcraft");
    let path_text = path.to_str().expect("local path").to_string();
    let doc = DocId(710);
    let file_id = Uuid::new_v4();
    let server = "http://127.0.0.1:18095";
    let gate = CommitGate::new();
    let transport = Arc::new(RecordingTransport::new(file_id, Some(gate.clone())));
    let mut runtime = Runtime::with_test_transport(Some(root.path()), server, transport.clone()).expect("test Cloud client");
    runtime.bind_test_document(doc, local_link(&path, server, file_id, Uuid::new_v4())).expect("linked file");
    let links = runtime.links.as_ref().expect("links store").clone();

    assert!(runtime.on_local_save(doc, &path_text, b"snapshot".to_vec()).expect("queue snapshot"));
    gate.wait_until_entered();
    runtime.sync_live_documents(&HashSet::new());
    assert!(path.exists(), "working copy stays while the upload is in flight");
    assert!(links.get(&path).expect("read link").is_some(), "link stays while the upload is in flight");

    gate.release();
    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    pump_until_idle(&mut runtime, &mut app, &HashSet::new());
    runtime.sync_live_documents(&HashSet::new());
    assert!(!path.exists(), "working copy is deleted after the upload succeeded");
    assert!(links.get(&path).expect("read link").is_none(), "link is removed with the working copy");
}

#[test]
fn closing_a_synced_document_deletes_its_working_copy_and_link() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "synced.pcraft");
    let doc = DocId(711);
    let server = "http://127.0.0.1:18095";
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.bind_test_document(doc, local_link(&path, server, Uuid::new_v4(), Uuid::new_v4())).expect("linked file");
    let links = runtime.links.as_ref().expect("links store").clone();

    runtime.sync_live_documents(&HashSet::from([doc]));
    assert!(path.exists(), "an open document keeps its working copy");
    runtime.sync_live_documents(&HashSet::new());
    assert!(!path.exists() && links.get(&path).expect("read link").is_none(), "closed document leaves no copy or link");
}

#[test]
fn closing_a_read_only_document_deletes_its_working_copy() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "read-only.pcraft");
    let doc = DocId(712);
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.bind_test_document(doc, local_link(&path, "http://127.0.0.1:18095", Uuid::new_v4(), Uuid::new_v4())).expect("linked file");
    runtime.read_only_documents.insert(doc);

    runtime.sync_live_documents(&HashSet::new());
    assert!(!path.exists(), "read-only working copy is deleted on close");
}

#[test]
fn a_failed_upload_keeps_the_working_copy_and_link_after_close() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "offline.pcraft");
    let path_text = path.to_str().expect("local path").to_string();
    let doc = DocId(713);
    let server = "http://127.0.0.1:18095";
    let mut runtime = Runtime::with_test_transport(Some(root.path()), server, Arc::new(UnreachableTransport)).expect("test Cloud client");
    runtime.bind_test_document(doc, local_link(&path, server, Uuid::new_v4(), Uuid::new_v4())).expect("linked file");
    let links = runtime.links.as_ref().expect("links store").clone();

    assert!(runtime.on_local_save(doc, &path_text, b"snapshot".to_vec()).expect("queue snapshot"));
    let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    wait_until(Instant::now() + Duration::from_secs(10), "failed upload", || {
        runtime.apply_worker_result(&mut app);
        runtime.sync_live_documents(&HashSet::new());
        !runtime.request_busy
    });
    runtime.sync_live_documents(&HashSet::new());
    assert!(runtime.pending_uploads.contains_key(&path), "failed upload stays pending for retry");
    assert!(path.exists(), "working copy survives a failed upload");
    assert!(links.get(&path).expect("read link").is_some(), "link survives a failed upload");
}

#[test]
fn a_local_save_that_was_never_queued_keeps_the_working_copy() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "signed-out.pcraft");
    let path_text = path.to_str().expect("local path").to_string();
    let doc = DocId(714);
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.bind_test_document(doc, local_link(&path, "http://127.0.0.1:18095", Uuid::new_v4(), Uuid::new_v4())).expect("linked file");

    assert!(runtime.on_local_save(doc, &path_text, b"snapshot".to_vec()).is_err(), "signed-out save cannot upload");
    runtime.sync_live_documents(&HashSet::new());
    assert!(path.exists(), "the only copy of an unsynced save is kept");
}

#[test]
fn a_bound_save_without_a_link_keeps_the_working_copy() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "unlinked.pcraft");
    let path_text = path.to_str().expect("local path").to_string();
    let doc = DocId(718);
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.bind_test_document(doc, local_link(&path, "http://127.0.0.1:18095", Uuid::new_v4(), Uuid::new_v4())).expect("linked file");
    runtime.links.as_ref().expect("links store").remove(&path).expect("remove link");

    assert_eq!(runtime.on_local_save(doc, &path_text, b"snapshot".to_vec()), Ok(false), "nothing to upload to");
    runtime.sync_live_documents(&HashSet::new());
    assert!(path.exists(), "a save that was not uploaded is kept");
}

#[test]
fn a_running_save_job_defers_deleting_the_working_copy() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let path = working_copy(root.path(), "saving.pcraft");
    let doc = DocId(715);
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.bind_test_document(doc, local_link(&path, "http://127.0.0.1:18095", Uuid::new_v4(), Uuid::new_v4())).expect("linked file");
    runtime.set_saving_paths(HashSet::from([path.clone()]));

    runtime.sync_live_documents(&HashSet::new());
    assert!(path.exists(), "a file a save job is writing is not deleted");
}

#[test]
fn files_outside_the_working_directory_are_never_deleted() {
    let root = tempfile::tempdir().expect("temporary Cloud data directory");
    let own = root.path().join("my-photo.pcraft");
    std::fs::write(&own, b"user file").expect("write user file");
    std::fs::create_dir_all(root.path().join("CraftCloud").join("working")).expect("create working directory");
    let escape = root.path().join("CraftCloud").join("working").join("..").join("..").join("my-photo.pcraft");
    let mut runtime = Runtime::new(Some(root.path()));
    runtime.bind_test_document(DocId(716), local_link(&own, "http://127.0.0.1:18095", Uuid::new_v4(), Uuid::new_v4())).expect("linked file");
    runtime.bind_test_document(DocId(717), local_link(&escape, "http://127.0.0.1:18095", Uuid::new_v4(), Uuid::new_v4())).expect("linked file");

    runtime.sync_live_documents(&HashSet::new());
    assert!(own.exists(), "a user's own file survives closing, including via a path that escapes the working directory");
}
