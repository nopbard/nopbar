//! Read-only automatic destinations. Accept only complete, known DestList
//! layouts and file destinations confirmed by their numbered shell links.
use super::*;
use windows::Win32::System::Com::StructuredStorage::{CreateILockBytesOnHGlobal, StgOpenStorageOnILockBytes};
use windows::Win32::System::Com::{STATFLAG_NONAME, STATSTG, STGM_READ, STGM_SHARE_EXCLUSIVE};

pub(super) fn app_hash(id: &str) -> u64 {
    let mut crc = u64::MAX;
    for byte in id.to_uppercase().encode_utf16().flat_map(u16::to_le_bytes) {
        crc ^= u64::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x92c6_4265_d321_39a4 } else { 0 };
        }
    }
    crc
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn entries(bytes: &[u8]) -> Option<Vec<(u32, String)>> {
    let version = u32_at(bytes, 0)?;
    let length_offset = match version {
        1 => 112,
        3 | 4 => 128,
        _ => return None,
    };
    let count = u32_at(bytes, 4)?;
    let pinned = u32_at(bytes, 8)?;
    if count > 8192 || pinned > count {
        return None;
    }
    let mut tail = bytes.get(32..)?;
    let mut found = Vec::new();
    for _ in 0..count {
        let number = u32_at(tail, 88)?;
        let rank = u32_at(tail, 108)? as i32;
        let chars = u16::from_le_bytes(tail.get(length_offset..length_offset + 2)?.try_into().ok()?);
        let path_start = length_offset + 2;
        let path_end = path_start + usize::from(chars) * 2;
        let path = tail.get(path_start..path_end)?;
        if rank >= 0 {
            let units: Vec<_> = path.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
            found.push((rank, number, String::from_utf16(&units).ok()?));
        }
        tail = tail.get(path_end..)?;
        if version != 1 {
            if u32_at(tail, 0)? != 0 {
                return None;
            } // unfamiliar extension
            tail = tail.get(4..)?;
        }
    }
    if !tail.is_empty() || found.len() != pinned as usize {
        return None;
    }
    found.sort_by_key(|entry| entry.0);
    Some(found.into_iter().map(|(_, n, path)| (n, path)).collect())
}

pub(super) unsafe fn paths(appid: &str, limit: usize) -> Option<Vec<String>> {
    if limit == 0 { return Some(Vec::new()); }
    let file = PathBuf::from(std::env::var_os("APPDATA")?)
        .join(r"Microsoft\Windows\Recent\AutomaticDestinations")
        .join(format!("{:x}.automaticDestinations-ms", app_hash(appid)));
    read_file(&file, limit)
}

unsafe fn read_file(file: &std::path::Path, limit: usize) -> Option<Vec<String>> {
    use std::io::Read;
    // Parse a private snapshot. No COM object ever opens Explorer's file.
    const MAX: usize = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(file).ok()?.take(MAX as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX {
        return None;
    }
    let memory = CreateILockBytesOnHGlobal(None, true).ok()?;
    memory.WriteAt(0, bytes.as_ptr().cast(), bytes.len() as u32, None).ok()?;
    let mode = STGM_READ | STGM_SHARE_EXCLUSIVE;
    let storage = StgOpenStorageOnILockBytes(&memory, None, mode, None, None).ok()?;
    let stream = storage.OpenStream(windows::core::w!("DestList"), None, mode, 0).ok()?;
    let mut stat = STATSTG::default();
    stream.Stat(&mut stat, STATFLAG_NONAME).ok()?;
    if stat.cbSize > bytes.len() as u64 {
        return None;
    }
    bytes.resize(stat.cbSize as usize, 0);
    let mut got = 0;
    stream
        .Read(bytes.as_mut_ptr().cast(), bytes.len() as u32, Some(&mut got))
        .ok()
        .ok()?;
    if got as usize != bytes.len() {
        return None;
    }
    let mut result = Vec::new();
    for (number, path) in entries(&bytes)? {
        if !std::path::Path::new(&path).is_absolute() || path.contains(['\0', '"']) {
            continue;
        }
        let Ok(s) = storage.OpenStream(&HSTRING::from(format!("{number:x}")), None, mode, 0) else {
            continue;
        };
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        if link.cast::<IPersistStream>().ok()?.Load(&s).is_err() {
            continue;
        }
        let mut dest = vec![0; 32768];
        if link.GetPath(&mut dest, std::ptr::null_mut(), 0).is_ok() && wstr(&dest).eq_ignore_ascii_case(&path) {
            result.push(path);
            if result.len() == limit { break; }
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_compound_file_is_read_only_and_confirms_the_link_target() {
        use windows::Win32::System::Com::StructuredStorage::StgCreateDocfile;
        use windows::Win32::System::Com::{STGC_DEFAULT, STGM_CREATE, STGM_READWRITE};
        let temp = std::env::temp_dir().join(format!("nopbar-pinned-{}.automaticDestinations-ms", uuid::Uuid::new_v4()));
        unsafe {
            assert!(CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok());
            let file = r"C:\Example\Memo.txt";
            let mode = STGM_CREATE | STGM_READWRITE | STGM_SHARE_EXCLUSIVE;
            {
                let storage = StgCreateDocfile(&HSTRING::from(temp.to_string_lossy().as_ref()), mode, None).unwrap();
                let mut bytes = vec![0u8; 32 + 130];
                bytes[..4].copy_from_slice(&4u32.to_le_bytes());
                bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
                bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
                bytes[32 + 88] = 10;
                bytes[32 + 128] = file.len() as u8;
                bytes.extend(file.encode_utf16().flat_map(u16::to_le_bytes));
                bytes.extend([0; 4]);
                let data = storage.CreateStream(windows::core::w!("DestList"), mode, 0, 0).unwrap();
                data.Write(bytes.as_ptr().cast(), bytes.len() as u32, None).ok().unwrap();
                let dest = storage.CreateStream(windows::core::w!("a"), mode, 0, 0).unwrap();
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(&HSTRING::from(file)).unwrap();
                link.cast::<IPersistStream>().unwrap().Save(&dest, true).unwrap();
                storage.Commit(STGC_DEFAULT.0 as u32).unwrap();
            }
            let before = std::fs::read(&temp).unwrap();
            let result = read_file(&temp, 12);
            assert_eq!(std::fs::read(&temp).unwrap(), before);
            std::fs::remove_file(temp).unwrap();
            windows::Win32::System::Com::CoUninitialize();
            assert_eq!(result, Some(vec![file.into()]));
        }
    }

    #[test]
    fn explicit_appid_hash_matches_windows() {
        assert_eq!(app_hash("Microsoft.InternetExplorer.Default"), 0x28c8b86deab549a1);
        assert_eq!(app_hash("Microsoft.Windows.StickyNotes"), 0x337ed59af273c758);
    }

    #[test]
    fn destlist_checks_layout_count_order_and_bounds() {
        for (version, offset) in [(1u32, 112usize), (3, 128), (4, 128)] {
            let mut bytes = vec![0; 32];
            bytes[..4].copy_from_slice(&version.to_le_bytes());
            bytes[4..8].copy_from_slice(&3u32.to_le_bytes());
            bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
            for (i, rank) in [(1u32, 8i32), (2, -1), (3, 2)] {
                let mut record = vec![0; offset];
                record[88..92].copy_from_slice(&i.to_le_bytes());
                record[108..112].copy_from_slice(&rank.to_le_bytes());
                bytes.extend(record);
                bytes.extend(1u16.to_le_bytes());
                bytes.extend((64 + i as u16).to_le_bytes());
                if version != 1 {
                    bytes.extend([0; 4]);
                }
            }
            assert_eq!(entries(&bytes), Some(vec![(3, "C".into()), (1, "A".into())]));
            assert!(entries(&bytes[..bytes.len() - 1]).is_none());
            bytes[8] = 1;
            assert!(entries(&bytes).is_none());
        }
        assert!(entries(&[9; 32]).is_none());
    }
}
