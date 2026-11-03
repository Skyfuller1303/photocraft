//! The Common Item Dialog with an extra push button. The only file of this crate that allows
//! `unsafe`: every block is a COM call or the handover of a COM-allocated string.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::rc::Rc;

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE, FDEOR_DEFAULT, FDESVR_DEFAULT, FOS_ALLOWMULTISELECT, FileOpenDialog, FileSaveDialog, IFileDialog, IFileDialogControlEvents,
    IFileDialogControlEvents_Impl, IFileDialogCustomize, IFileDialogEvents, IFileDialogEvents_Impl, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{Interface, PCWSTR, PWSTR, Ref, Result as WinResult, implement};

use crate::{Dialog, Kind, Outcome, filter_spec};

/// The id of the extra button among the dialog's custom controls.
const BUTTON_ID: u32 = 0x4343;

/// Shows `dialog` and waits for it to end. Blocks, so run it on a thread of its own: it starts COM
/// in a single-threaded apartment there, as rfd does. `Err` when the dialog could not be shown.
pub fn show(dialog: &Dialog) -> Result<Outcome, String> {
    // SAFETY: balanced by `Com`'s drop, which runs after `run` released every COM object.
    let _com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.ok().map(|()| Com).map_err(|e| e.message())?;
    run(dialog).map_err(|e| e.message())
}

/// Uninitializes COM on the thread that initialized it.
struct Com;

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: pairs the successful `CoInitializeEx` in `show`.
        unsafe { CoUninitialize() };
    }
}

/// The extra button's state: `Some(name)` once clicked, `name` being the file name box's text.
type Clicked = Rc<RefCell<Option<Option<String>>>>;

/// Receives the dialog's events; only the button click matters.
#[implement(IFileDialogEvents, IFileDialogControlEvents)]
struct Sink {
    clicked: Clicked,
}

impl IFileDialogEvents_Impl for Sink_Impl {
    fn OnFileOk(&self, _: Ref<IFileDialog>) -> WinResult<()> {
        Ok(())
    }
    fn OnFolderChanging(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> WinResult<()> {
        Ok(())
    }
    fn OnFolderChange(&self, _: Ref<IFileDialog>) -> WinResult<()> {
        Ok(())
    }
    fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> WinResult<()> {
        Ok(())
    }
    fn OnShareViolation(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> WinResult<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
    fn OnTypeChange(&self, _: Ref<IFileDialog>) -> WinResult<()> {
        Ok(())
    }
    fn OnOverwrite(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> WinResult<FDE_OVERWRITE_RESPONSE> {
        Ok(FDEOR_DEFAULT)
    }
}

impl IFileDialogControlEvents_Impl for Sink_Impl {
    fn OnItemSelected(&self, _: Ref<IFileDialogCustomize>, _: u32, _: u32) -> WinResult<()> {
        Ok(())
    }
    fn OnButtonClicked(&self, control: Ref<IFileDialogCustomize>, id: u32) -> WinResult<()> {
        let Some(control) = control.as_ref().filter(|_| id == BUTTON_ID) else { return Ok(()) };
        let dialog: IFileDialog = control.cast()?;
        *self.clicked.borrow_mut() = Some(typed_name(&dialog));
        // Show returns with this code; `run` tells the click from Cancel by `clicked`.
        // SAFETY: `dialog` is the live dialog this event came from.
        unsafe { dialog.Close(ERROR_CANCELLED.to_hresult()) }
    }
    fn OnCheckButtonToggled(&self, _: Ref<IFileDialogCustomize>, _: u32, _: windows::core::BOOL) -> WinResult<()> {
        Ok(())
    }
    fn OnControlActivating(&self, _: Ref<IFileDialogCustomize>, _: u32) -> WinResult<()> {
        Ok(())
    }
}

/// The text in the dialog's file name box, `None` when it is empty or can't be read.
fn typed_name(dialog: &IFileDialog) -> Option<String> {
    // SAFETY: `dialog` is live; the string is COM-allocated and freed by `take_string`.
    let name = unsafe { dialog.GetFileName() }.ok()?;
    let name = take_string(name).to_string_lossy().into_owned();
    (!name.trim().is_empty()).then_some(name)
}

/// Copies a COM-allocated wide string and frees it.
fn take_string(s: PWSTR) -> OsString {
    // SAFETY: `s` came from a COM call that allocates with `CoTaskMemAlloc` and is NUL terminated;
    // it isn't used after the free.
    unsafe {
        if s.is_null() {
            return OsString::new();
        }
        let copy = OsString::from_wide(s.as_wide());
        CoTaskMemFree(Some(s.0.cast_const().cast()));
        copy
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The file system path of `item`.
fn item_path(item: &IShellItem) -> WinResult<PathBuf> {
    // SAFETY: `item` is live; the string is freed by `take_string`.
    unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map(|p| PathBuf::from(take_string(p)))
}

fn run(d: &Dialog) -> WinResult<Outcome> {
    // SAFETY: COM is initialized on this thread (`show`); every pointer handed to a call below
    // (the wide strings, the filter specs) outlives the call that reads it.
    unsafe {
        let open: Option<IFileOpenDialog> = match d.kind {
            Kind::Open { .. } => Some(CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?),
            Kind::Save => None,
        };
        let dialog: IFileDialog = match &open {
            Some(open) => open.cast()?,
            None => CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?,
        };

        // Like rfd: the first type's first extension is the default, no types means All Files.
        let mut types: Vec<(Vec<u16>, Vec<u16>)> = d.filters.iter().map(|f| (wide(&f.name), wide(&filter_spec(&f.extensions)))).collect();
        let default_ext = d.filters.first().and_then(|f| f.extensions.first()).map(|ext| wide(ext));
        if let Some(ext) = &default_ext {
            dialog.SetDefaultExtension(PCWSTR(ext.as_ptr()))?;
        }
        if types.is_empty() {
            types.push((wide("All Files"), wide("*.*")));
        }
        let specs: Vec<COMDLG_FILTERSPEC> = types.iter().map(|(name, spec)| COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }).collect();
        dialog.SetFileTypes(&specs)?;

        if let Some(dir) = &d.directory {
            // SetFolder, not SetDefaultFolder, which doesn't guarantee the folder. A folder the
            // shell can't parse leaves the dialog on its own default.
            let dir = dir.to_string_lossy();
            let dir = wide(dir.strip_prefix("\\\\?\\").unwrap_or(&dir));
            if let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(dir.as_ptr()), None) {
                dialog.SetFolder(&folder)?;
            }
        }
        if let Some(name) = &d.file_name {
            let name = wide(name);
            dialog.SetFileName(PCWSTR(name.as_ptr()))?;
        }
        if matches!(d.kind, Kind::Open { multiple: true }) {
            dialog.SetOptions(dialog.GetOptions()? | FOS_ALLOWMULTISELECT)?;
        }

        let clicked = Clicked::default();
        let mut advised = None;
        if let Some(label) = &d.button {
            let label = wide(label);
            let custom: IFileDialogCustomize = dialog.cast()?;
            custom.AddPushButton(BUTTON_ID, PCWSTR(label.as_ptr()))?;
            let events: IFileDialogEvents = Sink { clicked: clicked.clone() }.into();
            advised = Some(dialog.Advise(&events)?);
        }

        let shown = dialog.Show(d.owner.map(|hwnd| HWND(hwnd as *mut _)));
        if let Some(cookie) = advised {
            // Nothing to do about a failure: the dialog is done either way.
            let _ = dialog.Unadvise(cookie);
        }
        if let Some(name) = clicked.borrow_mut().take() {
            return Ok(Outcome::Button(name));
        }
        match shown {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(Outcome::Cancelled),
            Err(e) => return Err(e),
        }
        match &open {
            Some(open) if matches!(d.kind, Kind::Open { multiple: true }) => {
                let items = open.GetResults()?;
                let paths = (0..items.GetCount()?).map(|i| item_path(&items.GetItemAt(i)?)).collect::<WinResult<Vec<_>>>()?;
                Ok(Outcome::Picked(paths))
            }
            _ => Ok(Outcome::Picked(vec![item_path(&dialog.GetResult()?)?])),
        }
    }
}
