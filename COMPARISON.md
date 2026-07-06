# SWEET vs Photoshop & Blender — honest capability comparison

_Grounded in the actual source (~14,400 lines of Rust across the workspace), not the roadmap._
_Last updated: 2026-06-29._

## Framing

Photoshop (~35 years, millions of LOC) and Blender (~30 years, ~2–3M LOC of C/C++/Python)
are decades-deep, hundred-person-year products. SWEET is a **~14k-line native prototype**.
This is not a parity contest. The honest headline: SWEET has a **working cross-section of
both a raster editor and a 3D DCC in one unified canvas** — unusual for its size — but each
individual feature is "v0.1" depth.

Legend: ✅ solid · 🟡 basic/partial · ⬜ absent

## SWEET vs Photoshop (2D / raster)

| Capability | SWEET | Photoshop |
|---|---|---|
| Raster layers (opacity, visibility, reorder) | ✅ | ✅ |
| Layer groups / masks / clipping / styles | ⬜ | ✅ |
| Blend modes | 🟡 8 | ✅ ~27 |
| Brush engine | 🟡 size/hardness/flow/opacity/pressure, smudge, stabilizer, symmetry, 3 tips | ✅ deep dynamics, presets, texture |
| Selections | 🟡 rectangle marquee only | ✅ lasso/poly/wand/quick-select/pen/color-range/feather/refine |
| Adjustments (non-destructive) | ✅ 13 (B/C, Hue/Sat, Levels, Exposure, Vibrance, WhiteBalance, Posterize, Threshold, Invert, Box+Gaussian blur, Sharpen, Edge) | ✅ + Curves, Color Balance, Selective Color, Camera Raw… |
| Filters | 🟡 blur/sharpen/edge convolutions | ✅ full gallery + Liquify, smart filters |
| Gradient | ✅ linear/radial, selection-aware | ✅ + reflected/diamond/angle, gradient maps, editor |
| Transform | 🟡 layer flip / rotate 90° | ✅ free transform, scale/rotate/skew/warp/perspective |
| Crop · Text/type · Clone/heal | ⬜ · ⬜ · ⬜ (smudge only) | ✅ · ✅ · ✅ |
| Canvas size | 🟡 fixed square surface (arbitrary size = M5, pending) | ✅ arbitrary, huge |
| Color depth / management | 🟡 8-bit sRGB (linear-HDR compositor internally) | ✅ 8/16/32-bit, CMYK, Lab, ICC |
| File formats | 🟡 open PNG/JPG/BMP/GIF/WebP, export PNG, native `.sweet` | ✅ PSD + 30+ |
| Undo/redo | ✅ paint + scene | ✅ + history panel/states |

## SWEET vs Blender (3D / DCC)

| Capability | SWEET | Blender |
|---|---|---|
| Viewport / camera | ✅ wgpu, persp/ortho, grid, depth | ✅ + Eevee/Cycles viewport |
| Primitives | 🟡 cube/sphere/plane/lathe/pipe | ✅ full library |
| Mesh editing (half-edge kernel) | 🟡 extrude, inset, loop-cut, bevel edge+vertex | ✅ full edit mode: knife, dissolve, spin, proportional… |
| Selection modes | 🟡 face pick | ✅ vert/edge/face, loops/rings, box/circle/lasso |
| Subdivision | ✅ Catmull-Clark | ✅ + multires |
| Modifier stack | 🟡 Mirror / Array / Subdivide / Decimate | ✅ 50+ |
| Booleans (CSG) | ✅ | ✅ |
| Rigging | 🟡 auto-rig (spine), bones | ✅ full armature, IK/FK, constraints |
| Skinning / weights | 🟡 auto weights + deform | ✅ weight painting, envelopes, correctives |
| Animation | 🟡 keyframes (transform + bones), timeline | ✅ graph editor, dope sheet, NLA, drivers |
| Sculpting | 🟡 Draw/Smooth/Flatten/Pinch | ✅ dyntopo, multires, 20+ brushes, masking |
| UV editing | ⬜ | ✅ |
| Materials / shaders | ⬜ | ✅ node-based PBR |
| Rendering | ⬜ realtime preview only | ✅ Cycles path tracer + Eevee |
| Texture paint | 🟡 triplanar paint-on-mesh | ✅ full projection paint |
| Import/export (FBX/OBJ/glTF/USD) | ⬜ native `.sweet` only | ✅ |
| Physics / sim · geometry/compositor nodes | ⬜ | ✅ |

## Where SWEET actually wins

- **Unified canvas** — 2D paint, photo, and 3D modeling live in **one document/scene**.
  Paint directly on a mesh; turn a painting into a heightmap mesh. Neither Photoshop
  (2D only) nor Blender (3D-first, weak 2D) does this natively. This is the core bet.
- **Native Rust + wgpu**, `<8 ms` frame budget, one ~20 MB app vs multi-GB installs.
- **Principled internals for its size** — half-edge mesh kernel, command-delta undo,
  linear-HDR compositor.

## Bottom line

SWEET isn't competing with Photoshop or Blender on feature depth — that's a
decades-and-millions-of-lines gap. What it has proven at 14k lines is the **thesis**:
a unified 2D+3D native canvas on one modern engine is buildable and already usable across
a broad surface. The strategic value is the architecture and the unified-canvas idea, not
parity. See [ROADMAP.md](ROADMAP.md) for how the gaps above are prioritized.
