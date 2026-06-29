//! # suite-video — the Video app (NLE).
//!
//! **Converges** from the existing ScenePilot + COMPOSITOR product
//! (docs/reference/scenepilot-compositor-reality-check.md). docs/03 §4.6.
//!
//! Domain core: codec decode/encode, NLE editing semantics, transitions, compositing,
//! color grading. Reconciliation pending: C++/Qt + FFmpeg today → a Rust/wgpu app on this
//! platform; the FFmpeg executor (Fork B, ships now) → a native GPU compositor (Fork A,
//! later), with the `scenepilot.editplan.v0` contract as the swappable seam.

fn main() {
    println!("SWEET · Video — NLE (converges from COMPOSITOR)");
    println!("frame budget: {} ms", suite_gpu::FRAME_BUDGET_MS);
    println!("status: scaffold only. See docs/reference/ for the convergence plan.");
}
