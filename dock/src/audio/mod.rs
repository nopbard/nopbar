//! Core Audio beyond the master volume: per-app session volume (the dock's
//! wheel on an icon, the "playing" speaker badge) and the output / input
//! device lists with the default-device switch (the Control Center).
//!
//! Every COM call runs on a worker thread; the UI threads only post
//! requests and take answers. The pure parts (wheel math, which audio
//! session belongs to which dock app, device sorting and icons) are unit
//! tested; nothing in the tests talks to the audio service.

pub mod devices;
mod policy;
pub mod session;
