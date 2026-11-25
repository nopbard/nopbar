//! The Night light CloudStore state in Bond CompactBinary v1.

use std::sync::atomic::{AtomicU64, Ordering};
use windows::core::{w, HSTRING};
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_BINARY, RRF_RT_REG_BINARY,
};

pub(super) const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreductionstate\windows.data.bluelightreduction.bluelightreductionstate";
const HEADER: &[u8] = b"\x43\x42\x01\x00";
const OUTER: &[u8] = b"\x43\x42\x01\x00\x0a\x02\x01\x00\x2a\x06";
const LIST: &[u8] = b"\x2a\x2b\x0e";
const MAX: usize = 4096;
static LAST_WRITTEN: AtomicU64 = AtomicU64::new(0);

struct Field<'a> {
    id: u16,
    kind: u8,
    raw: &'a [u8],
    value: &'a [u8],
}

struct State<'a> {
    stamp: u64,
    fields: Vec<Field<'a>>,
}

fn take<'a>(data: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (first, rest) = data.split_at_checked(n)?;
    *data = rest;
    Some(first)
}

fn byte(data: &mut &[u8]) -> Option<u8> {
    Some(take(data, 1)?[0])
}

fn vlq(data: &mut &[u8]) -> Option<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let n = byte(data)?;
        if shift == 63 && n > 1 {
            return None;
        }
        value |= u64::from(n & 0x7f) << shift;
        if n < 128 {
            return Some(value);
        }
    }
    None
}

fn put_vlq(out: &mut Vec<u8>, mut n: u64) {
    while n >= 128 {
        out.push((n as u8) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn count(data: &mut &[u8]) -> Option<usize> {
    let n = usize::try_from(vlq(data)?).ok()?;
    (n <= MAX).then_some(n)
}

fn skip(data: &mut &[u8], kind: u8, depth: u8) -> Option<()> {
    if depth >= 16 {
        return None;
    }
    match kind {
        2 | 3 | 14 => {
            byte(data)?;
        }
        4..=6 | 15..=17 => {
            vlq(data)?;
        }
        7 => {
            take(data, 4)?;
        }
        8 => {
            take(data, 8)?;
        }
        9 | 18 => {
            let n = count(data)?;
            take(data, if kind == 18 { n * 2 } else { n })?;
        }
        10 => loop {
            match data.first().copied()? {
                0 => {
                    byte(data)?;
                    break;
                }
                1 => {
                    byte(data)?;
                }
                _ => {
                    field(data, depth + 1)?;
                }
            }
        },
        11 | 12 => {
            let item = byte(data)?;
            for _ in 0..count(data)? {
                skip(data, item, depth + 1)?;
            }
        }
        13 => {
            let key = byte(data)?;
            let val = byte(data)?;
            for _ in 0..count(data)? {
                skip(data, key, depth + 1)?;
                skip(data, val, depth + 1)?;
            }
        }
        _ => return None,
    }
    Some(())
}

fn field<'a>(data: &mut &'a [u8], depth: u8) -> Option<Field<'a>> {
    let start = *data;
    let tag = byte(data)?;
    let id = match tag >> 5 {
        6 => u16::from(byte(data)?),
        7 => u16::from_le_bytes(take(data, 2)?.try_into().ok()?),
        id => u16::from(id),
    };
    let kind = tag & 31;
    let value = *data;
    skip(data, kind, depth)?;
    Some(Field {
        id,
        kind,
        raw: &start[..start.len() - data.len()],
        value: &value[..value.len() - data.len()],
    })
}

impl<'a> State<'a> {
    fn parse(blob: &'a [u8]) -> Option<Self> {
        let mut outer = blob.strip_prefix(OUTER)?;
        let stamp = vlq(&mut outer)?;
        outer = outer.strip_prefix(LIST)?;
        let n = count(&mut outer)?;
        let mut inner = take(&mut outer, n)?.strip_prefix(HEADER)?;
        if outer != [0, 0, 0] {
            return None;
        }
        let mut fields = Vec::new();
        while inner != [0] {
            let f = field(&mut inner, 0)?;
            if fields
                .last()
                .is_some_and(|prev: &Field<'_>| prev.id >= f.id)
                || !matches!((f.id, f.kind), (0, 16) | (10, 16) | (20, 6) | (30, 2))
                    && matches!(f.id, 0 | 10 | 20 | 30)
                || f.id == 30 && !matches!(f.value, [0] | [1])
            {
                return None;
            }
            fields.push(f);
        }
        Some(Self { stamp, fields })
    }

    fn supported(&self) -> bool {
        !self.fields.iter().any(|f| f.id == 30 && f.value == [0])
    }

    fn on(&self) -> bool {
        self.fields.iter().any(|f| f.id == 0)
    }

    fn encode(
        &self,
        on: bool,
        now: u64,
        last: Option<u64>,
        filetime: u64,
    ) -> Option<(Vec<u8>, u64)> {
        let stamp = now
            .max(self.stamp.checked_add(2)?)
            .max(last.map_or(Some(0), |n| n.checked_add(2))?);
        let mut inner = HEADER.to_vec();
        let mut untouched = self
            .fields
            .iter()
            .filter(|f| !matches!(f.id, 0 | 10 | 20))
            .peekable();
        if on {
            inner.extend([0x10, 0]);
        }
        for id in [10, 20] {
            while untouched.peek().is_some_and(|f| f.id < id) {
                inner.extend(untouched.next()?.raw);
            }
            if id == 10 {
                inner.extend([0xd0, 10, 2]);
            } else {
                inner.extend([0xc6, 20]);
                put_vlq(&mut inner, filetime);
            }
        }
        for f in untouched {
            inner.extend(f.raw);
        }
        inner.push(0);
        let mut out = OUTER.to_vec();
        put_vlq(&mut out, stamp);
        out.extend(LIST);
        put_vlq(&mut out, inner.len() as u64);
        out.extend(inner);
        out.extend([0, 0, 0]);
        (out.len() <= MAX).then_some((out, stamp))
    }
}

fn read() -> Option<Vec<u8>> {
    let mut data = vec![0; MAX];
    let mut size = MAX as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(KEY),
            w!("Data"),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    let size = size as usize;
    data.get(..size)?;
    data.truncate(size);
    Some(data)
}

/// The outer option is the format being available, the inner one is hardware support.
pub(super) fn state() -> Option<Option<bool>> {
    let blob = read()?;
    let state = State::parse(&blob)?;
    Some(state.supported().then(|| state.on()))
}

pub(super) fn set(on: bool) -> Option<()> {
    if cfg!(test) {
        return None;
    }
    let blob = read()?;
    let state = State::parse(&blob)?;
    if !state.supported() {
        return None;
    }
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let filetime = 116_444_736_000_000_000 + u64::try_from(since_epoch.as_nanos() / 100).ok()?;
    let last = LAST_WRITTEN.load(Ordering::Relaxed);
    let (bytes, stamp) = state.encode(
        on,
        since_epoch.as_secs(),
        (last != 0).then_some(last),
        filetime,
    )?;
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(KEY),
            w!("Data"),
            REG_BINARY.0,
            Some(bytes.as_ptr().cast()),
            bytes.len() as u32,
        )
        .ok()
        .ok()?;
    }
    LAST_WRITTEN.store(stamp, Ordering::Relaxed);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_night_light_preserves_unknown_fields_and_advances_both_timestamps() {
        let mut off = OUTER.to_vec();
        off.extend([42, 0x2a, 0x2b, 0x0e, 12]);
        off.extend(HEADER);
        off.extend([0x23, 91, 0x42, 1, 0xc6, 20, 3, 0, 0, 0, 0]);
        let state = State::parse(&off).unwrap();
        assert!(!state.on());
        let (on, stamp) = state.encode(true, 20, Some(48), 1000).unwrap();
        assert_eq!(stamp, 50);
        let state = State::parse(&on).unwrap();
        assert!(state.on());
        assert_eq!(
            state.fields.iter().map(|f| f.id).collect::<Vec<_>>(),
            [0, 1, 2, 10, 20]
        );
        assert_eq!(state.fields[1].raw, [0x23, 91]);
        assert_eq!(state.fields[4].value, [0xe8, 7]);
        let (off, _) = state.encode(false, 60, None, 2000).unwrap();
        assert!(!State::parse(&off).unwrap().on());
        assert!(State::parse(&off[..off.len() - 1]).is_none());
        assert!(set(true).is_none());
    }
}
