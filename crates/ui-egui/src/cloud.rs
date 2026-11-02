//! Native CraftCloud state and local-file identities.
//!
//! Network work is kept behind explicit calls to the native SDK. The persisted config contains
//! server metadata only; the SDK owns refresh tokens in the OS keychain.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool, mpsc},
};

use craftcloud_client::{CloudClient, CloudError, CloudTransport, Proof, UploadContent};
use craftcloud_client_native::{CloudLink, FileLeaseHeartbeat, KeychainTokenStore, LinksStore, SaveOutcome, UreqTransport, VersionCache};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use craftcloud_egui::{Action, Dialog, Level, PickerState, Text, Translator, WizardText};

#[path = "cloud_permissions.rs"]
pub mod permissions;

#[path = "cloud_save_chooser.rs"]
pub(crate) mod save_chooser;

struct CloudTranslator;

impl Translator for CloudTranslator {
    fn text(&self, key: Text) -> &str {
        match key {
            Text::Server => tl!("Server"),
            Text::Email => tl!("Email"),
            Text::Password => tl!("Password"),
            Text::SecondFactor => tl!("Second factor"),
            Text::SecondFactorHint => tl!("Six-digit authenticator code or ten-character recovery code"),
            Text::SignIn => tl!("Sign in"),
            Text::Account => tl!("Account"),
            Text::Organization => tl!("Organization"),
            Text::SignOut => tl!("Sign out"),
            Text::Browse => tl!("Browse"),
            Text::SharedWithMe => tl!("Shared with me"),
            Text::Workspace => tl!("Workspace"),
            Text::Project => tl!("Project"),
            Text::Files => tl!("Files"),
            Text::More => tl!("Load more"),
            Text::Save => tl!("Save"),
            Text::FileName => tl!("File name"),
            Text::NewFile => tl!("Save"),
            Text::NewVersion => tl!("New version"),
            Text::KeepBoth => tl!("Keep both"),
            Text::Overwrite => tl!("Overwrite"),
            Text::ConfirmOverwrite => tl!("Confirm overwrite"),
            Text::Cancel => tl!("Cancel"),
            Text::ReadOnly => tl!("Open read-only"),
            Text::LockedBy => tl!("Being edited by"),
            Text::Progress => tl!("Progress"),
            Text::Retry => tl!("Retry upload"),
            Text::Offline => tl!("Offline. Saved locally; Cloud upload pending."),
            Text::Connect => tl!("Trust this server and continue"),
            Text::Close => tl!("Close"),
        }
    }

    fn wizard_text(&self, key: WizardText) -> &str {
        match key {
            WizardText::CloudRoot => tl!("CraftCloud"),
            WizardText::Back => tl!("Back"),
            WizardText::Loading => tl!("Loading…"),
            WizardText::ChooseOrganization => tl!("Choose an organization"),
            WizardText::ChooseWorkspace => tl!("Choose a workspace"),
            WizardText::ChooseProject => tl!("Choose a project"),
            WizardText::NoOrganizations => tl!("No organizations available."),
            WizardText::NoWorkspaces => tl!("This organization has no workspaces."),
            WizardText::NoFiles => tl!("This project has no files yet."),
            WizardText::NoProjects => tl!("This workspace has no projects yet. Create one to save into it."),
            WizardText::NewProject => tl!("New project"),
            WizardText::ProjectName => tl!("Project name"),
            WizardText::CreateProject => tl!("Create project"),
            WizardText::CreateProjectForbidden => tl!("You are not allowed to create projects here."),
            WizardText::ProjectNameTaken => tl!("A project with this name already exists."),
            WizardText::SaveTo => tl!("Save to"),
            WizardText::ChooseProjectFirst => tl!("Choose a project to save into first."),
            WizardText::WillConnectTo => tl!("Will connect to"),
            WizardText::AssumedHttp => tl!("No scheme entered: http:// is used for this local address."),
            WizardText::AssumedHttps => tl!("No scheme entered: https:// is used."),
        }
    }
}

const CONFIG_FILE: &str = "app.json";

/// The formats the CraftCloud save step offers: `(id, label)`. The first is the default.
const SAVE_FORMATS: [(&str, &str); 2] = [("pcraft", "PhotoCraft (.pcraft)"), ("psd", "Photoshop (.psd)")];

/// Trusted server metadata. Credentials never belong in this file.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrustedServer {
    pub url: String,
    pub trusted: bool,
    #[serde(default)]
    pub account: Option<String>,
}

/// App-local Cloud settings, stored beneath the PhotoCraft config directory.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub servers: Vec<TrustedServer>,
    #[serde(default)]
    pub selected_server: Option<String>,
    /// Last used organization, workspace and project per server URL. Ids only, no credentials.
    #[serde(default)]
    pub last_used: BTreeMap<String, LastSelection>,
    /// "Don't show again" of the Save As chooser: Save As goes straight to the native dialog.
    /// Preferences › File Handling › Ask where to save turns the chooser back on.
    #[serde(default)]
    pub skip_save_chooser: bool,
}

/// Ids of the hierarchy the user picked last on one server. Entries that no longer exist are
/// ignored when they are looked up.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LastSelection {
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
}

impl Config {
    /// Load metadata without creating a directory or touching the keychain.
    pub fn load(app_data: Option<&Path>) -> Result<Self, String> {
        let Some(path) = config_path(app_data) else {
            return Ok(Self::default());
        };
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("CraftCloud settings: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("CraftCloud settings: {e}")),
        }
    }

    /// Persist metadata atomically. Missing app-data path means persistence is unavailable.
    pub fn save(&self, app_data: Option<&Path>) -> Result<(), String> {
        let path = config_path(app_data).ok_or("PhotoCraft data directory is unavailable")?;
        let parent = path.parent().ok_or("CraftCloud settings path has no parent")?;
        fs::create_dir_all(parent).map_err(|e| format!("CraftCloud settings: {e}"))?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| format!("CraftCloud settings: {e}"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|e| format!("CraftCloud settings: {e}"))?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!("CraftCloud settings: {error}"));
        }
        Ok(())
    }

    pub fn selected(&self) -> Option<&TrustedServer> {
        let url = self.selected_server.as_deref()?;
        self.servers.iter().find(|server| server.url == url && server.trusted)
    }

    pub fn trust(&mut self, url: String) -> Result<(), String> {
        validate_server_url(&url)?;
        if let Some(server) = self.servers.iter_mut().find(|server| server.url == url) {
            server.trusted = true;
        } else {
            self.servers.push(TrustedServer { url: url.clone(), trusted: true, account: None });
        }
        self.selected_server = Some(url);
        Ok(())
    }
}

/// Cloud cache and mutable working files stay in distinct directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub working: PathBuf,
    pub keychain_locks: PathBuf,
}

impl Paths {
    pub fn under(app_data: &Path) -> Self {
        let root = app_data.join("CraftCloud");
        Self { cache: root.join("cache"), working: root.join("working"), keychain_locks: root.clone(), root }
    }

    /// Whether `path` is a regular file inside the working directory. Only such files may be deleted
    /// when a document closes; a user's own file is never inside it.
    pub fn owns(&self, path: &Path) -> bool {
        let (Ok(working), Some(parent), Some(name)) = (fs::canonicalize(&self.working), path.parent(), path.file_name()) else { return false };
        let Ok(parent) = fs::canonicalize(parent) else { return false };
        if !parent.starts_with(&working) {
            return false;
        }
        match fs::symlink_metadata(parent.join(name)) {
            Ok(meta) => meta.is_file(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }
}

fn config_path(app_data: Option<&Path>) -> Option<PathBuf> {
    app_data.map(|path| path.join("CraftCloud").join(CONFIG_FILE))
}

/// Document name without a trailing image or document extension, so `photo.png` is offered as
/// `photo.pcraft` rather than `photo.png.pcraft`.
/// `name` without a trailing `.pcraft` or `.psd` (any letter case).
fn strip_format_extension(name: &str) -> &str {
    for (id, _) in SAVE_FORMATS {
        let suffix = id.len() + 1;
        if let Some(at) = name.len().checked_sub(suffix)
            && name.is_char_boundary(at)
            && name[at..].starts_with('.')
            && name[at + 1..].eq_ignore_ascii_case(id)
        {
            return &name[..at];
        }
    }
    name
}

fn document_stem(name: &str) -> &str {
    const KNOWN: [&str; 14] = ["pcraft", "psd", "png", "jpg", "jpeg", "tif", "tiff", "webp", "bmp", "gif", "heic", "heif", "avif", "tga"];
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && KNOWN.iter().any(|known| extension.eq_ignore_ascii_case(known)) => stem,
        _ => name,
    }
}

fn safe_file_stem(name: &str, extension: &str) -> String {
    let stem = name.strip_suffix(extension).unwrap_or(name);
    let clean: String = stem.chars().filter(|ch| !ch.is_control() && !"<>:\"/\\|?*".contains(*ch)).collect();
    let clean = clean.trim().trim_matches('.');
    if clean.is_empty() { "Untitled".into() } else { clean.to_string() }
}

/// Require HTTPS except explicit loopback test instances. Reject credentials, fragments and
/// non-origin URLs so server identity cannot change through a crafted endpoint.
pub fn validate_server_url(raw: &str) -> Result<(), String> {
    let url = url::Url::parse(raw).map_err(|_| "Invalid CraftCloud server URL")?;
    let loopback = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if (url.scheme() != "https" && !(loopback && url.scheme() == "http"))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("CraftCloud requires an HTTPS server URL (HTTP is allowed only on loopback)".into());
    }
    Ok(())
}

/// Snapshot the path based SDK link before starting a save upload. The captured file and base
/// version stay fixed even if the active document or its local path changes while the worker runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadTarget {
    pub server: String,
    pub file_id: Uuid,
    pub base_version: Option<Uuid>,
}

pub fn upload_target(link: &craftcloud_client_native::CloudLink, read_only: bool) -> Result<UploadTarget, CloudError> {
    if read_only {
        return Err(CloudError::Forbidden);
    }
    validate_server_url(&link.server).map_err(|_| CloudError::InvalidUrl)?;
    let file_id = Uuid::parse_str(&link.file_id).map_err(|_| CloudError::InvalidUrl)?;
    let base_version = link.base_version.as_deref().map(Uuid::parse_str).transpose().map_err(|_| CloudError::InvalidUrl)?;
    Ok(UploadTarget { server: link.server.clone(), file_id, base_version })
}

/// Upload an immutable snapshot already exported and saved locally. This function never writes to
/// the local path; it advances the stored base only after the version commit succeeds.
pub fn upload_saved<T: CloudTransport>(
    client: &CloudClient<T>,
    links: &LinksStore,
    local_path: &Path,
    link: &craftcloud_client_native::CloudLink,
    bytes: Vec<u8>,
    read_only: bool,
) -> Result<SaveOutcome, CloudError> {
    upload_saved_with_key(client, links, local_path, link, bytes, read_only, Uuid::new_v4().to_string())
}

fn upload_saved_with_key<T: CloudTransport>(
    client: &CloudClient<T>,
    links: &LinksStore,
    local_path: &Path,
    link: &craftcloud_client_native::CloudLink,
    bytes: Vec<u8>,
    read_only: bool,
    idempotency_key: String,
) -> Result<SaveOutcome, CloudError> {
    let target = upload_target(link, read_only)?;
    let current = links.get(local_path)?.ok_or(CloudError::NotFound)?;
    if current.server != link.server || current.file_id != link.file_id || current.base_version != link.base_version {
        return Err(CloudError::VersionConflict(target.base_version));
    }
    let version = client.upload_with_cancel(
        &UploadContent { file_id: target.file_id, base_version: target.base_version, idempotency_key, bytes },
        Arc::new(AtomicBool::new(false)),
    )?;
    let mut updated = current;
    updated.base_version = Some(version.id.to_string());
    updated.sha256 = version.sha256.clone();
    links.upsert(updated)?;
    Ok(SaveOutcome::Uploaded(version))
}

fn upload_target_version<T: CloudTransport>(
    client: &CloudClient<T>,
    links: &LinksStore,
    local_path: &Path,
    link: &CloudLink,
    bytes: Vec<u8>,
    idempotency_key: String,
) -> Result<SaveOutcome, CloudError> {
    let file_id = Uuid::parse_str(&link.file_id).map_err(|_| CloudError::InvalidResponse)?;
    let base_version = link.base_version.as_deref().map(Uuid::parse_str).transpose().map_err(|_| CloudError::InvalidResponse)?;
    let version = client.upload_with_cancel(&UploadContent { file_id, base_version, idempotency_key, bytes }, Arc::new(AtomicBool::new(false)))?;
    let mut committed = link.clone();
    committed.base_version = Some(version.id.to_string());
    committed.sha256 = version.sha256.clone();
    committed.local_path = local_path.to_path_buf();
    links.upsert(committed)?;
    Ok(SaveOutcome::Uploaded(version))
}

/// Create remote metadata only once, persist its identity before uploading, then send the exact
/// bytes already written by PhotoCraft. Retries reuse that identity and never rewrite the file.
fn upload_new_snapshot<T: CloudTransport>(
    client: &CloudClient<T>,
    links: &LinksStore,
    local_path: &Path,
    server: &str,
    target: &craftcloud_client_native::NewFile,
    bytes: Vec<u8>,
) -> Result<SaveOutcome, CloudError> {
    let mut link = if let Some(link) = links.get(local_path)? {
        if link.server != server {
            return Err(CloudError::Forbidden);
        }
        link
    } else {
        let file: craftcloud_client::File = client.post(
            &format!("/api/v1/projects/{}/files", target.project_id),
            &serde_json::json!({"name": target.name.as_str(), "app": target.app.as_str(), "format": target.format.as_str()}),
        )?;
        let link = CloudLink {
            local_path: local_path.to_path_buf(),
            server: server.to_string(),
            file_id: file.id.to_string(),
            base_version: None,
            sha256: String::new(),
        };
        links.upsert(link.clone())?;
        link
    };
    let file_id = Uuid::parse_str(&link.file_id).map_err(|_| CloudError::InvalidResponse)?;
    let version = client.upload_with_cancel(
        &UploadContent {
            file_id,
            base_version: link.base_version.as_deref().map(Uuid::parse_str).transpose().map_err(|_| CloudError::InvalidResponse)?,
            idempotency_key: target.idempotency_key.clone(),
            bytes,
        },
        Arc::new(AtomicBool::new(false)),
    )?;
    link.base_version = Some(version.id.to_string());
    link.sha256 = version.sha256.clone();
    links.upsert(link)?;
    Ok(SaveOutcome::Uploaded(version))
}

/// Save a downloaded immutable version into a separate editable working file.
pub fn working_path(paths: &Paths, server: &str, file_id: Uuid, version_id: Uuid, extension: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    server.hash(&mut hasher);
    let server_key = format!("{:016x}", hasher.finish());
    let extension = craftcloud_client_native::cache_extension(extension);
    paths.working.join(server_key).join(file_id.to_string()).join(format!("{version_id}.{extension}"))
}

fn create_working_file(base_path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let parent = base_path.parent().ok_or("Cloud working path has no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let stem = base_path.file_stem().and_then(|name| name.to_str()).ok_or("Cloud working filename is invalid")?;
    let extension = base_path.extension().and_then(|value| value.to_str()).ok_or("Cloud working extension is invalid")?;
    for attempt in 0..32 {
        let path = if attempt == 0 { base_path.to_path_buf() } else { parent.join(format!("{stem}-{}.{}", Uuid::new_v4(), extension)) };
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error.to_string());
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Could not allocate a unique CraftCloud working file after 32 attempts".into())
}

/// A Cloud deep link after syntactic validation. The caller must still authenticate through the
/// app's own device session and apply the server permission checks before opening the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub server: String,
    pub file_id: Uuid,
}

pub fn parse_open_link(raw: &str) -> Result<OpenRequest, String> {
    let url = url::Url::parse(raw).map_err(|_| "Invalid CraftCloud open link")?;
    if url.scheme() != "craftcloud"
        || url.host_str() != Some("open")
        || !(url.path().is_empty() || url.path() == "/")
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid CraftCloud open link".into());
    }
    let mut server = None;
    let mut file = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "server" if server.is_none() => server = Some(value.into_owned()),
            "file" if file.is_none() => file = Some(value.into_owned()),
            _ => return Err("Invalid CraftCloud open link".into()),
        }
    }
    let server = server.ok_or("CraftCloud open link has no server")?;
    validate_server_url(&server)?;
    let file_id = Uuid::parse_str(&file.ok_or("CraftCloud open link has no file")?).map_err(|_| "CraftCloud open link has an invalid file id")?;
    Ok(OpenRequest { server, file_id })
}

/// UI state is ephemeral. No picker credentials enter PhotoCraft's persisted UI state.
pub struct Runtime {
    data_dir: Option<PathBuf>,
    pub config: Config,
    picker: PickerState,
    pending_open: Option<OpenRequest>,
    pending_locked_open: Option<(String, craftcloud_client::File, Vec<u8>)>,
    pending_admissions: HashMap<PathBuf, PendingAdmission>,
    local_open_requests: Vec<String>,
    startup_error: Option<String>,
    return_to_save: bool,
    client: Option<NativeClient>,
    client_server: Option<String>,
    links: Option<Arc<LinksStore>>,
    cache: Option<VersionCache>,
    documents: HashMap<photocraft_doc::DocId, PathBuf>,
    pending_new_paths: HashMap<photocraft_doc::DocId, PathBuf>,
    /// Working copies of closed documents, deleted once nothing about them is still unsynced.
    closed_working: HashSet<PathBuf>,
    /// Working copies with a local save that was not queued for upload (for example while signed out).
    unsynced_paths: HashSet<PathBuf>,
    /// Paths of background save jobs that are still running.
    saving_paths: HashSet<PathBuf>,
    leases: HashMap<photocraft_doc::DocId, FileLeaseHeartbeat>,
    read_only_documents: HashSet<photocraft_doc::DocId>,
    pending_uploads: HashMap<PathBuf, PendingUpload>,
    pending_new_uploads: HashMap<PathBuf, PendingNewUpload>,
    retry_path: Option<PathBuf>,
    queued_uploads: VecDeque<QueuedCloudSave>,
    pending_new_file: Option<PendingNewFile>,
    deferred_local_save: Option<DeferredLocalSave>,
    save_as_psd: bool,
    save_locked_recovery: bool,
    /// The Save As question, while it is open.
    save_chooser: Option<save_chooser::Chooser>,
    /// "Save on this computer" was chosen: the app continues with the native Save As.
    local_save: Option<save_chooser::Chooser>,
    request_tx: mpsc::Sender<WorkerResult>,
    request_rx: mpsc::Receiver<WorkerResult>,
    request_busy: bool,
    request_generation: u64,
    active_request: Option<RequestContext>,
    selected_org: Option<Uuid>,
    selected_workspace: Option<Uuid>,
    selected_project: Option<Uuid>,
    /// Remembered selection of the current server, read when the hierarchy is loaded.
    preferred: LastSelection,
    /// Fill the save dialog's file name from the active document on the next frame.
    prefill_file_name: bool,
    files: HashMap<Uuid, craftcloud_client::File>,
    workspace_names: HashMap<Uuid, String>,
    document_workspaces: HashMap<photocraft_doc::DocId, String>,
    locks_supported: bool,
}

#[derive(Clone)]
struct AppTransport(Arc<dyn CloudTransport>);
impl CloudTransport for AppTransport {
    fn send(&self, request: craftcloud_client::Request) -> Result<craftcloud_client::Response, CloudError> {
        self.0.send(request)
    }
}

type NativeClient = Arc<CloudClient<AppTransport>>;
type OpenPayload = (craftcloud_client::File, Vec<u8>, bool, Option<String>, Option<FileLeaseHeartbeat>);
type CopyDetailsPayload = (PathBuf, craftcloud_client_native::NewFile, String, Vec<u8>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestContext {
    server: String,
    generation: u64,
}

struct WorkerResult {
    context: RequestContext,
    event: WorkerEvent,
}

enum WorkerEvent {
    Login(Result<(NativeClient, craftcloud_client::DeviceUser, bool), CloudError>),
    Organizations(Result<Vec<craftcloud_client::Organization>, CloudError>),
    Workspaces(Result<Vec<craftcloud_client::Workspace>, CloudError>),
    Projects(Result<Vec<craftcloud_client::Project>, CloudError>),
    ProjectCreated(Result<craftcloud_client::Project, CloudError>),
    Files(Result<Vec<craftcloud_client::File>, CloudError>),
    Open(Result<Box<OpenPayload>, CloudError>),
    CopyDetails(Result<Box<CopyDetailsPayload>, CloudError>),
    Upload(Result<(PathBuf, SaveOutcome), (PathBuf, CloudError)>),
    Logout(Result<(), CloudError>),
}

impl WorkerResult {
    fn new(context: RequestContext, event: WorkerEvent) -> Self {
        Self { context, event }
    }
}

fn send_worker(tx: &mpsc::Sender<WorkerResult>, context: RequestContext, event: WorkerEvent) {
    let _ = tx.send(WorkerResult::new(context, event));
}

enum PageKind {
    Organizations,
    Workspaces(Uuid),
    Projects(Uuid),
    Files(Uuid),
}

#[derive(Clone)]
struct PendingUpload {
    link: craftcloud_client_native::CloudLink,
    bytes: Vec<u8>,
    idempotency_key: String,
    bind_doc: Option<photocraft_doc::DocId>,
    workspace: Option<String>,
}

struct PendingAdmission {
    link: CloudLink,
    read_only: bool,
    lease: Option<FileLeaseHeartbeat>,
    workspace: Option<String>,
}

#[derive(Clone)]
struct PendingNewUpload {
    project_id: Uuid,
    name: String,
    app: String,
    format: String,
    idempotency_key: String,
    server: String,
    bytes: Vec<u8>,
}

impl PendingNewUpload {
    fn target(&self) -> craftcloud_client_native::NewFile {
        craftcloud_client_native::NewFile {
            project_id: self.project_id,
            name: self.name.clone(),
            app: self.app.clone(),
            format: self.format.clone(),
            idempotency_key: self.idempotency_key.clone(),
        }
    }
}

#[derive(Clone)]
struct PendingNewFile {
    doc: photocraft_doc::DocId,
    project_id: Uuid,
    name: String,
    format: String,
    bind_document: bool,
    server: String,
}

enum QueuedCloudSave {
    Linked { doc: photocraft_doc::DocId, path: PathBuf, link: craftcloud_client_native::CloudLink, bytes: Vec<u8>, key: String },
    New { path: PathBuf, target: craftcloud_client_native::NewFile, server: String, bytes: Vec<u8> },
    AfterNew { doc: photocraft_doc::DocId, path: PathBuf, bytes: Vec<u8> },
}

pub(crate) struct DeferredLocalSave {
    pub path: String,
    pub copy: bool,
    pub doc: photocraft_doc::DocId,
}

fn spawn_pages(client: NativeClient, tx: mpsc::Sender<WorkerResult>, kind: PageKind, context: RequestContext) -> Result<(), String> {
    std::thread::Builder::new()
        .name("photocraft-cloud-list".into())
        .spawn(move || {
            let result = match kind {
                PageKind::Organizations => WorkerEvent::Organizations(all_pages::<craftcloud_client::Organization>(&client, "/api/v1/organizations")),
                PageKind::Workspaces(org) => {
                    WorkerEvent::Workspaces(all_pages::<craftcloud_client::Workspace>(&client, &format!("/api/v1/organizations/{org}/workspaces")))
                }
                PageKind::Projects(workspace) => {
                    WorkerEvent::Projects(all_pages::<craftcloud_client::Project>(&client, &format!("/api/v1/workspaces/{workspace}/projects")))
                }
                PageKind::Files(project) => WorkerEvent::Files(all_pages::<craftcloud_client::File>(&client, &format!("/api/v1/projects/{project}/files"))),
            };
            send_worker(&tx, context, result);
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn all_pages<T: serde::de::DeserializeOwned>(client: &CloudClient<AppTransport>, path: &str) -> Result<Vec<T>, CloudError> {
    let mut values = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10_000 {
        let url = if let Some(cursor) = cursor.as_deref() {
            format!("{path}?cursor={}", url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect::<String>())
        } else {
            path.to_string()
        };
        let page: craftcloud_client::Page<T> = client.get(&url)?;
        values.extend(page.items);
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(values);
        }
    }
    Err(CloudError::InvalidResponse)
}

#[derive(Deserialize)]
struct SharedTarget {
    target_kind: String,
    target_id: Uuid,
    role: String,
    #[allow(dead_code)]
    workspace: SharedName,
    project: Option<SharedName>,
    file: Option<SharedName>,
}
#[derive(Deserialize)]
struct SharedName {
    id: Uuid,
    name: String,
}

fn can_edit_file(client: &CloudClient<AppTransport>, file: &craftcloud_client::File) -> Result<bool, CloudError> {
    let orgs = all_pages::<craftcloud_client::Organization>(client, "/api/v1/organizations")?;
    let org_role = orgs.iter().find(|org| org.id == file.organization_id).map(|org| org.role.as_str()).unwrap_or("");
    let workspaces =
        all_pages::<craftcloud_client::Workspace>(client, &format!("/api/v1/organizations/{}/workspaces", file.organization_id)).unwrap_or_default();
    let workspace_role = workspaces.iter().find(|workspace| workspace.id == file.workspace_id).and_then(|workspace| workspace.workspace_role.as_deref());
    let grants = match all_pages::<SharedTarget>(client, &format!("/api/v1/organizations/{}/shared-with-me", file.organization_id)) {
        Ok(items) => items
            .into_iter()
            .map(|grant| {
                let _ = (grant.workspace.id, grant.workspace.name, grant.project, grant.file);
                permissions::GrantPermission { target_kind: grant.target_kind, target_id: grant.target_id, role: grant.role }
            })
            .collect::<Vec<_>>(),
        Err(CloudError::NotFound | CloudError::Forbidden) => Vec::new(),
        Err(error) => return Err(error),
    };
    Ok(permissions::file_is_editable(org_role, workspace_role, org_role.eq_ignore_ascii_case("guest"), file.id, file.project_id, &grants))
}

enum LeaseAcquire {
    Acquired(FileLeaseHeartbeat),
    Locked(String),
}

fn acquire_file_lease(client: &NativeClient, id: Uuid, timeout: std::time::Duration) -> Result<LeaseAcquire, CloudError> {
    if !craftcloud_client_native::wait_for_release(&id.to_string(), timeout) {
        return Ok(LeaseAcquire::Locked(tl!("The previous CraftCloud lease is still releasing.").into()));
    }
    match client.acquire_lock(id, "photocraft") {
        Ok(_) => {
            let heartbeat_client = client.clone();
            let release_client = client.clone();
            Ok(LeaseAcquire::Acquired(FileLeaseHeartbeat::start_keyed(
                id.to_string(),
                move || {
                    let _ = heartbeat_client.acquire_lock(id, "photocraft");
                },
                move || {
                    let _ = release_client.release_lock(id);
                },
            )))
        }
        Err(CloudError::Forbidden) => Ok(LeaseAcquire::Locked("another editor".into())),
        Err(CloudError::FileLocked(owner)) => Ok(LeaseAcquire::Locked(owner.unwrap_or_else(|| "another editor".into()))),
        Err(error) => Err(error),
    }
}

fn prepare_open(client: NativeClient, file: craftcloud_client::File, locks_supported: bool) -> Result<OpenPayload, CloudError> {
    let editable = can_edit_file(&client, &file)?;
    let bytes = client.download(file.id, file.head_version_id)?;
    if !editable || !locks_supported {
        return Ok((file, bytes, editable, None, None));
    }
    match acquire_file_lease(&client, file.id, std::time::Duration::from_secs(5))? {
        LeaseAcquire::Acquired(lease) => Ok((file, bytes, true, None, Some(lease))),
        LeaseAcquire::Locked(owner) => Ok((file, bytes, false, Some(owner), None)),
    }
}

pub fn handles(id: &str) -> bool {
    matches!(id, "file.craftcloud.open" | "file.craftcloud.save" | "file.craftcloud.save_psd")
}

pub fn is_enabled(app: &crate::PhotocraftApp, id: &str) -> bool {
    match id {
        "file.craftcloud.open" => true,
        "file.craftcloud.save" => !app.cloud.configured() || app.session.active().is_some(),
        "file.craftcloud.save_psd" => !app.cloud.configured() || app.session.active().is_some(),
        _ => false,
    }
}

pub fn menu_items(app: &crate::PhotocraftApp) -> Vec<crate::menus::MenuItem> {
    [
        ("file.craftcloud.open", "open", vec!["File".to_string()]),
        ("file.craftcloud.save", "save", vec!["File".to_string()]),
        ("file.craftcloud.save_psd", "save_psd", vec!["File".to_string()]),
    ]
    .into_iter()
    .map(|(id, action, path)| {
        let setup = !app.cloud.configured();
        let label = match (action, setup) {
            ("open", true) => tl!("Set up CraftCloud to open…"),
            ("save", true) => tl!("Set up CraftCloud to save…"),
            ("save_psd", true) => tl!("Set up CraftCloud to save as PSD…"),
            ("open", false) => tl!("Open from CraftCloud…"),
            ("save", false) => tl!("Save to CraftCloud…"),
            _ => tl!("Save as PSD in CraftCloud…"),
        };
        crate::menus::MenuItem { id: id.into(), label: label.into(), path, shortcut: None, enabled: is_enabled(app, id), checked: None, color: None }
    })
    .collect()
}

impl Runtime {
    pub fn new(data_dir: Option<&Path>) -> Self {
        let (config, startup_error) = match Config::load(data_dir) {
            Ok(config) => (config, None),
            Err(error) => (Config::default(), Some(error)),
        };
        let picker = PickerState { error: startup_error.clone(), ..PickerState::default() };
        let (request_tx, request_rx) = mpsc::channel();
        let paths = data_dir.map(Paths::under);
        Self {
            data_dir: data_dir.map(Path::to_path_buf),
            config,
            picker,
            pending_open: None,
            pending_locked_open: None,
            pending_admissions: HashMap::new(),
            local_open_requests: Vec::new(),
            startup_error,
            return_to_save: false,
            client: None,
            client_server: None,
            links: paths.as_ref().map(|paths| Arc::new(LinksStore::new(&paths.root))),
            cache: paths.as_ref().map(|paths| VersionCache::new(&paths.cache)),
            documents: HashMap::new(),
            pending_new_paths: HashMap::new(),
            closed_working: HashSet::new(),
            unsynced_paths: HashSet::new(),
            saving_paths: HashSet::new(),
            leases: HashMap::new(),
            read_only_documents: HashSet::new(),
            pending_uploads: HashMap::new(),
            pending_new_uploads: HashMap::new(),
            retry_path: None,
            queued_uploads: VecDeque::new(),
            pending_new_file: None,
            deferred_local_save: None,
            save_as_psd: false,
            save_locked_recovery: false,
            save_chooser: None,
            local_save: None,
            request_tx,
            request_rx,
            request_busy: false,
            request_generation: 0,
            active_request: None,
            selected_org: None,
            selected_workspace: None,
            selected_project: None,
            preferred: LastSelection::default(),
            prefill_file_name: false,
            files: HashMap::new(),
            workspace_names: HashMap::new(),
            document_workspaces: HashMap::new(),
            locks_supported: false,
        }
    }

    fn begin_request(&mut self, server: &str) -> Option<RequestContext> {
        if self.request_busy {
            return None;
        }
        self.request_generation = self.request_generation.wrapping_add(1);
        let context = RequestContext { server: server.to_string(), generation: self.request_generation };
        self.active_request = Some(context.clone());
        self.request_busy = true;
        Some(context)
    }

    fn finish_request(&mut self) {
        self.request_busy = false;
        self.active_request = None;
    }

    fn load_organizations(&mut self) {
        if self.request_busy || !self.picker.organizations.is_empty() {
            return;
        }
        let (Some(client), Some(server)) = (self.client.clone(), self.client_server.clone()) else { return };
        self.reload_preferred(&server);
        let Some(context) = self.begin_request(&server) else { return };
        if let Err(error) = spawn_pages(client, self.request_tx.clone(), PageKind::Organizations, context) {
            self.finish_request();
            self.picker.error = Some(error);
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_transport(data_dir: Option<&Path>, server: &str, transport: Arc<dyn CloudTransport>) -> Result<Self, CloudError> {
        let mut runtime = Self::new(data_dir);
        runtime.config.trust(server.to_string()).map_err(|_| CloudError::InvalidUrl)?;
        let client = Arc::new(CloudClient::new(server.to_string(), AppTransport(transport))?);
        client.set_tokens(craftcloud_client::DeviceTokens {
            access_token: craftcloud_client::Secret::new("test-access"),
            refresh_token: craftcloud_client::Secret::new("test-refresh"),
            expires_at: "2999-01-01T00:00:00Z".into(),
        })?;
        runtime.client = Some(client);
        runtime.client_server = Some(server.to_string());
        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) fn bind_test_document(&mut self, doc: photocraft_doc::DocId, link: craftcloud_client_native::CloudLink) -> Result<(), CloudError> {
        self.links.as_ref().ok_or(CloudError::Offline)?.upsert(link.clone())?;
        self.documents.insert(doc, link.local_path);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn queue_test_new_file(
        &mut self,
        doc: photocraft_doc::DocId,
        project_id: Uuid,
        name: String,
        format: String,
        server: String,
        bind_document: bool,
    ) {
        self.pending_new_file = Some(PendingNewFile { doc, project_id, name, format, bind_document, server });
    }

    #[cfg(test)]
    pub(crate) fn queue_test_admission_with_lease(&mut self, path: &str, link: CloudLink, released: Arc<std::sync::atomic::AtomicUsize>) {
        let path_buf = PathBuf::from(path);
        let key = link.file_id.clone();
        let lease = FileLeaseHeartbeat::start_keyed(
            key,
            || {},
            move || {
                released.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );
        self.pending_admissions.insert(path_buf.clone(), PendingAdmission { link, read_only: false, lease: Some(lease), workspace: None });
        self.local_open_requests.push(path_buf.to_string_lossy().into_owned());
    }

    pub fn begin_open(&mut self) {
        self.return_to_save = false;
        if let Some(server) = self.config.selected() {
            self.picker.server_url = server.url.clone();
            if let Some(account) = &server.account {
                self.picker.email = account.clone();
            }
        }
        self.picker.dialog = Some(if self.config.selected().is_none() {
            Dialog::Server
        } else if self.client.is_some() {
            Dialog::Browse
        } else {
            Dialog::Login
        });
        if self.client.is_some() && self.picker.organizations.is_empty() {
            self.load_organizations();
            return;
        }
        if self.client.is_some()
            && self.files.is_empty()
            && let Some(project) = self.selected_project
        {
            self.picker.dialog = Some(Dialog::Browse);
            self.select_project(project);
        }
    }

    pub fn configured(&self) -> bool {
        self.config.selected().is_some()
    }

    pub(crate) fn blocks_input(&self) -> bool {
        self.picker.dialog.is_some() || self.save_locked_recovery || self.save_chooser.is_some()
    }

    /// Whether File › Save As asks "CraftCloud or this computer" first. Without an app data
    /// directory (a host that cannot store CraftCloud settings or working files) CraftCloud cannot
    /// be used, so Save As goes straight to the native dialog.
    pub(crate) fn asks_where_to_save(&self) -> bool {
        self.data_dir.is_some() && !self.config.skip_save_chooser
    }

    /// The setting behind Preferences › File Handling › Ask where to save.
    pub(crate) fn ask_setting(&self) -> bool {
        !self.config.skip_save_chooser
    }

    /// Preferences › File Handling › Ask where to save. Failing to persist it is reported in the
    /// CraftCloud dialogs' error line like other settings; the choice still applies this session.
    pub(crate) fn set_asks_where_to_save(&mut self, ask: bool) {
        if self.config.skip_save_chooser != ask {
            return;
        }
        self.config.skip_save_chooser = !ask;
        if let Err(error) = self.config.save(self.data_dir.as_deref()) {
            self.picker.error = Some(error);
        }
    }

    /// Opens the Save As question for `chooser`'s document. A second request while one is open (or
    /// while the CraftCloud wizard is) keeps what is on screen.
    pub(crate) fn open_save_chooser(&mut self, chooser: save_chooser::Chooser) {
        if self.save_chooser.is_none() && self.local_save.is_none() && self.picker.dialog.is_none() {
            self.save_chooser = Some(chooser);
        }
    }

    #[cfg(test)]
    pub(crate) fn save_chooser_open(&self) -> bool {
        self.save_chooser.is_some()
    }

    /// The Save As that continues in the native dialog after "Save on this computer".
    pub(crate) fn take_local_save(&mut self) -> Option<save_chooser::Chooser> {
        self.local_save.take()
    }

    fn answer_save_chooser(&mut self, outcome: save_chooser::Outcome) {
        let Some(chooser) = self.save_chooser.take() else { return };
        let save_chooser::Outcome::Choose { choice, remember } = outcome else { return };
        if remember {
            self.set_asks_where_to_save(false);
        }
        match choice {
            save_chooser::Choice::Cloud => self.begin_save(),
            save_chooser::Choice::Computer => self.local_save = Some(chooser),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_server_url(&self) -> &str {
        &self.picker.server_url
    }

    pub fn open_link(&mut self, raw: &str) -> Result<(), String> {
        if self.request_busy {
            return Err(tl!("Wait for the current CraftCloud request to finish before changing servers.").into());
        }
        let parsed = parse_open_link(raw)?;
        if self.has_pending_file_admission(parsed.file_id, &parsed.server) {
            return Ok(());
        }
        let request = self.prepare_open_link(raw)?;
        if let Some(request) = request {
            if self.client_server.as_deref() == Some(request.server.as_str()) {
                if let Some(client) = self.client.clone() {
                    self.download_linked_file(client, request.file_id, self.locks_supported);
                }
            } else {
                self.config.selected_server = Some(request.server.clone());
                self.picker.server_url = request.server.clone();
                self.client = None;
                self.client_server = None;
                self.picker.dialog = Some(Dialog::Login);
            }
        }
        Ok(())
    }

    pub(crate) fn open_path_for_link(&self, raw: &str) -> Result<Option<String>, String> {
        let request = parse_open_link(raw)?;
        let Some(links) = &self.links else { return Ok(None) };
        for path in self.documents.values() {
            if let Some(link) = links.get(path).map_err(|error| error.to_string())?
                && link.server == request.server
                && link.file_id == request.file_id.to_string()
            {
                return Ok(path.to_str().map(str::to_owned));
            }
        }
        for (path, admission) in &self.pending_admissions {
            if admission.link.server == request.server && admission.link.file_id == request.file_id.to_string() {
                return Ok(path.to_str().map(str::to_owned));
            }
        }
        Ok(None)
    }

    pub fn begin_save(&mut self) {
        self.save_as_psd = false;
        self.begin_save_intent();
    }

    fn begin_save_intent(&mut self) {
        self.return_to_save = true;
        self.prefill_file_name = true;
        self.picker.selected_file = None;
        self.picker.save_formats = SAVE_FORMATS.iter().map(|(id, label)| (id.to_string(), tl!(label).to_string())).collect();
        self.picker.save_format_label = tl!("Format").into();
        self.picker.save_format = Some(if self.save_as_psd { "psd" } else { "pcraft" }.into());
        if let Some(server) = self.config.selected() {
            self.picker.server_url = server.url.clone();
            if let Some(account) = &server.account {
                self.picker.email = account.clone();
            }
        }
        self.picker.dialog = Some(if self.config.selected().is_none() {
            Dialog::Server
        } else if self.client.is_some() && self.selected_project.is_none() {
            Dialog::Browse
        } else if self.client.is_some() {
            Dialog::Save
        } else {
            Dialog::Login
        });
        if self.client.is_some() && self.picker.organizations.is_empty() {
            self.picker.dialog = Some(Dialog::Browse);
            self.load_organizations();
            return;
        }
        if self.client.is_some()
            && self.files.is_empty()
            && let Some(project) = self.selected_project
        {
            self.picker.dialog = Some(Dialog::Browse);
            self.select_project(project);
        }
    }

    pub fn begin_save_psd(&mut self) {
        self.save_as_psd = true;
        self.begin_save_intent();
    }

    pub(crate) fn take_deferred_local_save(&mut self) -> Option<DeferredLocalSave> {
        self.deferred_local_save.take()
    }

    pub(crate) fn cancel_pending_new_file(&mut self, doc: photocraft_doc::DocId) {
        if self.pending_new_file.as_ref().is_some_and(|pending| pending.doc == doc) {
            self.pending_new_file = None;
        }
    }

    /// Validate a launch link before any keychain or network access. Unknown servers are held for
    /// explicit trust confirmation in the SDK server dialog.
    pub fn prepare_open_link(&mut self, raw: &str) -> Result<Option<OpenRequest>, String> {
        let request = parse_open_link(raw)?;
        if self.request_busy {
            return Err(tl!("Wait for the current CraftCloud request to finish before changing servers.").into());
        }
        if self.config.servers.iter().any(|server| server.url == request.server && server.trusted) {
            if self.config.selected().is_none_or(|server| server.url != request.server) {
                self.clear_hierarchy();
            }
            self.config.selected_server = Some(request.server.clone());
            self.picker.server_url = request.server.clone();
            if let Some(account) = self.config.selected().and_then(|server| server.account.clone()) {
                self.picker.email = account;
            }
            self.pending_open = Some(request.clone());
            self.picker.dialog = Some(if self.client.is_some() { Dialog::Browse } else { Dialog::Login });
            return Ok(Some(request));
        }
        self.picker.server_url = request.server.clone();
        self.picker.dialog = Some(Dialog::Server);
        self.pending_open = Some(request);
        Ok(None)
    }

    /// Render the SDK picker using the app's current English/German language choice.
    pub fn show(&mut self, app: &mut crate::PhotocraftApp, ctx: &egui::Context, app_lang: &str) -> Vec<Action> {
        let _ = app_lang;
        self.apply_worker_result(app);
        if self.blocks_input() && !self.request_busy && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.handle_action(app, Action::Cancel);
        }
        self.sync_picker(app);
        if let Some(chooser) = self.save_chooser.as_mut()
            && let Some(outcome) = save_chooser::show(ctx, chooser)
        {
            self.answer_save_chooser(outcome);
        }
        let actions = self.picker.show(ctx, &CloudTranslator);
        for action in &actions {
            self.handle_action(app, action.clone());
        }
        if self.return_to_save && self.picker.dialog == Some(Dialog::Save) && !self.files.is_empty() {
            let mut files: Vec<_> = self.files.values().map(|file| (file.id.to_string(), file.name.clone())).collect();
            files.sort_by_key(|left| left.1.to_lowercase());
            let selected = self.picker.selected_file.clone();
            egui::Window::new(tl!("Existing CraftCloud files")).resizable(true).show(ctx, |ui| {
                ui.label(tl!("Select a file to save a new version."));
                for (id, name) in &files {
                    if ui.selectable_label(selected.as_deref() == Some(id.as_str()), name).clicked() {
                        self.picker.selected_file = Some(id.clone());
                        self.picker.file_name = name.clone();
                    }
                }
            });
        }
        if self.save_locked_recovery {
            let mut selected = None;
            egui::Window::new(tl!("CraftCloud save is locked")).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(tl!("Another editor holds the file lock. Your local save is safe."));
                if ui.button(tl!("Retry upload")).clicked() {
                    selected = Some(Action::RetryUpload);
                }
                if ui.button(tl!("Save as a copy")).clicked() {
                    selected = Some(Action::KeepBoth);
                }
                if ui.button(tl!("Close")).clicked() {
                    selected = Some(Action::Cancel);
                }
            });
            if let Some(action) = selected {
                self.handle_action(app, action);
            }
        }
        actions
    }

    pub fn wants_local_save(&self, doc: photocraft_doc::DocId, path: &str) -> bool {
        self.pending_new_file.as_ref().is_some_and(|pending| pending.doc == doc)
            || (self.documents.contains_key(&doc) && self.links.as_ref().is_some_and(|links| links.get(Path::new(path)).ok().flatten().is_some()))
            || self.pending_new_paths.get(&doc).is_some_and(|pending_path| pending_path == Path::new(path))
    }

    pub fn has_pending_new_file(&self, doc: photocraft_doc::DocId) -> bool {
        self.pending_new_file.as_ref().is_some_and(|pending| pending.doc == doc)
    }

    pub fn workspace_label(&self, doc: photocraft_doc::DocId) -> Option<String> {
        self.document_workspaces.get(&doc).cloned().or_else(|| self.documents.contains_key(&doc).then(|| tl!("Workspace").to_string()))
    }

    pub(crate) fn take_local_open_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.local_open_requests)
    }

    pub(crate) fn did_open_file(&mut self, doc: photocraft_doc::DocId, path: &str) -> bool {
        let path_buf = PathBuf::from(path);
        let Some(admission) = self.pending_admissions.remove(&path_buf) else { return false };
        let read_only = admission.read_only;
        if let Some(links) = &self.links {
            if let Err(error) = links.upsert(admission.link.clone()) {
                self.picker.error = Some(format!("CraftCloud links: {error:?}"));
                return read_only;
            }
        } else {
            self.picker.error = Some(tl!("CraftCloud links are unavailable.").into());
            return read_only;
        }
        self.documents.insert(doc, path_buf);
        if read_only {
            self.read_only_documents.insert(doc);
        }
        if let Some(workspace) = admission.workspace {
            self.document_workspaces.insert(doc, workspace);
        }
        if let Some(lease) = admission.lease {
            self.leases.insert(doc, lease);
        }
        read_only
    }

    pub(crate) fn cancel_open_file(&mut self, path: &str) {
        let path = PathBuf::from(path);
        self.pending_admissions.remove(&path);
        self.local_open_requests.retain(|pending| Path::new(pending) != path.as_path());
    }

    #[cfg(test)]
    pub(crate) fn has_pending_admission(&self, path: &str) -> bool {
        self.pending_admissions.contains_key(Path::new(path))
    }

    pub fn on_local_save(&mut self, doc: photocraft_doc::DocId, path: &str, bytes: Vec<u8>) -> Result<bool, String> {
        let bound = self.documents.contains_key(&doc) && !self.read_only_documents.contains(&doc);
        let result = self.on_local_save_inner(doc, path, bytes);
        if bound && let Some(data_dir) = &self.data_dir {
            let path = PathBuf::from(path);
            if matches!(result, Ok(true)) {
                self.unsynced_paths.remove(&path);
            } else if Paths::under(data_dir).owns(&path) {
                // The save is on disk but was not queued (an error, or no link to upload to), so the
                // working copy is the only copy of it, even while an earlier upload is still running.
                self.unsynced_paths.insert(path);
            }
        }
        result
    }

    fn on_local_save_inner(&mut self, doc: photocraft_doc::DocId, path: &str, bytes: Vec<u8>) -> Result<bool, String> {
        if let Some(pending) = self.pending_new_file.as_ref().filter(|pending| pending.doc == doc).cloned() {
            if self.client.is_none() {
                return Err(tl!("Sign in to CraftCloud before uploading this save.").into());
            }
            if self.links.is_none() {
                return Err(tl!("CraftCloud links are unavailable.").into());
            }
            let Some(server) = self.config.selected().map(|server| server.url.clone()) else {
                return Err(tl!("No trusted CraftCloud server is selected.").into());
            };
            if self.client_server.as_deref() != Some(server.as_str()) || server != pending.server {
                self.pending_new_file = Some(pending);
                return Err(tl!("Sign in to the CraftCloud server selected for this save.").into());
            }
            let path = PathBuf::from(path);
            let target = craftcloud_client_native::NewFile {
                project_id: pending.project_id,
                name: pending.name,
                app: "photocraft".into(),
                format: pending.format,
                idempotency_key: Uuid::new_v4().to_string(),
            };
            if pending.bind_document {
                self.documents.insert(doc, path.clone());
                self.read_only_documents.remove(&doc);
                self.pending_new_paths.insert(doc, path.clone());
                if let Some(workspace) = self.selected_workspace.and_then(|id| self.workspace_names.get(&id)).cloned() {
                    self.document_workspaces.insert(doc, workspace);
                }
            }
            self.queued_uploads.push_back(QueuedCloudSave::New { path, target, server, bytes });
            self.pending_new_file = None;
            self.start_next_upload()?;
            return Ok(true);
        }
        if !self.documents.contains_key(&doc) {
            return Ok(false);
        }
        if self.read_only_documents.contains(&doc) {
            return Err(tl!("This CraftCloud file is open read-only. Use Save As to create an editable copy.").into());
        }
        let Some(links) = &self.links else { return Ok(false) };
        let Some(link) = links.get(Path::new(path)).map_err(|e| e.to_string())? else {
            if self.pending_new_paths.get(&doc).is_some_and(|pending_path| pending_path == Path::new(path)) {
                self.queued_uploads.push_back(QueuedCloudSave::AfterNew { doc, path: PathBuf::from(path), bytes });
                self.start_next_upload()?;
                return Ok(true);
            }
            return Ok(false);
        };
        if self.client.is_none() {
            return Err(tl!("Sign in to CraftCloud before uploading this save.").into());
        }
        if self.client_server.as_deref() != Some(link.server.as_str()) {
            self.pending_uploads
                .insert(PathBuf::from(path), PendingUpload { link, bytes, idempotency_key: Uuid::new_v4().to_string(), bind_doc: None, workspace: None });
            return Err(tl!("Sign in to the CraftCloud server linked to this document before uploading.").into());
        }
        let path = PathBuf::from(path);
        let idempotency_key = Uuid::new_v4().to_string();
        self.pending_uploads.insert(
            path.clone(),
            PendingUpload { link: link.clone(), bytes: bytes.clone(), idempotency_key: idempotency_key.clone(), bind_doc: None, workspace: None },
        );
        self.queued_uploads.push_back(QueuedCloudSave::Linked { doc, path, link, bytes, key: idempotency_key });
        self.start_next_upload()?;
        Ok(true)
    }

    fn start_next_upload(&mut self) -> Result<(), String> {
        if self.request_busy {
            return Ok(());
        }
        let Some(job) = self.queued_uploads.pop_front() else { return Ok(()) };
        let Some(client) = self.client.clone() else {
            self.queued_uploads.push_front(job);
            return Err(tl!("Sign in to CraftCloud before uploading this save.").into());
        };
        let Some(links) = self.links.clone() else {
            self.queued_uploads.push_front(job);
            return Err(tl!("CraftCloud links are unavailable.").into());
        };
        let job = match job {
            QueuedCloudSave::AfterNew { doc, path, bytes } => {
                let Some(link) = links.get(&path).map_err(|error| error.to_string())? else {
                    self.queued_uploads.push_front(QueuedCloudSave::AfterNew { doc, path, bytes });
                    return Err(tl!("The first CraftCloud save is still pending.").into());
                };
                QueuedCloudSave::Linked { doc, path, link, bytes, key: Uuid::new_v4().to_string() }
            }
            job => job,
        };
        let (path, server) = match &job {
            QueuedCloudSave::Linked { path, link, .. } => (path.clone(), link.server.clone()),
            QueuedCloudSave::New { path, server, .. } => (path.clone(), server.clone()),
            QueuedCloudSave::AfterNew { .. } => return Err(tl!("The first CraftCloud save is still pending.").into()),
        };
        if self.client_server.as_deref() != Some(server.as_str()) {
            self.queued_uploads.push_front(job);
            return Err(tl!("Sign in to the CraftCloud server linked to this document before uploading.").into());
        }
        if let QueuedCloudSave::New { path, target, server, bytes } = &job {
            self.pending_new_uploads.insert(
                path.clone(),
                PendingNewUpload {
                    project_id: target.project_id,
                    name: target.name.clone(),
                    app: target.app.clone(),
                    format: target.format.clone(),
                    idempotency_key: target.idempotency_key.clone(),
                    server: server.clone(),
                    bytes: bytes.clone(),
                },
            );
        }
        let job = match job {
            QueuedCloudSave::Linked { doc, path, mut link, bytes, key } => {
                let current = links.get(&path).map_err(|error| error.to_string())?.ok_or_else(|| tl!("CraftCloud link is unavailable.").to_string())?;
                if current.server != link.server || (current.file_id != link.file_id && self.documents.get(&doc) != Some(&path)) {
                    return Err(tl!("This document is linked to a different CraftCloud file.").into());
                }
                link = current;
                self.pending_uploads.insert(
                    path.clone(),
                    PendingUpload { link: link.clone(), bytes: bytes.clone(), idempotency_key: key.clone(), bind_doc: None, workspace: None },
                );
                QueuedCloudSave::Linked { doc, path, link, bytes, key }
            }
            QueuedCloudSave::AfterNew { doc, path, bytes } => {
                let Some(link) = links.get(&path).map_err(|error| error.to_string())? else {
                    return Err(tl!("The first CraftCloud save is still pending.").into());
                };
                QueuedCloudSave::Linked { doc, path, link, bytes, key: Uuid::new_v4().to_string() }
            }
            job => job,
        };
        let Some(context) = self.begin_request(&server) else {
            self.queued_uploads.push_front(job);
            return Ok(());
        };
        let tx = self.request_tx.clone();
        let worker_path = path.clone();
        let spawn = std::thread::Builder::new().name("photocraft-cloud-save".into()).spawn(move || {
            let result = match job {
                QueuedCloudSave::Linked { path, link, bytes, key, .. } => upload_saved_with_key(&client, &links, &path, &link, bytes, false, key),
                QueuedCloudSave::New { path, target, server, bytes } => upload_new_snapshot(&client, &links, &path, &server, &target, bytes),
                QueuedCloudSave::AfterNew { path, .. } => return send_worker(&tx, context, WorkerEvent::Upload(Err((path, CloudError::InvalidResponse)))),
            }
            .map(|outcome| (worker_path.clone(), outcome))
            .map_err(|error| (worker_path, error));
            send_worker(&tx, context, WorkerEvent::Upload(result));
        });
        if let Err(error) = spawn {
            self.finish_request();
            self.pending_uploads.remove(&path);
            return Err(error.to_string());
        }
        Ok(())
    }

    /// Tells the runtime which paths background save jobs are still writing.
    pub fn set_saving_paths(&mut self, paths: HashSet<PathBuf>) {
        self.saving_paths = paths;
    }

    /// Whether an upload for `path` is queued, running or waiting for a retry.
    fn upload_in_progress(&self, path: &Path) -> bool {
        self.pending_uploads.contains_key(path)
            || self.pending_new_uploads.contains_key(path)
            || self.queued_uploads.iter().any(|job| match job {
                QueuedCloudSave::New { path: queued, .. } | QueuedCloudSave::AfterNew { path: queued, .. } | QueuedCloudSave::Linked { path: queued, .. } => queued == path,
            })
    }

    pub fn sync_live_documents(&mut self, live: &HashSet<photocraft_doc::DocId>) {
        let closed: Vec<_> = self.documents.keys().filter(|doc| !live.contains(doc)).copied().collect();
        for doc in closed {
            if let Some(path) = self.documents.remove(&doc)
                && !self.saving_paths.contains(&path)
                && self.data_dir.as_deref().is_some_and(|data_dir| Paths::under(data_dir).owns(&path))
            {
                self.closed_working.insert(path);
            }
            self.pending_new_paths.remove(&doc);
            self.leases.remove(&doc);
            self.read_only_documents.remove(&doc);
            self.document_workspaces.remove(&doc);
        }
        self.purge_closed_working();
    }

    /// Deletes the working copy and link of closed documents whose uploads all succeeded. A path with
    /// a queued, running or failed upload, or with a save that was never queued, keeps both.
    fn purge_closed_working(&mut self) {
        let Some(data_dir) = self.data_dir.clone() else { return };
        let paths = Paths::under(&data_dir);
        let ready: Vec<PathBuf> = self
            .closed_working
            .iter()
            .filter(|path| {
                !self.upload_in_progress(path)
                    && !self.unsynced_paths.contains(*path)
                    && !self.saving_paths.contains(*path)
                    && !self.documents.values().any(|open| open == *path)
                    && !self.pending_admissions.contains_key(*path)
            })
            .cloned()
            .collect();
        for path in ready {
            self.closed_working.remove(&path);
            if !paths.owns(&path) {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    // Keep the link so the copy stays usable; report once.
                    self.picker.error = Some(format!("Could not remove the CraftCloud working copy: {error}"));
                    continue;
                }
            }
            if let Some(links) = &self.links
                && let Err(error) = links.remove(&path)
            {
                self.picker.error = Some(format!("CraftCloud links: {error:?}"));
            }
            self.unsynced_paths.remove(&path);
        }
    }

    pub fn detach_document(&mut self, doc: photocraft_doc::DocId) {
        self.documents.remove(&doc);
        self.pending_new_paths.remove(&doc);
        self.leases.remove(&doc);
        self.read_only_documents.remove(&doc);
        self.document_workspaces.remove(&doc);
    }

    fn handle_action(&mut self, app: &mut crate::PhotocraftApp, action: Action) {
        match action {
            Action::AddServer(_url) if self.request_busy => {
                self.picker.error = Some(tl!("Wait for the current CraftCloud request to finish before changing servers.").into())
            }
            Action::AddServer(url) => match self.config.trust(url).and_then(|()| self.config.save(self.data_dir.as_deref())) {
                Ok(()) => {
                    let selected = self.config.selected().map(|server| server.url.clone());
                    if self.client_server.as_deref() != selected.as_deref() {
                        self.client = None;
                        self.client_server = None;
                    }
                    self.clear_hierarchy();
                    self.picker.dialog = Some(Dialog::Login);
                    self.picker.error = None;
                }
                Err(error) => self.picker.error = Some(error),
            },
            Action::Login { server, email, password, second_factor } => self.login(server, email, password, second_factor),
            Action::SelectOrganization(id) => {
                if let Ok(id) = Uuid::parse_str(&id) {
                    self.select_organization(id);
                }
            }
            Action::SelectWorkspace(id) => {
                if let Ok(id) = Uuid::parse_str(&id) {
                    self.select_workspace(id);
                }
            }
            Action::SelectProject(id) => {
                if let Ok(id) = Uuid::parse_str(&id) {
                    self.select_project(id);
                }
            }
            Action::GoTo(level) => self.go_to(level),
            Action::CreateProject { name } => self.create_project(name),
            Action::OpenShared => self.list_shared(),
            Action::OpenFile(id) => {
                if let Ok(id) = Uuid::parse_str(&id) {
                    if self.return_to_save {
                        self.picker.selected_file = Some(id.to_string());
                        if let Some(file) = self.files.get(&id) {
                            self.picker.file_name = file.name.clone();
                        }
                        self.picker.dialog = Some(Dialog::Save);
                        return;
                    }
                    if let Some(file) = self.files.get(&id)
                        && let Some(path) = self.open_path_for_file(&file.id.to_string(), self.client_server.as_deref().unwrap_or_default())
                        && app.show_if_open(&path)
                    {
                        self.picker.dialog = None;
                        return;
                    }
                    self.open_file(id);
                }
            }
            Action::Logout => self.logout(),
            Action::More => self.picker.error = Some("There are no additional items to load".into()),
            Action::SaveNew { name } => self.save_new(app, name),
            Action::SaveVersion { file_id } => {
                if let Ok(id) = Uuid::parse_str(&file_id) {
                    self.save_version(app, id, false);
                }
            }
            Action::KeepBoth => self.keep_both_failed(),
            Action::Overwrite => self.overwrite_failed(),
            Action::RetryUpload => {
                self.save_locked_recovery = false;
                self.retry_upload()
            }
            Action::Cancel => {
                self.save_chooser = None;
                self.return_to_save = false;
                self.save_as_psd = false;
                self.save_locked_recovery = false;
                self.pending_locked_open = None;
                self.pending_open = None;
                self.picker.dialog = None;
            }
            Action::OpenReadOnly => {
                if let Some((server, file, bytes)) = self.pending_locked_open.take() {
                    if let Err(error) = self.install_download(server, file, bytes, false, None, None) {
                        self.picker.error = Some(error);
                    } else {
                        self.picker.dialog = None;
                        self.picker.error = None;
                    }
                } else {
                    self.picker.dialog = None;
                }
            }
        }
    }

    /// Mirror the runtime selection into the picker, and prefill the save name once per save.
    fn sync_picker(&mut self, app: &crate::PhotocraftApp) {
        self.picker.selected_organization = self.selected_org.map(|id| id.to_string());
        self.picker.selected_workspace = self.selected_workspace.map(|id| id.to_string());
        self.picker.selected_project = self.selected_project.map(|id| id.to_string());
        self.picker.loading = self.request_busy;
        if std::mem::take(&mut self.prefill_file_name)
            && let Some(st) = app.session.active()
        {
            let extension = if self.save_as_psd { "psd" } else { "pcraft" };
            self.picker.file_name = craftcloud_egui::file_name_with_extension(document_stem(&st.doc.name), extension);
        }
        // The format dropdown of the save step decides between the native format and PSD; the
        // file name follows it.
        if let Some(format) = self.picker.save_format.as_deref()
            && (format == "psd") != self.save_as_psd
        {
            self.save_as_psd = format == "psd";
            let name = self.picker.file_name.trim();
            if !name.is_empty() {
                let stem = strip_format_extension(name);
                self.picker.file_name = craftcloud_egui::file_name_with_extension(stem, format);
            }
        }
    }

    fn reload_preferred(&mut self, server: &str) {
        self.preferred = self.config.last_used.get(server).cloned().unwrap_or_default();
    }

    /// Remember the current selection for the server. Failing to save it is not worth an error.
    fn remember_selection(&mut self) {
        let Some(server) = self.client_server.clone() else { return };
        let entry = LastSelection {
            organization: self.selected_org.map(|id| id.to_string()),
            workspace: self.selected_workspace.map(|id| id.to_string()),
            project: self.selected_project.map(|id| id.to_string()),
        };
        if self.config.last_used.get(&server) != Some(&entry) {
            self.config.last_used.insert(server, entry);
            let _ = self.config.save(self.data_dir.as_deref());
        }
    }

    /// Back to the list at `level`; the lists below it are dropped.
    fn go_to(&mut self, level: Level) {
        if self.request_busy {
            return;
        }
        // Going back is a deliberate choice: do not jump forward again to the remembered entries.
        self.preferred = LastSelection::default();
        match level {
            Level::Organizations => {
                self.selected_org = None;
                self.selected_workspace = None;
                self.selected_project = None;
                self.picker.workspaces.clear();
                self.picker.projects.clear();
                self.workspace_names.clear();
            }
            Level::Workspaces => {
                self.selected_workspace = None;
                self.selected_project = None;
                self.picker.projects.clear();
            }
            Level::Projects => {
                self.selected_project = None;
            }
        }
        self.files.clear();
        self.picker.files.clear();
        self.picker.selected_file = None;
        self.picker.error = None;
        self.picker.show_new_project = false;
        self.picker.dialog = Some(Dialog::Browse);
    }

    fn create_project(&mut self, name: String) {
        if self.request_busy {
            return;
        }
        let (Some(client), Some(server), Some(workspace)) = (self.client.clone(), self.client_server.clone(), self.selected_workspace) else { return };
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(context) = self.begin_request(&server) else { return };
        let tx = self.request_tx.clone();
        self.picker.error = None;
        std::thread::spawn(move || {
            send_worker(&tx, context, WorkerEvent::ProjectCreated(client.create_project(workspace, &name)));
        });
    }

    /// Select the remembered entry of a freshly loaded list, or its only entry.
    fn auto_select(items: &[(String, String)], remembered: Option<&str>) -> Option<Uuid> {
        craftcloud_egui::pick_default(items, remembered).and_then(|id| Uuid::parse_str(&id).ok())
    }

    fn clear_hierarchy(&mut self) {
        self.selected_org = None;
        self.selected_workspace = None;
        self.selected_project = None;
        self.files.clear();
        self.workspace_names.clear();
        self.picker.organizations.clear();
        self.picker.workspaces.clear();
        self.picker.projects.clear();
        self.picker.files.clear();
        self.picker.selected_file = None;
    }

    fn login(&mut self, server: String, email: String, password: String, second_factor: Option<String>) {
        if self.request_busy {
            return;
        }
        if validate_server_url(&server).is_err() || self.config.selected().is_none_or(|s| s.url != server) {
            self.picker.error = Some("Trust this server before signing in".into());
            return;
        }
        let Some(paths) = self.data_dir.as_deref().map(Paths::under) else {
            self.picker.error = Some("PhotoCraft data directory is unavailable".into());
            return;
        };
        let challenge_id = self.picker.second_factor_challenge_id.clone();
        let tx = self.request_tx.clone();
        let Some(context) = self.begin_request(&server) else { return };
        std::thread::Builder::new()
            .name("photocraft-cloud-auth".into())
            .spawn(move || {
                let result = (|| {
                    let client = Arc::new(CloudClient::new(server.clone(), AppTransport(Arc::new(UreqTransport::default())))?);
                    let store =
                        Arc::new(KeychainTokenStore::new("CraftCloud").bound(server.clone(), &email, "photocraft").with_lock_dir(paths.keychain_locks.clone()));
                    client.set_refresh_token_store(store.clone())?;
                    let discovery = client.discover()?;
                    if let Some(code) = second_factor {
                        let challenge = challenge_id.ok_or(CloudError::Unauthorized)?;
                        let input = craftcloud_egui::classify_second_factor(&code)
                            .ok_or_else(|| CloudError::Validation("Enter a six-digit code or ten-character recovery code".into()))?;
                        let proof = match input {
                            craftcloud_egui::SecondFactorInput::Totp(code) => Proof::totp(code),
                            craftcloud_egui::SecondFactorInput::Recovery(code) => Proof::recovery(code),
                        };
                        client.complete_second_factor(&challenge, proof)?;
                    } else {
                        if let Ok(Some(refresh)) = store.load() {
                            client.restore_refresh_token(refresh);
                            match client.current_user() {
                                Ok(user) => return Ok((client, user, discovery.feature_flags.locks || discovery.features.iter().any(|f| f == "file_locks"))),
                                Err(CloudError::Unauthorized) => {}
                                Err(error) => return Err(error),
                            }
                        }
                        client.login(&email, &password, "app", "photocraft", "PhotoCraft")?;
                    }
                    let user = client.current_user()?;
                    Ok((client, user, discovery.feature_flags.locks || discovery.features.iter().any(|f| f == "file_locks")))
                })();
                send_worker(&tx, context, WorkerEvent::Login(result));
            })
            .map_err(|error| {
                self.finish_request();
                self.picker.error = Some(error.to_string());
            })
            .ok();
    }

    fn select_organization(&mut self, id: Uuid) {
        if self.request_busy {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let Some(server) = self.client_server.clone() else { return };
        self.selected_org = Some(id);
        self.selected_workspace = None;
        self.selected_project = None;
        self.remember_selection();
        self.workspace_names.clear();
        self.files.clear();
        self.picker.workspaces.clear();
        self.picker.projects.clear();
        self.picker.files.clear();
        let Some(context) = self.begin_request(&server) else { return };
        if let Err(error) = spawn_pages(client, self.request_tx.clone(), PageKind::Workspaces(id), context) {
            self.finish_request();
            self.picker.error = Some(error);
        }
    }
    fn select_workspace(&mut self, id: Uuid) {
        if self.request_busy {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let Some(server) = self.client_server.clone() else { return };
        self.selected_workspace = Some(id);
        self.selected_project = None;
        self.remember_selection();
        self.files.clear();
        self.picker.projects.clear();
        self.picker.files.clear();
        let Some(context) = self.begin_request(&server) else { return };
        if let Err(error) = spawn_pages(client, self.request_tx.clone(), PageKind::Projects(id), context) {
            self.finish_request();
            self.picker.error = Some(error);
        }
    }
    fn select_project(&mut self, id: Uuid) {
        if self.request_busy {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let Some(server) = self.client_server.clone() else { return };
        self.selected_project = Some(id);
        self.remember_selection();
        self.files.clear();
        self.picker.files.clear();
        self.picker.selected_file = None;
        let Some(context) = self.begin_request(&server) else { return };
        if let Err(error) = spawn_pages(client, self.request_tx.clone(), PageKind::Files(id), context) {
            self.finish_request();
            self.picker.error = Some(error);
        }
    }
    fn list_shared(&mut self) {
        if self.request_busy {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let Some(server) = self.client_server.clone() else { return };
        let Some(org) = self.selected_org else { return };
        let Some(context) = self.begin_request(&server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let result = all_pages::<SharedTarget>(&client, &format!("/api/v1/organizations/{org}/shared-with-me")).and_then(|targets| {
                let mut ids = HashSet::new();
                for target in targets {
                    if let Some(file) = target.file {
                        ids.insert(file.id);
                    }
                    if target.target_kind == "project" {
                        let project_id = target.project.map(|project| project.id).unwrap_or(target.target_id);
                        for file in all_pages::<craftcloud_client::File>(&client, &format!("/api/v1/projects/{project_id}/files"))? {
                            ids.insert(file.id);
                        }
                    }
                }
                ids.into_iter().map(|id| client.get::<craftcloud_client::File>(&format!("/api/v1/files/{id}"))).collect::<Result<Vec<_>, _>>()
            });
            send_worker(&tx, context, WorkerEvent::Files(result));
        });
    }
    fn open_file(&mut self, id: Uuid) {
        let Some(client) = self.client.clone() else { return };
        let Some(file) = self.files.get(&id).cloned() else { return };
        let locks_supported = self.locks_supported;
        self.download_file(client, file, locks_supported);
    }
    fn open_path_for_file(&self, id: &str, server: &str) -> Option<String> {
        let links = self.links.as_ref()?;
        self.documents
            .values()
            .find_map(|path| {
                let link = links.get(path).ok().flatten()?;
                (link.server == server && link.file_id == id).then(|| path.to_str().map(str::to_owned)).flatten()
            })
            .or_else(|| {
                self.pending_admissions.iter().find_map(|(path, admission)| {
                    (admission.link.server == server && admission.link.file_id == id).then(|| path.to_str().map(str::to_owned)).flatten()
                })
            })
    }
    fn has_pending_file_admission(&self, id: Uuid, server: &str) -> bool {
        self.pending_admissions.values().any(|admission| admission.link.server == server && admission.link.file_id == id.to_string())
    }
    fn download_file(&mut self, client: NativeClient, file: craftcloud_client::File, locks_supported: bool) {
        if self.request_busy {
            return;
        }
        let Some(server) = self.client_server.clone() else { return };
        if self.has_pending_file_admission(file.id, &server) {
            return;
        }
        let Some(context) = self.begin_request(&server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let result = prepare_open(client, file, locks_supported).map(Box::new);
            send_worker(&tx, context, WorkerEvent::Open(result));
        });
    }
    fn download_linked_file(&mut self, client: NativeClient, id: Uuid, locks_supported: bool) {
        if self.request_busy {
            return;
        }
        let Some(server) = self.client_server.clone() else { return };
        if self.has_pending_file_admission(id, &server) {
            return;
        }
        let Some(context) = self.begin_request(&server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let result = client
                .get::<craftcloud_client::File>(&format!("/api/v1/files/{id}"))
                .and_then(|file| prepare_open(client, file, locks_supported))
                .map(Box::new);
            send_worker(&tx, context, WorkerEvent::Open(result));
        });
    }
    fn save_new(&mut self, app: &mut crate::PhotocraftApp, name: String) {
        let Some(project_id) = self.selected_project else {
            self.picker.error = Some(tl!("Choose a CraftCloud project first.").into());
            return;
        };
        let Some(doc) = app.session.active().map(|st| st.doc.id) else {
            self.picker.error = Some(tl!("Open a document before saving to CraftCloud.").into());
            return;
        };
        if self.client.is_none() || self.config.selected().is_none() {
            self.picker.error = Some(tl!("Sign in to CraftCloud before saving.").into());
            return;
        }
        if self.request_busy || self.deferred_local_save.is_some() {
            self.picker.error = Some(tl!("A CraftCloud operation is already running.").into());
            return;
        }
        let format = if self.save_as_psd { "psd" } else { "pcraft" };
        self.save_as_psd = false;
        let suffix = format!(".{format}");
        let cloud_name = if name.to_ascii_lowercase().ends_with(&suffix) { name.trim().to_string() } else { format!("{}{suffix}", name.trim()) };
        let stem = safe_file_stem(&cloud_name, &suffix);
        let Some(data_dir) = self.data_dir.as_deref() else {
            self.picker.error = Some(tl!("PhotoCraft data directory is unavailable.").into());
            return;
        };
        let path = Paths::under(data_dir).working.join("new").join(format!("{}-{stem}{suffix}", Uuid::new_v4()));
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            self.picker.error = Some(error.to_string());
            return;
        }
        let Some(path) = path.to_str().map(str::to_owned) else {
            self.picker.error = Some(tl!("Cloud working path is not valid Unicode.").into());
            return;
        };
        let bind_document = format == "pcraft";
        let Some(server) = self.config.selected().map(|server| server.url.clone()) else { return };
        if self.client_server.as_deref() != Some(server.as_str()) {
            self.picker.error = Some(tl!("Sign in to the selected CraftCloud server before saving.").into());
            return;
        }
        self.pending_new_file = Some(PendingNewFile { doc, project_id, name: cloud_name, format: format.into(), bind_document, server });
        self.deferred_local_save = Some(DeferredLocalSave { path, copy: !bind_document, doc });
        self.return_to_save = false;
        self.picker.selected_file = None;
        self.picker.dialog = None;
    }

    fn save_version(&mut self, app: &mut crate::PhotocraftApp, id: Uuid, overwrite: bool) {
        if self.request_busy {
            return;
        }
        let Some(file) = self.files.get(&id).cloned() else { return };
        let Some(st) = app.session.active() else { return };
        if self.read_only_documents.contains(&st.doc.id) {
            self.picker.error = Some(tl!("This CraftCloud document is open read-only. Save a new editable copy first.").into());
            return;
        }
        if st.revision != st.saved_revision {
            self.picker.error = Some(tl!("Save this document locally before creating a CraftCloud version.").into());
            return;
        }
        let Some(path) = st.path.as_deref().map(PathBuf::from) else {
            self.picker.error = Some(tl!("Save this document locally before creating a CraftCloud version.").into());
            return;
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.picker.error = Some(error.to_string());
                return;
            }
        };
        let matches_format = match file.format.to_ascii_lowercase().as_str() {
            "psd" => photocraft_io::is_psd(&bytes),
            "pcraft" => photocraft_format::is_pcraft(&bytes),
            _ => path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case(&file.format)),
        };
        if !matches_format {
            self.picker.error = Some(tl!("This local file does not match the selected CraftCloud format. Save a local copy in that format first.").into());
            return;
        }
        if self.save_as_psd && !file.format.eq_ignore_ascii_case("psd") {
            self.picker.error = Some(tl!("Choose a PSD CraftCloud file for this save.").into());
            return;
        }
        let doc = st.doc.id;
        let Some(client) = self.client.clone() else { return };
        let Some(links) = self.links.clone() else { return };
        let Some(server) = self.config.selected().map(|s| s.url.clone()) else { return };
        if self.client_server.as_deref() != Some(server.as_str()) {
            self.picker.error = Some(tl!("Sign in to the selected CraftCloud server before saving.").into());
            return;
        }
        let existing = match links.get(&path) {
            Ok(link) => link,
            Err(error) => {
                self.picker.error = Some(format!("CraftCloud links: {error:?}"));
                return;
            }
        };
        let mut link = existing.unwrap_or_else(|| craftcloud_client_native::CloudLink {
            local_path: path.clone(),
            server: server.clone(),
            file_id: id.to_string(),
            base_version: file.head_version_id.map(|v| v.to_string()),
            sha256: file.sha256.clone().unwrap_or_default(),
        });
        link.local_path = path.clone();
        link.server = server.clone();
        link.file_id = id.to_string();
        link.base_version = file.head_version_id.map(|v| v.to_string());
        if overwrite {
            link.base_version = file.head_version_id.map(|v| v.to_string());
        }
        let Some(context) = self.begin_request(&server) else { return };
        self.return_to_save = false;
        self.save_as_psd = false;
        let idempotency_key = Uuid::new_v4().to_string();
        let workspace = self.workspace_names.get(&file.workspace_id).cloned();
        self.pending_uploads.insert(
            path.clone(),
            PendingUpload { link: link.clone(), bytes: bytes.clone(), idempotency_key: idempotency_key.clone(), bind_doc: Some(doc), workspace },
        );
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let failure_path = path.clone();
            let result = can_edit_file(&client, &file)
                .and_then(|editable| {
                    if !editable {
                        return Err(CloudError::Forbidden);
                    }
                    upload_target_version(&client, &links, &path, &link, bytes, idempotency_key)
                })
                .map(|outcome| (path.clone(), outcome))
                .map_err(|error| (failure_path, error));
            send_worker(&tx, context, WorkerEvent::Upload(result));
        });
    }

    fn retry_upload(&mut self) {
        let Some(path) = self.retry_path.clone() else { return };
        if let Some(pending) = self.pending_uploads.get(&path).cloned() {
            self.retry_linked_upload(path, pending);
        } else if let Some(pending) = self.pending_new_uploads.get(&path).cloned() {
            self.retry_new_upload(path, pending);
        }
    }

    fn retry_linked_upload(&mut self, path: PathBuf, pending: PendingUpload) {
        let Some(client) = self.client.clone() else { return };
        let Some(links) = self.links.clone() else { return };
        if self.client_server.as_deref() != Some(pending.link.server.as_str()) {
            self.picker.error = Some(tl!("Sign in to the CraftCloud server linked to this document before uploading.").into());
            return;
        }
        if self.request_busy {
            return;
        }
        let Some(context) = self.begin_request(&pending.link.server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let failure_path = path.clone();
            let result = if pending.bind_doc.is_some() {
                upload_target_version(&client, &links, &path, &pending.link, pending.bytes, pending.idempotency_key)
            } else {
                upload_saved_with_key(&client, &links, &path, &pending.link, pending.bytes, false, pending.idempotency_key)
            }
            .map(|outcome| (path.clone(), outcome))
            .map_err(|error| (failure_path, error));
            send_worker(&tx, context, WorkerEvent::Upload(result));
        });
    }

    fn retry_new_upload(&mut self, path: PathBuf, pending: PendingNewUpload) {
        let Some(client) = self.client.clone() else { return };
        let Some(links) = self.links.clone() else { return };
        if self.client_server.as_deref() != Some(pending.server.as_str()) {
            self.picker.error = Some(tl!("Sign in to the CraftCloud server selected for this save.").into());
            return;
        }
        if self.request_busy {
            return;
        }
        let Some(context) = self.begin_request(&pending.server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let failure_path = path.clone();
            let result = upload_new_snapshot(&client, &links, &path, &pending.server, &pending.target(), pending.bytes)
                .map(|outcome| (path.clone(), outcome))
                .map_err(|error| (failure_path, error));
            send_worker(&tx, context, WorkerEvent::Upload(result));
        });
    }

    fn overwrite_failed(&mut self) {
        let Some(path) = self.retry_path.clone() else { return };
        let Some(pending) = self.pending_uploads.get(&path).cloned() else { return };
        let Some(client) = self.client.clone() else { return };
        let Some(links) = self.links.clone() else { return };
        if self.client_server.as_deref() != Some(pending.link.server.as_str()) {
            self.picker.error = Some(tl!("Sign in to the CraftCloud server linked to this document before uploading.").into());
            return;
        }
        if self.request_busy {
            return;
        }
        let Some(context) = self.begin_request(&pending.link.server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let current = if pending.bind_doc.is_some() { pending.link.clone() } else { links.get(&path)?.ok_or(CloudError::NotFound)? };
                if current.server != pending.link.server || current.file_id != pending.link.file_id {
                    return Err(CloudError::VersionConflict(None));
                }
                let id = Uuid::parse_str(&pending.link.file_id).map_err(|_| CloudError::InvalidResponse)?;
                let latest: craftcloud_client::File = client.get(&format!("/api/v1/files/{id}"))?;
                let version = client.upload_with_cancel(
                    &UploadContent { file_id: id, base_version: latest.head_version_id, idempotency_key: pending.idempotency_key, bytes: pending.bytes },
                    Arc::new(AtomicBool::new(false)),
                )?;
                let mut updated = current;
                updated.base_version = Some(version.id.to_string());
                updated.sha256 = version.sha256.clone();
                links.upsert(updated)?;
                Ok((path.clone(), SaveOutcome::Uploaded(version)))
            })();
            let result = result.map_err(|error| (path, error));
            send_worker(&tx, context, WorkerEvent::Upload(result));
        });
    }

    fn keep_both_failed(&mut self) {
        let Some(path) = self.retry_path.clone() else { return };
        let Some(pending) = self.pending_uploads.get(&path).cloned() else { return };
        let Some(client) = self.client.clone() else { return };
        let Some(data_dir) = self.data_dir.clone() else { return };
        if self.client_server.as_deref() != Some(pending.link.server.as_str()) {
            self.picker.error = Some(tl!("Sign in to the CraftCloud server linked to this document before uploading.").into());
            return;
        }
        if self.request_busy {
            return;
        }
        let Some(context) = self.begin_request(&pending.link.server) else { return };
        let tx = self.request_tx.clone();
        let server = pending.link.server.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let id = Uuid::parse_str(&pending.link.file_id).map_err(|_| CloudError::InvalidResponse)?;
                let old: craftcloud_client::File = client.get(&format!("/api/v1/files/{id}"))?;
                let target = craftcloud_client_native::NewFile {
                    project_id: old.project_id,
                    name: format!("{} copy", old.name),
                    app: old.app,
                    format: old.format,
                    idempotency_key: Uuid::new_v4().to_string(),
                };
                let extension = craftcloud_client_native::cache_extension(&target.format);
                let copy_path = Paths::under(&data_dir).working.join("copies").join(format!("{}.{}", Uuid::new_v4(), extension));
                Ok((copy_path, target, server, pending.bytes))
            })();
            send_worker(&tx, context, WorkerEvent::CopyDetails(result.map(Box::new)));
        });
    }
    fn logout(&mut self) {
        let Some(client) = self.client.clone() else {
            self.picker.dialog = Some(Dialog::Login);
            return;
        };
        if self.request_busy {
            return;
        }
        let Some(server) = self.client_server.clone() else { return };
        let Some(context) = self.begin_request(&server) else { return };
        let tx = self.request_tx.clone();
        std::thread::spawn(move || {
            send_worker(&tx, context, WorkerEvent::Logout(client.logout()));
        });
    }

    fn apply_worker_result(&mut self, app: &mut crate::PhotocraftApp) {
        let Ok(result) = self.request_rx.try_recv() else { return };
        if self.active_request.as_ref() != Some(&result.context) {
            return;
        }
        let server = result.context.server.clone();
        let upload_failed = matches!(&result.event, WorkerEvent::Upload(Err(_)));
        self.finish_request();
        match result.event {
            WorkerEvent::Login(Ok((client, user, locks_supported))) => {
                self.picker.account_name = user.display_name;
                self.config.selected_server = Some(server.clone());
                self.picker.server_url = server.clone();
                self.client_server = Some(server.clone());
                let client_server = self.client_server.clone();
                if let Some(server) = self.config.servers.iter_mut().find(|server| Some(server.url.as_str()) == client_server.as_deref()) {
                    server.account = Some(user.email.clone());
                }
                if let Err(error) = self.config.save(self.data_dir.as_deref()) {
                    self.picker.error = Some(error);
                }
                self.client = Some(client.clone());
                self.reload_preferred(&server);
                self.locks_supported = locks_supported;
                self.picker.dialog = Some(if self.return_to_save && self.selected_project.is_some() { Dialog::Save } else { Dialog::Browse });
                self.picker.error = None;
                self.picker.requires_second_factor = false;
                self.picker.second_factor_challenge_id = None;
                if let Some(request) = self.pending_open.take() {
                    if request.server == server {
                        self.download_linked_file(client, request.file_id, self.locks_supported);
                    }
                } else {
                    if let Some(context) = self.begin_request(&server)
                        && let Err(error) = spawn_pages(client, self.request_tx.clone(), PageKind::Organizations, context)
                    {
                        self.finish_request();
                        self.picker.error = Some(error);
                    }
                }
            }
            WorkerEvent::Login(Err(CloudError::SecondFactorChallenge(challenge))) => {
                self.picker.requires_second_factor = true;
                self.picker.second_factor_challenge_id = Some(challenge);
                self.picker.password.clear();
                self.picker.dialog = Some(Dialog::Login);
            }
            WorkerEvent::Login(Err(error)) => self.show_error(error),
            WorkerEvent::Organizations(Ok(items)) => {
                self.picker.organizations = items.into_iter().map(|x| (x.id.to_string(), x.name)).collect();
                if self.selected_org.is_none()
                    && let Some(id) = Self::auto_select(&self.picker.organizations, self.preferred.organization.as_deref())
                {
                    self.select_organization(id);
                }
            }
            WorkerEvent::Workspaces(Ok(items)) => {
                self.workspace_names = items.iter().map(|workspace| (workspace.id, workspace.name.clone())).collect();
                self.picker.workspaces = items.into_iter().map(|x| (x.id.to_string(), x.name)).collect();
                if self.selected_workspace.is_none()
                    && let Some(id) = Self::auto_select(&self.picker.workspaces, self.preferred.workspace.as_deref())
                {
                    self.select_workspace(id);
                }
            }
            WorkerEvent::Projects(Ok(items)) => {
                self.picker.projects = items.into_iter().map(|x| (x.id.to_string(), x.name)).collect();
                if self.selected_project.is_none()
                    && let Some(id) = Self::auto_select(&self.picker.projects, self.preferred.project.as_deref())
                {
                    self.select_project(id);
                }
            }
            WorkerEvent::ProjectCreated(Ok(project)) => {
                let id = project.id;
                self.picker.projects.push((id.to_string(), project.name));
                self.picker.new_project_name.clear();
                self.picker.show_new_project = false;
                self.picker.error = None;
                self.select_project(id);
            }
            WorkerEvent::ProjectCreated(Err(CloudError::Forbidden)) => self.picker.report_create_project_forbidden(&CloudTranslator),
            WorkerEvent::ProjectCreated(Err(CloudError::Server { status: 409, .. })) => self.picker.report_project_name_taken(&CloudTranslator),
            WorkerEvent::ProjectCreated(Err(error)) => self.show_error(error),
            WorkerEvent::Files(Ok(items)) => {
                self.files = items.iter().map(|f| (f.id, f.clone())).collect();
                self.picker.files = items.into_iter().map(|f| (f.id.to_string(), f.name)).collect();
                if self.return_to_save {
                    self.picker.dialog = Some(Dialog::Save);
                }
            }
            WorkerEvent::Open(Ok(payload)) if payload.3.is_some() => {
                let (file, bytes, _editable, locked_by, _lease) = *payload;
                self.picker.locked_by = locked_by;
                self.picker.dialog = Some(Dialog::Lock);
                self.pending_locked_open = Some((server.clone(), file, bytes));
            }
            WorkerEvent::Open(Ok(payload)) => {
                let (file, bytes, editable, locked_by, lease) = *payload;
                match self.install_download(server.clone(), file, bytes, editable, locked_by, lease) {
                    Ok(()) => {
                        self.picker.dialog = None;
                        self.picker.error = None;
                    }
                    Err(e) => self.picker.error = Some(e),
                }
            }
            WorkerEvent::Upload(Ok((path, _))) => {
                if let Some(pending) = self.pending_uploads.remove(&path)
                    && let Some(doc) = pending.bind_doc
                {
                    if app.session.documents().iter().any(|document| document.doc.id == doc) {
                        self.documents.insert(doc, path.clone());
                        self.read_only_documents.remove(&doc);
                        if let Some(workspace) = pending.workspace {
                            self.document_workspaces.insert(doc, workspace);
                        }
                    }
                    if let Some(link) = self.links.as_ref().and_then(|links| links.get(&path).ok().flatten()) {
                        for queued in &mut self.queued_uploads {
                            if let QueuedCloudSave::Linked { path: queued_path, link: queued_link, .. } = queued
                                && queued_path == &path
                            {
                                *queued_link = link.clone();
                            }
                        }
                    }
                }
                self.pending_new_uploads.remove(&path);
                self.pending_new_paths.retain(|_, pending_path| pending_path != &path);
                if self.retry_path.as_ref() == Some(&path) {
                    self.retry_path = None;
                }
                self.picker.error = None;
                self.save_locked_recovery = false;
            }
            WorkerEvent::Logout(result) => {
                self.client = None;
                self.client_server = None;
                self.picker.dialog = Some(Dialog::Login);
                if let Err(e) = result {
                    self.picker.error = Some(format!("Signed out locally; server logout failed: {e:?}"));
                }
            }
            WorkerEvent::Upload(Err((path, error))) => {
                if let Some(new) = self.pending_new_uploads.get(&path).cloned()
                    && let Some(link) = self.links.as_ref().and_then(|links| links.get(&path).ok().flatten())
                {
                    self.pending_uploads.insert(
                        path.clone(),
                        PendingUpload { link, bytes: new.bytes.clone(), idempotency_key: new.idempotency_key.clone(), bind_doc: None, workspace: None },
                    );
                    self.pending_new_uploads.remove(&path);
                }
                self.retry_path = (self.pending_uploads.contains_key(&path) || self.pending_new_uploads.contains_key(&path)).then_some(path);
                let message = crate::i18n::fmt(tl!("CraftCloud upload failed: {error}"), &[("error", &format!("{error:?}"))]);
                crate::notices::error(app, message);
                self.save_locked_recovery = matches!(error, CloudError::FileLocked(_));
                self.show_error(error);
            }
            WorkerEvent::Workspaces(Err(e))
            | WorkerEvent::Projects(Err(e))
            | WorkerEvent::Files(Err(e))
            | WorkerEvent::Organizations(Err(e))
            | WorkerEvent::Open(Err(e)) => self.show_error(e),
            WorkerEvent::CopyDetails(Ok(payload)) => {
                let (path, target, copy_server, bytes) = *payload;
                self.queued_uploads.push_back(QueuedCloudSave::New { path, target, server: copy_server, bytes });
            }
            WorkerEvent::CopyDetails(Err(error)) => self.show_error(error),
        }
        if !upload_failed
            && !self.request_busy
            && let Err(error) = self.start_next_upload()
        {
            self.picker.error = Some(error);
        }
    }

    fn install_download(
        &mut self,
        server: String,
        file: craftcloud_client::File,
        bytes: Vec<u8>,
        editable: bool,
        locked_by: Option<String>,
        lease: Option<FileLeaseHeartbeat>,
    ) -> Result<(), String> {
        let data = self.data_dir.as_deref().ok_or("PhotoCraft data directory is unavailable")?;
        let version = file.head_version_id.ok_or("CraftCloud file has no saved version")?;
        if self.open_path_for_file(&file.id.to_string(), &server).is_some() {
            return Ok(());
        }
        let cache = self.cache.as_ref().ok_or("CraftCloud cache is unavailable")?;
        cache.store(&server, &file.id.to_string(), &version.to_string(), &file.format, &bytes).map_err(|e| e.to_string())?;
        let paths = Paths::under(data);
        let base_path = working_path(&paths, server.as_str(), file.id, version, &file.format);
        let path = create_working_file(&base_path, &bytes)?;
        let path_text = match path.to_str() {
            Some(path) => path.to_owned(),
            None => {
                let _ = fs::remove_file(&path);
                return Err("Cloud working path is not valid Unicode".into());
            }
        };
        let link = craftcloud_client_native::CloudLink {
            local_path: path.clone(),
            server: server.clone(),
            file_id: file.id.to_string(),
            base_version: Some(version.to_string()),
            sha256: file.sha256.unwrap_or_default(),
        };
        let workspace = self.workspace_names.get(&file.workspace_id).cloned();
        self.pending_admissions.insert(path.clone(), PendingAdmission { link, read_only: !editable || locked_by.is_some(), lease, workspace });
        if !self.local_open_requests.iter().any(|pending| Path::new(pending) == path.as_path()) {
            self.local_open_requests.push(path_text);
        }
        if let Some(owner) = locked_by {
            self.picker.locked_by = Some(owner);
            self.picker.dialog = Some(Dialog::Lock);
        }
        Ok(())
    }

    fn show_error(&mut self, error: CloudError) {
        self.picker.error = Some(match error {
            CloudError::Offline => {
                self.picker.dialog = Some(Dialog::Offline);
                tl!("Connection ended after the request may have completed. Check the file before retrying.").into()
            }
            CloudError::Unreachable => {
                self.picker.dialog = Some(Dialog::Offline);
                tl!("CraftCloud server is unreachable. Retry when connected.").into()
            }
            CloudError::TlsHandshake => {
                tl!("The server does not speak HTTPS at this address. Check the address and port, or use http:// for a local test server.").into()
            }
            CloudError::VersionConflict(_) => {
                self.picker.dialog = Some(Dialog::Conflict);
                tl!("This file changed on CraftCloud. Save as a new file or reload the latest version.").into()
            }
            CloudError::FileLocked(owner) => {
                self.picker.dialog = None;
                format!("{} {}", tl!("File is being edited by"), owner.unwrap_or_else(|| tl!("another editor").into()))
            }
            other => format!("CraftCloud: {other:?}"),
        });
    }

    pub fn take_pending_open(&mut self) -> Option<OpenRequest> {
        self.pending_open.take()
    }

    pub fn has_startup_error(&self) -> bool {
        self.startup_error.is_some()
    }
}

#[cfg(test)]
#[path = "cloud_lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "cloud_e2e_tests.rs"]
mod cloud_e2e_tests;

#[cfg(test)]
#[path = "cloud_wizard_tests.rs"]
mod wizard_tests;

#[cfg(test)]
#[path = "cloud_chooser_tests.rs"]
mod chooser_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NoCalls(AtomicUsize);

    impl CloudTransport for NoCalls {
        fn send(&self, _request: craftcloud_client::Request) -> Result<craftcloud_client::Response, CloudError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(craftcloud_client::Response { status: 200, headers: Vec::new(), body: br#"{"items":[],"next_cursor":null}"#.to_vec() })
        }
    }

    fn cloud_file(id: Uuid, version: Uuid) -> craftcloud_client::File {
        craftcloud_client::File {
            id,
            organization_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: "cloud-file".into(),
            app: "photocraft".into(),
            format: "pcraft".into(),
            size_bytes: 0,
            sha256: Some(String::new()),
            head_version_id: Some(version),
            version: 1,
            created_at: String::new(),
            can_share: false,
            can_force_lock: false,
            lock: None,
        }
    }

    #[test]
    fn missing_config_needs_no_filesystem_or_credentials() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
        assert_eq!(Config::load(Some(Path::new("missing-config-root"))).unwrap(), Config::default());
    }

    #[test]
    fn selected_server_must_be_trusted_and_present() {
        let mut config = Config::default();
        assert!(config.selected().is_none());
        config.trust("https://cloud.example".into()).unwrap();
        assert_eq!(config.selected().map(|s| s.url.as_str()), Some("https://cloud.example"));
        config.servers[0].trusted = false;
        assert!(config.selected().is_none());
    }

    #[test]
    fn only_tls_or_loopback_server_urls_are_accepted() {
        for valid in ["https://cloud.example", "http://127.0.0.1:18095", "http://[::1]:18095"] {
            assert!(validate_server_url(valid).is_ok(), "{valid}");
        }
        for invalid in ["http://cloud.example", "https://user:pass@cloud.example", "https://cloud.example/path", "https://cloud.example/?token=x"] {
            assert!(validate_server_url(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn working_and_immutable_cache_roots_are_separate() {
        let paths = Paths::under(Path::new("app-data"));
        assert_ne!(paths.cache, paths.working);
        assert_eq!(paths.keychain_locks, paths.root);
    }

    #[test]
    fn deep_link_requires_valid_cloud_scheme_server_and_file_id() {
        let file = Uuid::new_v4();
        let encoded = url::form_urlencoded::byte_serialize(b"http://127.0.0.1:18095").collect::<String>();
        let raw = format!("craftcloud://open?server={encoded}&file={file}");
        assert_eq!(parse_open_link(&raw), Ok(OpenRequest { server: "http://127.0.0.1:18095".into(), file_id: file }));
        assert!(parse_open_link("https://open?server=https%3A%2F%2Fcloud.example&file=bad").is_err());
        assert!(parse_open_link("craftcloud://open/unexpected?server=https%3A%2F%2Fcloud.example&file=00000000-0000-0000-0000-000000000000").is_err());
    }

    #[test]
    fn readonly_and_malformed_cloud_identities_cannot_become_upload_targets() {
        let link = craftcloud_client_native::CloudLink {
            local_path: PathBuf::from("working.pcraft"),
            server: "https://cloud.example".into(),
            file_id: Uuid::new_v4().to_string(),
            base_version: Some(Uuid::new_v4().to_string()),
            sha256: String::new(),
        };
        assert_eq!(upload_target(&link, true), Err(CloudError::Forbidden));
        assert_eq!(upload_target(&link, false).map(|t| t.server), Ok(link.server.clone()));
        let mut invalid = link;
        invalid.base_version = Some("bad".into());
        assert_eq!(upload_target(&invalid, false), Err(CloudError::InvalidUrl));
    }

    #[test]
    fn cloud_setup_does_not_create_client_when_unconfigured() {
        let runtime = Runtime::new(None);
        assert!(runtime.config.selected().is_none());
        assert!(runtime.pending_open.is_none());
    }

    #[test]
    fn cloud_menu_offers_setup_when_no_server_is_trusted() {
        let app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let items = crate::menus::menu_items(&app);
        let open = items.iter().find(|item| item.id == "file.craftcloud.open");
        let save = items.iter().find(|item| item.id == "file.craftcloud.save");
        assert_eq!(open.map(|item| item.label.as_str()), Some("Set up CraftCloud to open…"));
        assert_eq!(save.map(|item| item.label.as_str()), Some("Set up CraftCloud to save…"));
        assert!(open.is_some_and(|item| item.enabled));
    }

    #[test]
    fn cloud_save_menu_items_follow_save_as_in_order() {
        let app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let items = crate::menus::menu_items(&app);
        let at = |id: &str| items.iter().position(|item| item.id == id).map_or(-1, |position| position as isize);
        let save_as = at("file.saveAs");
        assert_eq!(at("file.craftcloud.save"), save_as + 1);
        assert_eq!(at("file.craftcloud.save_psd"), save_as + 2);
    }

    #[test]
    fn trusted_deep_link_is_staged_without_network_access() {
        let mut runtime = Runtime::new(None);
        runtime.config.trust("https://cloud.example".into()).unwrap();
        let file = Uuid::new_v4();
        let link = format!("craftcloud://open?server=https%3A%2F%2Fcloud.example&file={file}");
        assert_eq!(runtime.open_link(&link), Ok(()));
        assert_eq!(runtime.take_pending_open(), Some(OpenRequest { server: "https://cloud.example".into(), file_id: file }));
    }

    #[test]
    fn browse_after_deep_link_session_loads_organizations_for_open_and_save() {
        let root = tempfile::tempdir().unwrap();
        let server = "http://127.0.0.1:18095";
        let transport = Arc::new(NoCalls(AtomicUsize::new(0)));
        let mut runtime = Runtime::with_test_transport(Some(root.path()), server, transport.clone()).unwrap();
        runtime.pending_open = Some(OpenRequest { server: server.into(), file_id: Uuid::new_v4() });
        runtime.pending_open.take(); // Deep-link download has been handled; later menu actions must hydrate browse.
        runtime.picker.organizations.clear();
        runtime.selected_project = None;
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());

        runtime.begin_open();
        assert!(matches!(runtime.picker.dialog, Some(Dialog::Browse)));
        wait_for_request(&mut runtime, &mut app);
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);

        runtime.begin_save();
        assert!(matches!(runtime.picker.dialog, Some(Dialog::Browse)));
        assert!(runtime.return_to_save);
        wait_for_request(&mut runtime, &mut app);
        assert_eq!(transport.0.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn stale_server_generation_result_cannot_finish_or_mutate_current_request() {
        let mut runtime = Runtime::new(None);
        let stale = runtime.begin_request("https://a.example").unwrap();
        runtime.finish_request();
        let current = runtime.begin_request("https://b.example").unwrap();
        runtime.request_tx.send(WorkerResult { context: stale, event: WorkerEvent::Organizations(Ok(Vec::new())) }).unwrap();
        let mut app = crate::PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());

        runtime.apply_worker_result(&mut app);

        assert_eq!(runtime.active_request, Some(current));
        assert!(runtime.request_busy);
        assert!(runtime.picker.organizations.is_empty());
    }

    #[test]
    fn reopening_after_runtime_restart_preserves_existing_working_file() {
        let root = tempfile::tempdir().unwrap();
        let server = "http://127.0.0.1:18095";
        let transport = Arc::new(NoCalls(AtomicUsize::new(0)));
        let file_id = Uuid::new_v4();
        let version = Uuid::new_v4();
        let file = cloud_file(file_id, version);
        let paths = Paths::under(root.path());
        let existing_path = working_path(&paths, server, file_id, version, "pcraft");

        let mut first = Runtime::with_test_transport(Some(root.path()), server, transport.clone()).unwrap();
        first.install_download(server.into(), file.clone(), b"cloud snapshot".to_vec(), true, None, None).unwrap();
        fs::write(&existing_path, b"locally saved edits").unwrap();

        let mut restarted = Runtime::with_test_transport(Some(root.path()), server, transport).unwrap();
        restarted.install_download(server.into(), file, b"cloud snapshot".to_vec(), true, None, None).unwrap();

        assert_eq!(fs::read(&existing_path).unwrap(), b"locally saved edits");
        let admitted_path = restarted.pending_admissions.keys().next().unwrap();
        assert_ne!(admitted_path, &existing_path);
        assert_eq!(fs::read(admitted_path).unwrap(), b"cloud snapshot");
        assert_eq!(restarted.local_open_requests.len(), 1);
    }

    #[test]
    fn duplicate_pending_open_reuses_admission_without_allocating_another_file() {
        let root = tempfile::tempdir().unwrap();
        let server = "http://127.0.0.1:18095";
        let transport = Arc::new(NoCalls(AtomicUsize::new(0)));
        let mut runtime = Runtime::with_test_transport(Some(root.path()), server, transport).unwrap();
        let file_id = Uuid::new_v4();
        let file = cloud_file(file_id, Uuid::new_v4());

        runtime.install_download(server.into(), file.clone(), b"first bytes".to_vec(), true, None, None).unwrap();
        let admitted_path = runtime.pending_admissions.keys().next().unwrap().clone();
        runtime.install_download(server.into(), file, b"duplicate bytes".to_vec(), true, None, None).unwrap();

        assert_eq!(runtime.pending_admissions.len(), 1);
        assert_eq!(runtime.local_open_requests.len(), 1);
        assert_eq!(fs::read(admitted_path).unwrap(), b"first bytes");
    }

    #[test]
    fn duplicate_deep_link_while_admission_pending_keeps_original_lease() {
        let root = tempfile::tempdir().unwrap();
        let server = "http://127.0.0.1:18095";
        let transport = Arc::new(NoCalls(AtomicUsize::new(0)));
        let mut runtime = Runtime::with_test_transport(Some(root.path()), server, transport.clone()).unwrap();
        let file_id = Uuid::new_v4();
        let path = root.path().join("pending.pcraft");
        let released = Arc::new(AtomicUsize::new(0));
        runtime.queue_test_admission_with_lease(
            path.to_str().unwrap(),
            CloudLink {
                local_path: path.clone(),
                server: server.into(),
                file_id: file_id.to_string(),
                base_version: Some(Uuid::new_v4().to_string()),
                sha256: String::new(),
            },
            released.clone(),
        );
        let encoded = url::form_urlencoded::byte_serialize(server.as_bytes()).collect::<String>();
        let deep_link = format!("craftcloud://open?server={encoded}&file={file_id}");

        runtime.open_link(&deep_link).unwrap();
        let client = runtime.client.as_ref().unwrap().clone();
        runtime.download_linked_file(client.clone(), file_id, true);
        runtime.download_file(client, cloud_file(file_id, Uuid::new_v4()), true);

        assert!(runtime.has_pending_admission(path.to_str().unwrap()));
        assert!(runtime.pending_open.is_none(), "duplicate link must not queue a second open request");
        assert_eq!(released.load(Ordering::SeqCst), 0, "duplicate open must keep original lease");
        assert_eq!(transport.0.load(Ordering::SeqCst), 0, "duplicate open must not fetch file or acquire another lease");
        assert!(!runtime.request_busy);

        runtime.cancel_open_file(path.to_str().unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while released.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(released.load(Ordering::SeqCst), 1, "cancel should release the held lease exactly once");
    }

    fn wait_for_request(runtime: &mut Runtime, app: &mut crate::PhotocraftApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.request_busy && std::time::Instant::now() < deadline {
            runtime.apply_worker_result(app);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!runtime.request_busy, "organization list request did not complete");
        runtime.apply_worker_result(app);
    }

    #[test]
    fn release_timeout_never_attempts_a_new_lock() {
        let root = tempfile::tempdir().unwrap();
        let transport = Arc::new(NoCalls(AtomicUsize::new(0)));
        let runtime = Runtime::with_test_transport(Some(root.path()), "http://127.0.0.1:18095", transport.clone()).unwrap();
        let client = runtime.client.as_ref().unwrap().clone();
        let file_id = Uuid::new_v4();
        let (release_started_tx, release_started_rx) = mpsc::channel();
        let (finish_release_tx, finish_release_rx) = mpsc::channel();
        let lease = FileLeaseHeartbeat::start_keyed(
            file_id.to_string(),
            || {},
            move || {
                let _ = release_started_tx.send(());
                let _ = finish_release_rx.recv();
            },
        );
        let dropping = std::thread::spawn(move || drop(lease));
        release_started_rx.recv_timeout(std::time::Duration::from_secs(2)).expect("release started");

        let result = acquire_file_lease(&client, file_id, std::time::Duration::from_millis(25)).unwrap();
        assert!(matches!(result, LeaseAcquire::Locked(_)));
        assert_eq!(transport.0.load(Ordering::SeqCst), 0, "timeout must not call acquire_lock");

        let _ = finish_release_tx.send(());
        dropping.join().unwrap();
    }

    #[test]
    fn reopening_a_linked_file_focuses_the_existing_document_without_a_second_request() {
        let root = tempfile::tempdir().unwrap();
        let server = "http://127.0.0.1:18095";
        let transport = Arc::new(NoCalls(AtomicUsize::new(0)));
        let mut runtime = Runtime::with_test_transport(Some(root.path()), server, transport.clone()).unwrap();
        let path = root.path().join("working.pcraft");
        fs::write(&path, b"existing document").unwrap();
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", serde_json::json!({"width": 8, "height": 8})).unwrap();
        let doc = session.active().unwrap().doc.id;
        session.active_mut().unwrap().path = Some(path.to_string_lossy().into_owned());
        let mut app = crate::PhotocraftApp::new(session, crate::Services::default());
        let file_id = Uuid::new_v4();
        let file = craftcloud_client::File {
            id: file_id,
            organization_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: "existing".into(),
            app: "photocraft".into(),
            format: "pcraft".into(),
            size_bytes: 0,
            sha256: Some(String::new()),
            head_version_id: Some(Uuid::new_v4()),
            version: 1,
            created_at: String::new(),
            can_share: false,
            can_force_lock: false,
            lock: None,
        };
        runtime.files.insert(file_id, file);
        runtime
            .bind_test_document(
                doc,
                CloudLink { local_path: path.clone(), server: server.into(), file_id: file_id.to_string(), base_version: None, sha256: String::new() },
            )
            .unwrap();

        runtime.handle_action(&mut app, Action::OpenFile(file_id.to_string()));
        assert_eq!(app.session.active().unwrap().doc.id, doc);
        assert_eq!(transport.0.load(Ordering::SeqCst), 0, "deduplicated open must not acquire a second lease");
    }
}
