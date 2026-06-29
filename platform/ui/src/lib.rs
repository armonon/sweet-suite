//! # suite-ui — the shell.
//!
//! Window chrome, panel docking, the thin top bar, the left tool strip, the
//! **contextual right panel** (reskins by selection), the bottom timeline dock,
//! multi-window/multi-monitor, and the command palette. Every app gets the same
//! skeleton; only the contents differ. docs/02 §3.1, docs/01 §0.
//!
//! Status: stub. Phase 3 builds this (and is where the UI-framework decision lands).

#![allow(dead_code)]

/// The shell skeleton every app reuses. Build ONE contextual panel that reskins by
/// selection type — never a panel per discipline. docs/01 Phase 3.
pub struct Shell;
