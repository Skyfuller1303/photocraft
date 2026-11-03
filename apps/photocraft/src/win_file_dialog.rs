//! File › Open and Save As on Windows with a CraftCloud button, as Photoshop's dialogs have
//! ("Save to CraftCloud", "Open from CraftCloud").
//!
//! rfd has no custom controls, so these two dialogs use the Common Item Dialog directly
//! (`photocraft-winfile`, which holds the COM code and its `unsafe`). The request becomes the same
//! dialog rfd showed: the same owner window, start folder, suggested name, file types in the same
//! order with the same default, and multiple selection for File › Open. Clicking the button closes
//! the dialog and answers [`FileDialogAnswer::Cloud`] with the file name typed so far; Cancel still
//! answers nothing, so the two are never confused. Every other dialog (folders, exports, scripts)
//! stays with rfd.

use std::path::{Path, PathBuf};

use photocraft_ui_egui::{FileDialogAnswer, FileDialogReply, FileDialogRequest};
use photocraft_winfile::{Dialog, FileType, Kind, Outcome};

use crate::services::{answer, answer_one, dialog_file_name, netpbm_as_pnm, open_filter_extensions, open_filters, save_filters};

fn file_types(filters: impl IntoIterator<Item = (impl Into<String>, Vec<String>)>) -> Vec<FileType> {
    filters.into_iter().map(|(name, extensions)| FileType { name: name.into(), extensions }).collect()
}

/// The dialog `request` asks for, with the CraftCloud button labelled `button` (none when `None`).
/// `None` for a folder picker, which stays with rfd.
fn dialog_for(request: &FileDialogRequest, owner: Option<isize>, button: Option<&str>) -> Option<Dialog> {
    let button = button.map(str::to_string);
    match request {
        FileDialogRequest::Open { multiple, initial_dir, extensions } => {
            let filters = match extensions {
                // A command-specific filter must not inherit the image formats used by File › Open.
                Some(exts) => file_types([("Supported Files", open_filter_extensions(&exts.iter().map(String::as_str).collect::<Vec<_>>()))]),
                None => file_types(open_filters()),
            };
            Some(Dialog { kind: Kind::Open { multiple: *multiple }, owner, directory: initial_dir.as_ref().map(PathBuf::from), file_name: None, filters, button })
        }
        FileDialogRequest::Save { suggested } => {
            let suggested = netpbm_as_pnm(suggested);
            let directory = Path::new(&suggested).parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);
            Some(Dialog {
                kind: Kind::Save,
                owner,
                directory,
                file_name: dialog_file_name(&suggested, true),
                filters: file_types(save_filters(&suggested)),
                button,
            })
        }
        FileDialogRequest::Folder { .. } => None,
    }
}

/// The app's answer for how `dialog` ended; `None` is Cancel.
fn answer_for(dialog: &Dialog, outcome: Outcome) -> Option<FileDialogAnswer> {
    match outcome {
        Outcome::Picked(paths) if dialog.kind == Kind::Save => paths.first().map(|path| answer_one(path, true, FileDialogAnswer::SaveTo)),
        Outcome::Picked(paths) if paths.is_empty() => None,
        Outcome::Picked(paths) => Some(answer(paths.iter().map(PathBuf::as_path), false, FileDialogAnswer::Paths)),
        Outcome::Button(typed) => {
            // Only Save As has a name to carry over to CraftCloud.
            let file_name = typed.filter(|_| dialog.kind == Kind::Save).and_then(|typed| photocraft_winfile::name_stem(&typed, &dialog.filters));
            Some(FileDialogAnswer::Cloud { file_name })
        }
        Outcome::Cancelled => None,
    }
}

/// Shows File › Open or Save As with the CraftCloud button, on a helper thread, and answers through
/// `reply` like `show_file_dialog` does. Only a dialog whose `reply` carries a button label is
/// handled; any other request (a folder picker, a script's file) comes back for rfd.
pub(crate) fn show(request: FileDialogRequest, parent: Option<&eframe::Frame>, reply: FileDialogReply) -> Option<(FileDialogRequest, FileDialogReply)> {
    use eframe::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
    let owner = parent.and_then(|p| p.window_handle().ok()).and_then(|h| match h.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    });
    // Without a button there is nothing rfd can't do: exports, scripts and Place keep using it.
    let Some(button) = reply.cloud_button().map(str::to_string) else { return Some((request, reply)) };
    let Some(dialog) = dialog_for(&request, owner, Some(&button)) else { return Some((request, reply)) };
    if let Err(e) = std::thread::Builder::new().name("file dialog".into()).spawn(move || {
        let answer = match photocraft_winfile::show(&dialog) {
            Ok(outcome) => answer_for(&dialog, outcome),
            // Like rfd, which takes a dialog that can't be shown for Cancel.
            Err(e) => {
                log::warn!("the Windows file dialog failed: {e}");
                None
            }
        };
        reply.send(answer.or_else(|| crate::file_dialog_probe::known_problem().map(FileDialogAnswer::Unavailable)));
    }) {
        log::error!("couldn't wait for the file dialog: {e}");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts(d: &Dialog) -> Vec<(String, Vec<String>)> {
        d.filters.iter().map(|f| (f.name.clone(), f.extensions.clone())).collect()
    }

    #[test]
    fn open_gets_every_type_in_the_order_rfd_had_them() {
        let request = FileDialogRequest::Open { multiple: true, initial_dir: Some("C:\\pics".into()), extensions: None };
        let d = dialog_for(&request, Some(7), Some("Open from CraftCloud")).expect("open dialog");
        assert_eq!(d.kind, Kind::Open { multiple: true });
        assert_eq!(d.owner, Some(7));
        assert_eq!(d.directory, Some(PathBuf::from("C:\\pics")));
        assert_eq!(d.file_name, None);
        assert_eq!(d.button.as_deref(), Some("Open from CraftCloud"));
        let want: Vec<_> = open_filters().into_iter().map(|(name, extensions)| (name.to_string(), extensions)).collect();
        assert_eq!(exts(&d), want);
        assert_eq!(d.filters[0].name, "All Formats");
    }

    #[test]
    fn a_command_specific_open_keeps_only_its_extensions_and_no_button() {
        let request = FileDialogRequest::Open { multiple: false, initial_dir: None, extensions: Some(vec!["wasm".into()]) };
        let d = dialog_for(&request, None, None).expect("open dialog");
        assert_eq!(d.kind, Kind::Open { multiple: false });
        assert_eq!(exts(&d), vec![("Supported Files".to_string(), vec!["wasm".to_string()])]);
        assert_eq!(d.button, None);
    }

    #[test]
    fn save_starts_on_the_suggested_type_name_and_folder() {
        let request = FileDialogRequest::Save { suggested: "C:\\pics\\art.pcraft".into() };
        let d = dialog_for(&request, None, Some("Save to CraftCloud")).expect("save dialog");
        assert_eq!(d.kind, Kind::Save);
        assert_eq!(d.directory, Some(PathBuf::from("C:\\pics")));
        assert_eq!(d.file_name.as_deref(), Some("art"));
        assert_eq!(d.filters.first().map(|f| f.name.as_str()), Some("PhotoCraft"));
        assert_eq!(d.filters.len(), save_filters("art.pcraft").len());
        assert_eq!(d.button.as_deref(), Some("Save to CraftCloud"));
    }

    #[test]
    fn a_bare_save_name_has_no_folder() {
        let d = dialog_for(&FileDialogRequest::Save { suggested: "Untitled-1.psd".into() }, None, None).expect("save dialog");
        assert_eq!(d.directory, None);
        assert_eq!(d.file_name.as_deref(), Some("Untitled-1"));
    }

    #[test]
    fn folders_stay_with_rfd() {
        assert!(dialog_for(&FileDialogRequest::Folder { initial_dir: None }, None, Some("x")).is_none());
    }

    #[test]
    fn answers_follow_how_the_dialog_ended() {
        let save = dialog_for(&FileDialogRequest::Save { suggested: "C:\\pics\\art.psd".into() }, None, Some("c")).expect("save dialog");
        let open = dialog_for(&FileDialogRequest::Open { multiple: true, initial_dir: None, extensions: None }, None, Some("c")).expect("open dialog");

        assert_eq!(answer_for(&save, Outcome::Cancelled), None);
        assert_eq!(answer_for(&open, Outcome::Cancelled), None);
        assert_eq!(answer_for(&save, Outcome::Picked(vec!["C:\\pics\\x.png".into()])), Some(FileDialogAnswer::SaveTo("C:\\pics\\x.png".into())));
        assert_eq!(
            answer_for(&open, Outcome::Picked(vec!["C:\\a.png".into(), "C:\\b.png".into()])),
            Some(FileDialogAnswer::Paths(vec!["C:\\a.png".into(), "C:\\b.png".into()]))
        );
        assert_eq!(answer_for(&open, Outcome::Picked(vec![])), None);

        // The button is an answer of its own, never Cancel.
        assert_eq!(answer_for(&save, Outcome::Button(Some("My art.psd".into()))), Some(FileDialogAnswer::Cloud { file_name: Some("My art".into()) }));
        assert_eq!(answer_for(&save, Outcome::Button(Some("scan.2024".into()))), Some(FileDialogAnswer::Cloud { file_name: Some("scan.2024".into()) }));
        assert_eq!(answer_for(&save, Outcome::Button(None)), Some(FileDialogAnswer::Cloud { file_name: None }));
        assert_eq!(answer_for(&open, Outcome::Button(Some("typed.png".into()))), Some(FileDialogAnswer::Cloud { file_name: None }));
    }
}
