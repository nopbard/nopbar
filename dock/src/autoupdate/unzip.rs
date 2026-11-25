//! A strict reader for the portable zip (scripts/package.ps1 -Zip). Only
//! what an update needs and nothing that could write outside the target
//! folder:
//! - entries must be plain files in the zip's root whose names are in the
//!   allow list (`nopbar.exe`, `LICENSE.txt`; any case); a name with a
//!   slash or backslash, `..`, a drive letter or colon, a control
//!   character, a directory entry, a symbolic link (Unix mode in the
//!   external attributes), an encrypted entry, ZIP64, a multi-disk archive,
//!   a duplicate or any other name refuses the whole zip;
//! - methods: stored or deflate; sizes are capped; each file's CRC-32 and
//!   size must match the central directory, and the local header must
//!   agree with it;
//! - files are written with the allow list's own spelling into a fresh
//!   folder (`create_new`: nothing that is there already is followed or
//!   overwritten).

use std::path::Path;

/// One file to take out of the zip.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The allow list's spelling.
    pub name: &'static str,
    method: u16,
    crc: u32,
    comp_size: u64,
    size: u64,
    local_offset: u64,
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| "zip: cut short".to_string())
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| "zip: cut short".to_string())
}

const EOCD_SIG: u32 = 0x0605_4b50;
const CEN_SIG: u32 = 0x0201_4b50;
const LOC_SIG: u32 = 0x0403_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;

/// The allow-listed spelling of `raw`, or why it is refused.
pub fn check_name(raw: &[u8], allowed: &[&'static str]) -> Result<&'static str, String> {
    let name = std::str::from_utf8(raw).map_err(|_| "zip: a name is not UTF-8".to_string())?;
    let shown: String = name.chars().take(80).collect();
    if name.is_empty() {
        return Err("zip: an empty name".into());
    }
    if name.contains(['/', '\\']) {
        return Err(format!("zip: {shown:?} is not in the zip's root (folders, absolute paths and .. are refused)"));
    }
    if name.contains(':') {
        return Err(format!("zip: {shown:?} has a drive letter or stream name"));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("zip: {shown:?} has a control character"));
    }
    if name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return Err(format!("zip: {shown:?} is not a plain file name"));
    }
    allowed.iter().find(|a| a.eq_ignore_ascii_case(name)).copied().ok_or_else(|| format!("zip: unexpected file {shown:?}"))
}

/// Reads the central directory of `zip` and checks every entry. `allowed`
/// are the only names that may be in it, `required` must be among them,
/// `max_file` caps each file's size and `max_total` all of them together.
pub fn entries(zip: &[u8], allowed: &[&'static str], required: &[&str], max_file: u64, max_total: u64) -> Result<Vec<Entry>, String> {
    // End of central directory: 22 bytes plus a comment of up to 64 KB.
    if zip.len() < 22 {
        return Err("zip: too short".into());
    }
    let lowest = zip.len().saturating_sub(22 + 0xFFFF);
    let eocd = (lowest..=zip.len() - 22).rev().find(|&i| u32_at(zip, i).ok() == Some(EOCD_SIG)).ok_or("zip: no end of central directory")?;
    if eocd >= 20 && u32_at(zip, eocd - 20)? == ZIP64_LOCATOR_SIG {
        return Err("zip: ZIP64 is not supported".into());
    }
    let (disk, cd_disk) = (u16_at(zip, eocd + 4)?, u16_at(zip, eocd + 6)?);
    let (on_disk, total) = (u16_at(zip, eocd + 8)?, u16_at(zip, eocd + 10)?);
    let (cd_size, cd_off) = (u32_at(zip, eocd + 12)? as usize, u32_at(zip, eocd + 16)? as usize);
    if disk != 0 || cd_disk != 0 || on_disk != total {
        return Err("zip: multi-disk archives are not supported".into());
    }
    if total == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF {
        return Err("zip: ZIP64 is not supported".into());
    }
    if cd_off.checked_add(cd_size).is_none_or(|end| end > eocd) {
        return Err("zip: central directory out of bounds".into());
    }
    if total as usize > allowed.len() {
        return Err(format!("zip: {total} entries, expected at most {}", allowed.len()));
    }
    let mut out: Vec<Entry> = Vec::new();
    let mut at = cd_off;
    let mut sum = 0u64;
    for _ in 0..total {
        if u32_at(zip, at)? != CEN_SIG {
            return Err("zip: bad central directory entry".into());
        }
        let made_by = u16_at(zip, at + 4)?;
        let flags = u16_at(zip, at + 8)?;
        let method = u16_at(zip, at + 10)?;
        let crc = u32_at(zip, at + 16)?;
        let comp_size = u32_at(zip, at + 20)? as u64;
        let size = u32_at(zip, at + 24)? as u64;
        let name_len = u16_at(zip, at + 28)? as usize;
        let extra_len = u16_at(zip, at + 30)? as usize;
        let comment_len = u16_at(zip, at + 32)? as usize;
        let disk_start = u16_at(zip, at + 34)?;
        let external = u32_at(zip, at + 38)?;
        let local_offset = u32_at(zip, at + 42)? as u64;
        let raw_name = zip.get(at + 46..at + 46 + name_len).ok_or("zip: name cut short")?;
        if raw_name.last() == Some(&b'/') || raw_name.last() == Some(&b'\\') {
            return Err("zip: folders are not expected".into());
        }
        let name = check_name(raw_name, allowed)?;
        if flags & 0x0001 != 0 || flags & 0x0040 != 0 {
            return Err(format!("zip: {name} is encrypted"));
        }
        if method != 0 && method != 8 {
            return Err(format!("zip: {name} uses compression method {method}"));
        }
        if disk_start != 0 {
            return Err("zip: multi-disk archives are not supported".into());
        }
        if comp_size == 0xFFFF_FFFF || size == 0xFFFF_FFFF || local_offset == 0xFFFF_FFFF {
            return Err("zip: ZIP64 is not supported".into());
        }
        // Unix host (3): the file type is in the top 16 bits; only regular
        // files (or none given) are taken. DOS attributes: no directory.
        let host = made_by >> 8;
        let unix_type = (external >> 16) & 0o170000;
        if host == 3 && unix_type != 0 && unix_type != 0o100000 {
            return Err(format!("zip: {name} is a link or special file"));
        }
        if external & 0x10 != 0 {
            return Err(format!("zip: {name} is a folder"));
        }
        if out.iter().any(|e| e.name == name) {
            return Err(format!("zip: {name} is in it twice"));
        }
        if size > max_file {
            return Err(format!("zip: {name} is {size} bytes, more than {max_file}"));
        }
        if method == 0 && comp_size != size {
            return Err(format!("zip: {name} sizes disagree"));
        }
        sum += size;
        if sum > max_total {
            return Err(format!("zip: files over {max_total} bytes"));
        }
        out.push(Entry { name, method, crc, comp_size, size, local_offset });
        at += 46 + name_len + extra_len + comment_len;
    }
    for r in required {
        if !out.iter().any(|e| e.name.eq_ignore_ascii_case(r)) {
            return Err(format!("zip: {r} is missing"));
        }
    }
    Ok(out)
}

/// The bytes of one entry, inflated and checked (local header, size, CRC).
pub fn read(zip: &[u8], e: &Entry) -> Result<Vec<u8>, String> {
    let at = e.local_offset as usize;
    if u32_at(zip, at)? != LOC_SIG {
        return Err(format!("zip: {}: bad local header", e.name));
    }
    let flags = u16_at(zip, at + 6)?;
    let method = u16_at(zip, at + 8)?;
    let name_len = u16_at(zip, at + 26)? as usize;
    let extra_len = u16_at(zip, at + 28)? as usize;
    let raw_name = zip.get(at + 30..at + 30 + name_len).ok_or("zip: local name cut short")?;
    // The local header must name the same file the same way.
    let local_name = std::str::from_utf8(raw_name).map_err(|_| "zip: a local name is not UTF-8".to_string())?;
    if !local_name.eq_ignore_ascii_case(e.name) || method != e.method || flags & 0x0041 != 0 {
        return Err(format!("zip: {}: local header disagrees with the central directory", e.name));
    }
    let start = at + 30 + name_len + extra_len;
    let data = zip.get(start..start + e.comp_size as usize).ok_or_else(|| format!("zip: {}: data cut short", e.name))?;
    let out = match e.method {
        0 => data.to_vec(),
        _ => super::inflate::inflate(data, e.size as usize)?,
    };
    if out.len() as u64 != e.size {
        return Err(format!("zip: {}: {} bytes, expected {}", e.name, out.len(), e.size));
    }
    if crc32(&out) != e.crc {
        return Err(format!("zip: {}: CRC mismatch", e.name));
    }
    Ok(out)
}

/// Takes every entry out of `zip` into `dest`, which must not exist yet
/// (it is created; its parent must exist). Returns the names written.
/// On an error, whatever was written is removed again.
pub fn extract(zip: &[u8], dest: &Path, allowed: &[&'static str], required: &[&str], max_file: u64, max_total: u64) -> Result<Vec<&'static str>, String> {
    let list = entries(zip, allowed, required, max_file, max_total)?;
    // Everything is read and checked before anything is written.
    let mut files = Vec::new();
    for e in &list {
        files.push((e.name, read(zip, e)?));
    }
    std::fs::create_dir(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let write = || -> Result<(), String> {
        use std::io::Write;
        for (name, bytes) in &files {
            let p = dest.join(name);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_dir_all(dest);
        return Err(e);
    }
    Ok(files.iter().map(|(n, _)| *n).collect())
}

/// CRC-32 (IEEE 802.3, as zip uses it).
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *v = c;
        }
        t
    });
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::autoupdate::testutil::Scratch;

    pub const ALLOWED: &[&str] = &["nopbar.exe", "LICENSE.txt"];
    pub const REQUIRED: &[&str] = &["nopbar.exe"];

    /// A zip of stored entries: (name, data, made_by, external attributes).
    pub fn stored_zip(files: &[(&[u8], &[u8], u16, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data, made_by, ext) in files {
            let off = out.len() as u32;
            let crc = crc32(data);
            out.extend_from_slice(&LOC_SIG.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // stored
            out.extend_from_slice(&[0; 4]); // time, date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(data);
            central.extend_from_slice(&CEN_SIG.to_le_bytes());
            central.extend_from_slice(&made_by.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&[0; 4]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal
            central.extend_from_slice(&ext.to_le_bytes());
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name);
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn zip1(name: &[u8]) -> Vec<u8> {
        stored_zip(&[(b"nopbar.exe", b"exe", 0x0014, 0x20), (name, b"x", 0x0014, 0x20)])
    }

    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn the_real_portable_zip_extracts() {
        // Made by Compress-Archive (like package.ps1 -Zip): nopbar.exe
        // deflated, LICENSE.txt tiny.
        let zip = include_bytes!("testdata/portable.zip");
        let list = entries(zip, ALLOWED, REQUIRED, 1 << 20, 1 << 20).unwrap();
        assert_eq!(list.iter().map(|e| e.name).collect::<Vec<_>>(), ["nopbar.exe", "LICENSE.txt"]);
        assert_eq!(list[0].method, 8, "deflated");
        let exe = read(zip, &list[0]).unwrap();
        assert_eq!(exe.len(), 3904);
        assert!(exe.starts_with(b"NopBar line 0 : the quick brown fox"));
        assert_eq!(crate::autoupdate::sha256::hex(&crate::autoupdate::sha256::of(&exe).unwrap()), "a995bd5c331507afe4f9ad6955877578f10c7d561e734998c8d61783e7804036");
        assert_eq!(read(zip, &list[1]).unwrap(), b"MIT");
        let s = Scratch::new("unzip-real");
        let dest = s.0.join("staged");
        assert_eq!(extract(zip, &dest, ALLOWED, REQUIRED, 1 << 20, 1 << 20).unwrap(), ["nopbar.exe", "LICENSE.txt"]);
        assert_eq!(std::fs::read(dest.join("LICENSE.txt")).unwrap(), b"MIT");
        assert_eq!(std::fs::read(dest.join("nopbar.exe")).unwrap(), exe);
        // The folder must be new.
        assert!(extract(zip, &dest, ALLOWED, REQUIRED, 1 << 20, 1 << 20).is_err());
    }

    #[test]
    fn zip_slip_and_odd_names_are_refused() {
        for bad in [
            &b"../nopbar.exe"[..],
            b"..\\nopbar.exe",
            b"/nopbar.exe",
            b"\\nopbar.exe",
            b"C:\\Windows\\nopbar.exe",
            b"C:nopbar.exe",
            b"sub/LICENSE.txt",
            b"LICENSE.txt:evil",
            b"..",
            b".",
            b"LICENSE.txt.",
            b"LICENSE.txt ",
            b"nopbar.dll",
            b"evil.exe",
            b"LICENSE\x01.txt",
            b"\xff\xfe",
            b"",
        ] {
            let s = Scratch::new("unzip-slip");
            let dest = s.0.join("staged");
            let r = extract(&zip1(bad), &dest, ALLOWED, REQUIRED, 1 << 20, 1 << 20);
            assert!(r.is_err(), "{:?} must be refused", String::from_utf8_lossy(bad));
            assert!(!dest.exists(), "nothing written for {:?}", String::from_utf8_lossy(bad));
            assert!(!s.0.join("nopbar.exe").exists() && !s.0.parent().unwrap().join("nopbar.exe").exists());
        }
        // A folder entry.
        assert!(entries(&stored_zip(&[(b"nopbar.exe", b"e", 0x14, 0), (b"LICENSE.txt/", b"", 0x14, 0x10)]), ALLOWED, REQUIRED, 99, 99).is_err());
    }

    #[test]
    fn links_duplicates_and_missing_files_are_refused() {
        // A Unix symlink (mode 0o120777) named like an allowed file.
        let link = stored_zip(&[(b"nopbar.exe", b"exe", 0x0314, 0o100644 << 16), (b"LICENSE.txt", b"/etc/passwd", 0x0314, 0o120777 << 16)]);
        assert!(entries(&link, ALLOWED, REQUIRED, 99, 99).unwrap_err().contains("link"));
        // A regular Unix file is fine.
        let unix = stored_zip(&[(b"nopbar.exe", b"exe", 0x0314, 0o100755 << 16)]);
        assert!(entries(&unix, ALLOWED, REQUIRED, 99, 99).is_ok());
        // DOS directory attribute.
        assert!(entries(&stored_zip(&[(b"nopbar.exe", b"exe", 0x14, 0x10)]), ALLOWED, REQUIRED, 99, 99).is_err());
        // The same name twice (any case).
        let dup = stored_zip(&[(b"nopbar.exe", b"a", 0x14, 0), (b"NOPBAR.EXE", b"b", 0x14, 0)]);
        assert!(entries(&dup, ALLOWED, REQUIRED, 99, 99).is_err());
        // No nopbar.exe.
        let none = stored_zip(&[(b"LICENSE.txt", b"MIT", 0x14, 0)]);
        assert!(entries(&none, ALLOWED, REQUIRED, 99, 99).unwrap_err().contains("missing"));
        // Case is normalised to the allow list's spelling.
        let upper = stored_zip(&[(b"NopBar.EXE", b"exe", 0x14, 0)]);
        assert_eq!(entries(&upper, ALLOWED, REQUIRED, 99, 99).unwrap()[0].name, "nopbar.exe");
    }

    #[test]
    fn sizes_and_damage_are_caught() {
        let z = stored_zip(&[(b"nopbar.exe", b"0123456789", 0x14, 0)]);
        assert!(entries(&z, ALLOWED, REQUIRED, 9, 99).is_err(), "file cap");
        assert!(entries(&z, ALLOWED, REQUIRED, 99, 9).is_err(), "total cap");
        // Flip a data byte: CRC.
        let mut bad = z.clone();
        bad[30 + 10] ^= 1;
        let e = entries(&bad, ALLOWED, REQUIRED, 99, 99).unwrap();
        assert!(read(&bad, &e[0]).unwrap_err().contains("CRC"));
        // Truncated: no end record.
        assert!(entries(&z[..z.len() - 5], ALLOWED, REQUIRED, 99, 99).is_err());
        assert!(entries(b"PK", ALLOWED, REQUIRED, 99, 99).is_err());
        // Encrypted flag in the central directory.
        let mut enc = z.clone();
        let cd = z.len() - 22 - (46 + 10);
        enc[cd + 8] |= 1;
        assert!(entries(&enc, ALLOWED, REQUIRED, 99, 99).unwrap_err().contains("encrypted"));
        // Local header naming another file.
        let mut swapped = z.clone();
        swapped[30] = b'N';
        swapped[31] = b'X';
        let e = entries(&swapped, ALLOWED, REQUIRED, 99, 99).unwrap();
        assert!(read(&swapped, &e[0]).is_err());
        // More entries than allowed names.
        let many = stored_zip(&[(b"nopbar.exe", b"a", 0x14, 0), (b"LICENSE.txt", b"b", 0x14, 0), (b"x", b"c", 0x14, 0)]);
        assert!(entries(&many, ALLOWED, REQUIRED, 99, 99).is_err());
    }

    #[test]
    fn names_are_checked_alone() {
        assert_eq!(check_name(b"LICENSE.TXT", ALLOWED), Ok("LICENSE.txt"));
        assert!(check_name(b"nopbar.exe::$DATA", ALLOWED).is_err());
        assert!(check_name(b"nopbar.exe/", ALLOWED).is_err());
    }
}
