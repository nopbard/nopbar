//! SHA-256 through Windows CNG (bcrypt.dll, the SHA-256 pseudo-handle of
//! Windows 10+): streamed, so a download is hashed while it is written.

use windows::Win32::Security::Cryptography::{BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALG_HANDLE};

pub struct Sha256(BCRYPT_HASH_HANDLE);

impl Sha256 {
    pub fn new() -> Result<Sha256, String> {
        let mut h = BCRYPT_HASH_HANDLE::default();
        let r = unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0) };
        if r.is_err() || h.is_invalid() {
            return Err(format!("sha256: BCryptCreateHash {:#x}", r.0));
        }
        Ok(Sha256(h))
    }

    pub fn update(&mut self, data: &[u8]) -> Result<(), String> {
        // BCryptHashData takes at most u32::MAX bytes at a time.
        for chunk in data.chunks(1 << 30) {
            let r = unsafe { BCryptHashData(self.0, chunk, 0) };
            if r.is_err() {
                return Err(format!("sha256: BCryptHashData {:#x}", r.0));
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<[u8; 32], String> {
        let mut out = [0u8; 32];
        let r = unsafe { BCryptFinishHash(self.0, &mut out, 0) };
        if r.is_err() {
            return Err(format!("sha256: BCryptFinishHash {:#x}", r.0));
        }
        Ok(out)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyHash(self.0);
        }
    }
}

pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 64 hex digits (either case) -> the digest.
pub fn parse_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn of(data: &[u8]) -> Result<[u8; 32], String> {
    let mut h = Sha256::new()?;
    h.update(data)?;
    h.finish()
}

/// The digest of a file, read in 64 KB pieces.
pub fn of_file(path: &std::path::Path) -> Result<[u8; 32], String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new()?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n])?;
    }
    h.finish()
}

/// `got` is exactly `want` (64 hex digits from the manifest).
pub fn matches(got: &[u8; 32], want: &str) -> bool {
    parse_hex(want).is_some_and(|w| &w == got)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(hex(&of(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&of(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // Streamed in odd pieces = in one go.
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut h = Sha256::new().unwrap();
        for c in data.chunks(997) {
            h.update(c).unwrap();
        }
        assert_eq!(h.finish().unwrap(), of(&data).unwrap());
        // One million "a" (FIPS 180-2 test).
        let mut h = Sha256::new().unwrap();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]).unwrap();
        }
        assert_eq!(hex(&h.finish().unwrap()), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn verification_is_exact() {
        let d = of(b"abc").unwrap();
        assert!(matches(&d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert!(matches(&d, "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"), "case does not matter");
        assert!(matches(&d, " ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n"), "surrounding space does not matter");
        assert!(!matches(&d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ae"), "one digit off");
        assert!(!matches(&d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015"), "short");
        assert!(!matches(&d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad00"), "long");
        assert!(!matches(&d, "za7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"), "not hex");
        assert!(!matches(&d, ""));
    }

    #[test]
    fn files_hash_like_bytes() {
        let dir = crate::autoupdate::testutil::Scratch::new("sha-file");
        let p = dir.0.join("f.bin");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 256) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(of_file(&p).unwrap(), of(&data).unwrap());
        assert!(of_file(&dir.0.join("missing")).is_err());
    }
}
