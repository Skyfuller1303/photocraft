//! Preferences › CraftCloud: which server the app uses, who is signed in there, and whether Save As
//! asks where to save.
//!
//! The list of servers, the selected one and the remembered folders are the CraftCloud settings
//! (`app.json`); passwords never are. The signed-in session lives in the OS keychain, one per
//! server and account, so switching servers signs in again from the keychain without a password.

use egui::{Align, Layout, RichText, Ui};

use super::{Runtime, dialog, validate_server_url};
use crate::theme::Tokens;

/// The message shown while a request prevents changing servers.
fn busy_message() -> String {
    tl!("Wait for the current CraftCloud request to finish before changing servers.").into()
}

impl Runtime {
    /// Makes `url` (a trusted server) the one the app uses. A session of another server is left, the
    /// loaded organizations, workspaces, projects and files are dropped, and the stored session of the
    /// new server is restored when the keychain has one.
    pub(crate) fn select_server(&mut self, url: &str) -> Result<(), String> {
        if self.request_busy {
            return Err(busy_message());
        }
        if self.config.selected().is_some_and(|server| server.url == url) {
            return Ok(());
        }
        if !self.config.servers.iter().any(|server| server.url == url && server.trusted) {
            return Err(tl!("No trusted CraftCloud server is selected.").into());
        }
        self.config.selected_server = Some(url.to_string());
        self.server_changed();
        self.save_config();
        Ok(())
    }

    /// Adds the server typed in Preferences, trusts it and switches to it. A missing scheme is
    /// completed the way the SDK's server step completes it (`http://` only for loopback hosts).
    pub(crate) fn add_server(&mut self, input: &str) -> Result<(), String> {
        if self.request_busy {
            return Err(busy_message());
        }
        let url = craftcloud_egui::normalize_server_url(input).ok_or_else(|| tl!("Enter a server address.").to_string())?;
        validate_server_url(&url)?;
        let was_selected = self.config.selected().is_some_and(|server| server.url == url);
        self.config.trust(url)?;
        if !was_selected {
            self.server_changed();
        }
        self.save_config();
        self.server_input.clear();
        Ok(())
    }

    /// Removes a server and what was remembered about it. Removing the selected one leaves nothing
    /// selected.
    pub(crate) fn remove_server(&mut self, url: &str) -> Result<(), String> {
        if self.request_busy {
            return Err(busy_message());
        }
        let before = self.config.servers.len();
        self.config.servers.retain(|server| server.url != url);
        if self.config.servers.len() == before {
            return Ok(());
        }
        self.config.last_used.remove(url);
        if self.config.selected_server.as_deref() == Some(url) {
            self.config.selected_server = None;
            self.server_changed();
        }
        self.save_config();
        Ok(())
    }

    /// The selected server changed: forget the session and lists of the old one.
    fn server_changed(&mut self) {
        let selected = self.config.selected().cloned();
        if self.client_server.as_deref() != selected.as_ref().map(|server| server.url.as_str()) {
            self.client = None;
            self.client_server = None;
            self.locks_supported = false;
        }
        self.clear_hierarchy();
        self.cloud_dialog = None;
        self.preferred = selected.as_ref().and_then(|server| self.config.last_used.get(&server.url).cloned()).unwrap_or_default();
        self.picker.server_url = selected.as_ref().map(|server| server.url.clone()).unwrap_or_default();
        self.picker.email = selected.as_ref().and_then(|server| server.account.clone()).unwrap_or_default();
        self.picker.password.clear();
        self.picker.second_factor.clear();
        self.picker.requires_second_factor = false;
        self.picker.second_factor_challenge_id = None;
        self.picker.account_name.clear();
        self.picker.error = None;
        if self.client.is_none() && selected.is_some() {
            self.restore_session();
        }
    }

    /// Failing to persist the settings is shown like the other CraftCloud errors; the change still
    /// applies for this session.
    fn save_config(&mut self) {
        if let Err(error) = self.config.save(self.data_dir.as_deref()) {
            self.picker.error = Some(error);
        }
    }

    /// The signed-in account of the selected server, if the app is signed in to it.
    pub(crate) fn signed_in_account(&self) -> Option<String> {
        let server = self.config.selected()?;
        (self.client.is_some() && self.client_server.as_deref() == Some(server.url.as_str())).then(|| server.account.clone().unwrap_or_else(|| self.picker.account_name.clone()))
    }

    /// The Preferences › CraftCloud page. `ask` is the "Ask where to save" checkbox, which only
    /// exists where Save As has a CraftCloud question (not on Windows, whose native dialog has a
    /// CraftCloud button). Returns whether `ask` was changed.
    pub(crate) fn prefs_page(&mut self, ui: &mut Ui, ask: Option<&mut bool>) -> bool {
        let t = Tokens::get(ui.ctx());
        let busy = self.request_busy;
        let heading = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).font(crate::theme::semibold(13.0)).color(t.text));
            ui.add_space(4.0);
        };

        heading(ui, tl!("Servers"));
        if self.config.servers.is_empty() {
            ui.label(RichText::new(tl!("No CraftCloud server is set up.")).color(t.text_dim));
        }
        let mut choose = None;
        let mut remove = None;
        let selected = self.config.selected_server.clone();
        for server in &self.config.servers {
            ui.horizontal(|ui| {
                let active = selected.as_deref() == Some(server.url.as_str()) && server.trusted;
                ui.add_enabled_ui(server.trusted && !busy, |ui| {
                    if ui.radio(active, &server.url).clicked() && !active {
                        choose = Some(server.url.clone());
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if crate::widgets::secondary_button(ui, tl!("Remove"), 0.0).clicked() {
                            remove = Some(server.url.clone());
                        }
                    });
                });
            });
        }
        if let Some(url) = choose
            && let Err(error) = self.select_server(&url)
        {
            self.picker.error = Some(error);
        }
        if let Some(url) = remove
            && let Err(error) = self.remove_server(&url)
        {
            self.picker.error = Some(error);
        }
        ui.add_space(8.0);

        ui.label(RichText::new(tl!("Server address")).font(crate::theme::medium(12.0)).color(t.text_dim));
        let mut add = false;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let width = (ui.available_width() - 120.0).clamp(140.0, 360.0);
                let id = ui.scope_id().with("cloud-add-server");
                let response = crate::widgets::text_field(ui, id, &mut self.server_input, "cloud.example.com", width, false).accessible_name(tl!("Server address"));
                let ready = craftcloud_egui::normalize_server_url(&self.server_input).is_some();
                let entered = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let clicked = ui.add_enabled_ui(ready, |ui| crate::widgets::primary_button(ui, tl!("Add server"), 0.0).clicked()).inner;
                add = ready && (clicked || entered);
            });
        });
        if let Some(url) = craftcloud_egui::normalize_server_url(&self.server_input) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tl!("Will connect to")).color(t.text_dim));
                ui.monospace(&url);
            });
            let typed = self.server_input.trim();
            if typed != url {
                let hint = if url.starts_with("http://") {
                    tl!("No scheme entered: http:// is used for this local address.")
                } else {
                    tl!("No scheme entered: https:// is used.")
                };
                ui.label(RichText::new(hint).color(t.text_faint).size(11.5));
            }
        }
        ui.label(RichText::new(tl!("Adding a server trusts it. Only add servers you know.")).color(t.text_faint).size(11.5));
        if add {
            let input = self.server_input.clone();
            self.picker.error = None;
            if let Err(error) = self.add_server(&input) {
                self.picker.error = Some(error);
            }
        }
        ui.add_space(12.0);

        heading(ui, tl!("Account"));
        match self.config.selected().cloned() {
            None => {
                ui.label(RichText::new(tl!("Add a server to sign in.")).color(t.text_dim));
            }
            Some(server) => {
                self.picker.server_url = server.url.clone();
                if let Some(account) = self.signed_in_account() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::i18n::fmt(tl!("Signed in as {email}"), &[("email", &account)])).color(t.text));
                        ui.add_enabled_ui(!busy, |ui| {
                            if crate::widgets::secondary_button(ui, tl!("Sign out"), 0.0).clicked() {
                                self.logout();
                            }
                        });
                    });
                } else if busy {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(t.text_dim));
                        ui.label(RichText::new(tl!("Loading…")).color(t.text_dim));
                    });
                } else {
                    ui.label(RichText::new(tl!("Not signed in")).color(t.text_dim));
                    ui.add_space(4.0);
                    if let Some(craftcloud_egui::Action::Login { server, email, password, second_factor }) = dialog::sign_in_form(ui, &mut self.picker, false) {
                        self.login(server, email, password, second_factor);
                    }
                }
            }
        }
        if let Some(error) = &self.picker.error {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(error).color(t.danger).size(12.5)).wrap());
        }

        let mut changed = false;
        if let Some(ask) = ask {
            ui.add_space(12.0);
            heading(ui, tl!("Saving"));
            changed = crate::widgets::checkbox(ui, ask, tl!("Ask where to save")).changed();
        }
        changed
    }
}
