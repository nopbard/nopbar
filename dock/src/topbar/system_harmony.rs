//! A made-up PC for the design audit's "About this PC" picture (tests
//! only; about_harmony.rs): the facts set before anything reads this PC's,
//! so nothing of this PC is drawn.

use super::*;

/// The made-up PC's name (what `faked` checks).
const PC: &str = "하모니 노트북";

/// Sets the made-up facts if none are read yet; true when the facts in
/// force are the made-up ones (false: this PC's were read first, by
/// another test of this process; nothing may be drawn from them).
pub(in crate::topbar) fn fake() -> bool {
    let _ = INFO.set(SysInfo {
        pc: PC.into(),
        maker: "Harmony Works H14".into(),
        windows: "Windows 11 Pro".into(),
        version: "25H2".into(),
        build: "26200.1000".into(),
        cpu: "Harmony Core 8-Core Processor".into(),
        gpu: "Harmony Graphics 780".into(),
        ram_kb: 16 << 20,
        ..Default::default()
    });
    INFO.get().is_some_and(|i| i.pc == PC)
}
