//! The CraftCloud Save and Open dialogs.
//!
//! Both are one modal in PhotoCraft's own style, modelled on the cloud documents dialogs of
//! Creative Cloud apps: a centred title, a location area that walks organization, workspace and
//! project as rows, and a footer with "This computer" on the left and the answer on the right.
//! The dialog only draws and reports [`Event`]s; [`Runtime`] owns the requests and the lists, so
//! the sign-in, listing, project creation and upload logic is the one the SDK picker used.

use craftcloud_egui::{Action, Level, PickerState};
use egui::{Align, Align2, Key, Layout, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};
use photocraft_doc::DocId;
use uuid::Uuid;

use super::{Runtime, SAVE_FORMATS, document_stem, save_chooser, strip_format_extension};
use crate::FormatChoices;
use crate::theme::Tokens;

/// Height of one list row.
const ROW_H: f32 = 44.0;

/// What the dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Save,
    Open,
}

/// The dialog while it is open. The lists it shows live in the [`Runtime`].
pub(crate) struct State {
    pub(crate) mode: Mode,
    /// The document being saved; `None` takes the active one when the dialog first draws.
    pub(crate) doc: Option<DocId>,
    choices: FormatChoices,
    plain_save: bool,
    /// File name handed over by the caller, with or without extension.
    suggested: Option<String>,
    /// The name field: the file name without its extension.
    pub(crate) name: String,
    /// The format dropdown is on Photoshop.
    pub(crate) psd: bool,
    named: bool,
    focus_name: bool,
}

impl State {
    fn save(doc: Option<DocId>, suggested: Option<String>, choices: FormatChoices, plain_save: bool) -> Self {
        Self { mode: Mode::Save, doc, choices, plain_save, suggested, name: String::new(), psd: false, named: false, focus_name: true }
    }

    fn open() -> Self {
        Self {
            mode: Mode::Open,
            doc: None,
            choices: FormatChoices::default(),
            plain_save: false,
            suggested: None,
            name: String::new(),
            psd: false,
            named: true,
            focus_name: false,
        }
    }
}

/// What the location area shows right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum View {
    /// No trusted server: the settings are the way on.
    NoServer,
    /// A server, but nobody signed in.
    SignIn,
    Organizations,
    Workspaces,
    Projects,
    /// The files of the selected project.
    Files,
    /// The files shared with the user (Open only).
    Shared,
}

/// What the user did this frame.
enum Event {
    Back(Level),
    Refresh,
    SelectOrganization(Uuid),
    SelectWorkspace(Uuid),
    SelectProject(Uuid),
    OpenShared,
    SignIn(Action),
    CreateProject(String),
    Save(String),
    SaveVersion(Uuid),
    Open(Uuid),
    Computer,
    Cancel,
    OpenSettings,
}

/// Everything one frame of the dialog reads besides its own [`State`].
struct Frame<'a> {
    view: View,
    busy: bool,
    title: String,
    parent: Option<Level>,
    server_label: String,
    picker: &'a mut PickerState,
    files: Vec<&'a craftcloud_client::File>,
}

/// A file name that can be a CraftCloud file name: not empty, no path or reserved characters.
pub(crate) fn name_is_valid(stem: &str) -> bool {
    let stem = stem.trim();
    !stem.is_empty() && stem.chars().count() <= 200 && !stem.trim_matches('.').is_empty() && !stem.chars().any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
}

/// `2026-11-02T10:11:12Z` as `2026-11-02`; anything shorter stays as it is.
fn short_date(timestamp: &str) -> &str {
    timestamp.get(..10).unwrap_or(timestamp)
}

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

impl Runtime {
    /// Opens the Cloud Save dialog for `doc`. `file_name` pre-fills the name (the document's own
    /// name without it); "This computer" hands `choices` and `plain_save` back to the native Save As.
    /// Returns whether it opened: a dialog or prompt that is already up stays, and the caller says so.
    pub(crate) fn show_cloud_save(&mut self, doc: DocId, file_name: Option<String>, choices: FormatChoices, plain_save: bool) -> bool {
        self.save_as_psd = false;
        self.open_cloud_dialog(State::save(Some(doc), file_name, choices, plain_save))
    }

    /// Opens the Cloud Open dialog.
    pub(crate) fn show_cloud_open(&mut self) -> bool {
        self.save_as_psd = false;
        self.open_cloud_dialog(State::open())
    }

    /// File › Save to CraftCloud…: the active document, native format.
    pub fn begin_save(&mut self) {
        self.save_as_psd = false;
        self.open_cloud_dialog(State::save(None, None, FormatChoices::default(), false));
    }

    /// File › Save as PSD in CraftCloud….
    pub fn begin_save_psd(&mut self) {
        self.save_as_psd = true;
        self.open_cloud_dialog(State::save(None, None, FormatChoices::default(), false));
    }

    /// A second request while a dialog or prompt is up keeps what is on screen.
    fn open_cloud_dialog(&mut self, mut state: State) -> bool {
        state.psd = self.save_as_psd;
        if self.cloud_dialog.is_some() || self.save_chooser.is_some() {
            return false;
        }
        if matches!(self.picker.dialog, Some(craftcloud_egui::Dialog::Server | craftcloud_egui::Dialog::Login)) {
            self.picker.dialog = None;
        }
        if self.picker.dialog.is_some() {
            return false;
        }
        // Shared files are an Open-only list; a dialog never opens inside it.
        if std::mem::take(&mut self.browsing_shared) {
            self.files.clear();
            self.picker.files.clear();
        }
        self.dialog_upload = None;
        self.picker.error = None;
        self.picker.selected_file = None;
        self.picker.show_new_project = false;
        self.picker.new_project_name.clear();
        if let Some(server) = self.config.selected() {
            self.picker.server_url = server.url.clone();
            if let Some(account) = &server.account {
                self.picker.email = account.clone();
            }
        }
        self.cloud_dialog = Some(state);
        self.prepare_browse();
        true
    }

    /// Gets the lists the dialog needs: signs in again from the keychain, loads the organizations,
    /// or reloads the files of the remembered project.
    fn prepare_browse(&mut self) {
        let Some(server) = self.config.selected() else { return };
        if self.client.is_none() {
            if server.account.is_some() {
                self.restore_session();
            }
            return;
        }
        if self.picker.organizations.is_empty() {
            self.load_organizations();
            return;
        }
        if self.files.is_empty()
            && !self.browsing_shared
            && let Some(project) = self.selected_project
        {
            self.select_project(project);
        }
    }

    /// The level the location area shows.
    pub(crate) fn dialog_view(&self) -> View {
        if self.config.selected().is_none() {
            View::NoServer
        } else if self.client.is_none() || self.client_server.as_deref() != self.config.selected().map(|server| server.url.as_str()) {
            View::SignIn
        } else if self.selected_org.is_none() {
            View::Organizations
        } else if self.browsing_shared {
            View::Shared
        } else if self.selected_workspace.is_none() {
            View::Workspaces
        } else if self.selected_project.is_none() {
            View::Projects
        } else {
            View::Files
        }
    }

    /// The list Back returns to. Organization and workspace lists with a single entry are skipped,
    /// as they are when the dialog opens.
    fn parent_level(&self, view: View) -> Option<Level> {
        let skip_workspaces = self.picker.workspaces.len() == 1;
        let skip_organizations = self.picker.organizations.len() == 1;
        match view {
            View::Files => Some(Level::Projects),
            View::Shared => Some(Level::Workspaces),
            View::Projects if !skip_workspaces => Some(Level::Workspaces),
            View::Projects | View::Workspaces if !skip_organizations => Some(Level::Organizations),
            _ => None,
        }
    }

    /// Reloads the list the dialog shows.
    fn refresh_view(&mut self) {
        if self.request_busy {
            return;
        }
        // A deliberate reload stays where the user is.
        self.preferred = super::LastSelection::default();
        match self.dialog_view() {
            View::Organizations => {
                self.picker.organizations.clear();
                self.load_organizations();
            }
            View::Workspaces => {
                if let Some(org) = self.selected_org {
                    self.select_organization(org);
                }
            }
            View::Projects => {
                if let Some(workspace) = self.selected_workspace {
                    self.select_workspace(workspace);
                }
            }
            View::Files => {
                if let Some(project) = self.selected_project {
                    self.select_project(project);
                }
            }
            View::Shared => self.list_shared(),
            View::NoServer | View::SignIn => {}
        }
    }

    /// Draws the dialog and carries out what the user did.
    pub(super) fn show_cloud_dialog(&mut self, app: &mut crate::PhotocraftApp, ctx: &egui::Context) {
        let Some(mut state) = self.cloud_dialog.take() else { return };
        if !state.named {
            state.named = true;
            state.doc = state.doc.or_else(|| app.session.active().map(|st| st.doc.id));
            let from_document = state.doc.and_then(|doc| app.session.documents().iter().find(|st| st.doc.id == doc)).map(|st| st.doc.name.clone());
            let name = state.suggested.take().or(from_document).unwrap_or_default();
            let name = std::path::Path::new(&name).file_name().and_then(|file| file.to_str()).unwrap_or(&name).to_string();
            let stem = document_stem(&name);
            state.name = if stem.trim().is_empty() { "Untitled".into() } else { stem.to_string() };
        }
        if let Some(server) = self.config.selected() {
            self.picker.server_url = server.url.clone();
        }
        let view = self.dialog_view();
        let mut files: Vec<_> = if matches!(view, View::Files | View::Shared) { self.files.values().collect() } else { Vec::new() };
        files.sort_by_key(|file| file.name.to_lowercase());
        let title_of = |items: &[(String, String)], id: Option<Uuid>| {
            id.and_then(|id| items.iter().find(|(item, _)| *item == id.to_string())).map_or_else(|| "…".to_string(), |(_, name)| name.clone())
        };
        let server = self.config.selected();
        let host = server.and_then(|server| url::Url::parse(&server.url).ok()).and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_default();
        let server_label = match server.and_then(|server| server.account.as_deref()) {
            Some(account) if view != View::SignIn => format!("{account} · {host}"),
            _ => host,
        };
        let title = match view {
            View::NoServer | View::SignIn | View::Organizations => server_label.clone(),
            View::Workspaces => title_of(&self.picker.organizations, self.selected_org),
            View::Projects => title_of(&self.picker.workspaces, self.selected_workspace),
            View::Files => title_of(&self.picker.projects, self.selected_project),
            View::Shared => tl!("Shared with me").to_string(),
        };
        let mut events = Vec::new();
        let mut frame = Frame {
            view,
            busy: self.request_busy,
            title,
            parent: self.parent_level(view),
            server_label,
            picker: &mut self.picker,
            files,
        };
        draw(ctx, &mut state, &mut frame, &mut events);
        if state.mode == Mode::Save {
            self.save_as_psd = state.psd;
        }
        self.cloud_dialog = Some(state);
        for event in events {
            self.apply_dialog_event(app, ctx, event);
        }
    }

    fn apply_dialog_event(&mut self, app: &mut crate::PhotocraftApp, ctx: &egui::Context, event: Event) {
        match event {
            Event::Back(level) => self.go_to(level),
            Event::Refresh => self.refresh_view(),
            Event::SelectOrganization(id) => self.select_organization(id),
            Event::SelectWorkspace(id) => self.select_workspace(id),
            Event::SelectProject(id) => self.select_project(id),
            Event::OpenShared => self.list_shared(),
            Event::SignIn(action) => {
                if let Action::Login { server, email, password, second_factor } = action {
                    self.login(server, email, password, second_factor);
                }
            }
            Event::CreateProject(name) => self.create_project(name),
            Event::Save(name) => self.handle_action(app, Action::SaveNew { name }),
            Event::SaveVersion(id) => self.handle_action(app, Action::SaveVersion { file_id: id.to_string() }),
            Event::Open(id) => self.handle_action(app, Action::OpenFile(id.to_string())),
            Event::Computer => {
                let Some(state) = self.cloud_dialog.take() else { return };
                match state.mode {
                    Mode::Save => {
                        if let Some(doc) = state.doc.or_else(|| app.session.active().map(|st| st.doc.id)) {
                            self.local_save = Some(save_chooser::Chooser::new(doc, state.choices, state.plain_save));
                        }
                    }
                    Mode::Open => self.local_open = true,
                }
                self.save_as_psd = false;
                self.picker.error = None;
            }
            Event::Cancel => self.handle_action(app, Action::Cancel),
            Event::OpenSettings => {
                self.cloud_dialog = None;
                crate::prefs_ui::open_preferences(app, "craftcloud");
            }
        }
        // The dialog's own repaint after an answer, so lists that just arrived show at once.
        ctx.request_repaint();
    }

    /// The Open dialog's "This computer": the native Open dialog comes up.
    pub(crate) fn take_local_open(&mut self) -> bool {
        std::mem::take(&mut self.local_open)
    }

    #[cfg(test)]
    pub(crate) fn cloud_dialog_mode(&self) -> Option<Mode> {
        self.cloud_dialog.as_ref().map(|state| state.mode)
    }

    #[cfg(test)]
    pub(crate) fn cloud_dialog_name(&self) -> Option<&str> {
        self.cloud_dialog.as_ref().map(|state| state.name.as_str())
    }
}

/// Draws the modal for one frame.
fn draw(ctx: &egui::Context, state: &mut State, frame: &mut Frame, events: &mut Vec<Event>) {
    let screen = ctx.content_rect();
    let width = (screen.width() - 80.0).clamp(480.0, 800.0);
    let height = (screen.height() - 80.0).clamp(380.0, 580.0);
    let first_frame = std::mem::take(&mut state.focus_name);
    let save = state.mode == Mode::Save;
    egui::Modal::new(egui::Id::unique("craftcloud-dialog")).show(ctx, |ui| {
        ui.set_width(width);
        title_bar(ui, state.mode, events);
        ui.add_space(6.0);
        crate::widgets::hairline(ui);
        ui.add_space(10.0);
        if save {
            name_row(ui, state, frame, first_frame);
            ui.add_space(10.0);
        }
        let list_height = (height - if save { 250.0 } else { 208.0 }).max(110.0);
        location(ui, state, frame, list_height, events);
        ui.add_space(8.0);
        status_line(ui, state, frame);
        footer(ui, state, frame, events);
        keys(ctx, state, frame, events);
    });
}

fn title_bar(ui: &mut Ui, mode: Mode, events: &mut Vec<Event>) {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let title = match mode {
        Mode::Save => tl!("Save to CraftCloud"),
        Mode::Open => tl!("Open from CraftCloud"),
    };
    // Painted text: name the title for accessibility (and so tests and agents can find it).
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Heading, true, title));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, title, crate::theme::semibold(15.0), t.text);
    let close = Rect::from_min_size(pos2(rect.right() - 28.0, rect.top()), vec2(28.0, 28.0));
    let clicked = ui.scope_builder(UiBuilder::new().max_rect(close), |ui| crate::icons::button(ui, "x", 28.0, false, tl!("Close")).clicked()).inner;
    if clicked {
        events.push(Event::Cancel);
    }
}

/// "Save as", the name with its validity mark, and the format.
fn name_row(ui: &mut Ui, state: &mut State, frame: &Frame, first_frame: bool) {
    let t = Tokens::get(ui.ctx());
    ui.add_enabled_ui(!frame.busy, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tl!("Save as")).font(crate::theme::medium(13.0)).color(t.text));
            let field_width = (ui.available_width() - 262.0).max(120.0);
            let response = crate::widgets::text_field(ui, egui::Id::unique("craftcloud-dialog-name"), &mut state.name, tl!("File name"), field_width, false).accessible_name(tl!("File name"));
            if first_frame {
                response.request_focus();
            }
            let (mark, _) = ui.allocate_exact_size(vec2(18.0, 24.0), Sense::hover());
            if name_is_valid(&state.name) {
                crate::icons::paint(ui, mark, "check", 15.0, t.accent_text);
            } else {
                crate::icons::paint(ui, mark, "x", 15.0, t.danger);
            }
            ui.add_space(6.0);
            let mut psd = state.psd;
            crate::widgets::dropdown_literal(ui, "craftcloud-format", tl!("Format"), &mut psd, &[(false, SAVE_FORMATS[0].1), (true, SAVE_FORMATS[1].1)], 190.0);
            state.psd = psd;
        });
    });
}

/// The framed location area: header row, then the rows of the current level.
fn location(ui: &mut Ui, state: &mut State, frame: &mut Frame, list_height: f32, events: &mut Vec<Event>) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::new().stroke(Stroke::new(1.0, t.card_border)).corner_radius(t.radius).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(list_height + 38.0);
        match frame.view {
            View::NoServer => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(tl!("No CraftCloud server is set up.")).color(t.text_dim));
                    ui.add_space(10.0);
                    if crate::widgets::secondary_button(ui, tl!("Open CraftCloud settings…"), 0.0).clicked() {
                        events.push(Event::OpenSettings);
                    }
                });
            }
            View::SignIn => sign_in_panel(ui, frame, events),
            _ => browser(ui, state, frame, list_height, events),
        }
    });
}

fn sign_in_panel(ui: &mut Ui, frame: &mut Frame, events: &mut Vec<Event>) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::fmt(tl!("Sign in to {server}"), &[("server", &frame.server_label)])).font(crate::theme::semibold(14.0)).color(t.text));
    ui.add_space(8.0);
    if frame.busy {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0).color(t.text_dim));
            ui.label(RichText::new(tl!("Loading…")).color(t.text_dim));
        });
    } else if let Some(action) = sign_in_form(ui, frame.picker, false) {
        events.push(Event::SignIn(action));
    }
    ui.add_space(10.0);
    if ui.link(tl!("Open CraftCloud settings…")).clicked() {
        events.push(Event::OpenSettings);
    }
}

fn browser(ui: &mut Ui, state: &mut State, frame: &mut Frame, list_height: f32, events: &mut Vec<Event>) {
    let t = Tokens::get(ui.ctx());
    let busy = frame.busy;
    ui.horizontal(|ui| {
        ui.set_height(30.0);
        if let Some(level) = frame.parent {
            ui.add_enabled_ui(!busy, |ui| {
                if crate::icons::button(ui, "chevron-left", 28.0, false, tl!("Back")).clicked() {
                    events.push(Event::Back(level));
                }
            });
        }
        let title_width = (ui.available_width() - 120.0).max(60.0);
        crate::widgets::elided_label(ui, &frame.title, title_width);
        if busy {
            ui.add(egui::Spinner::new().size(14.0).color(t.text_dim));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if crate::icons::button(ui, "rotate-cw", 28.0, false, tl!("Refresh")).clicked() {
                    events.push(Event::Refresh);
                }
                if state.mode == Mode::Save
                    && frame.view == View::Projects
                    && crate::icons::button(ui, "folder-plus", 28.0, false, tl!("New project")).clicked()
                {
                    frame.picker.show_new_project = !frame.picker.show_new_project;
                }
            });
        });
    });
    ui.add_space(4.0);
    if state.mode == Mode::Save && frame.view == View::Projects && frame.picker.show_new_project {
        new_project_form(ui, frame, events);
        ui.add_space(6.0);
    }
    egui::ScrollArea::vertical().id_salt("craftcloud-dialog-list").auto_shrink([false, false]).max_height(list_height).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.add_enabled_ui(!busy, |ui| rows(ui, state, frame, events));
    });
}

fn new_project_form(ui: &mut Ui, frame: &mut Frame, events: &mut Vec<Event>) {
    let busy = frame.busy;
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 150.0).max(120.0);
        let id = egui::Id::unique("craftcloud-dialog-project");
        let response = crate::widgets::text_field(ui, id, &mut frame.picker.new_project_name, tl!("Project name"), width, false).accessible_name(tl!("Project name"));
        let name = frame.picker.new_project_name.trim().to_string();
        let ready = !name.is_empty() && !busy;
        let entered = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
        let clicked = ui.add_enabled_ui(ready, |ui| crate::widgets::primary_button(ui, tl!("Create project"), 0.0).clicked()).inner;
        if ready && (clicked || entered) {
            events.push(Event::CreateProject(name));
        }
    });
}

fn empty_state(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.vertical_centered(|ui| {
        ui.add_space(36.0);
        ui.label(RichText::new(text).color(t.text_dim));
    });
}

/// The rows of the current level.
fn rows(ui: &mut Ui, state: &mut State, frame: &mut Frame, events: &mut Vec<Event>) {
    let open = state.mode == Mode::Open;
    let busy = frame.busy;
    match frame.view {
        View::Organizations => {
            if frame.picker.organizations.is_empty() && !busy {
                empty_state(ui, tl!("No organizations available."));
            }
            for (id, name) in &frame.picker.organizations {
                if row(ui, "cloud", name, None, None, true, false).clicked()
                    && let Ok(id) = Uuid::parse_str(id)
                {
                    events.push(Event::SelectOrganization(id));
                }
            }
        }
        View::Workspaces => {
            if open && row(ui, "cloud", tl!("Shared with me"), None, None, true, false).clicked() {
                events.push(Event::OpenShared);
            }
            if frame.picker.workspaces.is_empty() && !busy {
                empty_state(ui, tl!("This organization has no workspaces."));
            }
            for (id, name) in &frame.picker.workspaces {
                if row(ui, "folder", name, None, None, true, false).clicked()
                    && let Ok(id) = Uuid::parse_str(id)
                {
                    events.push(Event::SelectWorkspace(id));
                }
            }
        }
        View::Projects => {
            if frame.picker.projects.is_empty() && !busy {
                if open {
                    empty_state(ui, tl!("This workspace has no projects yet."));
                } else {
                    empty_state(ui, tl!("This workspace has no projects yet. Create one to save into it."));
                    ui.vertical_centered(|ui| {
                        if ui.link(tl!("Create project")).clicked() {
                            frame.picker.show_new_project = true;
                        }
                    });
                }
            }
            for (id, name) in &frame.picker.projects {
                if row(ui, "folder", name, None, None, true, false).clicked()
                    && let Ok(id) = Uuid::parse_str(id)
                {
                    events.push(Event::SelectProject(id));
                }
            }
        }
        View::Files | View::Shared => {
            if frame.files.is_empty() && !busy {
                empty_state(ui, if frame.view == View::Shared { tl!("Nothing has been shared with you.") } else { tl!("This project has no files yet.") });
            }
            for file in &frame.files {
                let selected = frame.picker.selected_file.as_deref() == Some(file.id.to_string().as_str());
                let mut detail = short_date(&file.created_at).to_string();
                if open && file.size_bytes > 0 {
                    detail = format!("{detail} · {}", size_text(file.size_bytes));
                }
                let badge = file
                    .lock
                    .as_ref()
                    .filter(|lock| !lock.mine && !lock.held_by_current_session)
                    .map(|lock| format!("{} {}", tl!("Being edited by"), lock.holder_display_name));
                let response = row(ui, "file", &file.name, Some(&detail), badge.as_deref(), false, selected);
                if response.clicked() {
                    if selected && !open {
                        frame.picker.selected_file = None;
                    } else {
                        frame.picker.selected_file = Some(file.id.to_string());
                        if !open {
                            state.name = document_stem(&file.name).to_string();
                        }
                    }
                }
                if open && response.double_clicked() {
                    events.push(Event::Open(file.id));
                }
            }
        }
        View::NoServer | View::SignIn => {}
    }
}

/// One full-width rounded row: icon, name, optional details on the right, optional chevron.
fn row(ui: &mut Ui, icon: &str, title: &str, detail: Option<&str>, badge: Option<&str>, chevron: bool, selected: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), title));
    let hovered = response.hovered() && ui.is_enabled();
    let fill = if selected {
        t.row_selected
    } else if hovered {
        t.hover
    } else {
        t.field
    };
    let outline = if selected { t.accent_border } else { t.field_border };
    ui.painter().rect_filled(rect, t.radius, fill);
    ui.painter().rect_stroke(rect, t.radius, Stroke::new(1.0, outline), StrokeKind::Inside);
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(1.0), t.radius, Stroke::new(2.0, t.accent), StrokeKind::Outside);
    }
    crate::icons::paint(ui, Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), vec2(20.0, 20.0)), icon, 18.0, t.icon);
    let mut right = rect.right() - 14.0;
    if chevron {
        crate::icons::paint(ui, Rect::from_center_size(pos2(rect.right() - 22.0, rect.center().y), vec2(20.0, 20.0)), "chevron-right", 16.0, t.text_dim);
        right -= 24.0;
    }
    for (text, color) in [(detail, t.text_dim), (badge, t.warning)].into_iter().filter_map(|(text, color)| text.map(|text| (text, color))) {
        let galley = ui.painter().layout_no_wrap(text.to_owned(), crate::theme::medium(12.0), color);
        let size = galley.size();
        ui.painter().galley(pos2(right - size.x, rect.center().y - size.y / 2.0), galley, color);
        right -= size.x + 14.0;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(title.to_owned(), crate::theme::medium(13.5), t.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - rect.left() - 50.0).max(40.0));
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(pos2(rect.left() + 46.0, rect.center().y - galley.size().y / 2.0), galley, t.text);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The error of the last request, or which file the Save writes a new version of.
fn status_line(ui: &mut Ui, state: &State, frame: &Frame) {
    let t = Tokens::get(ui.ctx());
    if let Some(error) = &frame.picker.error {
        ui.add(egui::Label::new(RichText::new(error).color(t.danger).size(12.5)).wrap());
    } else if state.mode == Mode::Save
        && let Some(id) = frame.picker.selected_file.as_deref()
        && let Some(file) = frame.files.iter().find(|file| file.id.to_string() == id)
    {
        let text = crate::i18n::fmt(tl!("Saves a new version of {name}"), &[("name", &file.name)]);
        ui.add(egui::Label::new(RichText::new(text).color(t.text_dim).size(12.5)).wrap());
    } else {
        ui.add_space(4.0);
    }
    ui.add_space(6.0);
}

/// The event the primary button (and Enter) would cause, if it is available.
fn primary_event(state: &State, frame: &Frame) -> Option<Event> {
    if frame.busy {
        return None;
    }
    match state.mode {
        Mode::Save => {
            if frame.view != View::Files {
                return None;
            }
            if let Some(id) = frame.picker.selected_file.as_deref().and_then(|id| Uuid::parse_str(id).ok()) {
                return Some(Event::SaveVersion(id));
            }
            let name = state.name.trim();
            name_is_valid(name).then(|| Event::Save(strip_format_extension(name).to_string()))
        }
        Mode::Open => {
            if !matches!(frame.view, View::Files | View::Shared) {
                return None;
            }
            frame.picker.selected_file.as_deref().and_then(|id| Uuid::parse_str(id).ok()).map(Event::Open)
        }
    }
}

fn footer(ui: &mut Ui, state: &State, frame: &Frame, events: &mut Vec<Event>) {
    let primary = primary_event(state, frame);
    let can = primary.is_some();
    let busy = frame.busy;
    ui.horizontal(|ui| {
        if ui.add_enabled_ui(!busy, computer_button).inner.clicked() {
            events.push(Event::Computer);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if state.mode == Mode::Save { tl!("Save") } else { tl!("Open") };
            let buttons = [
                crate::widgets::DialogButton::new(crate::widgets::ButtonRole::Default, label, 96.0).enabled(can),
                crate::widgets::DialogButton::new(crate::widgets::ButtonRole::Cancel, tl!("Cancel"), 96.0),
            ];
            match crate::widgets::dialog_buttons(ui, &buttons) {
                Some(crate::widgets::ButtonRole::Default) => events.extend(primary),
                Some(crate::widgets::ButtonRole::Cancel) => events.push(Event::Cancel),
                _ => {}
            }
        });
    });
}

/// The secondary "This computer" button with the monitor icon.
fn computer_button(ui: &mut Ui) -> Response {
    let t = Tokens::get(ui.ctx());
    let label = tl!("This computer");
    let galley = ui.painter().layout_no_wrap(label.to_owned(), crate::theme::medium(13.0), t.text);
    let height = crate::widgets::button_height(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 28.0 + 26.0, height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    let hovered = response.hovered() && ui.is_enabled();
    let radius: f32 = if t.pro { height / 2.0 } else { t.radius_sm };
    if hovered || response.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, radius, t.hover);
    } else if !t.pro {
        ui.painter().rect_filled(rect, radius, t.field);
    }
    let stroke = if t.pro { Stroke::new(1.5, if hovered { t.text } else { t.text_dim }) } else { Stroke::new(1.0, t.field_border) };
    ui.painter().rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
    }
    crate::icons::paint(ui, Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(18.0, 18.0)), "monitor", 16.0, t.text);
    ui.painter().galley(pos2(rect.left() + 38.0, rect.center().y - galley.size().y / 2.0), galley, t.text);
    response
}

/// Enter saves or opens, Backspace and Alt+Left go one level up (not while typing).
fn keys(ctx: &egui::Context, state: &State, frame: &Frame, events: &mut Vec<Event>) {
    let focused = ctx.memory(|memory| memory.focused()).is_some();
    let (enter, back) = ctx.input(|input| (input.key_pressed(Key::Enter), input.key_pressed(Key::Backspace) || (input.modifiers.alt && input.key_pressed(Key::ArrowLeft))));
    if enter && !focused {
        events.extend(primary_event(state, frame));
    }
    if back
        && !ctx.egui_wants_keyboard_input()
        && !frame.busy
        && let Some(level) = frame.parent
    {
        events.push(Event::Back(level));
    }
}

/// The sign-in form of the CraftCloud dialogs and of Preferences › CraftCloud: email and password,
/// then the second factor when the server asks for it. Returns the login to run.
pub(super) fn sign_in_form(ui: &mut Ui, picker: &mut PickerState, busy: bool) -> Option<Action> {
    let t = Tokens::get(ui.ctx());
    let id = ui.scope_id().with("cloud-sign-in");
    let width = ui.available_width().clamp(160.0, 340.0);
    let mut action = None;
    let caption = |ui: &mut Ui, text: &str| {
        ui.label(RichText::new(text).font(crate::theme::medium(12.0)).color(t.text_dim));
    };
    ui.add_enabled_ui(!busy, |ui| {
        if picker.requires_second_factor {
            caption(ui, tl!("Email"));
            ui.label(RichText::new(picker.email.as_str()).color(t.text));
            ui.add_space(4.0);
            caption(ui, tl!("Second factor"));
            let response = crate::widgets::password_field(ui, id.with("second"), &mut picker.second_factor, "", width).accessible_name(tl!("Second factor"));
            ui.label(RichText::new(tl!("Six-digit authenticator code or ten-character recovery code")).color(t.text_faint).size(11.5));
            ui.add_space(6.0);
            let mut submit = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                submit |= crate::widgets::primary_button(ui, tl!("Sign in"), 90.0).clicked();
                if crate::widgets::secondary_button(ui, tl!("Cancel"), 90.0).clicked() {
                    // Back to the password step; the open challenge is abandoned.
                    picker.requires_second_factor = false;
                    picker.second_factor_challenge_id = None;
                    picker.second_factor.clear();
                    picker.error = None;
                    submit = false;
                }
            });
            if submit {
                match craftcloud_egui::classify_second_factor(&picker.second_factor) {
                    Some(craftcloud_egui::SecondFactorInput::Totp(code) | craftcloud_egui::SecondFactorInput::Recovery(code)) => {
                        picker.error = None;
                        action = Some(Action::Login { server: picker.server_url.clone(), email: picker.email.clone(), password: String::new(), second_factor: Some(code) });
                    }
                    None => picker.error = Some(tl!("Six-digit authenticator code or ten-character recovery code").into()),
                }
                picker.second_factor.clear();
            }
        } else {
            caption(ui, tl!("Email"));
            crate::widgets::text_field(ui, id.with("email"), &mut picker.email, "", width, false).accessible_name(tl!("Email"));
            ui.add_space(4.0);
            caption(ui, tl!("Password"));
            let password = crate::widgets::password_field(ui, id.with("password"), &mut picker.password, "", width).accessible_name(tl!("Password"));
            ui.add_space(6.0);
            let entered = password.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            let ready = !picker.email.trim().is_empty() && !picker.server_url.is_empty();
            let clicked = ui.add_enabled_ui(ready, |ui| crate::widgets::primary_button(ui, tl!("Sign in"), 90.0).clicked()).inner;
            if ready && (clicked || entered) {
                picker.error = None;
                action = Some(Action::Login { server: picker.server_url.clone(), email: picker.email.trim().to_string(), password: picker.password.clone(), second_factor: None });
                picker.password.clear();
            }
        }
    });
    action
}
