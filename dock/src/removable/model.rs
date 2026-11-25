//! What removable storage there is and what each device's row says, as
//! plain data (no Windows calls here; scan.rs reads the system, mod.rs
//! keeps the state). Everything in this file is tested.
//!
//! The scope is narrow on purpose: only USB, SD and MMC storage is offered,
//! and only when everything that would go with the eject (the target's
//! subtree and its removal / ejection relations) is known to be storage
//! and holds nothing Windows needs. Anything not known is not offered.

use std::time::{Duration, Instant};

/// cfgmgr32's bits this file reads (CM_DEVCAP_*, DN_*), as numbers so the
/// rules below stay plain data.
pub const CAP_REMOVABLE: u32 = 0x4;
pub const CAP_DOCKDEVICE: u32 = 0x8;
pub const CAP_SURPRISEREMOVALOK: u32 = 0x80;
pub const DN_STARTED: u32 = 0x8;
pub const DN_REMOVABLE: u32 = 0x4000;

/// How long a device that went away after a successful eject still shows
/// its "Safe to remove" row, and how long a refused eject says why.
pub const SAFE_SHOW: Duration = Duration::from_secs(5);
pub const REFUSED_SHOW: Duration = Duration::from_secs(12);

/// Further looks for drive letters Windows had not assigned yet, after a
/// scan that found a volume without one (then nothing until the next
/// device or drive letter event).
pub const LATE_LOOKS: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(15)];

/// One devnode: its capabilities (CM_DRP_CAPABILITIES; None when Windows
/// would not say), status (CM_Get_DevNode_Status), setup class
/// (CM_DRP_CLASS, "DiskDrive", "USB"...) and service (CM_DRP_SERVICE,
/// "USBSTOR"...).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Node {
    pub caps: Option<u32>,
    pub status: u32,
    pub class: String,
    pub service: String,
}

/// Pure: which devnode of a disk's chain (`chain[0]` the disk or CD drive
/// itself, then its parent, and so on up) Windows' Safely Remove would
/// eject: the closest one that is removable (the capability, or the
/// status flag Windows sets from it) and running. None when there is none,
/// a capability on the way could not be read (the walk does not guess
/// past it), or the closest removable one is a dock (never eject a whole
/// dock) or may be pulled out as it is (SurpriseRemovalOK: Windows does
/// not offer it either). The walk never goes past the closest removable
/// devnode, so a stick in a hub ejects the stick, not the hub.
pub fn ejectable_in(chain: &[Node]) -> Option<usize> {
    for (i, n) in chain.iter().enumerate() {
        let caps = n.caps?;
        if caps & CAP_REMOVABLE == 0 && n.status & DN_REMOVABLE == 0 {
            continue;
        }
        if caps & (CAP_DOCKDEVICE | CAP_SURPRISEREMOVALOK) != 0 || n.status & DN_STARTED == 0 {
            return None;
        }
        return Some(i);
    }
    None
}

/// Services of devnodes that are storage and nothing else: USB mass
/// storage (bulk-only and UAS).
const STORAGE_SERVICES: [&str; 2] = ["usbstor", "uaspstor"];
/// Setup classes of storage controllers: SCSI and storage adapters (UAS),
/// SD host controllers.
const STORAGE_CLASSES: [&str; 2] = ["scsiadapter", "sdhost"];
/// Setup classes of disks and CD drives.
const MEDIA_CLASSES: [&str; 2] = ["diskdrive", "cdrom"];
/// Setup classes everything that goes with a storage target (its subtree,
/// relations, and theirs) may have: disks, CD drives, their volumes and
/// shadow copies.
const RELATED_CLASSES: [&str; 4] = ["diskdrive", "cdrom", "volume", "volumesnapshot"];
/// The only storage buses offered (STORAGE_BUS_TYPE): USB, SD, MMC. Never
/// SATA, SAS, NVMe, RAID, SCSI or virtual disks, removable or not.
pub const OFFERED_BUSES: [i32; 3] = [7, 12, 13];

fn is_one_of(s: &str, list: &[&str]) -> bool {
    list.iter().any(|x| s.eq_ignore_ascii_case(x))
}

/// Pure: a devnode of these classes is a disk or CD drive.
pub fn is_media(class: &str) -> bool {
    is_one_of(class, &MEDIA_CLASSES)
}

/// Why a removable devnode is not offered (logged, never with its ID: it
/// holds the device's serial number).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Skip {
    /// It is not storage itself (a composite device, a hub, a bridge):
    /// its class and service.
    NotStorage(String, String),
    /// Something that would go with it is not storage (HID, audio,
    /// network): that devnode's class.
    OtherFunction(String),
    /// A disk that would go with it is on a bus never offered (SATA, NVMe,
    /// SCSI, RAID, virtual...): the STORAGE_BUS_TYPE.
    Bus(i32),
    /// A disk that would go with it holds Windows, its boot partition or
    /// the page file.
    Protected,
    /// A volume on it spans disks that would not go with it.
    Spanned,
    /// A volume that would go with it could not be opened or mapped to its
    /// disks: whether it is safe to eject is not known.
    Unmapped,
    /// Windows would not fully say what goes with it (what it could not
    /// list).
    Unknown(&'static str),
}

/// Pure: whether the removable devnode `chain[i]` (from ejectable_in) is
/// storage itself: the disk, USB mass storage, or a storage or SD host
/// controller. A phone, a composite device or a hub is not.
pub fn storage_target(chain: &[Node], i: usize) -> Result<(), Skip> {
    let t = &chain[i];
    if i > 0 && !is_one_of(&t.service, &STORAGE_SERVICES) && !is_one_of(&t.class, &STORAGE_CLASSES) {
        return Err(Skip::NotStorage(t.class.clone(), t.service.clone()));
    }
    Ok(())
}

/// One step of a devnode tree walk (CM_Get_Child / CM_Get_Sibling): the
/// next devnode, the end (CR_NO_SUCH_DEVNODE), or any other answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Next(u32),
    End,
    Error,
}

/// Pure: every devnode under `root` (not itself), walked with `child` and
/// `sibling`. None (not known, so not offered) when a step answered with
/// an error, or the tree is bigger than `max` or deeper than `depth`.
pub fn subtree(root: u32, child: impl Fn(u32) -> Step, sibling: impl Fn(u32) -> Step, max: usize, depth: usize) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((parent, d)) = stack.pop() {
        let mut cur = match child(parent) {
            Step::Next(c) => c,
            Step::End => continue,
            Step::Error => return None,
        };
        loop {
            if out.len() >= max || d >= depth {
                return None;
            }
            out.push(cur);
            stack.push((cur, d + 1));
            cur = match sibling(cur) {
                Step::Next(s) => s,
                Step::End => break,
                Step::Error => return None,
            };
        }
    }
    Some(out)
}

/// Pure: everything that would go with ejecting `target`, as a closure:
/// the target's subtree (`below`) and its removal and ejection relations
/// (`related`), then the subtree and relations of every devnode found that
/// way, and so on, each devnode once (the target not included). None (not
/// known, so not offered) when a query for any of them fails, or more
/// than `max` devnodes are reached.
pub fn closure(target: u32, below: impl Fn(u32) -> Option<Vec<u32>>, related: impl Fn(u32) -> Option<Vec<u32>>, max: usize) -> Option<Vec<u32>> {
    let mut seen = vec![target];
    let mut work = vec![target];
    while let Some(n) = work.pop() {
        for m in below(n)?.into_iter().chain(related(n)?) {
            if !seen.contains(&m) {
                if seen.len() > max {
                    return None;
                }
                seen.push(m);
                work.push(m);
            }
        }
    }
    seen.remove(0);
    Some(seen)
}

/// Everything that would go with ejecting a target, as scanned.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Scope {
    /// Every devnode in its closure (`closure`; None: not fully known).
    pub nodes: Option<Vec<Node>>,
    /// Each disk or CD drive that goes with it (under it or related):
    /// (its key, STORAGE_BUS_TYPE). None: one could not be identified.
    pub disks: Option<Vec<((u32, u32), i32)>>,
    /// A volume that goes with it could not be mapped to its disks (or a
    /// volume somewhere could not be placed at all).
    pub unmapped: bool,
    /// A volume that goes with it also lies on a disk that does not.
    pub spanned: bool,
}

/// Pure: the target may be offered only if everything that goes with it
/// is known, is storage (every devnode of its closure a disk, CD drive,
/// volume or shadow copy), is on a USB, SD or MMC bus, holds no
/// `protected` disk (Windows, boot, page file) and no volume that could
/// not be mapped or that spans other disks.
pub fn check_scope(s: &Scope, protected: &[(u32, u32)]) -> Result<(), Skip> {
    let nodes = s.nodes.as_ref().ok_or(Skip::Unknown("closure"))?;
    if let Some(n) = nodes.iter().find(|n| !is_one_of(&n.class, &RELATED_CLASSES)) {
        return Err(Skip::OtherFunction(n.class.clone()));
    }
    let disks = s.disks.as_ref().ok_or(Skip::Unknown("disks"))?;
    if disks.is_empty() {
        return Err(Skip::Unknown("disks"));
    }
    if let Some((_, bus)) = disks.iter().find(|(_, b)| !OFFERED_BUSES.contains(b)) {
        return Err(Skip::Bus(*bus));
    }
    if disks.iter().any(|(k, _)| protected.contains(k)) {
        return Err(Skip::Protected);
    }
    if s.unmapped {
        return Err(Skip::Unmapped);
    }
    if s.spanned {
        return Err(Skip::Spanned);
    }
    Ok(())
}

/// What a device is, for its glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    /// A USB stick or USB drive (bus type USB).
    #[default]
    Usb,
    /// An SD / MMC card.
    Card,
    /// A CD / DVD / Blu-ray drive.
    Disc,
}

/// Pure: the kind from the bus type and whether it is a CD drive.
pub fn kind_of(bus: i32, cdrom: bool) -> Kind {
    match bus {
        _ if cdrom => Kind::Disc,
        // BusTypeSd, BusTypeMmc.
        12 | 13 => Kind::Card,
        _ => Kind::Usb,
    }
}

/// A disk (or CD drive) that goes with an offered target, as scanned.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DiskInfo {
    /// (device type, number) from IOCTL_STORAGE_GET_DEVICE_NUMBER: how
    /// volumes find their disk.
    pub key: (u32, u32),
    /// Its interface path (lowercase): a removal notice names it.
    pub path: String,
    /// The devnode to eject: its instance ID and its own name.
    pub eject_id: String,
    pub eject_name: String,
    /// The disk's own name ("SanDisk Ultra USB Device").
    pub name: String,
    pub kind: Kind,
    /// Bytes (0: unknown, or no medium in it).
    pub size: u64,
}

/// A volume, as scanned.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct VolumeInfo {
    /// Every disk it lies on (DiskInfo::key; several for a spanned or
    /// striped volume).
    pub disks: Vec<(u32, u32)>,
    /// Drive letters ("E:"), sorted.
    pub letters: Vec<String>,
    pub label: String,
    /// Its interface path (lowercase; "" when it has none) and GUID name
    /// (`\\?\Volume{...}\`, lowercase): a removal notice names the one, a
    /// medium swapped for another changes the other.
    pub path: String,
    pub guid: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Volume {
    pub letters: Vec<String>,
    pub label: String,
}

/// One device the user can eject: everything under one ejectable devnode.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Device {
    /// The ejectable devnode's instance ID.
    pub id: String,
    /// Which connection of it this is (assign_gens): a device unplugged
    /// and plugged in again, or given another medium, is a new one, and
    /// nothing asked of the old one applies to it.
    pub gen: u64,
    pub name: String,
    pub kind: Kind,
    /// Bytes, all its disks together (0: unknown), and as Windows writes
    /// it ("29.8 GB"; filled in by the scan, "" when unknown).
    pub size: u64,
    pub size_text: String,
    pub volumes: Vec<Volume>,
    /// Its disks' interface paths, its volumes' interface paths and its
    /// volumes' GUID names (all lowercase, sorted).
    pub paths: Vec<String>,
    pub vol_paths: Vec<String>,
    pub vol_guids: Vec<String>,
}

impl Device {
    /// Its drive letters, in order ("E:", "F:").
    pub fn letters(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.volumes.iter().flat_map(|x| x.letters.iter().map(String::as_str)).collect();
        v.sort_unstable();
        v
    }

    /// Where the list puts it: by its first letter (devices without one
    /// last), then by name.
    fn order_key(&self) -> (bool, String, String) {
        let first = self.letters().first().map(|s| s.to_string());
        (first.is_none(), first.unwrap_or_default(), self.name.to_lowercase())
    }

    /// This connection of the device (status and ejects are bound to it).
    pub fn conn(&self) -> Conn {
        Conn { id: self.id.clone(), gen: self.gen }
    }

    /// A removal notice named one of its disks or volumes.
    pub fn touched_by(&self, removed: &[String]) -> bool {
        self.paths.iter().chain(&self.vol_paths).any(|p| removed.contains(p))
    }

    /// The second line: the volumes' letters and labels ("E: USB DISK ·
    /// F:"), with the size when known.
    pub fn detail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut vols: Vec<&Volume> = self.volumes.iter().filter(|v| !v.letters.is_empty() || !v.label.is_empty()).collect();
        vols.sort_by(|a, b| a.letters.first().cmp(&b.letters.first()));
        for v in vols {
            let letters = v.letters.join(" ");
            parts.push(match (letters.is_empty(), v.label.is_empty()) {
                (false, false) => format!("{letters} {}", v.label),
                (false, true) => letters,
                (true, _) => v.label.clone(),
            });
        }
        if !self.size_text.is_empty() {
            parts.push(self.size_text.clone());
        }
        parts.join(" · ")
    }
}

/// Pure: the devices, one per ejectable devnode, with their volumes. The
/// name is the disk's own unless several disks share the devnode (a card
/// reader with several slots): then the devnode's. Sorted by first drive
/// letter, then name; volumes in letter order.
pub fn group(disks: &[DiskInfo], volumes: &[VolumeInfo]) -> Vec<Device> {
    let mut out: Vec<(Device, usize)> = Vec::new();
    for d in disks {
        let i = match out.iter().position(|(x, _)| x.id == d.eject_id) {
            Some(i) => i,
            None => {
                out.push((Device { id: d.eject_id.clone(), name: d.name.clone(), kind: d.kind, ..Default::default() }, 0));
                out.len() - 1
            }
        };
        let (dev, n) = &mut out[i];
        *n += 1;
        if *n > 1 && !d.eject_name.is_empty() {
            dev.name = d.eject_name.clone();
        }
        if dev.name.is_empty() {
            dev.name = d.eject_name.clone();
        }
        dev.size += d.size;
        dev.paths.push(d.path.clone());
        for v in volumes.iter().filter(|v| v.disks.contains(&d.key)) {
            if dev.vol_guids.contains(&v.guid) && !v.guid.is_empty() {
                continue;
            }
            dev.volumes.push(Volume { letters: v.letters.clone(), label: v.label.clone() });
            if !v.path.is_empty() {
                dev.vol_paths.push(v.path.clone());
            }
            dev.vol_guids.push(v.guid.clone());
        }
    }
    let mut devs: Vec<Device> = out.into_iter().map(|(d, _)| d).collect();
    for d in &mut devs {
        d.volumes.sort_by(|a, b| (a.letters.is_empty(), a.letters.first()).cmp(&(b.letters.is_empty(), b.letters.first())));
        d.paths.sort();
        d.vol_paths.sort();
        d.vol_guids.sort();
    }
    devs.sort_by_key(|d| d.order_key());
    devs
}

/// Removal notices since the last look: the interface paths they named,
/// and whether some were lost (more than the list keeps): then any device
/// may have changed.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Notices {
    pub paths: Vec<String>,
    pub overflowed: bool,
}

impl Notices {
    /// Notices that may concern `d`: one named its disks or volumes, or
    /// some were lost.
    pub fn touch(&self, d: &Device) -> bool {
        self.overflowed || d.touched_by(&self.paths)
    }
}

/// Pure: each scanned device's connection number. A device keeps the one
/// it had in the last scan (`prev`) only while nothing says it changed:
/// same ID, same size, the same volumes (by GUID: another medium of the
/// same size has others), and no removal notice since that scan
/// (`removed`) naming one of its disks or volumes, then or now (unplugged
/// and back within one burst of notices), and no notice lost (then every
/// device gets a new one). Otherwise it gets a new one from `counter`.
pub fn assign_gens(prev: &[Device], next: &mut [Device], removed: &Notices, counter: &mut u64) {
    for d in next {
        let kept = prev
            .iter()
            .find(|p| p.id == d.id && p.size == d.size && p.vol_guids == d.vol_guids)
            .filter(|p| !removed.touch(p) && !removed.touch(d));
        d.gen = match kept {
            Some(p) => p.gen,
            None => {
                *counter += 1;
                *counter
            }
        };
    }
}

/// One connection of a device: its devnode ID and connection number.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Conn {
    pub id: String,
    pub gen: u64,
}

/// Pure: an eject asked for `c` still goes to the device as it is now:
/// the same connection is present (else it was unplugged, plugged in
/// again or given another medium meanwhile: the eject is dropped).
pub fn still_there(present: &[Device], c: &Conn) -> bool {
    present.iter().any(|d| d.id == c.id && d.gen == c.gen)
}

/// Pure: right before ejecting, after a fresh scan (`present`) and with
/// the removal notices that came in during that scan (`mid_scan`): the
/// device to eject, or "Couldn't eject — try again" when that connection
/// is gone, a notice named one of its disks or volumes meanwhile, or
/// notices were lost.
pub fn eject_go(present: &[Device], c: &Conn, mid_scan: &Notices) -> Result<Device, Refusal> {
    let d = present.iter().find(|d| d.id == c.id && d.gen == c.gen).ok_or(Refusal::Failed)?;
    if mid_scan.touch(d) {
        return Err(Refusal::Failed);
    }
    Ok(d.clone())
}

/// Why Windows would not eject a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Something has it open: an app or service when Windows said which.
    InUse(Option<String>),
    /// Its driver cannot be stopped this way (a legacy device or driver).
    Unsupported,
    /// Anything else (not found, changed, no rights...).
    Failed,
}

/// cfgmgr32's results this file reads (CONFIGRET).
pub const CR_SUCCESS: u32 = 0;
pub const CR_REMOVE_VETOED: u32 = 0x17;

/// Pure: CM_Request_Device_EjectW's answer (its CONFIGRET, the PNP_VETO_TYPE
/// and veto name it filled in) as Ok (ejected) or why not. Ok only for
/// success with no veto at all; any other answer is a failure (even "gone
/// already": it is never said to be safe unless Windows ejected it). An
/// app's veto names its module (a path: its file name is kept), a
/// service's its name; the other vetoes name a device ID, which says
/// nothing to the user.
pub fn outcome(cr: u32, veto: i32, name: &str) -> Result<(), Refusal> {
    match (cr, veto) {
        (CR_SUCCESS, 0) => Ok(()),
        (CR_SUCCESS | CR_REMOVE_VETOED, _) => match veto {
            // PNP_VetoWindowsApp, PNP_VetoWindowsService.
            3 | 4 => {
                let n = name.trim().trim_end_matches('\\');
                let n = n.rsplit(['\\', '/']).next().unwrap_or(n).trim();
                Err(Refusal::InUse((!n.is_empty()).then(|| n.to_string())))
            }
            // Unknown, pending close, outstanding open, device, driver:
            // something holds it.
            0 | 2 | 5 | 6 | 7 => Err(Refusal::InUse(None)),
            // Legacy device, legacy driver: the operation is not supported.
            1 | 11 => Err(Refusal::Unsupported),
            // Illegal request, power, non-disableable, rights, already
            // removed, others.
            _ => Err(Refusal::Failed),
        },
        _ => Err(Refusal::Failed),
    }
}

/// Pure: the locale key a refusal is said with, and the name to fill in
/// ({name}) if any.
pub fn refusal_words(r: &Refusal) -> (&'static str, Option<&str>) {
    match r {
        Refusal::InUse(Some(n)) => ("control.eject.in_use_by", Some(n.as_str())),
        Refusal::InUse(None) => ("control.eject.in_use", None),
        Refusal::Unsupported => ("control.eject.unsupported", None),
        Refusal::Failed => ("control.eject.failed", None),
    }
}

/// An eject asked for one connection of a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ejecting,
    /// Ejected at `at`; the device as it was (its row stays a moment after
    /// it went away).
    Safe { at: Instant, device: Device },
    Refused { at: Instant, why: Refusal },
}

/// What a device's row says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// Nothing asked: its letters and labels.
    Ready,
    Ejecting,
    Safe,
    Refused(Refusal),
}

/// One row of the Control Center's section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub device: Device,
    pub note: Note,
}

/// Pure: the rows, from the devices present and the ejects asked for (by
/// connection) at `now`: every present device with its connection's note
/// (a refusal for REFUSED_SHOW, "Safe to remove" for SAFE_SHOW), and a
/// device that went away after its eject for SAFE_SHOW, in its place in
/// the order, unless it is back already (a new connection: no note).
pub fn shown(present: &[Device], status: &[(Conn, Status)], now: Instant) -> Vec<Shown> {
    let of = |d: &Device| status.iter().find(|(c, _)| c.id == d.id && c.gen == d.gen).map(|(_, s)| s);
    let mut out: Vec<Shown> = present
        .iter()
        .map(|d| {
            let note = match of(d) {
                Some(Status::Ejecting) => Note::Ejecting,
                Some(Status::Safe { at, .. }) if now.saturating_duration_since(*at) < SAFE_SHOW => Note::Safe,
                Some(Status::Refused { at, why }) if now.saturating_duration_since(*at) < REFUSED_SHOW => Note::Refused(why.clone()),
                _ => Note::Ready,
            };
            Shown { device: d.clone(), note }
        })
        .collect();
    for (c, s) in status {
        if let Status::Safe { at, device } = s {
            if now.saturating_duration_since(*at) < SAFE_SHOW && !present.iter().any(|d| d.id == c.id) {
                out.push(Shown { device: device.clone(), note: Note::Safe });
            }
        }
    }
    out.sort_by_key(|s| s.device.order_key());
    out
}

/// Pure: the Control Center shows its removable storage section only while
/// it has a row: a device is connected, or one just ejected says it is
/// safe to remove.
pub fn section_visible(rows: &[Shown]) -> bool {
    !rows.is_empty()
}

/// Pure: the statuses still worth keeping at `now` with these devices
/// present: an eject on its way and notes still shown, but nothing of a
/// connection that is over (its device is present again as another one).
pub fn prune(status: &mut Vec<(Conn, Status)>, present: &[Device], now: Instant) {
    status.retain(|(c, s)| {
        if present.iter().any(|d| d.id == c.id && d.gen != c.gen) {
            return false;
        }
        match s {
            Status::Ejecting => true,
            Status::Safe { at, .. } => now.saturating_duration_since(*at) < SAFE_SHOW,
            Status::Refused { at, .. } => now.saturating_duration_since(*at) < REFUSED_SHOW,
        }
    });
}

/// Pure: how long until the next note runs out (the panel redraws then).
pub fn next_change(status: &[(Conn, Status)], now: Instant) -> Option<Duration> {
    status
        .iter()
        .filter_map(|(_, s)| match s {
            Status::Ejecting => None,
            Status::Safe { at, .. } => (*at + SAFE_SHOW).checked_duration_since(now),
            Status::Refused { at, .. } => (*at + REFUSED_SHOW).checked_duration_since(now),
        })
        .min()
}

/// How long after a failed registration for device notices it is tried
/// again (then not again until NopBar restarts).
pub const LISTEN_RETRIES: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(30), Duration::from_secs(300)];

/// Pure: the devices offered: none at all without device notices (every
/// check of a connection depends on them; nothing is scanned then either).
pub fn offered(scanned: Vec<Device>, notifying: bool) -> Vec<Device> {
    if notifying { scanned } else { Vec::new() }
}

/// Pure: whether a scan's devices still wait for drive letters (a device
/// with no volume yet, or a volume with no letter), and so the next late
/// look after `done` of them (None: no more).
pub fn next_look(devs: &[Device], done: usize) -> Option<Duration> {
    let waiting = devs.iter().any(|d| d.volumes.is_empty() || d.volumes.iter().any(|v| v.letters.is_empty()));
    if waiting { LATE_LOOKS.get(done).copied() } else { None }
}

/// Pure: the drive letters holding page files, from the registry's
/// ExistingPageFiles (`\??\C:\pagefile.sys`). None when an entry names no
/// drive letter: the page file's disk is then not known (and nothing is
/// offered).
pub fn pagefile_letters(entries: &[String]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for e in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let path = e.strip_prefix(r"\??\").unwrap_or(e);
        let mut c = path.chars();
        match (c.next(), c.next()) {
            (Some(l), Some(':')) if l.is_ascii_alphabetic() => out.push(format!("{}:", l.to_ascii_uppercase())),
            _ => return None,
        }
    }
    out.sort();
    out.dedup();
    Some(out)
}

/// Pure: a WM_DEVICECHANGE broadcast that means drive letters came or went
/// (DBT_DEVICEARRIVAL / DBT_DEVICEREMOVECOMPLETE for a DBT_DEVTYP_VOLUME).
pub fn letters_changed(event: usize, devicetype: u32) -> bool {
    matches!(event, 0x8000 | 0x8004) && devicetype == 2
}

/// Pure: a screen reader's name for a row's button: "Eject <name> (E:)",
/// from the two locale patterns (with and without letters).
pub fn eject_name(with_letters: &str, plain: &str, d: &Device) -> String {
    let letters = d.letters();
    if letters.is_empty() {
        plain.replace("{name}", &d.name)
    } else {
        with_letters.replace("{name}", &d.name).replace("{drives}", &letters.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(caps: u32, status: u32) -> Node {
        Node { caps: Some(caps), status, ..Default::default() }
    }

    fn typed(caps: u32, status: u32, class: &str, service: &str) -> Node {
        Node { caps: Some(caps), status, class: class.into(), service: service.into() }
    }

    fn class(c: &str) -> Node {
        Node { caps: Some(0), status: RUNNING, class: c.into(), service: String::new() }
    }

    const RUNNING: u32 = DN_STARTED;
    const USB: i32 = 7;
    const SATA: i32 = 11;

    #[test]
    fn the_closest_removable_devnode_is_ejected() {
        // USB stick: disk (not removable) -> USB mass storage (removable)
        // -> hub (removable too: on an external port) -> controller.
        let stick = [node(0, RUNNING), node(CAP_REMOVABLE, RUNNING | DN_REMOVABLE), node(CAP_REMOVABLE, RUNNING), node(0, RUNNING)];
        assert_eq!(ejectable_in(&stick), Some(1), "the stick, never its hub");
        // The disk itself removable.
        assert_eq!(ejectable_in(&[node(CAP_REMOVABLE, RUNNING), node(0, RUNNING)]), Some(0));
        // Only the status flag says so: still removable.
        assert_eq!(ejectable_in(&[node(0, RUNNING), node(0, RUNNING | DN_REMOVABLE)]), Some(1));
        // An internal disk: nothing to eject.
        assert_eq!(ejectable_in(&[node(0, RUNNING), node(0, RUNNING), node(0, RUNNING)]), None);
        assert_eq!(ejectable_in(&[]), None);
        // A capability Windows would not give: no guessing past it.
        let unread = Node { caps: None, status: RUNNING, ..Default::default() };
        assert_eq!(ejectable_in(&[node(0, RUNNING), unread, node(CAP_REMOVABLE, RUNNING)]), None);
    }

    #[test]
    fn docks_surprise_removal_and_stopped_devices_are_not_offered() {
        assert_eq!(ejectable_in(&[node(0, RUNNING), node(CAP_REMOVABLE | CAP_DOCKDEVICE, RUNNING), node(CAP_REMOVABLE, RUNNING)]), None);
        assert_eq!(ejectable_in(&[node(0, RUNNING), node(CAP_REMOVABLE | CAP_SURPRISEREMOVALOK, RUNNING), node(CAP_REMOVABLE, RUNNING)]), None);
        assert_eq!(ejectable_in(&[node(0, 0), node(CAP_REMOVABLE, DN_REMOVABLE)]), None);
    }

    #[test]
    fn only_storage_targets() {
        let disk = typed(0, RUNNING, "DiskDrive", "disk");
        let stick = [disk.clone(), typed(CAP_REMOVABLE, RUNNING, "USB", "USBSTOR")];
        assert_eq!(storage_target(&stick, 1), Ok(()));
        assert_eq!(storage_target(&[disk.clone(), typed(CAP_REMOVABLE, RUNNING, "SCSIAdapter", "UASPStor")], 1), Ok(()));
        assert_eq!(storage_target(&[typed(CAP_REMOVABLE, RUNNING, "DiskDrive", "disk")], 0), Ok(()), "the disk itself");
        // A composite device (a phone, a keyboard with a card slot).
        let composite = [disk.clone(), typed(0, RUNNING, "USB", "USBSTOR"), typed(CAP_REMOVABLE, RUNNING, "USB", "usbccgp")];
        assert_eq!(ejectable_in(&composite), Some(2));
        assert_eq!(storage_target(&composite, 2), Err(Skip::NotStorage("USB".into(), "usbccgp".into())));
        // A SATA controller is not a target either (hdc).
        assert!(storage_target(&[disk, typed(CAP_REMOVABLE, RUNNING, "HDC", "storahci")], 1).is_err());
    }

    /// A plain USB stick's scope: one disk under it, its volume related.
    fn stick_scope() -> Scope {
        Scope {
            nodes: Some(vec![class("DiskDrive"), class("Volume"), class("VolumeSnapshot")]),
            disks: Some(vec![((7, 2), USB)]),
            unmapped: false,
            spanned: false,
        }
    }

    #[test]
    fn usb_is_offered_sata_never() {
        let protected = [(7, 0)];
        assert_eq!(check_scope(&stick_scope(), &protected), Ok(()));
        // SD and MMC too; a USB DVD drive.
        for bus in [12, 13] {
            assert_eq!(check_scope(&Scope { disks: Some(vec![((7, 2), bus)]), ..stick_scope() }, &protected), Ok(()));
        }
        assert_eq!(check_scope(&Scope { nodes: Some(vec![class("CDROM")]), disks: Some(vec![((2, 0), USB)]), ..stick_scope() }, &protected), Ok(()));
        // A removable SATA disk (hot-plug bay), NVMe, SCSI, RAID, virtual:
        // never, removable bit or not.
        for bus in [SATA, 17, 1, 8, 14, 0] {
            assert_eq!(check_scope(&Scope { nodes: Some(vec![]), disks: Some(vec![((7, 1), bus)]), ..stick_scope() }, &protected), Err(Skip::Bus(bus)));
        }
        // Mixed: a USB target that would also take a SATA disk with it.
        assert_eq!(check_scope(&Scope { disks: Some(vec![((7, 2), USB), ((7, 3), SATA)]), ..stick_scope() }, &protected), Err(Skip::Bus(SATA)));
    }

    #[test]
    fn relations_and_subtree_must_be_storage_and_known() {
        let p = [(7, 0)];
        // Related HID or network functions: not offered.
        for c in ["HIDClass", "Net", "AudioEndpoint", ""] {
            let s = Scope { nodes: Some(vec![class("DiskDrive"), class("Volume"), class(c)]), ..stick_scope() };
            assert_eq!(check_scope(&s, &p), Err(Skip::OtherFunction(c.into())), "{c}");
        }
        // A HID child under it.
        assert_eq!(check_scope(&Scope { nodes: Some(vec![class("DiskDrive"), class("HIDClass")]), ..stick_scope() }, &p), Err(Skip::OtherFunction("HIDClass".into())));
        // The relations query failed; the subtree walk failed; a disk could
        // not be identified; nothing at all to eject.
        assert_eq!(check_scope(&Scope { nodes: None, ..stick_scope() }, &p), Err(Skip::Unknown("closure")));
        assert_eq!(check_scope(&Scope { disks: None, ..stick_scope() }, &p), Err(Skip::Unknown("disks")));
        assert_eq!(check_scope(&Scope { disks: Some(vec![]), ..stick_scope() }, &p), Err(Skip::Unknown("disks")));
        // Unmapped or spanned volumes.
        assert_eq!(check_scope(&Scope { unmapped: true, ..stick_scope() }, &p), Err(Skip::Unmapped));
        assert_eq!(check_scope(&Scope { spanned: true, ..stick_scope() }, &p), Err(Skip::Spanned));
    }

    #[test]
    fn a_protected_disk_anywhere_in_the_scope_keeps_it_out() {
        // The Windows (or boot, or page file) disk reached through the
        // relations, not under the target.
        let s = Scope { disks: Some(vec![((7, 2), USB), ((7, 5), USB)]), ..stick_scope() };
        assert_eq!(check_scope(&s, &[(7, 5)]), Err(Skip::Protected));
        assert_eq!(check_scope(&s, &[(7, 0)]), Ok(()));
    }

    /// A fake devnode tree: children of each devnode in order, and devnodes
    /// whose next sibling step fails.
    fn walk(tree: &[(u32, &[u32])], broken: &[u32]) -> Option<Vec<u32>> {
        let kids = |p: u32| tree.iter().find(|(n, _)| *n == p).map(|(_, k)| k.to_vec()).unwrap_or_default();
        let parent_of = |c: u32| tree.iter().find(|(_, k)| k.contains(&c)).map(|(n, _)| *n);
        subtree(
            1,
            |p| kids(p).first().map(|&c| Step::Next(c)).unwrap_or(Step::End),
            |c| {
                if broken.contains(&c) {
                    return Step::Error;
                }
                let sibs = parent_of(c).map(kids).unwrap_or_default();
                let i = sibs.iter().position(|&x| x == c).unwrap_or(usize::MAX);
                sibs.get(i.wrapping_add(1)).map(|&s| Step::Next(s)).unwrap_or(Step::End)
            },
            64,
            6,
        )
    }

    #[test]
    fn the_subtree_is_walked_and_a_sibling_error_is_unknown() {
        let tree: &[(u32, &[u32])] = &[(1, &[2, 3]), (2, &[4]), (3, &[])];
        let mut all = walk(tree, &[]).unwrap();
        all.sort();
        assert_eq!(all, [2, 3, 4]);
        // The step after the first child fails (not the end): not known,
        // so not offered.
        assert_eq!(walk(tree, &[2]), None);
        assert_eq!(walk(&[(1, &[])], &[]), Some(vec![]));
        // A child step that fails, and a tree too big, are not known either.
        assert_eq!(subtree(1, |_| Step::Error, |_| Step::End, 64, 6), None);
        assert_eq!(subtree(1, |p| if p < 100 { Step::Next(p + 1) } else { Step::End }, |_| Step::End, 64, 6), None);
    }

    #[test]
    fn kinds_follow_the_bus() {
        assert_eq!(kind_of(7, false), Kind::Usb);
        assert_eq!(kind_of(12, false), Kind::Card);
        assert_eq!(kind_of(13, false), Kind::Card);
        assert_eq!(kind_of(7, true), Kind::Disc, "a USB DVD drive");
    }

    fn disk(n: u32, id: &str, name: &str, size: u64) -> DiskInfo {
        DiskInfo { key: (7, n), path: format!("disk{n}"), eject_id: id.into(), eject_name: "USB Mass Storage Device".into(), name: name.into(), kind: Kind::Usb, size }
    }

    fn vol(n: u32, letters: &[&str], label: &str) -> VolumeInfo {
        VolumeInfo {
            disks: vec![(7, n)],
            letters: letters.iter().map(|s| s.to_string()).collect(),
            label: label.into(),
            path: format!("vol-{n}-{label}"),
            guid: format!("guid-{n}-{label}"),
        }
    }

    #[test]
    fn volumes_are_grouped_per_device() {
        let disks = [disk(2, "USB\\B", "SanDisk Ultra", 32), disk(1, "USB\\A", "Kingston", 16), disk(3, "USB\\R", "SD", 64), disk(4, "USB\\R", "CF", 8)];
        let vols = [vol(2, &["F:"], "DATA"), vol(1, &["E:"], ""), vol(2, &["G:"], "BOOT"), vol(3, &["H:"], "CARD"), vol(9, &["C:"], "Windows")];
        let devs = group(&disks, &vols);
        assert_eq!(devs.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["USB\\A", "USB\\B", "USB\\R"], "by first letter");
        assert_eq!(devs[0].name, "Kingston");
        assert_eq!(devs[1].letters(), ["F:", "G:"]);
        assert_eq!(devs[1].volumes[0].label, "DATA");
        assert_eq!(devs[1].size, 32);
        assert_eq!(devs[1].vol_paths, ["vol-2-BOOT", "vol-2-DATA"]);
        assert_eq!(devs[1].vol_guids, ["guid-2-BOOT", "guid-2-DATA"]);
        // A reader with two slots is one device, under the reader's name.
        assert_eq!(devs[2].name, "USB Mass Storage Device");
        assert_eq!(devs[2].size, 72);
        assert_eq!(devs[2].letters(), ["H:"]);
        assert_eq!(devs[2].paths, ["disk3", "disk4"]);
        // The internal disk's volume belongs to no device.
        assert!(devs.iter().all(|d| !d.letters().contains(&"C:")));
        // A device without letters comes last.
        let devs = group(&[disk(1, "USB\\A", "Empty reader", 0), disk(2, "USB\\B", "Stick", 0)], &[vol(2, &["E:"], "")]);
        assert_eq!(devs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Stick", "Empty reader"]);
    }

    #[test]
    fn the_detail_line_lists_letters_labels_and_size() {
        let mut d = group(&[disk(2, "USB\\B", "Stick", 0)], &[vol(2, &["G:"], "BOOT"), vol(2, &["F:"], "DATA"), vol(2, &[], "")]).remove(0);
        assert_eq!(d.detail(), "F: DATA · G: BOOT");
        d.size_text = "32 GB".into();
        assert_eq!(d.detail(), "F: DATA · G: BOOT · 32 GB");
        assert_eq!(Device::default().detail(), "");
        assert_eq!(eject_name("Eject {name} ({drives})", "Eject {name}", &d), "Eject Stick (F:, G:)");
        assert_eq!(eject_name("Eject {name} ({drives})", "Eject {name}", &Device { name: "Reader".into(), ..Default::default() }), "Eject Reader");
        // The page file's drives, from the registry's own spelling.
        let pf = |v: &[&str]| pagefile_letters(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(pf(&[r"\??\C:\pagefile.sys", r"\??\d:\pagefile.sys", ""]), Some(vec!["C:".into(), "D:".into()]));
        assert_eq!(pf(&[]), Some(vec![]), "no page file");
        assert_eq!(pf(&[r"\Device\HarddiskVolume3\pagefile.sys"]), None, "not known: nothing offered");
    }

    #[test]
    fn vetoes_say_what_holds_the_device() {
        assert_eq!(outcome(CR_SUCCESS, 0, ""), Ok(()));
        assert_eq!(outcome(0x0D, 0, ""), Err(Refusal::Failed), "no such devnode");
        assert_eq!(outcome(0x05, 0, ""), Err(Refusal::Failed), "out of memory");
        assert_eq!(outcome(CR_REMOVE_VETOED, 13, ""), Err(Refusal::Failed), "already removed");
        assert_eq!(outcome(CR_SUCCESS, 5, ""), Err(Refusal::InUse(None)), "success, but vetoed");
        assert_eq!(outcome(CR_REMOVE_VETOED, 3, "C:\\Windows\\System32\\notepad.exe"), Err(Refusal::InUse(Some("notepad.exe".into()))));
        assert_eq!(outcome(CR_REMOVE_VETOED, 4, "WSearch"), Err(Refusal::InUse(Some("WSearch".into()))));
        assert_eq!(outcome(CR_REMOVE_VETOED, 3, "  "), Err(Refusal::InUse(None)));
        assert_eq!(outcome(CR_REMOVE_VETOED, 5, "STORAGE\\Volume\\{1234}"), Err(Refusal::InUse(None)));
        assert_eq!(outcome(CR_REMOVE_VETOED, 6, "USBSTOR\\Disk"), Err(Refusal::InUse(None)));
        assert_eq!(outcome(CR_REMOVE_VETOED, 0, ""), Err(Refusal::InUse(None)));
        assert_eq!(outcome(CR_REMOVE_VETOED, 1, "x"), Err(Refusal::Unsupported));
        assert_eq!(outcome(CR_REMOVE_VETOED, 11, "legacysvc"), Err(Refusal::Unsupported));
        assert_eq!(outcome(CR_REMOVE_VETOED, 12, ""), Err(Refusal::Failed), "no rights");
        assert_eq!(outcome(CR_REMOVE_VETOED, 10, ""), Err(Refusal::Failed));
        assert_eq!(outcome(0x1F, 0, ""), Err(Refusal::Failed));
        assert_eq!(refusal_words(&Refusal::InUse(Some("notepad.exe".into()))), ("control.eject.in_use_by", Some("notepad.exe")));
        assert_eq!(refusal_words(&Refusal::InUse(None)), ("control.eject.in_use", None));
        assert_eq!(refusal_words(&Refusal::Unsupported), ("control.eject.unsupported", None));
        assert_eq!(refusal_words(&Refusal::Failed), ("control.eject.failed", None));
    }

    fn dev(id: &str, letter: &str, gen: u64) -> Device {
        Device {
            id: id.into(),
            gen,
            name: id.into(),
            volumes: vec![Volume { letters: vec![letter.into()], label: String::new() }],
            paths: vec![format!("{id}-disk")],
            vol_paths: vec![format!("{id}-vol")],
            vol_guids: vec![format!("{id}-guid")],
            ..Default::default()
        }
    }

    fn notices(paths: &[&str]) -> Notices {
        Notices { paths: paths.iter().map(|s| s.to_string()).collect(), overflowed: false }
    }

    fn conn(id: &str, gen: u64) -> Conn {
        Conn { id: id.into(), gen }
    }

    #[test]
    fn the_section_shows_while_a_device_is_there_or_just_ejected() {
        let t0 = Instant::now();
        let (a, b) = (dev("A", "E:", 1), dev("B", "F:", 2));
        assert!(!section_visible(&shown(&[], &[], t0)));
        let rows = shown(std::slice::from_ref(&a), &[], t0);
        assert!(section_visible(&rows));
        assert_eq!(rows[0].note, Note::Ready);
        let st = vec![(a.conn(), Status::Ejecting)];
        assert_eq!(shown(std::slice::from_ref(&a), &st, t0)[0].note, Note::Ejecting);
        let st = vec![(a.conn(), Status::Safe { at: t0, device: a.clone() })];
        let rows = shown(std::slice::from_ref(&b), &st, t0 + Duration::from_secs(1));
        assert_eq!(rows.iter().map(|r| (r.device.id.as_str(), r.note.clone())).collect::<Vec<_>>(), [("A", Note::Safe), ("B", Note::Ready)]);
        assert!(section_visible(&shown(&[], &st, t0 + Duration::from_secs(1))));
        assert!(!section_visible(&shown(&[], &st, t0 + SAFE_SHOW)));
        assert_eq!(next_change(&st, t0 + Duration::from_secs(1)), Some(SAFE_SHOW - Duration::from_secs(1)));
        assert_eq!(shown(std::slice::from_ref(&a), &st, t0 + SAFE_SHOW)[0].note, Note::Ready);
    }

    #[test]
    fn a_device_plugged_in_again_is_a_new_connection() {
        let t0 = Instant::now();
        let mut counter = 0;
        let mut first = vec![dev("A", "E:", 0)];
        assign_gens(&[], &mut first, &Notices::default(), &mut counter);
        assert_eq!(first[0].gen, 1);
        let mut again = vec![dev("A", "E:", 0)];
        assign_gens(&first, &mut again, &Notices::default(), &mut counter);
        assert_eq!(again[0].gen, 1, "nothing changed: the same connection");
        // Ejected, unplugged (a scan without it), back within SAFE_SHOW.
        let st = vec![(first[0].conn(), Status::Safe { at: t0, device: first[0].clone() })];
        let mut back = vec![dev("A", "E:", 0)];
        assign_gens(&[], &mut back, &Notices::default(), &mut counter);
        assert_eq!(back[0].gen, 2);
        let rows = shown(&back, &st, t0 + Duration::from_secs(1));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].note, Note::Ready, "not the old 'Safe to remove'");
        let mut pruned = st.clone();
        prune(&mut pruned, &back, t0 + Duration::from_secs(1));
        assert!(pruned.is_empty());
        // Back before a scan saw it gone: its disk's removal notice.
        let mut quick = vec![dev("A", "E:", 0)];
        assign_gens(&back, &mut quick, &notices(&["A-disk"]), &mut counter);
        assert_eq!(quick[0].gen, 3);
        // Another size: a new connection.
        let mut card = vec![Device { size: 64, ..dev("A", "E:", 0) }];
        assign_gens(&quick, &mut card, &Notices::default(), &mut counter);
        assert_eq!(card[0].gen, 4);
        assert!(!still_there(&card, &conn("A", 3)));
        assert!(still_there(&card, &conn("A", 4)));
        let mut st = vec![(conn("A", 3), Status::Ejecting)];
        prune(&mut st, &card, t0);
        assert!(st.is_empty());
    }

    #[test]
    fn a_volume_notice_or_a_volume_swap_is_a_new_connection() {
        let mut counter = 10;
        let prev = vec![dev("R", "E:", 10)];
        // A notice names one of its volume interfaces (the card was pulled
        // and another put in; the reader's disk stayed).
        let mut next = vec![dev("R", "E:", 0)];
        assign_gens(&prev, &mut next, &notices(&["R-vol"]), &mut counter);
        assert_eq!(next[0].gen, 11);
        // Same size, another card (another volume GUID), no notice seen.
        let mut swapped = vec![Device { vol_guids: vec!["other-guid".into()], ..dev("R", "E:", 0) }];
        assign_gens(&next, &mut swapped, &Notices::default(), &mut counter);
        assert_eq!(swapped[0].gen, 12);
        // The same card, nothing said: kept.
        let mut same = vec![Device { vol_guids: vec!["other-guid".into()], ..dev("R", "E:", 0) }];
        assign_gens(&swapped, &mut same, &notices(&["somebody-else"]), &mut counter);
        assert_eq!(same[0].gen, 12);
    }

    #[test]
    fn a_notice_during_the_last_scan_stops_the_eject() {
        let present = vec![dev("A", "E:", 4)];
        assert_eq!(eject_go(&present, &conn("A", 4), &Notices::default()).map(|d| d.gen), Ok(4));
        // Unplugged and back (or its card swapped) while the scan ran:
        // a notice named its disk or a volume: "Couldn't eject".
        assert_eq!(eject_go(&present, &conn("A", 4), &notices(&["A-vol"])), Err(Refusal::Failed));
        assert_eq!(eject_go(&present, &conn("A", 4), &notices(&["A-disk"])), Err(Refusal::Failed));
        // Another connection, or gone.
        assert_eq!(eject_go(&present, &conn("A", 3), &Notices::default()), Err(Refusal::Failed));
        assert_eq!(eject_go(&[], &conn("A", 4), &Notices::default()), Err(Refusal::Failed));
        // A notice about another device does not stop it.
        assert!(eject_go(&present, &conn("A", 4), &notices(&["B-vol"])).is_ok());
    }

    #[test]
    fn lost_notices_make_every_device_new_and_stop_every_eject() {
        let mut counter = 7;
        let prev = vec![dev("A", "E:", 6), dev("B", "F:", 7)];
        let lost = Notices { paths: Vec::new(), overflowed: true };
        // Some notices were dropped: nothing says which device they were
        // about, so every one is a new connection.
        let mut next = vec![dev("A", "E:", 0), dev("B", "F:", 0)];
        assign_gens(&prev, &mut next, &lost, &mut counter);
        assert_eq!(next.iter().map(|d| d.gen).collect::<Vec<_>>(), [8, 9]);
        // The next look without loss keeps them.
        let mut again = vec![dev("A", "E:", 0), dev("B", "F:", 0)];
        assign_gens(&next, &mut again, &Notices::default(), &mut counter);
        assert_eq!(again.iter().map(|d| d.gen).collect::<Vec<_>>(), [8, 9]);
        // Lost during the pre-eject scan: "Couldn't eject".
        assert_eq!(eject_go(&again, &conn("A", 8), &lost), Err(Refusal::Failed));
        assert!(eject_go(&again, &conn("A", 8), &Notices::default()).is_ok());
        // A queued eject of the connection before the loss is dropped too.
        assert!(!still_there(&next.iter().map(|d| Device { gen: d.gen + 10, ..d.clone() }).collect::<Vec<_>>(), &conn("A", 8)));
    }

    #[test]
    fn without_device_notices_nothing_is_offered() {
        let devs = vec![dev("A", "E:", 1)];
        assert_eq!(offered(devs.clone(), true), devs);
        assert!(offered(devs, false).is_empty());
        assert!(!section_visible(&shown(&offered(vec![dev("A", "E:", 1)], false), &[], Instant::now())));
        assert_eq!(LISTEN_RETRIES.map(|d| d.as_secs()), [5, 30, 300], "bounded, then not again");
    }

    /// A fake devnode graph for `closure`: each devnode's children and
    /// relations (None: the query fails).
    fn close(tree: &[(u32, &[u32])], rel: &[(u32, Option<&[u32]>)], max: usize) -> Option<Vec<u32>> {
        let mut v = closure(
            1,
            |n| Some(tree.iter().find(|(x, _)| *x == n).map(|(_, k)| k.to_vec()).unwrap_or_default()),
            |n| match rel.iter().find(|(x, _)| *x == n) {
                Some((_, Some(r))) => Some(r.to_vec()),
                Some((_, None)) => None,
                None => Some(Vec::new()),
            },
            max,
        )?;
        v.sort();
        Some(v)
    }

    #[test]
    fn the_scope_closes_over_relations_of_relations() {
        // Target 1: disk 2 under it; 2 relates to volume 3; 3 relates to
        // disk 4 (another storage device); 4 relates to volume 5.
        let tree: &[(u32, &[u32])] = &[(1, &[2])];
        let chain: &[(u32, Option<&[u32]>)] = &[(2, Some(&[3])), (3, Some(&[4])), (4, Some(&[5]))];
        assert_eq!(close(tree, chain, 64), Some(vec![2, 3, 4, 5]));
        let classes = |ids: &[u32]| -> Vec<Node> {
            ids.iter().map(|i| class(match i { 2 | 4 => "DiskDrive", 6 => "HIDClass", _ => "Volume" })).collect()
        };
        let scope = |ids: Vec<u32>| Scope { nodes: Some(classes(&ids)), ..stick_scope() };
        assert_eq!(check_scope(&scope(close(tree, chain, 64).unwrap()), &[]), Ok(()), "a chain of storage relations");
        // A related devnode (4) whose own child is a HID function.
        let tree_hid: &[(u32, &[u32])] = &[(1, &[2]), (4, &[6])];
        let got = close(tree_hid, chain, 64).unwrap();
        assert!(got.contains(&6), "found through a relation's subtree");
        assert_eq!(check_scope(&scope(got), &[]), Err(Skip::OtherFunction("HIDClass".into())));
        // A relation's own query fails: not known.
        let broken: &[(u32, Option<&[u32]>)] = &[(2, Some(&[3])), (3, None)];
        assert_eq!(close(tree, broken, 64), None);
        // Past the bound: not known.
        let many: Vec<u32> = (10..100).collect();
        let wide: &[(u32, Option<&[u32]>)] = &[(2, Some(&many))];
        assert_eq!(close(tree, wide, 64), None);
        assert!(close(tree, wide, 100).is_some());
        // Cycles end.
        let cyc: &[(u32, Option<&[u32]>)] = &[(2, Some(&[3])), (3, Some(&[2, 1]))];
        assert_eq!(close(tree, cyc, 64), Some(vec![2, 3]));
    }

    #[test]
    fn a_refusal_is_said_for_a_while_then_pruned() {
        let t0 = Instant::now();
        let a = dev("A", "E:", 1);
        let mut st = vec![(a.conn(), Status::Refused { at: t0, why: Refusal::InUse(None) }), (conn("X", 9), Status::Ejecting)];
        assert_eq!(shown(std::slice::from_ref(&a), &st, t0)[0].note, Note::Refused(Refusal::InUse(None)));
        assert_eq!(shown(std::slice::from_ref(&a), &st, t0 + REFUSED_SHOW)[0].note, Note::Ready);
        assert!(shown(&[], &st, t0).is_empty());
        prune(&mut st, std::slice::from_ref(&a), t0 + REFUSED_SHOW);
        assert_eq!(st, vec![(conn("X", 9), Status::Ejecting)]);
        assert_eq!(next_change(&st, t0), None);
    }

    #[test]
    fn late_drive_letters_are_looked_for_three_times_then_by_event() {
        let ready = vec![dev("A", "E:", 1)];
        let late = vec![Device { volumes: vec![Volume::default()], ..dev("A", "", 1) }];
        assert_eq!(next_look(&ready, 0), None);
        assert_eq!(next_look(&late, 0), Some(Duration::from_secs(2)));
        assert_eq!(next_look(&late, 1), Some(Duration::from_secs(5)));
        assert_eq!(next_look(&late, 2), Some(Duration::from_secs(15)));
        assert_eq!(next_look(&late, 3), None);
        assert_eq!(next_look(&[Device::default()], 0), Some(Duration::from_secs(2)));
        assert!(letters_changed(0x8000, 2) && letters_changed(0x8004, 2));
        assert!(!letters_changed(0x8000, 5));
        assert!(!letters_changed(0x0007, 2));
    }
}
