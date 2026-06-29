//! # suite-audio — the DAW.
//!
//! A **sibling binary**, kept process-isolated for real-time safety: its lock-free,
//! sub-millisecond audio thread is alien to a GPU frame loop and must never share or
//! block it. It wears the IDENTICAL shell, design system, and shortcuts, so the user
//! can't tell it's a separate process. docs/02 §0, §8.
//!
//! Domain core: the real-time audio thread, DSP, MIDI, instruments/effects (VST/AU/CLAP),
//! the mixer.

fn main() {
    println!("SWEET · Audio — DAW (sibling binary, real-time isolated)");
    println!(
        "UI frame budget: {} ms; the audio thread runs its own sub-ms deadline",
        suite_gpu::FRAME_BUDGET_MS
    );
    println!("status: scaffold only.");
}
