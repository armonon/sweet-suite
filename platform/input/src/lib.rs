//! # suite-input — input, gizmos, snapping, shortcuts.
//!
//! Pen/tablet/mouse handling, the **morphing gizmo/manipulator** framework, snapping/
//! magnet, the selection model, and the shortcut + command system with a command
//! palette. "Operate the same" comes from here — the same gesture means the same thing
//! everywhere. docs/02 §3.4, docs/01 §0 & §3.5.
//!
//! Status: stub.

#![allow(dead_code)]

/// The morphing transform gizmo — full 3D handles on a mesh, flat handles on a design
/// layer. Built **before** panels (Phase 1): it's the soul of the unified feel and it
/// teaches you what the interaction model actually needs. docs/01 Phase 1.
pub struct Gizmo;

/// Predictive, magnetic, forgiving snapping across every object type (vertex/edge/face/
/// grid/surface/face-mate). A real differentiator. TODO(Phase 7). docs/01 §3.5.
pub struct Snapping;
