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
| M4d | ~~**Move (2D)**~~ **DONE 2026-07-06** | Photoshop's most-used tool had no SWEET equivalent. | S–M | Shipped: `Tool::MoveLayer` ("MovL", **V**), commit-on-release like Gradient. Selection-aware: with a selection active, only that region moves (leaving a transparent hole); with none, the whole layer shifts. See DECISIONS.md. |

## Tier 1 — selections & non-destructive editing (do next)

The current rectangle-only selection + no masks is the biggest _depth_ gap on the 2D side.

| # | Item | Why it matters | Effort | Notes |
|---|---|---|---|---|
| S1 | ~~**Selection mask texture**~~ **DONE (core) 2026-07-06** | Unlocks everything below. | M | Shipped: `SelectionShape{Ellipse,Polygon}` + `rasterize_selection_mask` (on-demand CPU mask, not a standing per-frame buffer). Gradient + Move consult it for exact-shape correctness; the rect bound still drives the GPU scissor/Crop, unchanged. **Not yet masked: brush painting** inside a non-rect selection (still bounded by the bbox scissor only) — see S1b below. |
| S1b | ~~**Masked brush painting**~~ **DONE 2026-07-07** | Paint should respect the exact Ellipse/Lasso shape, not just its bounding box. | M | Shipped: `RasterCanvas::end_stroke_masked` — a stroke-end CPU blend against `pre_stroke` using the mask, not a shared-pipeline shader change. Caught a real wgpu `COPY_BYTES_PER_ROW_ALIGNMENT` bug via the GPU-backed test before it shipped (arbitrary stroke-region readback essentially never hits the 256-byte row alignment `copy_texture_to_buffer` requires). See DECISIONS.md. |
| S2 | ~~**Ellipse + Lasso selection**~~ **DONE 2026-07-06** | Real selection toolkit. | M | Shipped: `Tool::EllipseSelect` ("Elps") and `Tool::Lasso` ("Lso", hotkey **L**), both on the S1 mask infrastructure. Marching-ants generalized to trace the exact shape (ellipse as an N-gon, lasso as its own point path), not just a rect. |
| S2b | ~~**Quick Selection retargeting**~~ **DONE 2026-07-07** | Magic Wand should produce a reusable selection, not paint directly. | S | Shipped: `SelectionShape::Mask{width,height,data}` — a raw per-texel coverage mask, for the one shape (flood-fill) with no compact vertex/analytic form. `Renderer::magic_wand_select` (was `paint_magic_wand_fill`) returns it instead of mutating pixels; `rasterize_selection_mask`/`selection_shape_bounds`/Move all gained a `Mask` arm. Flood-fill BFS extracted into a pure `flood_fill_mask` fn (CPU-testable, no GPU device needed) per the project's existing pure-fn-for-pixel-math discipline. **Caught pre-ship:** `handle_left_release`'s Lasso-cancel check used `lasso_points.len() < 3` as a proxy for "was this a Lasso drag" — true for every non-Lasso selection tool, so it silently cancelled every Ellipse (and would have cancelled every new Mask) selection right after mouse-up. Fixed by checking the shape variant itself instead of the side-channel counter. **Still open:** marching-ants for a Mask selection draws its bounding box, not the exact flood-filled outline (documented in the inspector, not silently wrong); feather (blur the mask edge). |
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

## Photoshop tool-parity checklist (2026-07-06)

Armon asked to make sure SWEET has "all Photoshop tools." Honest answer up front: **it
doesn't, and several missing families (pen/vector shapes, healing/clone/history-brush,
dodge/burn/sponge, mixer brush, content-aware fill) are each real, multi-day undertakings
on their own** — this is Photoshop's entire toolbox, not a small ask. This checklist exists
so nothing gets silently dropped or double-counted against the tiers above. ✅ have · 🟡
partial/different semantics · ⬜ gap, with the effort tier it'd land in.

| Photoshop tool (toolbar group) | SWEET state | Where it'd land |
|---|---|---|
| Move | ✅ `Tool::MoveLayer` (M4d, 2026-07-06) | — |
| Rectangular Marquee | ✅ `RectSelect` (M3) | — |
| Elliptical Marquee | ✅ `Tool::EllipseSelect` (S1/S2, 2026-07-06) — Row/Column variants still ⬜ (niche) | — |
| Lasso | ✅ `Tool::Lasso` (S2, 2026-07-06) — Polygonal/Magnetic variants still ⬜ | — |
| Quick Selection | ✅ `SelectionShape::Mask` (S2b, 2026-07-07) — Magic Wand produces it; bbox-only marching-ants for now, exact outline trace not done | — |
| Magic Wand | ✅ `Renderer::magic_wand_select` (S2b, 2026-07-07) — now produces a reusable mask selection instead of flood-filling pixels directly | — |
| Crop | ✅ `crop_to_rect` (M4b) | — |
| Perspective Crop / Slice / Frame | ⬜ niche | Not scheduled |
| Eyedropper | ✅ | — |
| Color Sampler / Ruler / Note / Count | ⬜ niche | Not scheduled |
| Spot Healing / Healing / Patch / Content-Aware Move / Red Eye | ⬜ — needs inpainting-style algorithms | Large, own future tier |
| Brush | ✅ — tips, hardness/flow/pressure, stabilizer, symmetry, wrap, blend modes | — |
| Pencil | 🟡 — achievable today as Brush at hardness=1 | Not scheduled |
| Color Replacement | ⬜ | Medium |
| Mixer Brush | ⬜ — real wet-paint blending | Large |
| Clone Stamp / Pattern Stamp | ⬜ | Medium — next highest-value gap after Move |
| History Brush / Art History Brush | ⬜ — needs "paint from a prior undo state" | Large (new architecture) |
| Eraser | ✅ `BrushBlend::Erase` | — |
| Background Eraser / Magic Eraser | 🟡 achievable today: Magic Wand → select, then Erase-blend Paint over the selection | Small: a one-click "erase selection" action |
| Gradient | ✅ (M4) | — |
| Paint Bucket | 🟡 achievable today: Magic Wand → select, then Paint fills it (S1b masking) | Small: a one-click "fill selection" action |
| Blur / Sharpen / Smudge (drag brush) | 🟡 Smudge ✅ (brush param); Blur/Sharpen exist only as full-image adjustment filters, not a localized drag brush | Medium |
| Dodge / Burn / Sponge | ⬜ — brush-based tonal ops | Medium |
| Pen / Freeform / Curvature / anchor points | ⬜ — no vector-path architecture at all | Large, own future tier |
| Type | ⬜ | M4a (tracked, effort L) |
| Path/Shape Selection | N/A without vector paths | — |
| Shape tools (rect/ellipse/polygon/line/custom) | ⬜ as vector layers — SWEET's Add-Cube/Sphere/etc are 3D primitives, not 2D vector shapes | Large |
| Hand / Zoom / Rotate View | 🟡 — arrow-key orbit + ±zoom (3D camera) covers the 3D case; no dedicated 2D pan/zoom-drag tool | Small |
| Free Transform | ⬜ | M4c (tracked, effort M) |
| FG/BG color swatches + swap | ⬜ — one brush colour, no background swatch | Small |
| Quick Mask mode | ⬜ | Tier 1, once S1 lands |

**Bottom line:** SWEET covers the core paint/select/gradient/eyedropper/crop path solidly,
but is missing entire tool *families* — healing/clone/history-brush, dodge/burn/sponge,
mixer brush, pen/vector shapes, content-aware fill — each of which is its own real
project, not a quick add. **Tier 1's selection mask** (S1) paid off exactly as scoped: it
was the one piece of infrastructure that unlocked Elliptical Marquee, Lasso, masked brush
painting, and Quick Selection (S1/S1b/S2/S2b, all shipped) rather than each being a
one-off tool. What's left on that thread is layer masks (S3) and Quick Mask mode — both
direct reuses of the same mask machinery, not new infrastructure.
