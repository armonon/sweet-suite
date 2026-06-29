//! # suite-services — the boring, shared, essential layer.
//!
//! File I/O, **color management**, prefs (synced so your setup follows you across apps),
//! the plugin/extension host (write once, extend the whole suite), licensing, auto-update,
//! and crash/telemetry. docs/02 §3.9, docs/03 §1.8.
//!
//! Status: stub.

#![allow(dead_code)]

/// The shared color working space. **Linear, wide-gamut; all blending happens here.**
/// One layer, shared by visual + video — design it before the second object type exists.
/// docs/03 §1.8.
pub enum WorkingSpace {
    /// SDR-first default.
    LinearSrgb,
    /// HDR / wide-gamut path.
    AcesCg,
}

// TODO: io, prefs, the plugin host, licensing, auto-update, crash/telemetry.
