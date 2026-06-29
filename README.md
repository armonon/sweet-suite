# SWEET — the suite

One shared platform, three pro creative apps on top:

- **Visual** (`apps/visual`) — 3D modeling/sculpt + graphic design + natural-media paint on one canvas.
- **Video** (`apps/video`) — non-linear editor; converges from the existing COMPOSITOR product.
- **Audio** (`apps/audio`) — a DAW; a sibling binary kept process-isolated for real-time safety.

Consistency and feasibility come from the **shared platform** (`platform/*`), not from packaging.
Build the Visual app first and **extract** the platform from it — never build a speculative framework first.

## Start here

- **[CLAUDE.md](CLAUDE.md)** — architecture, roadmap, conventions, and the laws to hold.
- **[docs/](docs/)** — the three architecture docs: what to build, how three apps feel like one, and implementation-grade internals.

## Build

```sh
cargo build                          # whole workspace
cargo run --release -p suite-visual  # open the Visual app: cube + grid + camera toggle
```

The toolchain is pinned in `rust-toolchain.toml`. Phase 0 wires `wgpu 29`, `winit 0.30`, `glam 0.33`, `bytemuck 1.25`, and `pollster 0.4` in `[workspace.dependencies]`.

## Status

**Phase 0 landed (2026-06-25).** A wgpu renderer draws a colored cube, a triangle, a textured quad, and a procedural infinite grid through a camera that toggles perspective ↔ orthographic (press `O`). CPU+submit time fits the 8 ms budget on the dev Mac. See [DECISIONS.md](DECISIONS.md) for the close-out and [CLAUDE.md](CLAUDE.md) for what's next.
