//! What kind of PC this is, for the device drawn in "About this PC"
//! (about.rs): a laptop, a small one (UMPC), a handheld, a tablet, a
//! desktop, a mini PC or an all-in-one.
//!
//! What it goes by, read once on the system facts' helper thread
//! (system::warm), never on the UI thread:
//!
//! - the chassis type the firmware gives (SMBIOS type 3, the table
//!   `Win32_SystemEnclosure.ChassisTypes` shows, read directly with
//!   GetSystemFirmwareTable: no WMI);
//! - a battery (GetSystemPowerStatus);
//! - the power platform role the firmware prefers (a slate);
//! - the size of the PC's own screen (an embedded display's EDID: its
//!   first detailed timing in mm, else the header's cm), as a diagonal.
//!   Small panels' EDIDs run large (an example portable's 8.8" panel says
//!   10.06"), so the line between a small PC and a laptop sits at 11",
//!   below the smallest laptops sold (11.6").
//!
//! Whether a convertible is folded into a tablet right now
//! (SM_CONVERTIBLESLATEMODE) is read when the window opens; it only counts
//! for a convertible chassis (other PCs may report the slate mode too).
//!
//! decide() is pure and tested on a table of combinations.

/// The device drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Form {
    #[default]
    Laptop,
    /// A portable with a keyboard and a screen under 11" (Pocket PC, One
    /// Netbook).
    Umpc,
    /// A game handheld (chassis "Hand Held", or a tiny portable screen).
    Handheld,
    Tablet,
    Desktop,
    MiniPc,
    AllInOne,
}

impl Form {
    /// Its art's name in assets/devices ("laptop" -> laptop-light.png).
    pub fn art(self) -> &'static str {
        match self {
            Form::Laptop => "laptop",
            Form::Umpc => "umpc",
            Form::Handheld => "handheld",
            Form::Tablet => "tablet",
            Form::Desktop => "desktop",
            Form::MiniPc => "minipc",
            Form::AllInOne => "allinone",
        }
    }
}

/// What decide() goes by.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inputs {
    /// SMBIOS chassis types, in table order (the lock bit dropped).
    pub chassis: Vec<u8>,
    pub battery: bool,
    /// The firmware's preferred power profile is a slate.
    pub slate_role: bool,
    /// The PC's own (embedded) screen's diagonal, inches; None: no built-in
    /// screen found.
    pub diag_in: Option<f32>,
    /// A convertible is folded into a tablet now (read at opening).
    pub slate_now: bool,
}

/// Under this a portable is a small PC (see the module comment).
pub const UMPC_MAX_IN: f32 = 11.0;
/// Under this a portable of no known chassis is a handheld (7" game
/// handhelds; small PCs with keyboards start around 7.5" to 8").
pub const HANDHELD_MAX_IN: f32 = 7.5;

/// The device to draw (first known chassis type wins; see the tests for
/// the table):
///
/// - Hand Held (11): handheld. Tablet (30), Detachable (32): tablet.
///   Mini PC (35), Stick PC (36), Embedded PC (34): mini PC. All in One
///   (13): all-in-one.
/// - Portable, Laptop, Notebook, Sub Notebook (8, 9, 10, 14) and
///   Convertible (31): a small PC under 11", else a tablet when the
///   firmware says slate (or a convertible is folded now), else a laptop.
/// - Desktop kinds (3-7, 15-17, 23-25, 28, 29): a desktop, unless it has a
///   screen of its own (firmware that says "desktop" on a handheld or an
///   all-in-one): with a battery it is a portable of no known kind, without
///   one an all-in-one.
/// - Nothing known (Other, Unknown, none): with a battery (or a slate
///   role) a portable: a tablet if a slate, a handheld under 7.5", a
///   small PC under 11", else a laptop. Without: an all-in-one if it has a
///   screen of its own, else a desktop.
pub fn decide(i: &Inputs) -> Form {
    let small = i.diag_in.is_some_and(|d| d < UMPC_MAX_IN);
    let unknown_portable = || {
        if i.slate_role {
            Form::Tablet
        } else if i.diag_in.is_some_and(|d| d < HANDHELD_MAX_IN) {
            Form::Handheld
        } else if small {
            Form::Umpc
        } else {
            Form::Laptop
        }
    };
    for &c in &i.chassis {
        let found = match c & 0x7F {
            11 => Some(Form::Handheld),
            30 | 32 => Some(Form::Tablet),
            34..=36 => Some(Form::MiniPc),
            13 => Some(Form::AllInOne),
            k @ (8 | 9 | 10 | 14 | 31) => Some(if small {
                Form::Umpc
            } else if i.slate_role || (k == 31 && i.slate_now) {
                Form::Tablet
            } else {
                Form::Laptop
            }),
            3..=7 | 15..=17 | 23..=25 | 28 | 29 => Some(match (i.diag_in.is_some(), i.battery) {
                (false, _) => Form::Desktop,
                (true, true) => unknown_portable(),
                (true, false) => Form::AllInOne,
            }),
            // Other (1), Unknown (2), docking stations, expansion chassis
            // and the like: the next type, else the other facts.
            _ => None,
        };
        if let Some(f) = found {
            return f;
        }
    }
    if i.battery || i.slate_role {
        unknown_portable()
    } else if i.diag_in.is_some() {
        Form::AllInOne
    } else {
        Form::Desktop
    }
}

/// The chassis types in a raw SMBIOS table as GetSystemFirmwareTable('RSMB')
/// returns it (an 8-byte header, then the structures: type, length,
/// handle, the formatted part, strings up to a double NUL). Type 3's byte
/// at offset 5 is the chassis type (its top bit a lock flag). Stops at
/// the end-of-table structure (127) or at anything malformed.
pub fn smbios_chassis(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if raw.len() < 8 {
        return out;
    }
    let len = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let table = &raw[8..(8usize.saturating_add(len)).min(raw.len())];
    let mut i = 0usize;
    while i + 4 <= table.len() {
        let (ty, l) = (table[i], table[i + 1] as usize);
        if l < 4 || i + l > table.len() {
            break;
        }
        if ty == 3 && l > 5 {
            out.push(table[i + 5] & 0x7F);
        }
        if ty == 127 {
            break;
        }
        // The strings: up to the double NUL after the formatted part.
        let mut j = i + l;
        while j + 1 < table.len() && !(table[j] == 0 && table[j + 1] == 0) {
            j += 1;
        }
        i = j + 2;
    }
    out
}

/// A screen's size (mm, width x height) from its EDID: the first detailed
/// timing's image size, else the header's (cm). None for anything that is
/// not an EDID or says nothing (aspect-ratio-only sizes included).
pub fn edid_size_mm(e: &[u8]) -> Option<(u32, u32)> {
    if e.len() < 128 || e[..8] != [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0] {
        return None;
    }
    let d = &e[54..72];
    // A detailed timing (not a display descriptor): a pixel clock.
    if d[0] != 0 || d[1] != 0 {
        let w = d[12] as u32 | ((d[14] as u32 >> 4) << 8);
        let h = d[13] as u32 | ((d[14] as u32 & 0xF) << 8);
        // A few panels put 16 x 9 here (the aspect, not mm).
        if w >= 25 && h >= 25 {
            return Some((w, h));
        }
    }
    let (w, h) = (e[21] as u32 * 10, e[22] as u32 * 10);
    (w > 0 && h > 0).then_some((w, h))
}

/// The diagonal of a `w` x `h` mm screen, inches.
pub fn diagonal_in((w, h): (u32, u32)) -> f32 {
    ((w as f32).powi(2) + (h as f32).powi(2)).sqrt() / 25.4
}

/// The firmware's chassis types (empty when it gives none).
fn read_chassis() -> Vec<u8> {
    use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};
    unsafe {
        let n = GetSystemFirmwareTable(RSMB, 0, None);
        if n == 0 || n > 1 << 20 {
            return Vec::new();
        }
        let mut buf = vec![0u8; n as usize];
        let got = GetSystemFirmwareTable(RSMB, 0, Some(&mut buf));
        if got == 0 || got > n {
            return Vec::new();
        }
        buf.truncate(got as usize);
        smbios_chassis(&buf)
    }
}

/// A system battery is there (128: none; 255: unknown, taken as none).
fn read_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }.is_ok() && s.BatteryFlag != 128 && s.BatteryFlag != 255
}

fn read_slate_role() -> bool {
    use windows::Win32::System::Power::{PowerDeterminePlatformRoleEx, PlatformRoleSlate, POWER_PLATFORM_ROLE_V2};
    unsafe { PowerDeterminePlatformRoleEx(POWER_PLATFORM_ROLE_V2) == PlatformRoleSlate }
}

/// A convertible is in its tablet posture now (0: slate). Only asked of a
/// convertible (other PCs may say 0 too). Cheap: read at opening.
pub fn slate_now() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CONVERTIBLESLATEMODE};
    unsafe { GetSystemMetrics(SM_CONVERTIBLESLATEMODE) == 0 }
}

/// Everything but the posture (a few firmware, power and display reads:
/// on a helper thread).
pub fn read() -> Inputs {
    let diag_in = crate::displays::internal_edids()
        .iter()
        .filter_map(|e| edid_size_mm(e))
        .map(diagonal_in)
        .fold(None, |m: Option<f32>, d| Some(m.map_or(d, |m| m.max(d))));
    Inputs { chassis: read_chassis(), battery: read_battery(), slate_role: read_slate_role(), diag_in, slate_now: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(chassis: &[u8], battery: bool, diag: Option<f32>) -> Inputs {
        Inputs { chassis: chassis.to_vec(), battery, diag_in: diag, ..Default::default() }
    }

    #[test]
    fn the_form_follows_the_chassis_battery_and_screen() {
        use Form::*;
        let table: &[(Inputs, Form, &str)] = &[
            // Notebooks by their screen.
            (i(&[10], true, Some(14.0)), Laptop, "a 14\" notebook"),
            (i(&[9], true, Some(15.6)), Laptop, "a laptop"),
            (i(&[10], true, Some(11.6)), Laptop, "the smallest laptops"),
            (i(&[10], true, Some(10.06)), Umpc, "Portable example: 8.8\" panel, EDID says 10.06\""),
            (i(&[10], true, Some(7.0)), Umpc, "a notebook chassis has a keyboard"),
            (i(&[14], true, Some(8.0)), Umpc, "a sub notebook"),
            (i(&[8], true, None), Laptop, "a portable with its lid shut (no screen found)"),
            (i(&[31], true, Some(8.8)), Umpc, "a small convertible"),
            (i(&[31], true, Some(14.0)), Laptop, "a convertible used as a laptop"),
            (Inputs { slate_now: true, ..i(&[31], true, Some(14.0)) }, Tablet, "folded into a tablet"),
            (Inputs { slate_now: true, ..i(&[10], true, Some(14.0)) }, Laptop, "posture only counts on a convertible"),
            (Inputs { slate_role: true, ..i(&[9], true, Some(12.3)) }, Tablet, "Surface Pro: laptop chassis, slate role"),
            // Their own kinds.
            (i(&[11], true, Some(7.0)), Handheld, "hand held"),
            (i(&[30], true, Some(10.5)), Tablet, "tablet"),
            (i(&[32], true, Some(13.0)), Tablet, "detachable"),
            (i(&[35], false, None), MiniPc, "mini PC"),
            (i(&[36], false, None), MiniPc, "stick PC"),
            (i(&[34], false, None), MiniPc, "embedded PC"),
            (i(&[13], false, Some(23.8)), AllInOne, "all in one"),
            // Desktops.
            (i(&[3], false, None), Desktop, "desktop"),
            (i(&[4], false, None), Desktop, "low profile"),
            (i(&[7], false, None), Desktop, "tower"),
            (i(&[15], false, None), Desktop, "space-saving"),
            (i(&[16], false, None), Desktop, "lunch box"),
            (i(&[24], false, None), Desktop, "sealed-case"),
            (i(&[3], true, None), Desktop, "a desktop on a UPS: no screen of its own"),
            (i(&[3], true, Some(7.0)), Handheld, "a handheld whose firmware says desktop"),
            (i(&[3], true, Some(8.8)), Umpc, "a small PC whose firmware says desktop"),
            (i(&[3], false, Some(23.8)), AllInOne, "an all-in-one whose firmware says desktop"),
            // Nothing known.
            (i(&[], true, None), Laptop, "unknown with a battery"),
            (i(&[], false, None), Desktop, "unknown without one"),
            (i(&[2], true, Some(7.0)), Handheld, "unknown, a tiny screen"),
            (i(&[1], true, Some(8.4)), Umpc, "other, a small screen"),
            (i(&[2], false, Some(27.0)), AllInOne, "unknown, a big screen, no battery"),
            (Inputs { slate_role: true, ..i(&[2], true, Some(10.9)) }, Tablet, "unknown, a slate"),
            // The first type it knows wins.
            (i(&[2, 10], true, Some(14.0)), Laptop, "unknown, then notebook"),
            (i(&[12, 3], false, None), Desktop, "a docking station, then desktop"),
            (i(&[0x80 | 10], true, Some(14.0)), Laptop, "the lock bit is dropped"),
        ];
        for (inputs, want, why) in table {
            assert_eq!(decide(inputs), *want, "{why}: {inputs:?}");
        }
    }

    /// An SMBIOS structure: type, length, handle, the rest of its formatted
    /// part, then its strings (or the double NUL of none).
    fn structure(ty: u8, formatted: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut s = vec![ty, (4 + formatted.len()) as u8, 0x12, 0x00];
        s.extend_from_slice(formatted);
        if strings.is_empty() {
            s.extend_from_slice(&[0, 0]);
        } else {
            for t in strings {
                s.extend_from_slice(t.as_bytes());
                s.push(0);
            }
            s.push(0);
        }
        s
    }

    fn raw(structs: &[Vec<u8>]) -> Vec<u8> {
        let table: Vec<u8> = structs.concat();
        let mut r = vec![0, 3, 4, 0];
        r.extend_from_slice(&(table.len() as u32).to_le_bytes());
        r.extend(table);
        r
    }

    #[test]
    fn chassis_types_are_read_from_smbios() {
        let bios = structure(0, &[1, 2, 0, 0xF0, 0x03, 0xFF], &["Example Works", "2.10"]);
        let system = structure(1, &[1, 2, 3, 4], &["Example Works", "X14", "Default string", "123"]);
        // Type 3: manufacturer string 1, chassis type (lock bit set), ...
        let enclosure = structure(3, &[1, 0x80 | 10, 2, 3, 4], &["Example Works", "Default string"]);
        let end = structure(127, &[], &[]);
        assert_eq!(smbios_chassis(&raw(&[bios.clone(), system.clone(), enclosure.clone(), end.clone()])), vec![10]);
        // Two enclosures, in order.
        let dock = structure(3, &[0, 12], &[]);
        assert_eq!(smbios_chassis(&raw(&[enclosure.clone(), dock.clone(), end.clone()])), vec![10, 12]);
        // Nothing after the end.
        assert_eq!(smbios_chassis(&raw(&[bios.clone(), end.clone(), enclosure.clone()])), Vec::<u8>::new());
        // Malformed or empty: nothing, never a panic.
        assert!(smbios_chassis(&[]).is_empty());
        assert!(smbios_chassis(&[0, 3, 4, 0, 0xFF, 0xFF, 0xFF, 0xFF, 3]).is_empty());
        let mut cut = raw(std::slice::from_ref(&enclosure));
        cut.truncate(cut.len() - 6);
        assert_eq!(smbios_chassis(&cut), vec![10], "a cut-off string area still gives its type");
        assert!(smbios_chassis(&raw(&[vec![3, 2, 0, 0]])).is_empty(), "a length under 4");
        // A short type 3 (no chassis byte) is skipped.
        assert!(smbios_chassis(&raw(&[structure(3, &[1], &[]), end])).is_empty());
    }

    fn edid(dtd_mm: (u32, u32), header_cm: (u8, u8)) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        e[21] = header_cm.0;
        e[22] = header_cm.1;
        // A pixel clock, then the image size at 12..15.
        e[54] = 0x1A;
        e[55] = 0x36;
        e[54 + 12] = (dtd_mm.0 & 0xFF) as u8;
        e[54 + 13] = (dtd_mm.1 & 0xFF) as u8;
        e[54 + 14] = (((dtd_mm.0 >> 8) as u8) << 4) | ((dtd_mm.1 >> 8) as u8 & 0xF);
        e
    }

    #[test]
    fn screen_sizes_come_from_the_edid() {
        // This portable's panel: the header's cm are wrong, the timing's mm right.
        let gpd = edid((217, 135), (89, 50));
        assert_eq!(edid_size_mm(&gpd), Some((217, 135)));
        let d = diagonal_in((217, 135));
        assert!((d - 10.06).abs() < 0.01, "{d}");
        assert_eq!(decide(&i(&[10], true, Some(d))), Form::Umpc);
        // Over 255 mm: the high bits.
        assert_eq!(edid_size_mm(&edid((698, 392), (70, 39))), Some((698, 392)));
        // The aspect in place of mm: the header's cm.
        assert_eq!(edid_size_mm(&edid((16, 9), (31, 17))), Some((310, 170)));
        // No timing: the header.
        let mut none = edid((0, 0), (34, 19));
        none[54] = 0;
        none[55] = 0;
        assert_eq!(edid_size_mm(&none), Some((340, 190)));
        // Nothing at all, or not an EDID.
        assert_eq!(edid_size_mm(&edid((0, 0), (0, 0))), None);
        assert_eq!(edid_size_mm(&[0u8; 128]), None);
        assert_eq!(edid_size_mm(&gpd[..100]), None);
        // A 14" 16:10 panel.
        assert!((diagonal_in((302, 189)) - 14.03).abs() < 0.02);
    }

    #[test]
    fn every_form_has_its_art() {
        for f in [Form::Laptop, Form::Umpc, Form::Handheld, Form::Tablet, Form::Desktop, Form::MiniPc, Form::AllInOne] {
            assert!(super::super::about::devices().get(&format!("{}-light", f.art())).is_some(), "{f:?}");
            assert!(super::super::about::devices().get(&format!("{}-dark", f.art())).is_some(), "{f:?}");
        }
    }

    /// What this PC says (read only). `cargo test -p dock probe_form -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_form() {
        let mut inputs = read();
        inputs.slate_now = slate_now();
        println!("chassis {:?} battery {} slate role {} slate now {} diagonal {:?}", inputs.chassis, inputs.battery, inputs.slate_role, inputs.slate_now, inputs.diag_in);
        for e in crate::displays::internal_edids() {
            println!("  internal screen: {:?} mm", edid_size_mm(&e));
        }
        println!("form: {:?}", decide(&inputs));
    }
}
