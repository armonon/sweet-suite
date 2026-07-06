# SWEET roadmap — prioritized by the gaps in COMPARISON.md

_Ordering principle: close the gaps whose **absence makes SWEET feel broken** first, then
interop, then depth, then amplify the one thing neither Photoshop nor Blender does (the
unified 2D+3D canvas). Effort is a rough T-shirt size._

_Build posture: **clean-room by default.** Product-differentiating logic — text layout,
mesh import/export mapping, shading models, mesh/UV algorithms, selection & paint logic —
is hand-built, informed only by *reading* how other projects solve it, never by copying
code, regardless of license. External crates stay reserved for pure plumbing with no
creative logic of its own (GPU/OS bindings, serialization, raw image/font-file byte
decoding) — the ones already in the workspace (wgpu, winit, glam, serde, rfd, image, png)
fit that description. GPL/AGPL sources (Blender, Krita, A3D, GIMP) remain hard-blocked from
vendoring on top of that, as always — reference-only._

_Last updated: 2026-06-29._

---

## Tier 0 — the "believable raster editor" floor (do now)

These are table-stakes; their absence is the first thing a Photoshop user notices.

| # | Item | Why it matters | Effort | Notes |
|---|---|---|---|---|
| M5 | ~~**Arbitrary canvas size**~~ **DONE (core) 2026-07-06** | Fixed square surface is the single most glaring limit — real images aren't square. | M | Shipped: `RasterCanvas` width/height decoupled everywhere; "Import Image" sizes the whole canvas to the image's native aspect (no more white-padding); 90° rotate became a whole-document op; PaintCanvas 3D quad rescales to match. **Still open:** wiring the sparse `TileCanvas` (256² tiles, up to 4096² extent) as the main surface for truly huge/gigapixel canvases — a distinct, larger feature, tracked as M5b below. See DECISIONS.md 2026-07-06. |
| M5b | **Sparse/huge canvas via `TileCanvas`** | Split out of M5 — genuinely separate scope (sparse allocation, cross-tile brush stamping, compositor changes) from "not forced square". | L | `platform/gpu/src/tile_canvas.rs` already exists and is tested in isolation; wire it in as the main per-layer surface once there's a concrete need for canvases beyond ~4K². |
| M4a | **Text / type tool** | Every editor has text; its absence reads as "toy". | L | Font-file **byte parsing** (pure format decode) is fine to depend on; hand-build the layout/kerning/rasterization on top ourselves. Start: single-line Latin, font/size/color, rasterize to active layer — defer full Unicode shaping/bidi/complex scripts. |
| M4b | ~~**Crop**~~ **DONE 2026-07-06** | Trivially expected; unblocked by M5's canvas-resize machinery. | S | Shipped: `Renderer::crop_to_rect` reuses M3's rect selection as the crop source + M5's drain-and-recreate pattern. "Crop to Selection" in the RectSelect inspector. See DECISIONS.md. |
| M4c | **Free transform** (scale/rotate/skew of layer or selection) | Only flip/rotate-90 exists today. | M | Affine warp of the layer's pixels + a handle gizmo. |

## Tier 1 — selections & non-destructive editing (do next)

The current rectangle-only selection + no masks is the biggest _depth_ gap on the 2D side.

| # | Item | Why it matters | Effort | Notes |
|---|---|---|---|---|
| S1 | **Selection mask texture** | Unlocks everything below; replaces the single scissor-rect. | M | Per-pixel alpha mask instead of a rect; paint/gradient/adjustments sample it. |
| S2 | **Lasso + polygon + wand-as-selection + feather** | Real selection toolkit. | M | Builds directly on S1 (wand already exists as a fill; retarget to write the mask; feather = blur the mask). |
| S3 | **Layer masks** | The core of non-destructive editing. | M | Reuse the S1 mask machinery per layer. |

## Tier 2 — 3D interop & material credibility

Right now SWEET can't exchange 3D with any other tool, and the viewport is flat-shaded.

| # | Item | Why it matters | Effort | Notes |
|---|---|---|---|---|
| G1 | **glTF + OBJ import/export** | Lets SWEET participate in a real pipeline; huge leverage. | L | Hand-write the glTF JSON+binary reader/writer and the OBJ text parser ourselves (spec-compliance work, mechanical but real); map directly into the existing `Mesh`/half-edge model — no vendored 3D-format crate. |
| G2 | **Basic PBR material** (base color / metallic / roughness + one light) | The viewport looks flat without shading; needed before any render. | M | Principled-ish BRDF derived from the published Disney/Karis papers, written into the existing wgpu pipeline ourselves; per-object material on `Document`. |
| G3 | **UV unwrap** | Prerequisite for real texture painting/materials. | L | Clean-room angle-based/LSCM-style unwrap, built from published algorithm descriptions — no vendored atlas library. |

## Tier 3 — amplify the differentiator (the unified canvas)

This is where SWEET does what the incumbents can't. Lean in once the floor is solid.

| # | Item | Why it matters | Effort | Notes |
|---|---|---|---|---|
| U1 | **Depth + color/normal pass export** | Foundation for compositing 3D into 2D and for AI-render (clean-room, L4-safe; the A3D-inspired direction). | M | Render depth buffer + flat color/normal passes to PNG. No external services. |
| U2 | **Render 3D viewport → raster layer** | Bake the scene into a paint layer; true 2D↔3D round-trip in one doc. | S–M | Copy the composited viewport into a new `PaintLayer`. |
| U3 | **AI-render bridge (local ComfyUI only)** | Send U1 passes as ControlNet input, get a render back. | L | **Gated:** local/offline only by default; cloud APIs (Fal.ai etc.) need explicit sign-off — spend + secrets (L4). |

## Deliberately out of scope (for now)

Depth that would be years of work and isn't on the critical path to the thesis:
physics/simulation, geometry/compositor node graphs, Cycles-class path tracer, CMYK/print
+ ICC color management, PSD read/write, FBX/USD, NLA/graph-editor animation depth,
multires/dyntopo sculpting. Revisit only if a concrete user need pulls them in.

## Sequencing summary

```
Tier 0  (floor)      → M5 canvas (done) · crop (done) · text · free-transform · M5b sparse canvas
Tier 1  (selections) → mask texture → lasso/poly/wand/feather → layer masks
Tier 2  (3D interop) → glTF/OBJ I/O → PBR material → UV unwrap
Tier 3  (unified)    → depth/color passes → render-to-layer → local AI bridge
```

Rationale: Tiers 0–1 make SWEET a _credible_ 2D editor; Tier 2 makes it a _connectable_
3D tool; Tier 3 is the moat. Ship Tier 0 before anything glamorous — a text tool and
arbitrary canvas size buy more credibility than a path tracer.
