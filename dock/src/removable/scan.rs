//! Reading the removable storage from Windows, and ejecting it. Blocking
//! calls: run on the removable-storage helper thread only (mod.rs).
//!
//! The way Windows' own Safely Remove finds what to offer: every present
//! disk and CD drive (SetupAPI, GUID_DEVINTERFACE_DISK / _CDROM), its
//! devnode chain up to the root (cfgmgr32: CM_Get_Parent, the capabilities,
//! status, class and service of each), and the closest removable devnode on
//! it (model::ejectable_in) is the one to eject, but only if it is storage
//! itself (model::storage_target) and everything that would go with it is
//! known and fits model::check_scope: its closure (model::closure: its
//! whole subtree, CM_Get_Child / CM_Get_Sibling, any error making it
//! unknown; its removal and ejection relations, CM_Get_Device_ID_ListW with
//! CM_GETIDLIST_FILTER_REMOVALRELATIONS / _EJECTRELATIONS; then the subtree
//! and relations of every devnode found, at most MAX_SCOPE), and every disk
//! in it on a USB, SD or MMC bus. Those disks are opened (no access
//! asked, so nothing spins up or needs rights) for their number
//! (IOCTL_STORAGE_GET_DEVICE_NUMBER), bus (IOCTL_STORAGE_QUERY_PROPERTY)
//! and size (IOCTL_DISK_GET_DRIVE_GEOMETRY_EX).
//!
//! Volumes (SetupAPI, GUID_DEVINTERFACE_VOLUME) find their disks by the
//! same number, or by every extent (IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
//! its buffer grown to fit), then get their drive letters
//! (GetVolumePathNamesForVolumeNameW) and label (GetVolumeInformationW).
//! Fail closed: a target whose disks (under it or related) include the
//! disk of the Windows volume, of the boot (EFI system) partition
//! (HKLM\SYSTEM\Setup SystemPartition) or of a page file (Memory
//! Management's ExistingPageFiles) is never offered, and if any of those
//! cannot be found nothing is offered at all. Nor is a target with a
//! related volume that cannot be mapped or that spans other disks, and a
//! volume that cannot be placed at all (no devnode, no disks) keeps every
//! target out.
//!
//! Ejecting is CM_Request_Device_EjectW on that devnode, with a veto buffer
//! so Windows reports a refusal to us instead of showing its own message.
//! Logs never name a device ID (it holds the serial number).

use windows::core::{GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_SUCCESS, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, GetVolumePathNamesForVolumeNameW, QueryDosDeviceW,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceProperty, DISK_EXTENT, DISK_GEOMETRY_EX, GUID_DEVINTERFACE_CDROM, GUID_DEVINTERFACE_DISK, GUID_DEVINTERFACE_VOLUME,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
    STORAGE_PROPERTY_QUERY, VOLUME_DISK_EXTENTS,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ};
use windows::Win32::System::IO::DeviceIoControl;

use super::model::{self, DiskInfo, Node, Refusal, VolumeInfo};

/// FILE_DEVICE_DISK / FILE_DEVICE_CD_ROM / FILE_DEVICE_DVD, as
/// IOCTL_STORAGE_GET_DEVICE_NUMBER reports them.
const TYPE_DISK: u32 = 7;
const TYPE_CDROM: u32 = 2;
const TYPE_DVD: u32 = 51;
/// Devnode chains are short; this only stops a broken tree.
const MAX_DEPTH: usize = 24;
/// Under a storage devnode: more than this many devnodes, or deeper than
/// this, is not known as plain storage (and is not offered).
const MAX_BELOW: usize = 64;
const MAX_BELOW_DEPTH: usize = 6;
/// Devnodes that may go with one eject (its closure): past this it is not
/// known as plain storage (and is not offered).
const MAX_SCOPE: usize = 64;
/// Extents a volume may have before it is taken as unmappable.
const MAX_EXTENTS: usize = 1024;

/// What a scan found: the devices, and what it left out and why (log
/// lines, no device IDs).
pub struct Scan {
    pub devices: Vec<model::Device>,
    pub notes: Vec<String>,
}

fn wide_str(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// A handle closed when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Opens a device or volume path without asking for any access (enough for
/// the queries below; nothing is read or written).
fn open(path: &str) -> Option<Handle> {
    unsafe { CreateFileW(&HSTRING::from(path), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }
        .ok()
        .map(Handle)
}

/// An ioctl with no input and a fixed-size answer.
fn query<T: Default>(h: &Handle, code: u32) -> Option<T> {
    let mut out = T::default();
    let mut got = 0u32;
    unsafe { DeviceIoControl(h.0, code, None, 0, Some(&mut out as *mut T as *mut _), std::mem::size_of::<T>() as u32, Some(&mut got), None) }.ok()?;
    Some(out)
}

fn device_number(h: &Handle) -> Option<(u32, u32)> {
    query::<STORAGE_DEVICE_NUMBER>(h, IOCTL_STORAGE_GET_DEVICE_NUMBER).map(|n| (n.DeviceType, n.DeviceNumber))
}

/// The storage bus type (STORAGE_BUS_TYPE; 0 unknown).
fn bus_type(h: &Handle) -> i32 {
    let q = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceProperty, QueryType: PropertyStandardQuery, ..Default::default() };
    // The descriptor and the strings after it.
    let mut buf = vec![0u64; 128];
    let mut got = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&q as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            (buf.len() * 8) as u32,
            Some(&mut got),
            None,
        )
    }
    .is_ok();
    if !ok || (got as usize) < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return 0;
    }
    let d = unsafe { &*(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
    d.BusType.0
}

/// A devnode's capabilities (None: Windows would not say).
fn devnode_caps(dn: u32) -> Option<u32> {
    let mut caps = 0u32;
    let mut len = 4u32;
    let cr = unsafe { CM_Get_DevNode_Registry_PropertyW(dn, CM_DRP_CAPABILITIES, None, Some(&mut caps as *mut u32 as *mut _), &mut len, 0) };
    (cr == CR_SUCCESS).then_some(caps)
}

fn devnode_status(dn: u32) -> u32 {
    let mut status = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, dn, 0) };
    if cr == CR_SUCCESS { status.0 } else { 0 }
}

/// A devnode's string property (friendly name, description, class...).
fn devnode_text(dn: u32, prop: u32) -> String {
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    let cr = unsafe { CM_Get_DevNode_Registry_PropertyW(dn, prop, None, Some(buf.as_mut_ptr() as *mut _), &mut len, 0) };
    if cr == CR_SUCCESS { wide_str(&buf).trim().to_string() } else { String::new() }
}

/// Its friendly name, else its description.
fn devnode_name(dn: u32) -> String {
    let n = devnode_text(dn, CM_DRP_FRIENDLYNAME);
    if n.is_empty() { devnode_text(dn, CM_DRP_DEVICEDESC) } else { n }
}

fn devnode_id(dn: u32) -> Option<String> {
    let mut buf = [0u16; MAX_DEVICE_ID_LEN as usize + 1];
    (unsafe { CM_Get_Device_IDW(dn, &mut buf, 0) } == CR_SUCCESS).then(|| wide_str(&buf))
}

fn node_of(dn: u32) -> Node {
    Node { caps: devnode_caps(dn), status: devnode_status(dn), class: devnode_text(dn, CM_DRP_CLASS), service: devnode_text(dn, CM_DRP_SERVICE) }
}

/// The devnode and its ancestors up to the root, with what model needs.
fn chain_of(dn: u32) -> Vec<(u32, Node)> {
    let mut v = Vec::new();
    let mut cur = dn;
    for _ in 0..MAX_DEPTH {
        v.push((cur, node_of(cur)));
        let mut parent = 0u32;
        if unsafe { CM_Get_Parent(&mut parent, cur, 0) } != CR_SUCCESS {
            break;
        }
        cur = parent;
    }
    v
}

/// Everything under a devnode (CM_Get_Child / CM_Get_Sibling; a step
/// answering anything but success or "no such devnode" makes it unknown).
fn subtree_of(dn: u32) -> Option<Vec<u32>> {
    let step = |cr: CONFIGRET, next: u32| match cr {
        CR_SUCCESS => model::Step::Next(next),
        CR_NO_SUCH_DEVNODE => model::Step::End,
        _ => model::Step::Error,
    };
    model::subtree(
        dn,
        |p| {
            let mut c = 0u32;
            let cr = unsafe { CM_Get_Child(&mut c, p, 0) };
            step(cr, c)
        },
        |c| {
            let mut s = 0u32;
            let cr = unsafe { CM_Get_Sibling(&mut s, c, 0) };
            step(cr, s)
        },
        MAX_BELOW,
        MAX_BELOW_DEPTH,
    )
}

/// Every present interface of a class: (device path, devnode).
fn interfaces(class: &GUID) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(Some(class), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) else { return out };
        for i in 0..1024u32 {
            let mut data = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInterfaces(set, None, class, i, &mut data).is_err() {
                break;
            }
            let mut need = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut need), None);
            if need < 8 || need > 64 * 1024 {
                continue;
            }
            // u64s: the detail's alignment.
            let mut buf = vec![0u64; (need as usize).div_ceil(8)];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut info = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), need, None, Some(&mut info)).is_err() {
                continue;
            }
            let path_ptr = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let max = (need as usize - std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath)) / 2;
            let path = wide_str(std::slice::from_raw_parts(path_ptr, max));
            if !path.is_empty() {
                out.push((path, info.DevInst));
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// A present disk or CD drive interface: its path, devnode, whether it is
/// a CD drive.
struct DiskIf {
    path: String,
    devnode: u32,
    cdrom: bool,
}

fn disk_interfaces() -> Vec<DiskIf> {
    let mut out = Vec::new();
    for (class, cdrom) in [(GUID_DEVINTERFACE_DISK, false), (GUID_DEVINTERFACE_CDROM, true)] {
        out.extend(interfaces(&class).into_iter().map(|(path, devnode)| DiskIf { path, devnode, cdrom }));
    }
    out
}

/// One disk or CD drive opened: its key, bus type and size.
struct Opened {
    key: (u32, u32),
    bus: i32,
    size: u64,
}

fn open_disk(d: &DiskIf) -> Option<Opened> {
    let h = open(&d.path)?;
    let key = device_number(&h)?;
    let size = if d.cdrom { 0 } else { query::<DISK_GEOMETRY_EX>(&h, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX).map(|g| g.DiskSize.max(0) as u64).unwrap_or(0) };
    Some(Opened { key, bus: bus_type(&h), size })
}

/// Every disk a volume lies on: its own device number (basic disks, CDs),
/// else all its extents (the buffer grown until they fit). None: not known.
fn volume_disks(h: &Handle) -> Option<Vec<(u32, u32)>> {
    if let Some((t, n)) = device_number(h) {
        if t == TYPE_DISK || t == TYPE_CDROM || t == TYPE_DVD {
            return Some(vec![(t, n)]);
        }
    }
    let head = std::mem::offset_of!(VOLUME_DISK_EXTENTS, Extents);
    let each = std::mem::size_of::<DISK_EXTENT>();
    let mut room = 4usize;
    loop {
        let bytes = head + room * each;
        let mut buf = vec![0u64; bytes.div_ceil(8)];
        let mut got = 0u32;
        let r = unsafe {
            DeviceIoControl(h.0, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, 0, Some(buf.as_mut_ptr() as *mut _), (buf.len() * 8) as u32, Some(&mut got), None)
        };
        let count = unsafe { (*(buf.as_ptr() as *const VOLUME_DISK_EXTENTS)).NumberOfDiskExtents } as usize;
        match r {
            Ok(()) => {
                if count == 0 || count > room {
                    return None;
                }
                let base = unsafe { (buf.as_ptr() as *const u8).add(head) as *const DISK_EXTENT };
                let mut disks: Vec<(u32, u32)> = (0..count).map(|k| (TYPE_DISK, unsafe { (*base.add(k)).DiskNumber })).collect();
                disks.sort_unstable();
                disks.dedup();
                return Some(disks);
            }
            Err(e) if e.code() == HRESULT::from_win32(ERROR_MORE_DATA.0) || e.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) => {
                let want = count.max(room * 2);
                if want > MAX_EXTENTS || want <= room {
                    return None;
                }
                room = want;
            }
            Err(_) => return None,
        }
    }
}

/// A registry value (REG_SZ or REG_MULTI_SZ) under HKLM: its strings, or
/// Ok(None) when it does not exist.
fn reg_strings(key: &str, value: &str, multi: bool) -> Result<Option<Vec<String>>, String> {
    let mut buf = vec![0u16; 4096];
    let mut size = (buf.len() * 2) as u32;
    let flags = if multi { RRF_RT_REG_MULTI_SZ } else { RRF_RT_REG_SZ };
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(value), flags, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if r != ERROR_SUCCESS {
        return Err(format!("{value}: error {}", r.0));
    }
    let n = (size as usize / 2).min(buf.len());
    Ok(Some(buf[..n].split(|&c| c == 0).map(String::from_utf16_lossy).filter(|s| !s.is_empty()).collect()))
}

/// The volume Windows lives on ("\\?\Volume{...}\", lowercase).
fn windows_volume() -> Option<String> {
    let mut dir = [0u16; 260];
    let n = unsafe { windows::Win32::System::SystemInformation::GetWindowsDirectoryW(Some(&mut dir)) } as usize;
    if n == 0 || n >= dir.len() {
        return None;
    }
    let mut root = [0u16; 260];
    let mut vol = [0u16; 64];
    unsafe {
        if GetVolumePathNameW(PCWSTR(dir.as_ptr()), &mut root).is_err() || GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut vol).is_err() {
            return None;
        }
    }
    Some(wide_str(&vol).to_lowercase()).filter(|v| !v.is_empty())
}

/// A volume's drive letters ("E:"); folder mount points are left out.
fn letters_of(name: &str) -> Vec<String> {
    let mut buf = vec![0u16; 1024];
    let mut need = 0u32;
    if unsafe { GetVolumePathNamesForVolumeNameW(&HSTRING::from(name), Some(&mut buf), &mut need) }.is_err() {
        return Vec::new();
    }
    let mut v: Vec<String> = buf
        .split(|&c| c == 0)
        .map(String::from_utf16_lossy)
        .filter(|p| p.len() == 3 && p.ends_with(":\\"))
        .map(|p| p[..2].to_uppercase())
        .collect();
    v.sort();
    v
}

/// A volume's NT device ("\Device\HarddiskVolume3"), from its GUID name.
fn nt_device(guid_name: &str) -> String {
    let short = guid_name.trim_start_matches("\\\\?\\").trim_end_matches('\\');
    let mut buf = [0u16; 512];
    let n = unsafe { QueryDosDeviceW(&HSTRING::from(short), Some(&mut buf)) };
    if n == 0 { String::new() } else { wide_str(&buf) }
}

/// One volume as found: its GUID name (`\\?\Volume{...}\`), devnode (None:
/// not known), disks (None: could not be opened or mapped) and drive
/// letters.
struct Found {
    name: String,
    /// Its interface path (lowercase; "" for one without an interface).
    path: String,
    devnode: Option<u32>,
    disks: Option<Vec<(u32, u32)>>,
    letters: Vec<String>,
}

/// Every volume: those with a volume interface (and so a devnode), then
/// any other the mount manager lists (FindFirstVolumeW).
fn all_volumes() -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for (path, dn) in interfaces(&GUID_DEVINTERFACE_VOLUME) {
        let mut vol = [0u16; 64];
        let name = match unsafe { GetVolumeNameForVolumeMountPointW(&HSTRING::from(format!("{path}\\")), &mut vol) } {
            Ok(()) => wide_str(&vol),
            Err(_) => String::new(),
        };
        let disks = open(&path).and_then(|h| volume_disks(&h));
        let letters = if name.is_empty() { Vec::new() } else { letters_of(&name) };
        out.push(Found { name, path: path.to_lowercase(), devnode: Some(dn), disks, letters });
    }
    let mut buf = [0u16; 64];
    let Ok(find) = (unsafe { FindFirstVolumeW(&mut buf) }) else { return out };
    loop {
        let name = wide_str(&buf);
        if !out.iter().any(|v| v.name.eq_ignore_ascii_case(&name)) {
            let disks = open(name.trim_end_matches('\\')).and_then(|h| volume_disks(&h));
            out.push(Found { letters: letters_of(&name), name, path: String::new(), devnode: None, disks });
        }
        if unsafe { FindNextVolumeW(find, &mut buf) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindVolumeClose(find);
    }
    out
}

/// The disks never to offer: those of the Windows volume, the boot (EFI
/// system) partition and the page files. Err when any of them cannot be
/// found or mapped (then nothing is offered).
fn protected_disks(vols: &[Found]) -> Result<Vec<(u32, u32)>, String> {
    let windows = windows_volume().ok_or("the Windows volume is not known")?;
    let boot = reg_strings("SYSTEM\\Setup", "SystemPartition", false)?
        .and_then(|v| v.into_iter().next())
        .ok_or("the boot partition is not known")?;
    let pagefiles = match reg_strings("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Memory Management", "ExistingPageFiles", true)? {
        Some(v) => model::pagefile_letters(&v).ok_or("a page file's drive is not known")?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    let mut take = |what: &str, f: &dyn Fn(&Found) -> bool| -> Result<(), String> {
        let v = vols.iter().find(|v| f(v)).ok_or(format!("{what}: no such volume"))?;
        out.extend(v.disks.clone().ok_or(format!("{what}: its disks are not known"))?);
        Ok(())
    };
    take("the Windows volume", &|v| v.name.to_lowercase() == windows)?;
    take("the boot partition", &|v| !v.name.is_empty() && nt_device(&v.name).eq_ignore_ascii_case(&boot))?;
    for l in &pagefiles {
        take("a page file's drive", &|v| v.letters.contains(l))?;
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// A devnode's removal and ejection relations (CM_GETIDLIST_FILTER_
/// REMOVALRELATIONS / _EJECTRELATIONS), lowercase instance IDs. None:
/// Windows would not say for one of them.
fn relations_of(dn: u32) -> Option<Vec<String>> {
    let id = HSTRING::from(devnode_id(dn)?);
    let mut out = Vec::new();
    for filter in [CM_GETIDLIST_FILTER_REMOVALRELATIONS, CM_GETIDLIST_FILTER_EJECTRELATIONS] {
        let mut len = 0u32;
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, &id, filter) } != CR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; len.max(2) as usize];
        if unsafe { CM_Get_Device_ID_ListW(&id, &mut buf, filter) } != CR_SUCCESS {
            return None;
        }
        out.extend(buf.split(|&c| c == 0).map(|s| String::from_utf16_lossy(s).to_lowercase()).filter(|s| !s.is_empty()));
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// A devnode by its instance ID (None: not present).
fn locate(id: &str) -> Option<u32> {
    let mut dn = 0u32;
    (unsafe { CM_Locate_DevNodeW(&mut dn, &HSTRING::from(id), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS).then_some(dn)
}

/// Everything that would go with ejecting `target`: its closure (its
/// subtree and its removal and ejection relations, then theirs, and so on;
/// at most MAX_SCOPE devnodes, every query answered), the disks and CD
/// drives among them (opened), and whether a volume among them (or
/// anywhere, not placed) is unmapped or spans other disks.
fn scope_of(target: u32, ifs: &[DiskIf], vols: &[Found], lost_volumes: bool) -> (model::Scope, Vec<(u32, (u32, u32), i32, u64)>) {
    let mut scope = model::Scope { unmapped: lost_volumes, ..Default::default() };
    // A related ID that cannot be located is a failed query too.
    let related = |dn: u32| relations_of(dn)?.iter().map(|id| locate(id)).collect::<Option<Vec<u32>>>();
    let Some(all) = model::closure(target, subtree_of, related, MAX_SCOPE) else { return (scope, Vec::new()) };
    let nodes: Vec<(u32, Node)> = all.iter().map(|&d| (d, node_of(d))).collect();
    scope.nodes = Some(nodes.iter().map(|x| x.1.clone()).collect());
    // The disks and CD drives: the target itself, and in its closure.
    let mut media: Vec<u32> = Vec::new();
    if model::is_media(&node_of(target).class) {
        media.push(target);
    }
    media.extend(nodes.iter().filter(|(_, n)| model::is_media(&n.class)).map(|x| x.0));
    media.sort_unstable();
    media.dedup();
    let mut opened = Vec::new();
    let mut all_known = true;
    for dn in &media {
        match ifs.iter().find(|i| i.devnode == *dn).and_then(open_disk) {
            Some(o) => opened.push((*dn, o.key, o.bus, o.size)),
            None => all_known = false,
        }
    }
    scope.disks = all_known.then(|| opened.iter().map(|o| (o.1, o.2)).collect());
    // Volumes: those in the closure must be mapped, and lie only on these
    // disks; so must every volume on these disks.
    let keys: Vec<(u32, u32)> = opened.iter().map(|o| o.1).collect();
    for (dn, n) in &nodes {
        if n.class.eq_ignore_ascii_case("volume") {
            match vols.iter().find(|v| v.devnode == Some(*dn)).and_then(|v| v.disks.as_ref()) {
                Some(d) if d.iter().all(|k| keys.contains(k)) => {}
                Some(_) => scope.spanned = true,
                None => scope.unmapped = true,
            }
        }
    }
    for v in vols.iter().filter_map(|v| v.disks.as_ref()) {
        if v.iter().any(|k| keys.contains(k)) && !v.iter().all(|k| keys.contains(k)) {
            scope.spanned = true;
        }
    }
    (scope, opened)
}

/// Everything ejectable now, grouped per device (none when the protected
/// disks cannot be told), and the notes on what was left out.
pub fn scan() -> Scan {
    let mut notes = Vec::new();
    let ifs = disk_interfaces();
    // Each disk's ejectable target that is storage itself, once per target.
    let mut targets: Vec<u32> = Vec::new();
    for d in &ifs {
        let chain = chain_of(d.devnode);
        let nodes: Vec<Node> = chain.iter().map(|c| c.1.clone()).collect();
        let Some(i) = model::ejectable_in(&nodes) else { continue };
        if let Err(why) = model::storage_target(&nodes, i) {
            notes.push(format!("{} not offered: {why:?}", devnode_name(d.devnode)));
            continue;
        }
        if !targets.contains(&chain[i].0) {
            targets.push(chain[i].0);
        }
    }
    if targets.is_empty() {
        return Scan { devices: Vec::new(), notes };
    }
    let vols = all_volumes();
    let protected = match protected_disks(&vols) {
        Ok(p) => p,
        Err(why) => {
            notes.push(format!("nothing offered: {why}"));
            return Scan { devices: Vec::new(), notes };
        }
    };
    // A volume not placed at all (no devnode, not mapped) could be on any
    // disk: then nothing is offered.
    let lost = vols.iter().any(|v| v.devnode.is_none() && v.disks.is_none());
    let mut infos: Vec<DiskInfo> = Vec::new();
    for t in targets {
        let name = devnode_name(t);
        let (scope, opened) = scope_of(t, &ifs, &vols, lost);
        if let Err(why) = model::check_scope(&scope, &protected) {
            notes.push(format!("{name} not offered: {why:?}"));
            continue;
        }
        let Some(eject_id) = devnode_id(t) else { continue };
        for (dn, key, bus, size) in opened {
            let cdrom = ifs.iter().any(|i| i.devnode == dn && i.cdrom);
            let path = ifs.iter().find(|i| i.devnode == dn).map(|i| i.path.to_lowercase()).unwrap_or_default();
            infos.push(DiskInfo { key, path, eject_id: eject_id.clone(), eject_name: name.clone(), name: devnode_name(dn), kind: model::kind_of(bus, cdrom), size });
        }
    }
    let keys: Vec<(u32, u32)> = infos.iter().map(|d| d.key).collect();
    let vol_infos: Vec<VolumeInfo> = vols
        .iter()
        .filter_map(|v| {
            let disks = v.disks.clone()?;
            disks.iter().any(|d| keys.contains(d)).then(|| {
                let mut label = [0u16; 261];
                let _ = unsafe { GetVolumeInformationW(&HSTRING::from(v.name.as_str()), Some(&mut label), None, None, None, None) };
                VolumeInfo { disks, letters: v.letters.clone(), label: wide_str(&label).trim().to_string(), path: v.path.clone(), guid: v.name.to_lowercase() }
            })
        })
        .collect();
    let mut devs = model::group(&infos, &vol_infos);
    for d in &mut devs {
        d.size_text = size_text(d.size);
    }
    Scan { devices: devs, notes }
}

/// "29.8 GB", as Explorer writes it ("" when unknown).
fn size_text(bytes: u64) -> String {
    if bytes == 0 {
        return String::new();
    }
    let mut buf = [0u16; 32];
    match unsafe { windows::Win32::UI::Shell::StrFormatByteSizeEx(bytes, windows::Win32::UI::Shell::SFBS_FLAGS_ROUND_TO_NEAREST_DISPLAYED_DIGIT, &mut buf) } {
        Ok(()) => wide_str(&buf),
        Err(_) => String::new(),
    }
}

/// Asks Windows to eject the devnode `id`: Ok only once Windows ejected it
/// with no veto, else why not (a devnode not found is a failure too).
/// Blocks while Windows asks every app and driver holding it (seconds at
/// most).
pub fn eject(id: &str) -> Result<(), Refusal> {
    let mut dn = 0u32;
    let cr = unsafe { CM_Locate_DevNodeW(&mut dn, &HSTRING::from(id), CM_LOCATE_DEVNODE_NORMAL) };
    if cr != CR_SUCCESS {
        crate::log!("removable: eject: devnode not found (CONFIGRET {:#x})", cr.0);
        return Err(Refusal::Failed);
    }
    let mut veto = PNP_VETO_TYPE(0);
    let mut name = [0u16; 260];
    let cr = unsafe { CM_Request_Device_EjectW(dn, Some(&mut veto), Some(&mut name), 0) };
    let out = model::outcome(cr.0, veto.0, &wide_str(&name));
    if out.is_err() {
        // Only an app's or a service's name is logged (other vetoes name a
        // device ID, which holds a serial number).
        let who = if matches!(veto.0, 3 | 4) { wide_str(&name) } else { String::new() };
        crate::log!("removable: eject refused (CONFIGRET {:#x}, veto {} {who})", cr.0, veto.0);
    }
    out
}
