//! Files dragged out of NopBar (the island's screenshot): the shell's own
//! data object for the paths (what Explorer would drag: file names, shell
//! ids, everything targets look for) and our drag source. A path is never
//! read from disk to build it (it may sit on a slow or dead share).

use std::cell::Cell;

use windows::core::{Result, BOOL, HSTRING};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, E_POINTER, S_OK};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{CreateBindCtx, IDataObject};
use windows::Win32::System::Ole::{IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_LBUTTON, MK_RBUTTON};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IFileSystemBindData, IFileSystemBindData_Impl, IShellItem, ILFree, SHCreateDataObject, SHCreateItemFromParsingName, SHDoDragDrop, SHGetIDListFromObject,
    STR_FILE_SYS_BIND_DATA,
};
use windows_core::implement;

/// The last part of a path ("report.pdf"); a drive or share root as it is.
pub fn name_of(path: &str) -> &str {
    let t = path.trim_end_matches(['\\', '/']);
    match t.rfind(['\\', '/']) {
        Some(i) if i + 1 < t.len() && !t.ends_with(':') => &t[i + 1..],
        _ => path,
    }
}

/// Find data handed to the shell's parser, so a path becomes an item
/// without touching the disk.
#[implement(IFileSystemBindData)]
struct FindData(Cell<WIN32_FIND_DATAW>);

impl IFileSystemBindData_Impl for FindData_Impl {
    fn SetFindData(&self, pfd: *const WIN32_FIND_DATAW) -> windows_core::Result<()> {
        if !pfd.is_null() {
            self.0.set(unsafe { *pfd });
        }
        Ok(())
    }

    fn GetFindData(&self, pfd: *mut WIN32_FIND_DATAW) -> windows_core::Result<()> {
        if pfd.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *pfd = self.0.get() };
        Ok(())
    }
}

/// The absolute id list of `path` (freed with ILFree).
unsafe fn idlist(path: &str, dir: bool) -> Result<*mut ITEMIDLIST> {
    let mut fd = WIN32_FIND_DATAW { dwFileAttributes: if dir { FILE_ATTRIBUTE_DIRECTORY.0 } else { FILE_ATTRIBUTE_NORMAL.0 }, ..Default::default() };
    let name: Vec<u16> = name_of(path).encode_utf16().take(fd.cFileName.len() - 1).collect();
    fd.cFileName[..name.len()].copy_from_slice(&name);
    let bind: IFileSystemBindData = FindData(Cell::new(fd)).into();
    let ctx = CreateBindCtx(0)?;
    ctx.RegisterObjectParam(STR_FILE_SYS_BIND_DATA, &bind)?;
    let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), &ctx)?;
    SHGetIDListFromObject(&item)
}

/// The shell's data object for `paths` (each `(path, is a folder)`), from
/// any folders: absolute ids under the desktop. Built without reading the
/// disk; a path gone since is reported by the drop target.
unsafe fn data_object(paths: &[(String, bool)]) -> Result<IDataObject> {
    let mut ids: Vec<*mut ITEMIDLIST> = Vec::new();
    for (p, dir) in paths {
        match idlist(p, *dir) {
            Ok(id) => ids.push(id),
            Err(e) => crate::log!("drag out: an item cannot be dragged ({e})"),
        }
    }
    let r = if ids.is_empty() {
        Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
    } else {
        let list: Vec<*const ITEMIDLIST> = ids.iter().map(|p| *p as *const _).collect();
        SHCreateDataObject(None, Some(&list), None::<&IDataObject>)
    };
    for id in ids {
        ILFree(Some(id));
    }
    r
}

/// Our side of a drag out: drop when the button that started it comes up,
/// cancel on Esc or the other button.
#[implement(IDropSource)]
struct Source {
    button: u32,
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> windows_core::HRESULT {
        if escape.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        let other = if self.button == MK_RBUTTON.0 { MK_LBUTTON.0 } else { MK_RBUTTON.0 };
        if keys.0 & other != 0 {
            return DRAGDROP_S_CANCEL;
        }
        if keys.0 & self.button == 0 {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> windows_core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

thread_local! {
    /// OLE was initialized on this thread (once, for the process's life).
    static OLE: Cell<bool> = const { Cell::new(false) };
}

/// A drag needs OLE on the thread that starts it (the dock usually has it
/// already: it takes drops).
fn ensure_ole() {
    OLE.with(|o| {
        if !o.get() {
            let _ = unsafe { windows::Win32::System::Ole::OleInitialize(None) };
            o.set(true);
        }
    });
}

/// Drags `paths` out (a shell drag; the shell draws the picture), offering
/// only the effects `all`. Runs a modal loop: call with nothing borrowed.
pub unsafe fn drag_out_with(paths: &[(String, bool)], all: DROPEFFECT) -> Option<DROPEFFECT> {
    ensure_ole();
    let data = match data_object(paths) {
        Ok(d) => d,
        Err(e) => {
            crate::log!("drag out: cannot drag ({e})");
            return None;
        }
    };
    let source: IDropSource = Source { button: MK_LBUTTON.0 }.into();
    match SHDoDragDrop(None, &data, &source, all) {
        Ok(e) => Some(e),
        Err(e) => {
            crate::log!("drag out: drag failed ({e})");
            None
        }
    }
}

/// Drags files of NopBar's own (a screenshot written for the drag) out:
/// copy or move only. Returns the effect bits (0: nothing happened).
pub(crate) fn drag_own_files(paths: &[(String, bool)]) -> u32 {
    unsafe { drag_out_with(paths, DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0)).map(|e| e.0).unwrap_or(0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_the_last_part_of_a_path() {
        assert_eq!(name_of("C:\\Users\\me\\a.txt"), "a.txt");
        assert_eq!(name_of("C:\\Users\\me\\Folder\\"), "Folder");
        assert_eq!(name_of("C:\\"), "C:\\");
        assert_eq!(name_of("\\\\nas\\share\\x.png"), "x.png");
    }
}
