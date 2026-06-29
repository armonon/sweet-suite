//! # suite-design — the design system: tokens + the one component kit.
//!
//! The single biggest lever for "looks the same." There is physically one button
//! implementation; every app uses it. Build it as a real component library, not
//! per-app CSS. Token values live in `design-tokens/tokens.toml`. docs/02 §3.2.
//!
//! Status: stub.

#![allow(dead_code)]

/// Path (relative to the workspace root) to the design-token source of truth.
pub const TOKENS_PATH: &str = "design-tokens/tokens.toml";

/// Loaded design tokens (charcoal chrome + one accent, type scale, spacing, motion).
/// TODO(Phase 3): parse `TOKENS_PATH` and expose typed tokens.
pub struct Tokens;

// TODO(Phase 3): the component kit — Button, Slider, NumericDrag, ColorPicker, Menu,
// Dialog — one implementation each, shared by every app.
