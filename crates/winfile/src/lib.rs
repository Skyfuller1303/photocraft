//! The Windows Open and Save dialogs with one extra push button, as Photoshop's have
//! ("Save to Cloud Documents" in Save As, "Open from Cloud Documents" in Open).
//!
//! rfd, which PhotoCraft uses for its other file dialogs, has no custom controls. This crate shows
//! the Common Item Dialog (`IFileOpenDialog` / `IFileSaveDialog`) directly and adds the button with
//! `IFileDialogCustomize`. Clicking it closes the dialog and reports [`Outcome::Button`] with the
//! file name typed so far, which the caller turns into its own dialog. Cancel stays
//! [`Outcome::Cancelled`], so the two are never confused.
//!
//! The dialog itself ([`show`]) exists on Windows only and needs a real desktop to test. The pure
//! parts (file type specs, the typed name's stem) are portable and tested everywhere.
//!
//! Only `src/dialog.rs` allows `unsafe`; every block there has a `SAFETY:` comment.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::PathBuf;

#[cfg(target_os = "windows")]
mod dialog;
#[cfg(target_os = "windows")]
pub use dialog::show;

/// One entry of the dialog's file type list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType {
    pub name: String,
    /// Extensions without the dot; the first one is the type's default.
    pub extensions: Vec<String>,
}

/// What kind of dialog to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// File › Open; `multiple` allows several files.
    Open { multiple: bool },
    /// Save As.
    Save,
}

/// Everything [`show`] needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub kind: Kind,
    /// The owner window's `HWND`, which the dialog is modal to.
    pub owner: Option<isize>,
    /// The folder the dialog starts in.
    pub directory: Option<PathBuf>,
    /// The name the file name box starts with.
    pub file_name: Option<String>,
    /// The file types in display order; the first is selected and supplies the default extension.
    pub filters: Vec<FileType>,
    /// The label of the extra push button (already translated); no button when `None`.
    pub button: Option<String>,
}

/// How the dialog ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The files the user chose (one for Save and single Open).
    Picked(Vec<PathBuf>),
    /// The extra button was clicked: the file name box's text at that moment (`None` if empty).
    Button(Option<String>),
    /// Cancel, or the window's close button.
    Cancelled,
}

/// The `COMDLG_FILTERSPEC` pattern of `extensions`: `*.psd;*.psb`.
pub fn filter_spec(extensions: &[String]) -> String {
    extensions.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";")
}

/// `typed` without the extension of one of the dialog's file types (`Untitled-1.psd` → `Untitled-1`),
/// else as typed (`scan.2024` stays `scan.2024`: the dot is part of the name). `None` when nothing
/// is left, so a blank file name box asks the caller for a default.
pub fn name_stem(typed: &str, filters: &[FileType]) -> Option<String> {
    let typed = typed.trim();
    let stem = match typed.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && filters.iter().flat_map(|f| &f.extensions).any(|e| e.eq_ignore_ascii_case(ext)) => stem,
        _ => typed,
    };
    let stem = stem.trim();
    (!stem.is_empty()).then(|| stem.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> Vec<FileType> {
        vec![
            FileType { name: "PSD Document".into(), extensions: vec!["psd".into(), "psb".into()] },
            FileType { name: "JPEG".into(), extensions: vec!["jpg".into(), "jpeg".into()] },
        ]
    }

    #[test]
    fn spec_lists_every_extension() {
        assert_eq!(filter_spec(&["psd".into(), "psb".into()]), "*.psd;*.psb");
        assert_eq!(filter_spec(&["png".into()]), "*.png");
        assert_eq!(filter_spec(&[]), "");
    }

    #[test]
    fn stem_drops_a_listed_extension_only() {
        let t = types();
        assert_eq!(name_stem("Untitled-1.psd", &t).as_deref(), Some("Untitled-1"));
        assert_eq!(name_stem("photo.JPEG", &t).as_deref(), Some("photo"));
        assert_eq!(name_stem("scan.2024", &t).as_deref(), Some("scan.2024"));
        assert_eq!(name_stem("my art", &t).as_deref(), Some("my art"));
        assert_eq!(name_stem("a.b.psd", &t).as_deref(), Some("a.b"));
    }

    #[test]
    fn stem_of_nothing_is_none() {
        let t = types();
        assert_eq!(name_stem("", &t), None);
        assert_eq!(name_stem("   ", &t), None);
        assert_eq!(name_stem(".psd", &t).as_deref(), Some(".psd"));
    }
}
