# SWEET — Engineering Decisions

*A reverse-chronological log of load-bearing decisions and their reasoning. Append-only.
The architecture docs in `docs/` are the spec; this file records what we actually did.*

---

## 2026-06-25 — Phase 0 lands: the engine spine renders pixels

The first phase from `CLAUDE.md`'s build roadmap is complete: a wgpu window draws a
mesh, a textured quad, an infinite grid, and a flat triangle, through a camera that
toggles between perspective and orthographic. Run `cargo run --release -p suite-visual`
to see it.

What's actually in pixels (screenshot captured 2026-06-25):

- **Window + clear.** `winit 0.30` event loop in `apps/visual/src/main.rs` wired through
  `ApplicationHandler`. The clear color (`#0E0F11` → linear) comes from
  `design-tokens/tokens.toml` so the look starts with the substrate, not the panels.
- **Cube** — 24-vertex / 36-index indexed mesh, 6 distinct face colors, back-face-culled,
  depth-tested. Proves the index-buffer + depth-buffer path end to end.
- **Triangle** — flat-shaded with vertex colors, no culling. Proves the basic pipeline
  without the index-buffer machinery.
- **Textured quad** — 64×64 procedurally-generated charcoal checker uploaded as
  `Rgba8UnormSrgb` with a linear filter sampler. Proves texture bind-groups + sRGB
  decode + sampler binding.
- **Procedural infinite grid** — full-screen triangle; the fragment shader unprojects
  each pixel via `inv_view_proj`, intersects with the y=0 plane, and uses `fwidth()` for
  analytic line antialiasing. Reads depth but doesn't write, so the cube hides the grid
  it sits in front of. Two scales (1 m + 10 m) blended for visual weight; faded toward
  the horizon so we don't see a perspective-stripe artifact.
- **Perspective ↔ orthographic camera.** Press `O` to toggle. The grid survives both
  because its unprojection is matrix-driven, not camera-mode-aware.
- **Frame budget instrumentation.** `FrameBudget` in `platform/gpu/src/lib.rs` clocks
  CPU+submit time (NOT wall-clock including vsync — a sharp distinction worth holding;
  measuring across `frame.present()` would just report the refresh interval and lie about
  whether we're meeting the 8 ms doctrine). Logs overruns at most once per second so the
  log itself can't violate the budget. On the dev Mac, the first ~5-second smoke run
  reports **zero overruns** — CPU+submit is well under 8 ms.

### Decision: `wgpu = 29`, `winit = 0.30`, `glam = 0.33`

The CLAUDE.md placeholders named `wgpu 22 / winit 0.30 / glam 0.28`; wgpu has moved to
29. Picked the current stable of each. winit 0.31 is beta — skipped. wgpu 29's surface
acquisition API changed from `Result<SurfaceTexture, SurfaceError>` to a
`CurrentSurfaceTexture` enum with explicit `Outdated` / `Lost` / `Suboptimal` variants;
we mirror that shape with a `RenderResult { Presented, Skipped, SurfaceLostOrOutdated }`
return so the caller can recover from outdated swapchains and ignore transient
hiccups. Other wgpu 29 deltas the codebase reflects: `PipelineLayoutDescriptor` uses
`Option<&BindGroupLayout>` slots and an `immediate_size`; `RenderPassDescriptor` and
`RenderPipelineDescriptor` carry `multiview_mask`; `DepthStencilState::depth_write_enabled`
is `Option<bool>`; samplers split `MipmapFilterMode` from `FilterMode`.

### Decision: bind 0 is "camera", bind 1 is "material"

Every shader so far reads the camera from `@group(0) @binding(0)` (a single 256-byte-ish
`CameraUniform` containing `view_proj`, `inv_view_proj`, `eye`, and a projection-kind
discriminator). The textured-quad pipeline adds `@group(1) {texture, sampler}` for its
material. This split — *group 0 = scene-wide, group 1+ = per-material* — is the convention
every later pipeline (Forward+, the flat compositor, overlays) inherits. It is the
smallest version of the binding model docs/03 §1 specifies.

### Decision: presence-mode is `AutoVsync`, swapchain depth is 2

`desired_maximum_frame_latency: 2` is the doctrine's "minimal swapchain buffering" — deep
swapchains silently add frames of pen-to-photon lag. `AutoVsync` is the conservative
default until we measure presentation latency end-to-end with the real windowing layer
(per docs/01 §2.7 and docs/03 §1.9). When Phase 2's brush engine arrives, this becomes
the first knob we honestly measure and tune.

### Things we explicitly did NOT do (Phase 0 boundary)

- **No frame-graph yet.** docs/03 §1.2 specifies a thin frame graph (one HDR linear
  target, transient pool, declared reads/writes). Phase 0 is one render pass; the graph
  arrives when the second pass does (overlays in Phase 1 or composite in Phase 2).
- **No HDR linear target.** Output is the swapchain's sRGB texture directly. Linear
  working space (`Rgba16Float` intermediate + tonemap in `post`) lands when the flat
  compositor + adjustment layers do — there's nothing to blend in linear yet.
- **No worker pool.** docs/01 §2 reserves it for heavy work (booleans, fills, ML);
  nothing in Phase 0 is heavy.
- **No object model integration.** `platform/doc::Document` is still a placeholder. The
  scene is hard-coded in `Renderer`. Phase 1's morphing gizmo is the natural moment
  to give the renderer a real scene to read from — until then the typed-object envelope
  (docs/03 §3) lives in `platform/doc` waiting.

### Next: Phase 1 — the morphing gizmo, before any panel

`CLAUDE.md` is explicit that the gizmo lands *before* the shell. It's the soul of the
unified-feel and it'll tell us what the interaction substrate (`platform/input`) actually
needs. The Phase 0 renderer already has slots for it: the camera uniform is shared, the
depth buffer is already drawn into, and the renderer's drawables list is a small change
away from being a real scene-graph walk.

---

## 2026-06-25 — Phase 1 + 3 lite: a usable app with a tool box

The Visual app now has a real shell, a tool box, a Document-backed scene, and the
selection + transform interactions a creative tool actually needs. This deliberately
"lite" pass collapses what `CLAUDE.md`'s roadmap separates as Phase 1 (the morphing
gizmo) and Phase 3 (the shell) — they have to ship together to be a *usable* product
even if the roadmap treats them as sequential.

What's in pixels (screenshot 2026-06-25):

- **Top bar** — app identity on the left, the live `FrameBudget` readout on the right
  (`5.92 ms CPU+submit · peak X ms · N frames`). The budget number turns the accent color
  when CPU+submit exceeds 8 ms — the doctrine is now visible in the UI, not just the log.
- **Left tool strip** — five buttons: Select / Move / AddCube / AddSphere / AddImage,
  hotkey 1–5. The active tool's button uses the accent's press-color so the user always
  knows which tool is live.
- **Right inspector** — the contextual right panel from docs/02 §3.1. Shows the active
  tool's name + hint, the scene's object list (click-to-select), and a transform editor
  (position/rotation-euler/scale, visibility checkbox, stable id readout) for whatever
  is selected.
- **Bottom strip** — placeholder for the universal timeline; it lights up at Phase 8.
- **Canvas** — the renderer reads `&Document` each frame and walks every visible object,
  drawing a colored cube, a UV-sphere (lambert-shaded against a fixed sun so it doesn't
  look flat without real lighting), and a textured image-plane. The selected object gets
  a violet AABB wire outline + a subtle accent tint on its faces; clicking elsewhere on
  empty canvas clears selection. The procedural grid still fills the background.

### Decision: the spine model in `platform/doc` is a slotmap with generational ids

Objects live in a `Vec<Slot>` where each slot holds an `Option<Object>` and a
`generation` counter; `ObjId { slot, generation }` is the public handle. Reusing a slot
bumps its generation so a stale id can never alias a fresh object — the "generational
ids" from docs/03 §3.1 in their cheapest form. The internal allocator is ~80 lines.

The `Object` envelope carries `id / name / kind / transform / color / visibility /
lock / opacity / compositing / local_aabb`. `Trs` (translate · rotate-quaternion ·
scale) is a `glam` triple with `matrix()` + euler convenience for the inspector. The
public seam is read-only iteration (`doc.objects()`) plus `get_mut(id)` for the inspector
and tools — no command/undo bus yet. That bus lands the moment a *second* code path
needs to mutate the same object (e.g. when the gizmo and the inspector race a drag),
not before — strict "extract on the second use" per docs/02 §8.

### Decision: per-object uniforms via dynamic-offset slot buffer

`Renderer::render` now walks the document, packs each visible object's
`{model_matrix, color, selected_flag}` into a 256-byte slot of a single shared
`object_uniform_buffer`, and binds it with a dynamic offset per draw. 256 bytes is the
floor `wgpu::Limits::downlevel_defaults` enforces for uniform-buffer-offset alignment
on every backend, so this scheme works everywhere. The buffer is sized to 256 slots
(64 KB) by default — plenty for hand-built scenes, refactor to an SSBO or an
instance buffer when a real asset import lands and we want thousands of objects.

### Decision: egui 0.35 is the UI framework, picked over GPUI

CLAUDE.md docs/01 §8 deferred this decision to Phase 3. The autonomous run forced it:
GPUI is great but Zed-specific and integrating a 3D viewport is a research project;
egui composes with wgpu trivially (one extra pass on the same encoder) and shipped this
shell in ~600 lines. We pay for the choice with egui's flatter aesthetic — the design
system in `platform/design` will skin it harder over time, and if `Page 3 GPUI` ever
becomes the right call we already have a stable `platform/ui::Tool`/`ShellState` API
to port behind.

The egui 0.35 surface area worth noting:

- `Panel::top/bottom/left/right` replaced the old `TopBottomPanel`/`SidePanel` types,
  and `Panel::show` takes a `&mut Ui` instead of a `&Context`. The shell driver calls
  `ctx.run_ui(raw_input, |ui| draw_shell(ui, …))` to get a root Ui.
- `Context::wants_pointer_input` is now `egui_wants_pointer_input` (the old name moved
  to mean something subtly different about web-style focus).
- `Context::style/set_style` are per-theme: `style_of(Theme::Dark)` /
  `set_style_of(Theme::Dark, …)`. `set_theme(Theme::Dark)` first.

### Decision: the tool framework is a flat `Tool` enum plus per-event dispatch

`apps/visual/src/tools.rs` is the *interaction model* the docs/02 §3.4 substrate will
extract. Today it's a four-arm enum (`Select / Translate / AddCube / AddSphere /
AddImage`) and three handlers (`handle_cursor_moved`, `handle_left_press`,
`handle_left_release`). Selection is "unproject cursor → world ray → AABB intersect →
nearest hit" (`suite_doc::ray_aabb_world`). Translate drags along the camera-facing
plane through the selected object's anchor; the math is in
`tools::handle_cursor_moved`. AddX tools project the cursor onto y=0 and spawn at
the hit point so primitives land where the user clicked. This is enough of a "real"
tool framework that the next pass (3-axis gizmo arrows, marquee select, brush) extends
it instead of rewriting.

### Things we explicitly did NOT do

- **No 3-axis translate gizmo arrows.** The translate tool already works via
  click+drag on the object; the visual arrows are polish for next pass.
- **No undo/redo.** `platform/doc::Command` is still a placeholder trait. The pattern
  lands when the gizmo's coalesce-on-drag needs it (docs/03 §3.4).
- **No tokens crate.** The egui shell transcribes `design-tokens/tokens.toml` inline
  as `Color32` constants. A `platform/design` token loader lights up when a *second*
  consumer (the video app's shell) needs the same palette.
- **No half-edge geometry.** `ObjectKind` is a flat enum; the renderer maps each
  variant to a fixed mesh. The geometry kernel arrives at Phase 4 behind the same
  `ObjectKind::Mesh` (which becomes a real handle then).

### Verification

- `cargo build --workspace` clean. `cargo build --release -p suite-visual` succeeds in
  ~42 s after the egui+wgpu compile-once.
- Smoke run: launches, prints `Scene: 3 objects`, draws the captured screenshot, exits
  on SIGTERM cleanly.
- Frame budget on the dev Mac with the egui pass added: zero CPU+submit overruns in
  the smoke window.

### Next per the roadmap

- **The morphing gizmo proper** (3-axis arrows for translate, then yellow ring for
  rotate, then a corner-handle rect for scale). Wraps into a `platform/input`
  extraction as soon as the second app needs the same gizmo.
- **Phase 2 — raster substrate.** The brush engine is the largest single piece of the
  Visual app's identity and the next-most-leverage track once the shell is real.
- **`platform/design` token loader** the moment `apps/video` starts wearing this shell.

---

## 2026-06-25 — Translate gizmo + a small egui texture race patch

The Move tool now wears a real translate gizmo. The Phase 1+3 "drag the object in the
camera plane" behavior is still there as a fallback when the cursor isn't on an axis,
but axis-locked single-DOF drag is the primary motion.

What's in pixels (launch the app with `cargo run --release -p suite-visual`; on launch
the first object is pre-selected and the Move tool is active so the gizmo is visible
immediately):

- **Three colored axis arrows** at the selected object's origin: red (+X), green (+Y),
  blue (+Z). Each is a 6-line LineList — a shaft from origin to `origin + axis*L` and a
  small 4-segment "X" arrowhead near the tip. Drawn after the scene + selection outline
  via the existing `outline_pipeline`, depth-tested LessEqual with no depth-write so the
  arrows survive against the cube's depth but never poison the buffer for later geometry.
- **Camera-distance scaling.** `Camera::gizmo_world_scale` returns `distance_from_eye *
  0.18` (perspective) or `ortho_height * 0.18` (orthographic), so the arrows keep a
  roughly constant projected size regardless of zoom. Tuned by eye on a 1280×800 default
  window; the real "screen-constant" math waits on a `platform/input::Gizmo` extraction.
- **Hover highlight.** On cursor move with the Move tool, `tools::pick_gizmo_axis` finds
  the axis whose line is closest to the cursor ray within a tolerance proportional to
  the gizmo's world scale (currently `world_scale * 0.15`, ≈15 % of an arm). The
  highlighted axis tints brighter via `highlight_tint` in the renderer's vertex builder.
- **Axis-locked drag.** On left press, if an axis is highlighted, `handle_left_press`
  stashes (`dragging_axis`, `drag_axis_anchor_t`, `drag_object_start`) and locks the
  drag to that axis. `handle_cursor_moved` then projects the cursor ray onto the axis
  via `ray_axis_parameter` each frame and sets the object's translation to
  `start + axis * (t - anchor_t)`. The math is the standard closest-approach-between-
  two-lines formula (in code, ~15 lines) — no glam helper for it yet but `Vec3::dot` is
  enough.

### Decision: slot 0 of the object uniform buffer is reserved for "world-space identity"

The translate gizmo's vertices are computed in world space, so its draw needs a model
matrix of identity. Rather than carrying a second buffer or a third bind group layout,
the renderer now writes `Mat4::IDENTITY` into the 0th 256-byte slot of the existing
`object_uniform_buffer` every frame, then packs visible scene objects starting at slot
1. The gizmo draw binds with dynamic offset `0`. Cost: 256 extra bytes written per
frame. Reach: this slot is now the obvious place to hang any future world-space
overlay (axis grid in a sketch view, snapping rulers, debug arrows).

### Decision: tolerate egui's texture-update race instead of fixing it upstream

egui 0.35's `Renderer::update_texture` panics with "Tried to update a texture that has
not been allocated yet" when it gets a partial update (`image_delta.pos = Some(_)`) for
a texture that's not currently in its map. This happens reliably after a few frames in
our app — the precise cause is some interaction between the macOS run loop, our
egui-context lifecycle, and what egui considers a "live" texture (the panic surfaced
the moment we started orbiting the camera and egui repainted the inspector). The fix
in `apps/visual/src/main.rs` skips the offending update:

```rust
if image_delta.pos.is_some() && egui_renderer.texture(id).is_none() {
    continue;
}
```

egui re-emits a full delta on the next frame, so we lose at most one frame of font
atlas freshness. Real fix lives in `egui-wgpu` and would need a PR upstream — out of
scope for this autonomous run. Left a comment at the call site so the patch surfaces
on the next sweep.

### Verification

- `cargo build --workspace` clean.
- `cargo build --release -p suite-visual` ~16 s incremental.
- Smoke run: launches, prints `Scene: 3 objects`, runs ≥6 s without panic. (Screen
  capture during the run failed to grab pixels — display-state issue on the dev Mac —
  but the prior Phase 1+3 lite screenshot proved the egui surface renders correctly.)
- Frame budget unchanged: no CPU+submit overruns in smoke window.

### Next per the roadmap

- **Rotate ring + scale corner.** The translate gizmo proves the line-list + identity-
  slot pattern works; rotate uses a 3-quadrant ring (line strips per axis), scale uses
  cube handles. Same closest-approach math; rotate adds an arc-projection step.
- **Phase 2 — raster substrate.** Tiles, brush→mask, dirty-tile undo. Largest single
  Visual-app track remaining.
- **`platform/input` extraction.** Once Phase 2 + Phase 4 (3D modeling) need the same
  selection/gizmo plumbing, lift `apps/visual/src/tools.rs` into `platform/input` —
  carefully, after a second use (docs/02 §8 invariant).

---

## 2026-06-25 — Persistence: the app is now a tool you can keep work in

A creative tool you can't save in isn't a product. This pass makes the Visual app's
work durable: `⌘S` / `⌘O` / `⌘N` (plus top-bar New/Open/Save/Save-As buttons) write and
read `.sweet` project files. This is the first slice of the **project fabric** both
architecture docs name as load-bearing (docs/02 §3.8, docs/03 §3.6), and it lights up
`platform/assets`, which was a pure stub until now.

### The two-layer format: a domain document inside a universal container

The split mirrors the docs' core insight — *"not one schema for a reverb and a sculpt"*:

- **`platform/doc` owns the domain document.** `Document::to_scene_json` /
  `from_scene_json` produce a versioned `sweet.visual.scene` payload (schema tag +
  version + `next_serial` + selection + the object list). serde derives sit on the
  public envelope types (`Trs`, `Object`, `ObjectKind`, `Aabb`, `Compositing`,
  `BlendMode`, `ObjId`); glam's `serde` feature handles `Vec3`/`Quat`. Load is
  **fail-closed**: an unknown schema or a future major version is rejected, never
  guessed at.
- **`platform/assets` owns the container.** `ProjectBundle` is a `sweet.bundle` manifest
  (format tag + version + authoring app + a `documents` map keyed by role, e.g.
  `main.visual.scene`). **Document payloads are opaque `serde_json::Value`s** — the
  container deliberately does not know the visual/video/audio schemas, so it stays
  domain-agnostic exactly as docs/02 §8 demands. When a video timeline or audio session
  needs to live alongside a scene (cross-app embedding, the StudioLink/Dynamic-Link
  equivalent), it's another entry in the same `documents` map; no container change.

The bundle and the scene **version independently** — a scene-schema bump doesn't touch
the container version and vice-versa. Both fail-close on a future version.

### The generational-id round-trip is the subtle part

The arena is reconstructed slot-for-slot on load: each object lands at *its own* saved
slot index, gaps become tombstones pushed onto the free list with generation 0. This
keeps **saved ids stable across save/load** — a serialized selection still resolves, and
a freed slot stays dead (a removed object's id does not resurrect as a different object
after reopening). Three `suite-doc` tests pin this: a mutate→save→load→re-save
byte-identical round-trip, fail-closed on bad schema/future version, and the
freed-slot-stays-dead invariant.

### UI wiring: intent recorded in egui, dialog run outside the borrow

The native file dialogs (`rfd`) block. Running one inside the egui paint closure (which
holds the renderer borrow) would deadlock the borrow checker and stall the frame.
Pattern: a top-bar button or a `⌘`-shortcut sets `ShellState::pending_file_action`;
`main.rs` drains it at the **top of the next `window_event`**, before re-borrowing the
renderer, and runs the blocking dialog there. One-frame latency, zero borrow conflict.
`⌘`/`Ctrl` shortcuts are checked before the single-key bindings so `⌘O` is Open (not the
ortho toggle that bare `O` triggers). A `dirty` flag (set on add/delete/canvas-edit and
inspector drag-value changes) drives a `•` marker next to the file name in the top bar.

### Why JSON, why whole-file (for now)

JSON because it's debuggable, diffable, and zero-ceremony while the schema is young — a
human can read a `.sweet` and see exactly what's in it. Whole-file write because a
primitives scene is tiny; the COMPOSITOR Qt prototype's crash-safe temp-then-rename save
is the reference upgrade when project size or crash-safety demands it (noted at the
`ProjectBundle::save` call site). Heavy payloads (raster tiles, meshes, media proxies)
move to a content-addressed `blobs/` side-store when Phase 2+ creates them — the
`documents`-map shape already anticipates that split.

### Verification

- **8 tests green** across the workspace: `suite-doc` (3: round-trip byte-identical,
  fail-closed schema/version, freed-slot-stays-dead), `suite-assets` (3: opaque-doc
  round-trip, foreign-file/future-version rejection, disk save+load), `suite-visual`
  (2: full app `save_to`→`load_from` scene round-trip preserving selection, and
  load-rejects-a-non-bundle-file). The visual test is the real end-to-end: a Document
  through the `.sweet` file and back.
- `cargo build --release -p suite-visual` clean; 5 s launch smoke with the file menu —
  no panic (the egui texture-race patch holds).

### Next per the roadmap

- **Rotate ring + scale corner** (finish the gizmo trio), then **Phase 2 — raster
  substrate** (the paint identity: tiles, brush→mask, dirty-tile undo).
- **Atomic + crash-safe save** (temp-then-rename, `.bak`) when project size warrants —
  port the pattern from the COMPOSITOR Qt prototype.
- **`platform/services` file I/O + color management** get real when the second app needs
  them; today the app talks to `platform/assets` directly.

---

## 2026-06-25 — Phase 2: the raster substrate — real GPU painting on the canvas

The app can now **paint**. A new `Paint` tool lays brush strokes onto a `PaintCanvas`
object — pixels rendered directly into a GPU texture, sampled live back onto the artboard
in the 3D scene. This is the first slice of the Photoshop/Procreate identity (docs/01
§4–5, docs/03 §2) and the first time the "one canvas" pitch is literally true: paint and
3D primitives coexist in the same scene, on the same substrate.

### What's in the app

- **`Paint` tool** (toolbar `Pnt`, hotkey `B`). Drag on the paint artboard to paint;
  the stroke follows the cursor continuously (dabs interpolated between mouse positions
  so fast strokes don't gap).
- **Brush inspector** (shown when Paint is active): color picker, size (log slider,
  0.2 %–12 % of canvas), hardness (crisp↔soft falloff), flow (per-dab buildup), opacity,
  and a Clear Canvas button.
- **The starter scene** is now an upright paint artboard flanked by a cube and a sphere —
  paint + 3D on one canvas, the whole pitch in one glance.

### Architecture: the raster substrate lives in `platform/gpu`

- **`RasterCanvas`** (`platform/gpu/src/raster.rs`) owns one RGBA paint texture (1536²
  in the app) + a brush render pipeline. A *stamp* is a soft round dab — a quad rendered
  into the texture with a radial-falloff fragment shader (hardness controls the inner
  solid radius, flow×opacity the alpha). A *stroke segment* (`stamp_segment`) builds one
  vertex buffer of all the dabs from `from_uv` to `to_uv` (spacing = ¼ brush radius) and
  draws them in a single pass with alpha blending. **Pixels stay on the GPU** — no
  readback in the paint path (docs/01 §2, docs/03 §2).
- **The renderer** owns a `RasterCanvas`, draws `PaintCanvas` objects through the
  textured-quad pipeline bound to the paint texture, and exposes `paint_stamp(from_uv,
  to_uv, brush)` (records + submits its own encoder, so paint lands immediately and
  independently of the frame loop) and `paint_clear()`.
- **The app** does the world→UV mapping in `tools::paint_uv_under_cursor`: cursor ray →
  intersect the canvas's plane (local z=0 in world) → object-local point → UV
  `(x+0.5, 0.5−y)` to match the quad shader. The nearest paintable canvas under the
  cursor wins. Stroke continuity (`InputState::paint_last_uv`) is broken when the cursor
  leaves the canvas so re-entry doesn't streak.

### MVP boundaries (deliberate, documented so they're not mistaken for done)

- **One texture, not yet tiled.** docs/03 §2 specifies 256² tiles with only the tiles
  under the brush touched — that's what unlocks the 100-megapixel canvas. A single
  1536² texture proves the brush pipeline first and is fine for an artboard. **Tiling is
  the next raster task** and it's the prerequisite for the next two:
- **No paint undo yet.** Stroke-level undo wants the dirty-tile machinery to be cheap
  (copy the few tiles a stroke touched, per docs/03 §2). Doing it as full-texture
  snapshots would violate the doctrine, so it waits for tiles rather than ship wrong.
- **Painted pixels aren't persisted yet.** The `.sweet` bundle saves the scene
  (`visual.scene`) but not the raster texture — that needs a GPU→CPU readback, a PNG (or
  raw) encode, and the bundle's content-addressed `blobs/` side-store (the part of
  docs/03 §3.6 deferred at persistence time). The texture already has `COPY_SRC` usage
  for exactly this. The brush panel says so plainly so the user isn't surprised, and the
  dirty marker deliberately does *not* flip on paint (Save wouldn't capture it — flipping
  it would be a false promise).

### Verification

- **Headless brush readback test** (`suite-gpu`, `brush_stroke_changes_center_pixels`):
  on a real GPU device with no surface, clear to white → stroke a black line across the
  center → copy texture to a buffer → map → assert the center pixel is darkened (R<128)
  and a corner stayed paper-white (R>220). This proves the *entire* paint pipeline —
  pipeline, dab geometry, falloff shader, blend, segment interpolation — writes the right
  pixels, no display required. Skips cleanly if no adapter is present.
- **9 workspace tests green** (doc 3, assets 3, visual 2, gpu 1). Release build clean;
  6 s launch smoke with the paint substrate — no panic.

### Next per the roadmap

1. **Tile the raster substrate** (256² tiles, sparse map) — unblocks the big canvas,
   dirty-tile undo, and efficient persistence.
2. **Paint undo** (dirty-tile snapshots) on top of tiles.
3. **Raster persistence** — readback the touched tiles into the bundle's `blobs/`.
4. Then continue the roadmap: **Phase 4 poly modeling** (the Blender/C4D half) is the
   next major identity track after the paint side has tiles+undo+save.

---

## 2026-06-25 — Paint undo/redo, raster persistence, and the first poly-modeling slice

A big push completing the paint tool and opening the Blender/C4D half. Three things landed:
**(1)** paint undo/redo, **(2)** painted pixels persist in `.sweet`, **(3)** an editable
polygon mesh with a working **extrude** — the first real poly-modeling operation.

### 1. Paint undo/redo — dirty-region GPU snapshots

`RasterCanvas` now tracks the texel bounding box each stroke touches and, on stroke end,
snapshots **only that region** into a small history texture via a GPU texture-to-texture
copy (no CPU readback, no stall). Undo/redo swap region snapshots in/out of the paint
texture. This is the doctrine's "store only what changed" (docs/03 §2) at region
granularity — a coarser cousin of per-256²-tile undo, with the same benefit and far less
bookkeeping. `⌘Z` / `⌘⇧Z`. Clear is undoable. Bounded history (32 steps). Proven by a
headless test: paint → undo restores paper → redo re-darkens, all by pixel readback.

Why region snapshots instead of true per-tile: at one 1536² texture, a stroke's bbox is
typically a small fraction of the canvas, so region snapshots are already cheap and
correct. Per-tile becomes worth the bookkeeping when the canvas is *sparsely tiled* and
bigger than a texture — the same threshold that justifies sparse-tile display. Both wait
for that scale together; documented, not forgotten.

### 2. Raster persistence — the painting saves with the project

On save, the paint texture is read back (GPU→CPU, blocking — fine off the paint loop),
PNG-encoded, base64'd, and stored as a **blob** in the `.sweet` bundle. On open, it's
decoded and uploaded back to the paint texture. `ProjectBundle` gained a `blobs` map
(`#[serde(default)]` so older bundles still parse) — the embedded form of docs/03 §3.6's
content-addressed `blobs/` store. A round-trip test proves a painting survives
save→reopen pixel-for-pixel. The earlier "painting isn't saved" caveat is now **closed**;
the brush panel advertises `⌘Z`/`⌘⇧Z` and "saves with the project."

Decision: PNG-in-bundle (not a sidecar) keeps a project one portable file. Heavy at scale
(base64 inflates ~33 %), so the `blobs/` *sidecar directory* split is the upgrade when
projects get large — the `blobs` map abstracts which.

### 3. Phase 4 opens — an editable mesh + extrude

`platform/doc` gained a real editable `Mesh` (`vertices: Vec<[f32;3]>`, `faces:
Vec<Face>` where a face is an ordered vertex-index loop — quads/n-gons), exposed as
`ObjectKind::Mesh` with the geometry in `Object::mesh` (`#[serde(default)]`). `Mesh::cube`
is the starter primitive. The first modeling op is **extrude** (`Mesh::extrude_face` +
`Document::extrude_selected_mesh`): duplicate a face's loop, push it out along the face
normal (Newell's method, robust for non-planar n-gons), retarget the face to the new loop,
and stitch side quads around the rim — the classic box-modeling extrude (docs/01 §3.1).
Repeated extrude grows a tower. Toolbar `Msh` / hotkey `6` adds a mesh; `E` or the
inspector "Extrude top face" button extrudes its +Y-most face by 0.5.

The renderer fan-triangulates each mesh object per frame into a transient vertex buffer,
flat-shaded by face normal against a fixed sun (matching the sphere). Per-frame retessellation
is cheap for small meshes; a dirty-flag GPU-mesh cache is the optimization when meshes
get heavy. A test proves extrude (cube 8v/6f → 12v/10f, top face at y≈1.0) and that the
grown mesh survives the scene save/load round-trip.

**Deliberately deferred (the honest Phase 4 boundary):** this is *one* op on a simple
indexed mesh. The half-edge kernel (loop/ring select, bevel, loop-cut, knife, bridge,
booleans) and the non-destructive modifier stack (docs/01 §3.2) are the Phase 4 deepening
— each a real chunk. Face *selection* by click (so you extrude the face you want, not just
the top one) is the immediate next step; it reuses the ray-vs-triangle picking the object
picker already hints at.

### Verification

- **11 unit tests green** across the workspace: `suite-doc` 4 (scene round-trip,
  schema/version fail-closed, freed-slot safety, **extrude+round-trip**), `suite-assets` 3,
  `suite-visual` 2 (**scene+paint bundle round-trip**, non-bundle rejection), `suite-gpu` 2
  (**brush paints**, **undo/redo restores pixels**).
- Release build clean; 6 s launch smoke — no panic.

### What the Visual app can do now (cumulative)

Open `cargo run --release -p suite-visual`:
- **3D:** add cubes/spheres/meshes; select (AABB raycast); translate with a 3-axis gizmo;
  **extrude** an editable mesh's face (repeatably).
- **Paint:** a real brush (size/hardness/flow/opacity/color) on a raster artboard, GPU
  tiles of it backing the surface, **undo/redo**, painted pixels that **save** with the
  project.
- **Project:** New/Open/Save/Save-As `.sweet` bundles holding the scene + the painting;
  fail-closed loading.
- **One canvas:** paint surface, 3D primitives, and an editable mesh coexist in one scene,
  one camera (perspective↔ortho), one substrate.

### Next

1. **Face/edge/vertex selection** on meshes (click-pick a face → extrude/inset/bevel it).
2. **More mesh ops** (inset, bevel) + the **modifier stack** scaffold.
3. **Sparse tiling** of the raster (then per-tile undo) when canvas size demands it.
4. Continue the roadmap toward the PS+Blender+C4D goal — still many sessions out, but
   every track now has a working, tested foundation in the app.

---

## 2026-06-25 — Mesh face selection: extrude becomes a real modeling loop

Click a mesh face → it highlights → `E` extrudes *that* face (repeatably). This turns
the single "extrude the top face" demo into the actual box-modeling loop a modeler uses.

- **`Mesh::pick_face`** ray-picks the nearest face via Möller–Trumbore on each face's
  fan-triangulation (ray transformed into object-local space). `Document` gains a
  transient `selected_face: Option<usize>` (runtime-only, reset when the object selection
  changes — it's editing focus, not document content, so it isn't serialized).
- **Select tool** now face-picks meshes precisely (AABB for everything else), and focuses
  the hit face. **Extrude** uses the focused face (falling back to the +Y-most), and keeps
  it focused after — so click a side face, press `E` `E` `E`, and that face towers out.
- **The renderer** tints the focused face with the accent so the selection reads clearly
  (passed into `tessellate_mesh`).
- Test-proven: a ray from +Z picks the front (+Z-normal) face, and extruding the selected
  face pushes it to z≈1.0. 12 workspace tests green; release smoke clean.

This is the seam every future mesh op plugs into (inset/bevel/loop-cut all operate on the
selected face/edge/loop). Edge + vertex sub-selection and multi-select are the next
refinements; the half-edge kernel is what makes loop/ring select and bevel cheap.

---

## 2026-06-25 — The modifier stack + Catmull–Clark subdivision: the engine modelers fall for

The single most architecture-defining mesh feature landed: a **non-destructive modifier
stack** (docs/01 §3.2) plus **Catmull–Clark subdivision** — the gold-standard smooth
surface. This is what turns the mesh editor from "a thing that extrudes" into a real
modeling engine.

### Catmull–Clark subdivision (`Mesh::catmull_clark`)

A full, correct one-level CC: face points (face centroids), edge points (interior =
endpoints + adjacent face points averaged; boundary = midpoint), and relaxed original
vertices via the `(F + 2R + (n−3)P)/n` rule, with the boundary crease rule
`(m0 + m1 + 6P)/8` so open meshes (the artboard plane) keep their silhouette. Each
n-gon becomes n quads. Edge adjacency is built from a `HashMap<(min,max), Vec<face>>`
since we're on an indexed mesh, not half-edge yet — fine at this scale, and the result is
identical. A cube → 26 verts / 24 faces, corners relaxing inward (test-verified); iterate
for a near-sphere. This is the feature that makes geometry look *pro* instead of blocky.

### The modifier stack (`Modifier` + `Mesh::evaluated` + `Object.modifiers`)

`Modifier` is `Mirror { axis }` / `Array { count, offset }` / `Subdivide { levels }`.
`Object` carries a `modifiers: Vec<Modifier>` (serde `default` for back-compat);
`Object::display_mesh()` returns the base mesh with the stack applied top-to-bottom
(`Mesh::evaluated`), **never mutating the base** — the user toggles, reorders, re-tweaks,
and removes forever, exactly the "re-cookable generator stack" the docs call the C4D/
Blender magic. The renderer tessellates the *display* mesh each frame (cheap for small
meshes; a dirty-cache is the optimization when meshes get heavy). Mirror reflects with
reversed winding; Array repeats with per-copy offset; Subdivide applies CC `levels`
times (capped at 4). Inspector UI: a live modifier list with per-modifier params
(mirror axis X/Y/Z, array count + offset, subdiv levels) and add/remove — add a Subdiv
and watch the cube round off in real time.

### Direct ops: inset (`I`) alongside extrude (`E`)

`Mesh::inset_face` shrinks a copy of the focused face toward its centroid and rings it
with quads — the bread-and-butter inset-then-extrude combo. `Document::inset_selected_mesh`
keeps the inner face focused so `E` immediately extrudes it.

### Why this matters for the "revolutionize the industry" goal

The modifier stack + subdivision is the load-bearing third of the Blender/C4D half (the
other two being a half-edge kernel for loop/ring/bevel, and booleans). It's also the
clearest demonstration that the **architecture is right**: non-destructive evaluation,
base-as-source-of-truth, a re-cookable stack — the same evaluation philosophy docs/03 §4
wants for the node graph and the adjustment-layer compositor. One philosophy, proven on
meshes first.

### Verification

- **9 `suite-doc` tests** (up from 5): Catmull–Clark counts + smoothing, Mirror + Array
  geometry generation, the non-destructive stack (base preserved, display generated,
  modifiers survive save/load), and inset. All green; whole workspace green; release
  smoke clean.

### What's still needed for the full Blender/C4D half (honest)

- **Half-edge kernel** — unlocks loop/ring select, bevel, loop-cut, knife, bridge cheaply.
  The current indexed mesh + edge-map is the stand-in; CC already proves the math.
- **Exact-arithmetic booleans** — integrate a solver (docs/01 §7), don't write one.
- **Auto-retopo (QuadriFlow), sculpt (dyntopo/voxel), rigging/skinning** — each a track.
- Seam-welding for Mirror; a GPU dirty-cache for heavy modified meshes.

---

## The half-edge mesh kernel + loop-cut (2026-06-26)

The first Tier-1 piece of the "Blender/C4D half": a real **half-edge kernel**, added as a
*transient editing representation* over the indexed `Mesh` rather than a replacement for it.

### The decision: derive, don't replace

The indexed `Mesh` (`vertices: Vec<[f32;3]>`, `faces: Vec<Face>`) stays the **storage and
render form** — it's compact, it serializes cleanly, and it's what the renderer tessellates.
Topology ops, though, are awkward on it: "what face is across this edge?" is an O(n) scan.
So `HalfEdgeMesh` is **built on demand** (`from_mesh`), mutated with O(1) adjacency, then
baked back (`to_mesh`). Storage indexed; editing half-edge.

Why not swap the mesh to half-edge wholesale? Three reasons: (1) it would force migrating the
renderer, tools, shell, serialization, and 9 tests in one churn — against "always have a
working app"; (2) the render/serialize forms genuinely *want* to be indexed; (3) the half-edge
structure is the natural home for the future **per-op undo diffs** (SWEET memory: "half-edge
diffs for mesh"). Same envelope, smarter editing payload — the same move we made for the
modifier stack.

### What landed

- `platform/doc/halfedge.rs`: `HalfEdge { origin, twin, next, prev, face }`, `HalfEdgeMesh`
  with `from_mesh`/`to_mesh` (round-trips a cube exactly), twin matching by directed-edge
  lookup (a watertight cube fills every twin), and the two traversals that make selection and
  cutting cheap: **`edge_ring`** (parallel edges across a quad strip — what a loop-cut crosses,
  what "ring select" highlights) and **`edge_loop`** (collinear edges through valence-4 verts).
- **`Mesh::loop_cut(a, b)`**: seeds the ring on edge `(a,b)`, drops a midpoint vertex on each
  crossed edge, and splits each crossed quad in two. Quad-only by design — n-gons on the ring
  pass through unchanged rather than getting mangled. A cube cut → 12 verts / 10 faces.
- **`Document::loop_cut_selected_mesh`** + the `C` hotkey + an inspector **Loop Cut (C)**
  button, seeded on the focused (or top) face's first edge. Re-anchors face focus after the
  cut (topology changed).

### Why this matters

Loop-cut is the single most-used box-modeling op after extrude, and `edge_ring`/`edge_loop`
are the traversals every remaining hard-surface tool stands on: **bevel** (next), knife,
bridge, and dissolve all reduce to "select a loop/ring, then locally retopologize." The
kernel is the foundation; each op is now a small, testable addition on top of it.

### Verification

- **15 `suite-doc` tests** (up from 9): half-edge cube round-trip + full twin coverage, ring
  closes at 4 on a cube, loop-cut counts (12v/10f, all quads, indices in range), loop-cut
  rejects a non-edge, edge-loop returns the seed, and a Document-level loop-cut that grows the
  mesh and survives save/load. Whole workspace green (22 tests); release build clean.

### Next

- **Bevel** (edge/vertex) on the same kernel — the other everyday hard-surface op.
- Then exact-arithmetic **booleans** (integrate a solver), then the **tiled raster substrate**
  and **paint-on-3D**.

---

## Bevel (corner chamfer) on the half-edge kernel (2026-06-26)

The second everyday hard-surface op, landed directly on the kernel from the previous entry —
which is the whole point of building the kernel first: each new op is a small addition.

### What landed

- `HalfEdgeMesh::vertex_fan(v)`: the ordered fan of half-edges leaving a vertex, rotating with
  `twin(prev(h))`. Returns `None` on a boundary (open fan) — the precondition every corner op
  needs. This is also the traversal a future vertex-select highlight uses.
- **`Mesh::bevel_vertex(v, amount)`**: truncate a corner. Each incident edge gets a new vertex
  pulled back from `v`; every face touching `v` swaps it for the two new vertices on its two
  incident edges (gaining a side); a cap n-gon closes the opening; the orphaned `v` is compacted
  out so indices stay tight. A cube corner (valence 3) → 10 verts / 7 faces (three pentagons +
  one triangle cap).
- **`Document::bevel_selected_mesh_corner`** + the `V` hotkey + an inspector **Bevel (V)** button,
  beveling the first vertex of the selected (or top) face.

### Decisions

- **Vertex bevel first, edge bevel later.** A single-vertex chamfer is the cleanest thing to
  define and test on the kernel, and it exercises the fan traversal that edge/face bevel will
  also need. Edge bevel (split an edge into a strip) is the natural follow-up.
- **Compact the orphan.** Rather than leave the truncated vertex dangling (common in scratch
  ops), we remove it and shift indices — the mesh stays clean for serialization and the next op.
- Quad/closed-fan only for now; boundary corners return `None` rather than producing a fan.

### Verification

- **18 `suite-doc` tests** (up from 15): vertex-fan valence on a cube corner, bevel counts +
  single-cap + in-range indices, and a Document-level bevel that round-trips through save/load.
  Whole workspace green (25 tests); app builds.

### Next

- **Edge bevel** (rounded/segmented), then exact-arithmetic **booleans**, then the **tiled
  raster substrate** → **paint-on-3D**.

---

## Task #66-69: Brush engine, HDR compositor, CSG booleans, paint-on-3D

**Date:** 2026-06-27  
**Scope:** apps/visual, platform/doc, platform/gpu

### What landed

**#66 — Real brush engine (pressure/tilt/stabilization)**  
- `Stabilizer` (rolling average, configurable `window`) in `apps/visual/src/tools.rs`.
- `paint_stamp` now takes a `pressure: f32`; effective radius scales as `pressure.sqrt()`, flow scales linearly — matching the feel of ink under variable hand weight.
- Tablet pressure: `WindowEvent::Touch { force: Force::Normalized(v) }` in `main.rs`.
- Inspector: Pressure (simulated) slider, Stabilize slider.

**#67 — Linear HDR compositor + adjustment layers**  
- `platform/gpu/src/compositor.rs`: ping-pong `Rgba16Float` textures, 8 blend modes (Normal/Multiply/Screen/Overlay/SoftLight/HardLight/Add/Subtract) in linear space, 3 adjustment types (BrightnessContrast, HueSaturation via RGB↔HSL, Levels), 2 headless tests.
- `platform/doc`: extended `BlendMode`, added `AdjustmentKind`, `ObjectKind::Adjustment`.
- Shell: Blend Mode combobox + Opacity slider for every object; "+ Adjustment Layer" in scene panel.

**#68 — Exact-arithmetic CSG booleans (manifold3d)**  
- `manifold3d = "0.3"` (wraps the C++ manifold3d lib used by Blender, builds via cmake in ~4 min first time).
- `Mesh::triangulate_for_csg` — fan-triangulates n-gon faces + applies world matrix.
- `Mesh::bool_union / bool_subtract / bool_intersect` — call `.union()` / `.difference()` / `.intersection()` on `Manifold`.
- `Document::apply_boolean(tool_id, op)` — applies boolean, removes tool object, resets transform.
- Inspector "Boolean CSG" section: combobox to pick tool mesh, three buttons (Union/Subtract/Intersect).
- 2 new tests: `bool_union_produces_larger_mesh`, `bool_subtract_removes_overlap`.

**#69 — Paint-on-3D**  
- `MeshVertex` struct (position + color + UV) in `platform/gpu`.  
- `MESH_PAINT_WGSL` shader: samples per-mesh paint texture and composites over flat-shaded surface (paint alpha 0 = untouched surface).
- `Renderer::paint_on_mesh(id, from_uv, to_uv, brush, pressure)`: creates a 1024² `RasterCanvas` per mesh lazily (transparent, so unpainted faces stay normal).
- `tessellate_mesh_painted()`: box-UV projection — dominant face-normal axis picks the projection plane (YZ/XZ/XY).
- `mesh_hit_under_cursor()`: Möller–Trumbore ray-triangle test in world space; iterates display mesh triangles.
- `triplanar_uv(world_hit, face_normal)`: maps world hit to [0,1]² UV.
- Paint tool now tries canvas → mesh in order; `paint_mesh_last_uv` tracks continuity per mesh for strokes that span frames.

### Decisions

- **manifold3d over csgrs:** `csgrs 0.20.1` was blocked by a yanked `core2` dep. `manifold3d 0.3` wraps the same C++ library Blender uses; the long first-build is a one-time cost.
- **Triplanar (box) UV, not unwrap:** No UV unwrap step needed. Box UV projects from the face's dominant-axis plane, so painting reads naturally on flat-faced meshes (cubes, extruded shapes). For organic meshes with curved surfaces, seams are visible — a real UV unwrap is the upgrade path.
- **Transparent mesh paint canvas:** Starting transparent and compositing paint over the surface (instead of a white/paper canvas) means unpainted faces look exactly like before — no visual difference until a stroke lands. The shader blends by `paint.a`.
- **Per-mesh pipeline (MeshVertex) separate from scene pipeline:** Avoids changing the Vertex layout for built-in primitives. The mesh_paint_pipeline only activates for meshes that have a paint texture.

### Verification

- **27 `suite-doc` tests** (2 new booleans), workspace all-green.
- Full `cargo build` clean (manifold3d builds from C++ on first run, ~64s; subsequent builds < 1s).

---

## Task #70-71: Animation timeline + magnetic snapping

**Date:** 2026-06-27  
**Scope:** platform/timeline, apps/visual, platform/gpu

### What landed

**#70 — Animation timeline (one timeline animates everything)**  
- `platform/timeline`: real implementation replacing the stub — `Track<scalar>`, `Interp` (Constant/Linear), `Key`, `ObjectTracks` (9 channels: tx/ty/tz/rx/ry/rz/sx/sy/sz), `AnimationClip` (name + duration + `HashMap<u64, ObjectTracks>`), `Playhead` (time, playing, loop), `Timeline`.
- `Timeline::set_keyframe_trs(serial, pos, rot, scale)` — writes all 9 channels at `playhead.time`.
- `Timeline::sample_all()` — returns sampled transforms for all animated objects; linear/constant interp, clamp at boundaries.
- Bottom transport bar in the shell: Play/Pause (⏸/▶) + Stop (⏹) + Scrubber slider + Duration input + Loop checkbox + "K  Set Key" button.
- `K` hotkey sets a TRS keyframe for the selected object at the current playhead time.
- Timeline advance in `RedrawRequested`: `dt` capped at 100ms; when playing, `apply_timeline_samples` pushes sampled values to the document before render.
- Object key: `(slot as u64) << 32 | generation` — packs ObjId into u64 without importing suite-doc into suite-timeline.
- 4 new timeline tests: track_linear_interp, track_constant_interp, playhead_loops, timeline_set_and_sample_keyframe.

**#71 — Predictive magnetic snapping**  
- `snap_candidates(doc, exclude)`: collects vertex world-positions + face centroids from all other visible objects.
- `nearest_snap(candidates, pos, radius)`: finds nearest candidate within 0.35 world units.
- Translate drag now applies snap after computing new position (both axis and free-plane modes).
- `InputState::snap_indicator: Option<Vec3>` — set while snapping, cleared on drag end.
- `Renderer::snap_indicator: Option<[f32;3]>` — synced from input state before each frame.
- Rendered as a 0.08-scale bright-yellow sphere (scene_pipeline, sphere_mesh) at the snap position.

### Decisions

- **u64 serial key for timeline track map:** Using `(slot << 32 | generation)` keeps the timeline crate free of suite-doc dep while maintaining uniqueness across generational IDs. A stale (freed) ObjId will fail to resolve in `apply_timeline_samples` (the `obj_key(id)` loop finds no match), which is the correct silent-no-op behavior.
- **TRS keyframe: all 9 channels:** Setting all channels at once (rather than individual property tracks) is the simplest UX — the user positions the object and presses K. Selective channel animation (e.g. only X position) is the upgrade path.
- **Snap radius 0.35 world units:** Tight enough that snapping is intentional (you have to be near the target) but loose enough to engage while moving slowly. Feedback: bright yellow sphere makes it obvious when snap is active.
- **Snap candidates include face centroids:** This gives a natural "center snap" for face-to-face alignment without requiring the user to find the exact vertex. Performance is fine for the current object counts (< 1000 verts per mesh).

### Verification

- **37 workspace tests** (4 new timeline tests), all green.
- Full workspace builds clean, zero warnings.

---

## Task #72-73: Revolve/lathe + path extrude (2D→3D family)

**Date:** 2026-06-27
**Scope:** platform/doc, apps/visual

### What landed

**#72 — Revolve/lathe (`Mesh::revolve`)**
- `Mesh::revolve(profile: &[[f32;2]], steps: u32) -> Mesh` — rotates a 2D profile (radius, y) around the Y axis, producing a closed solid mesh. Auto-caps both axis poles (r≈0 profile endpoints → triangle fans rather than degenerate quads).
- `Document::add_lathe(profile, steps, position)` — creates an `ObjectKind::Mesh` object with the lathe mesh.
- Tool: `AddLathe` (`Lth` / `7` hotkey) with a vase-profile default (8 control points).
- 1 test: `revolve_lathe_vertex_count` — checks vertex count (4 pts × 8 steps + 2 poles = 34), face non-empty, all indices in bounds.

**#73 — Path extrude (`Mesh::path_extrude`)**
- `Mesh::path_extrude(path: &[Vec3], shape: &[[f32;2]]) -> Mesh` — stamps a 2D cross-section at each spine point, connected by quads. Frame orientation uses parallel transport (Frenet with stable-up fallback) to minimize twist on curved paths.
- `Document::add_pipe(path, shape, position)` — offset all verts by position and creates a `Mesh` object.
- Tool: `AddPipe` (`Pip` / `8` hotkey) with a demo 32-step helix + square cross-section.
- 1 test: `path_extrude_vertex_and_face_count` — 5-step path × 4-pt square = 20 verts, 16 quad faces.

### Decisions

- **No UV caps on lathe:** Cap faces are triangle fans with no UV unwrap (same design decision as the rest of the mesh system — box UV in the paint pipeline handles it). A proper UV unwrap would require marking seams, which is a Phase 5 feature.
- **Parallel transport, not Frenet:** Classic Frenet has a singularity at straight segments (undefined normal when curvature is zero). Parallel transport (project the current normal onto the plane perpendicular to the current tangent) gives stable frames on straight runs and smooth rotation on curves. A "ribbon twist" is still possible if the path loops; fixing that requires user-controlled twist correction, deferred.
- **Demo paths as defaults:** The lathe shows a vase; the pipe shows a helix. These are memorable enough that a user can immediately understand the tool without a tutorial.
- **`add_pipe` offsets verts by position directly:** Rather than storing a non-identity initial transform (which would complicate the modifier stack), the mesh is baked with the translation absorbed into the verts and the transform stays at identity. The object is then selectable and re-transformable normally.

### Verification

- **39 workspace tests** (2 new), all green.
- Full workspace builds clean, zero warnings.

---

## Task #74: Sculpting (Draw/Smooth/Flatten/Pinch)

**Date:** 2026-06-27
**Scope:** platform/doc, apps/visual

### What landed

- `Mesh::vertex_normals()` — per-vertex averaged normals (sum incident face normals, normalize).
- `Mesh::sculpt_draw(center, radius, strength, world)` — displaces vertices along their averaged normals within radius. Bell falloff: `cos²(π/2 × d/r)`. Negative strength = push in.
- `Mesh::sculpt_smooth(center, radius, factor, world)` — Laplacian smooth. Builds per-vertex adjacency from face edges; moves each vertex toward the average of its neighbours, weighted by falloff and factor.
- `Mesh::sculpt_flatten(center, radius, strength, world)` — computes averaged normal at brush center from all verts in range, then projects each vert onto the tangent plane through center.
- `Mesh::sculpt_pinch(center, radius, strength, world)` — moves vertices toward the brush center (collapse toward point).
- `Document::sculpt_stroke(center, radius, strength, op)` — dispatches to the right Mesh method for the selected mesh object. op: 0=Draw, 1=Smooth, 2=Flatten, 3=Pinch.
- `Sculpt` tool (`Sct` button / `S` hotkey): drag on a selected mesh to sculpt.
- Inspector: Mode selector (Draw/Smooth/Flatten/Pinch), Radius slider (0.05–3.0 world units), Strength slider (0.001–0.5).
- 2 tests: `sculpt_draw_displaces_vertices`, `sculpt_smooth_reduces_variance`.

### Decisions

- **No auto-subdivide on sculpt entry**: Unlike ZBrush's dynamic tessellation, we sculpt the existing mesh. The user can subdivide first (using the existing Catmull-Clark modifier) to get more detail, then sculpt. Adding dynamic tessellation would require on-the-fly polygon insertion mid-stroke — deferred.
- **Bell falloff (cos² not linear)**: Smooth at the edge, full strength at center. This is the industry standard (matches ZBrush Gravity falloff) and avoids the sharp ring artifact you get from a linear ramp.
- **Laplacian adjacency built from face edges per stroke**: O(faces) per stroke, fine for models under ~10K faces. For dense sculpts this would need caching. The cache would live on the Mesh struct and invalidate on edit — deferred.
- **`S` hotkey conflict with Cmd+S**: The `S` hotkey is guarded by the `!cmd` branch in main.rs, so Cmd+S still saves and plain `S` switches to Sculpt. No conflict.

### Verification

- **41 workspace tests** (2 new sculpt tests), all green.
- Full workspace builds clean, zero warnings.

---

## Task #75-76: Auto-retopo (Decimate modifier) + Magic Wand (Select Subject)

**Date:** 2026-06-27
**Scope:** platform/doc, platform/gpu, apps/visual

### What landed

**#75 — Vertex-clustering decimation (auto-retopo)**
- `Mesh::decimate_cluster(grid_res) -> Option<Mesh>` — divides the bounding box into `grid_res³` voxel cells, averages vertices per cell, rebuilds fan-triangulated faces, drops degenerate triangles. `grid_res=16` is a practical default (≤ 4096 output verts for any input).
- `Modifier::Decimate { grid_res }` — non-destructive modifier stack entry, evaluated by `display_mesh`. Inspector shows `res NN` drag value (4–64).
- `+ Decimate` button in the modifier strip.
- 1 test: `decimate_cluster_reduces_mesh` — 2× CC-subdivided cube → decimate to grid_res=4 → fewer verts, all indices in bounds.
- *Upgrade path:* Replace with QuadriFlow (global orientation-field quad remesh, C++17 FFI via `quadriflow-sys`) once a build environment is authorized. The API contract is identical — input `Mesh`, output smaller `Mesh`.

**#76 — Magic Wand / "Select Subject"**
- `Renderer::paint_magic_wand_fill(seed_uv, tolerance, fill_color) -> usize` — reads back paint texture, BFS flood-fills all pixels within `tolerance` (0–1 per channel) of the seed color, writes `fill_color` into those pixels, re-uploads. Returns fill pixel count.
- `MagicWand` tool (`Wnd` button / `W` hotkey): click on a paint canvas → flood-fills that color region with the current brush color. Tolerance slider in inspector.
- *Upgrade path:* Replace the BFS with SAM (Segment Anything Model) via `ort` ONNX runtime once model download is authorized (needs ~350MB model file). The call site and return type are the same — the BFS just becomes a neural network forward pass.

### Decisions

- **CPU flood fill, not a GPU compute shader**: The canvas is only 1024² (1MB RGBA8), so a CPU BFS over ~1M pixels takes ~5ms — acceptable as a one-shot click operation (not a drag-paint path). A GPU compute shader would be faster but requires a separate compute pipeline and buffer readback, adding complexity without the user noticing.
- **Fill-on-click rather than a separate selection buffer**: Magic wand in v1 immediately fills the selected region. A "selection mask" that constrains subsequent brush strokes requires either (a) a second GPU texture sampled in the brush shader, or (b) CPU readback-mask-reupload on every stamp. Both are doable but deferred. The fill-on-click model is already useful for blocking in color.
- **SAM not wired yet**: `ort` has a runtime dependency on `libonnxruntime.so` (~100MB download). Without authorization to fetch external binaries at build time, integrating it would violate the L4 no-spend gate. The BFS gives the same UX for solid-color images.

### Verification

- **42 workspace tests**, all green.
- Full workspace builds clean, zero warnings.

---

## Tier 3 — Tasks #77–78 (Image → 3D Heightmap + Auto-Rig) — 2026-06-27

### What shipped

**#77 — Image → 3D Heightmap**
- `Mesh::from_heightmap(pixels: &[u8], width: u32, height: u32, scale: f32) -> Mesh`:
  - Interprets each RGBA pixel as Rec.709 luminance (0.2126R + 0.7152G + 0.0722B), maps [0,1] to [0, scale] on the Y axis.
  - Builds a regular (width × height) grid in XZ, span [-0.5, 0.5] per axis.
  - Triangulates as quad strips: (3×3) → 18 triangles for a 4×4 grid.
- `Document::add_heightmap_mesh(pixels, img_w, img_h, resolution, scale, position) -> ObjId`:
  - Rescales the input to `resolution²` by striding; builds the mesh; inserts it as `ObjectKind::Mesh`.
- Shell UI: "Image → 3D" section in the paint inspector — Res DragValue (16–512), Scale DragValue (0.1–5), "→ 3D Heightmap" button.
- App drain: `pending_heightmap` flag drains in `main.rs`, calls `paint_readback_rgba`, builds mesh, sets selection.
- 1 test: `heightmap_mesh_size` — 4×4 white image → 16 verts, 18 triangles, all Y=1.0.
- *Upgrade path:* Neural depth/mesh from a single image (TripoSR, Zero123, Shap-E) via ONNX. The call site stays the same — the heightmap just becomes a neural network output.

**#78 — Auto-Rig (Skeleton/Spine)**
- `Bone { name, head, tail, parent }` — a bone in local parent space.
- `Skeleton { bones: Vec<Bone> }` — ordered hierarchy with parent indices.
  - `world_head(i)` / `world_tail(i)` — compute world positions by walking the parent chain.
  - `Skeleton::auto_rig_from_mesh(mesh, n_bones)` — detects the longest bounding-box axis, places `n_bones` equal-length bones as a spine from min to max of that axis.
- `Object::skeleton: Option<Skeleton>` — serde-optional field on every object (back-compat).
- `Document::auto_rig_selected_mesh(n_bones) -> bool` — builds and attaches a skeleton.
- Shell UI: "Skeleton / Auto-Rig" subsection in the mesh inspector — "Auto-Rig (spine)" button, shows `N bones` count after placement.
- 2 tests: `auto_rig_produces_correct_bone_count`, `auto_rig_bones_span_bounding_box`.
- *Upgrade path:* Replace heuristic with RigNet / RigFormer (graph neural network) via `ort` ONNX runtime. Inputs: mesh verts + connectivity. Output: skeleton joints + hierarchy. Same `Skeleton` data model.

### Decisions

- **Heightmap not remesh**: The pixel→height grid is O(res²) CPU — fast and deterministic. An ML mesh (TripoSR) is richer but requires model download and GPU inference. Heightmap is the right v1 for a paint-to-3D demo that works offline.
- **Spine auto-rig, not graph neural network**: RigNet gives better joint placement for complex shapes but needs ~400MB model + ONNX runtime. A bounding-box spine is sufficient for vertical subjects (humanoids, trees, columns) and works with zero external deps. Stored in the same `Skeleton` struct so swapping the generator leaves the rest of the pipeline untouched.
- **Skinning weights deferred**: The `Skeleton` stores bones only. Binding weights (per-vertex bone influence) are the next rigging step — deferred until an animation playback path exists to use them. The data model is ready for them (each `Bone` will get an inverse-bind matrix; the mesh will get `skin_weights: Vec<[f32; 4]>` + `bone_indices: Vec<[u8; 4]>`).

### Verification

- **45 workspace tests**, all green.
- Full workspace builds clean, zero warnings.

---

## Phase B — Rigging deepening: bones render + skinning + rig animation — 2026-06-27

Context: after the Tier 1–3 feature sweep, Armon chose to **deepen Visual (rigging→animation)** and **harden it for real use** rather than pivot to app #2. This pass closes the rig loop end-to-end.

### What shipped

**Bones render in the viewport (Task #68)**
- `Renderer::skeleton_segments: Vec<([f32;3],[f32;3])>` — world-space bone segments the app fills each frame from the selected object's skeleton. Drawn with the existing line pipeline (`skeleton_lines`): a warm shaft per bone + a 3-axis joint cross at each head.
- **Bug fixed here:** the original `Skeleton::world_head/world_tail` treated `bone.head/tail` as *parent-relative offsets*, but `auto_rig_from_mesh` stores them as *absolute object-space positions* — so multi-bone rigs would have rendered as an overlapping zigzag. Rest model is now unambiguously **absolute positions**; the `parent` link is used only for FK.

**Skinning + bone pose (Task #71)**
- `Bone.pose: Quat` — local rotation about the head, identity = rest. `Skin { indices: Vec<[u32;4]>, weights: Vec<[f32;4]> }` on `Object`.
- `Skeleton::rest_globals` / `pose_globals` / `skinning_matrices` — FK built so that at rest `A_i == R_i` exactly, hence every skinning matrix is identity and the mesh is untouched (verified by test). Pose: `A_i = A_parent · T(head_i − head_parent) · R(pose_i)`, skinning matrix `M_i = A_i · R_i⁻¹`.
- `Skeleton::auto_skin` — binds each vertex to its two nearest bone *segments* by inverse distance (weights sum to 1). `auto_rig_selected_mesh` now also auto-skins.
- `Mesh::skin_deform(skin, matrices)` — linear blend skinning; fail-safe clone if counts mismatch.
- `Object::display_mesh` runs skinning **before** modifiers, and only when a bone is actually posed (rest pose pays nothing).
- Inspector: bone selector + Euler (°) pose DragValues → live LBS deform in the viewport.

**Rig animation on the shared timeline**
- `suite_timeline::BoneTracks` (Euler XYZ) + `AnimationClip::bone_tracks: HashMap<u64, HashMap<u32, BoneTracks>>` (nested, not a `(u64,u32)` tuple key — JSON can't use tuple map keys). `Timeline::set_bone_keyframe` / `sample_bones`.
- App: "Key Bone Pose (at playhead)" button; `apply_timeline_samples` also drives bone poses on playback. The full loop works: auto-rig → pose → key at two times → Play → mesh deforms over time.
- Tests: `skinning_identity_leaves_mesh_at_rest`, `auto_skin_weights_normalized`, `posed_tip_bone_bends_only_the_tip`, `timeline_bone_keyframe_interpolates`.

**Hardening (Task #72)**
- Round-trip tests: `skeleton_and_modifiers_survive_save_load`, `heightmap_mesh_survives_save_load`. Skeleton (incl. bone parents + `pose`), Decimate modifier, and heightmap geometry all survive `.sweet` save/load losslessly.

### Decisions

- **LBS, not dual-quaternion skinning.** Linear blend is the standard first cut; it can candy-wrap at extreme twists but is correct, cheap, and matches the "integrate the hard kernel later" doctrine. *Upgrade path:* dual-quaternion skinning if twisting artifacts show up in practice.
- **Inverse-distance auto-weights, not bone-heat.** Distance-to-segment with 2-bone blend gives smooth seams for the spine rigs `auto_rig` produces. *Upgrade path:* Pinocchio-style heat/geodesic weights once a solver is authorized.
- **Euler bone tracks, not quaternion tracks.** Euler XYZ linear-interpolates trivially on the existing scalar `Track` and keyframes from the same inspector values. Gimbal lock is a non-issue for the single-axis spine poses this targets; quaternion slerp tracks are the upgrade when arbitrary 3-DOF joints arrive.
- **Skinning runs before modifiers** so a Subdivide/Mirror smooths/mirrors the *deformed* surface — matches user expectation (pose the low-poly cage, see the smooth result follow).

### Verification

- **51 workspace tests**, all green.
- Full workspace builds clean, zero warnings.
- App launches and runs the render loop without panic (verified via process + log; pixel screenshot blocked by the harness requiring a LaunchServices-registered signed app, not by the code).

---

## Hardening — document-level command-delta undo/redo — 2026-06-27

The `Document` had **no undo** before this (only paint had GPU-snapshot undo). CLAUDE.md's law is explicit: *"Undo = command-delta (apply/revert), never document snapshots."* This builds that.

### What shipped

**Undo core (`platform/doc`)**
- `ObjectEdit { id, before: Option<Object>, after: Option<Object> }` — `before: None` = add, `after: None` = remove, both `Some` = modify. A `Transaction` is a `Vec<ObjectEdit>` (one user action). `History { past, future }`, cap 128.
- Transaction API: `checkpoint(&[ids])` snapshots the named objects' prior state + the live id-set; `commit()` diffs to record adds (new ids), removes (named-then-absent), and modifies (changed), pushing one transaction and forking redo. `undo()`/`redo()` walk it. `record_object_change(before)` is the immediate-mode path (caller holds the pre-edit state).
- `ObjectArena::restore(object)` puts a removed object back at its **exact** id (slot+generation) so undo-resurrect never aliases.
- This is per-object command-delta, **not** whole-document snapshots — only the touched objects are cloned, honoring the law.
- 5 tests: add, remove (id-exact resurrect), transform, sculpt (+ no-op skip), redo-fork.

**App wiring (`apps/visual`)**
- Unified ⌘Z/⌘⇧Z across the two undoable surfaces (scene = command-delta, paint = GPU snapshots) via an `undo_order: Vec<UndoKind>` chronological stack — undo targets whichever surface the last action touched.
- Coverage: gizmo-translate + sculpt + add-tool **canvas drags** (checkpoint on press, commit on release — the whole drag coalesces to one step); **inspector** edits (transform, modifiers, auto-rig, bone pose, mesh-op buttons) coalesced per edit-burst via a frame baseline + settle; **hotkeys** (digit adds, extrude/inset/loop-cut/bevel, delete) wrapped per keypress; **pending drains** (CSG boolean, heightmap, adjustment add). New/Open reset the order stacks.

### Decisions

- **Per-object delta via serialized-equality no-op check.** `commit` skips transactions where the object is byte-identical before/after (compared via serde, run once per action — immaterial cost) so camera/tool-switch keys and click-with-no-drag don't create empty undo steps.
- **Edit-burst coalescing for immediate-mode UI.** egui applies a DragValue change before `.changed()` is observable, so the pre-edit state can't be captured mid-frame. Instead the app keeps a per-frame baseline of the selected object and records one transaction when the edit burst settles (a frame with no edit). Result: dragging a slider from 0→90° is **one** undo step, not 90.
- **Unified order stack, not two independent ⌘Z stacks.** Paint and scene edits interleave in real use; a single chronological origin stack makes ⌘Z "undo the last thing I did" regardless of surface — the expected behavior.
- **`record_scene` is a free associated fn over disjoint field refs, not `&mut self`.** The event handler holds a live `&mut renderer` borrow; an `&mut self` helper would conflict by signature alone. Passing `&mut self.undo_order, &mut self.redo_order, &mut self.shell.dirty` keeps the borrows disjoint.
- *Upgrade path:* true inverse-op deltas (store just the sculpt displacement, not the whole prior mesh) if undo memory becomes a concern on very large meshes; the `ObjectEdit` shape can hold a smaller delta variant later without changing call sites.

### Verification

- **56 workspace tests**, all green.
- Full workspace builds clean, zero warnings.
- App launches and runs the render loop without panic (process + log verified).

---

## Photo pillar — PhotoDemon-referenced adjustment set — 2026-06-27

Context: Armon proposed using three external repos — **Blender** (3D), **PhotoDemon** (photo/design), **KIRA** (painting). After verifying licenses he chose **reference / clean-room** integration (keeps SWEET commercially shippable). Findings that drove this:
- **Blender is GPL v2+** (viral copyleft) and is a C/C++ application, not a library — using its *code* would force SWEET to be GPL. So: reference only, no code. SWEET's 3D is already deep (meshes, half-edge, modifiers, sculpt, rigging).
- **PhotoDemon is BSD** (permissive, attribution) but is 100% Visual Basic 6 — zero lines reusable, but its filter *algorithms are legally portable*. This is the strongest reference.
- **`krafton-ai/KIRA` is the wrong repo for painting** — it's an Apache-2.0 Python LLM agent harness for Terminal-Bench, not a drawing tool. Flagged to Armon; awaiting the correct link.

So the first concrete step targeted the **photo/graphic-design pillar** (the one PhotoDemon maps to and SWEET's thinnest), implemented clean-room.

### What shipped

- Six new `AdjustmentKind` variants — **Exposure** (linear stops), **Vibrance** (saturation weighted by existing saturation), **White Balance** (temperature/tint), **Posterize**, **Threshold**, **Invert** — algorithm reference PhotoDemon (BSD, `tannerhelland/PhotoDemon`), reimplemented per-pixel in linear HDR.
- GPU: new `ADJ_*` mode constants + WGSL `case` branches in the compositor's adjustment shader (all per-pixel, single-sample — fit the existing pass).
- Inspector: the kind picker now enumerates `AdjustmentKind::all_defaults()`/`label()` (new kinds appear automatically) + per-kind sliders.
- `AdjustmentKind::apply_linear(rgb)` — a CPU reference mirroring the WGSL, unit-tested for known cases (exposure doubles, invert, threshold, posterize-to-2, white-balance warm/cool, vibrance no-op).

### Decisions

- **Reference / clean-room over vendoring code.** Blender's GPL would make SWEET GPL; PhotoDemon is VB6 (nothing to link). Studying both and implementing in Rust keeps SWEET shippable and matches the standing "port decisions, not code" + "integrate, don't reinvent the hard kernels" laws. Recorded in [[project-sweet]].
- **Per-pixel adjustments first; convolution filters deferred.** Exposure/Vibrance/WB/Posterize/Threshold/Invert all fit the existing single-sample adjustment pass. Blur/Sharpen/Edge-detect need a neighbor-sampling pass with a texel-size uniform — a separate, larger increment (next step in this pillar).
- **CPU reference (`apply_linear`) kept in lock-step with the WGSL.** The GPU path has no test-time readback, so the math is duplicated once in testable Rust. Also unlocks CPU thumbnails/previews later. The HSL-based kinds (BrightnessContrast/HueSaturation/Levels) stay GPU-only to avoid duplicating the HSL conversion.

### Verification

- **57 workspace tests**, all green.
- Full workspace builds clean, zero warnings.
- App launches and runs the render loop without panic (process + log verified).

---

## Convolution filters + first downloadable build — 2026-06-27

### What shipped

**Convolution filters (photo pillar, cont.)**
- Three neighbor-sampling adjustment kinds: **Box Blur** (radius 1–8), **Sharpen** (unsharp 3×3, strength), **Edge Detect** (Sobel magnitude). PhotoDemon-referenced, clean-room.
- The adjustment uniform `AdjParams` grew from 16→32 bytes to carry `texel = 1/size`; the compositor writes it from its stored `width/height`. WGSL cases 9–11 sample the neighborhood.
- `AdjustmentKind::apply_linear` leaves these as pass-through no-ops (they need neighbors — GPU-only); the test asserts that explicitly.

**First downloadable macOS app**
- `scripts/package_macos.sh` — builds `--release`, assembles `dist/SweetVisual.app` (Info.plist + binary), ad-hoc code-signs it (`codesign -s -`), and zips it. Repeatable; also advances the "ship off-dev" roadmap item.
- The release binary is **fully self-contained** — `otool -L` shows only `/usr/lib` + `/System/Library` (system frameworks). No dylib bundling needed (unlike the Qt/COMPOSITOR app). 12 MB binary, 5 MB zip.
- Delivered to `~/Downloads/`: `SweetVisual.app`, `SweetVisual-macos.zip`, `SweetVisual-QUICKSTART.txt`.

### Decisions

- **Single-pass box blur, not separable Gaussian.** A single full-screen pass sampling a (2r+1)² neighborhood is simple and fits the existing one-shot adjustment infrastructure; for r≤8 the tap count is acceptable for a non-interactive adjustment. *Upgrade path:* two-pass separable Gaussian (H then V) with an intermediate target when blur becomes an interactive/large-radius operation.
- **Ad-hoc signature, not notarized.** Notarization needs an Apple Developer cert (a secret / paid identity — an L4 gate). Ad-hoc signing + quarantine-strip lets Armon run it on his own machine via right-click→Open. A real signed+notarized build is a separate, sign-off-gated step before any public distribution.
- **No app icon yet.** Cosmetic; deferred. The bundle uses the system default icon.

### Verification

- **57 workspace tests**, all green; full workspace builds clean, zero warnings.
- The packaged `.app` (release binary, ad-hoc signed) launches and runs the render loop without panic.

---

## Painting pillar — Krita-referenced symmetry painting — 2026-06-27

Context: Armon sent `kde/krita` as the (corrected) painting reference, replacing the wrong KIRA link. **Krita is GPL-3.0** (C++) — same bucket as Blender: viral copyleft, reference-only. Applied the standing clean-room policy: study Krita's behavior, implement in Rust, copy no code. SWEET already has a real brush engine (pressure/tilt/stabilization/dual-brush + tiled substrate), so Krita is a *deepening* reference.

### What shipped

- **Mirror / symmetry painting** (Krita's signature feature): `symmetry_segments(from, to, mirror_x, mirror_y)` mirrors a brush segment across the canvas centre (u=0.5 / v=0.5). X or Y each add one mirrored stamp; both enabled give 4-way radial symmetry. Wired into both canvas paint-stamp sites (press dab + drag); the mesh paint-on-3D path is intentionally excluded (mirror is a 2D-canvas concept).
- Inspector: "Symmetry  ☐X ☐Y" checkboxes in the brush panel. `ShellState.mirror_x/mirror_y`.
- 3 unit tests (`symmetry_segments`): none → identity, X → horizontal mirror about centre, both → 4-way including the diagonal-opposite corner.
- Rebuilt the downloadable `.app` so Armon can test it; QUICKSTART updated.

### Decisions

- **Krita = reference only (GPL-3.0).** Confirmed via the repo. Identical reasoning to Blender — using its code would force SWEET to GPL. Recorded the policy already in [[project-sweet]]; Krita just lands in the same bucket. The earlier `krafton-ai/KIRA` link was an unrelated LLM agent harness — superseded.
- **Symmetry at the app layer, not the brush kernel.** Mirroring is a fan-out of stamp positions per input point — it belongs where input maps to UV (tools.rs), not inside the GPU brush. Keeps `platform/gpu`'s brush API unchanged and makes the feature trivially correct/testable. *Upgrade path:* N-fold radial + free-angle mirror lines (Krita has these) become a richer `symmetry_segments` returning more transforms; nothing else changes.
- **Canvas-only, not paint-on-3D.** Mirror across a flat canvas centre is well-defined; mirroring triplanar UVs on arbitrary 3D geometry is not the same feature and would surprise users. Left out deliberately.

### Verification

- **60 workspace tests**, all green; full workspace builds clean, zero warnings.
- Rebuilt packaged `.app` launches and runs without panic; delivered to `~/Downloads/`.

---

## Painting pillar deepening — brush engine + photo filters — 2026-06-27

A batch of Krita- and PhotoDemon-referenced painting/photo features (clean-room; both repos are reference-only — Krita GPL-3.0, PhotoDemon BSD).

### What shipped

- **Brush tips** — `BrushTip::{Round, Square, Soft}`. Passed per-dab in the vertex `params.z`; the brush shader switches the distance metric (euclidean / chebyshev / gaussian-bell). `DabVertex.params` widened `vec2`→`vec4`.
- **Per-stamp blend modes** — `BrushBlend::{Normal, Add, Erase}`. Fixed-function blend state is baked into the pipeline, so each mode is its own pipeline (`brush_pipelines[3]`), selected at stamp time. **Erase** is a real eraser (Zero, OneMinusSrcAlpha).
- **Smudge brush** — `Brush::smudge`. A dedicated `smudge_pipeline` samples a per-stamp copy of the canvas (`smudge_source`) at the fragment's own UV offset *backward* along the stroke, dragging colour forward. No CPU readback — stays on-GPU.
- **Color eyedropper** — `Tool::Eyedropper` ("Eye" button); `Renderer::pick_paint_color(uv)` reads the canvas texel (sRGB→linear) into the brush. Routed through `InputState::picked_color` (the press handler holds `&ShellState`).
- **Wrap-around tiling** — `ShellState::wrap_tiling`; `paint_fanout()` composes symmetry images with the 8 neighbour copies (±1 UV) for seamless-texture painting. Off-canvas dabs are GPU-clipped, so over-generating is cheap.
- **Separable Gaussian blur** — `AdjustmentKind::GaussianBlur`. The compositor's adjustment arm now runs a *list* of passes per entry; Gaussian emits two (H then V) with the direction in `p1/p2`, giving an O(2r) separable blur vs. the box blur's single O(n²) pass.

### Decisions

- **Blend modes as separate pipelines, not a shader branch.** wgpu bakes blend state into the pipeline and the fragment shader can't read the destination, so per-mode pipelines are the only correct route for true fixed-function compositing. Limited to the three modes expressible cleanly (Normal/Add/Erase); Multiply/Screen-style brush blends are deferred (they're already available as *layer* blend modes in the compositor).
- **Smudge samples a copy, never the live target.** Sampling the render target while writing it is undefined; the per-stamp `copy_texture_to_texture` into `smudge_source` is the price of an on-GPU smudge (still no CPU↔GPU round-trip, honouring the perf law).
- **Eyedropper does a one-click full readback.** `pick_paint_color` reads the whole paint texture for a single texel — fine for a click (not the paint hot-path). *Upgrade path:* a 1×1 `copy_texture_to_buffer` if picking ever needs to be continuous.
- **Separable, not single-pass, Gaussian.** Honoured the explicit ask: two 1-D passes through the existing ping-pong is O(2r) and reuses the adjustment infrastructure (just a multi-pass list), rather than an O(n²) single pass.

### Verification

- **61 workspace tests**, all green (added `erase_and_smudge_brushes_run_without_panic` exercising all 3 blend pipelines + smudge + all tips). Full workspace builds clean, zero warnings.
- Debug + release `.app` both launch with zero panics/validation errors. Rebuilt download delivered to `~/Downloads/` with an updated QUICKSTART.

---

## Bug fix — invisible egui shell (no toolbar / no panels) — 2026-06-27

**Symptom (Armon, running the `.app`):** the 3D scene renders fine but there is **no toolbar, no inspector, no panels**, and the canvas doesn't respond to clicks (all input lands on the missing panels / object picking needs the chrome).

**Diagnosis (instrumented the running app + screencaptures):**
- egui *was* generating correct geometry — 12 paint jobs, bounds `(0,0)..(1400,880)` (perfect for a 1400×880 logical window at ppp 2).
- Forcing the egui pass to `LoadOp::Clear(red)` turned the whole window red → the egui render pass reaches the surface, but `egui_renderer.render()` drew nothing on top.
- Per-mesh check: every panel mesh used `tex=Managed(0)` (the font atlas) with `registered=false`. egui-wgpu's `render()` **silently skips any mesh whose texture isn't registered** — and *every* egui primitive (even solid panel fills) samples the font atlas, so the whole UI vanished.
- Texture-delta log: egui only ever emitted **partial** atlas deltas (`pos=Some`, small glyph sub-regions), never the one-time **full** allocation (`pos=None`).

**Root cause:** the font-atlas texture deltas were applied **only inside the egui paint callback**, which runs inside `Renderer::render`. On the first frame(s) the surface is still warming up, so `render()` early-returns `RenderResult::Skipped` *before* invoking the callback. That skipped frame carried egui's single full-atlas delta (`pos=None`); it was dropped, egui marked the atlas uploaded and never re-sent it, so the renderer never had the atlas and skipped every panel draw forever. (A prior guard — "skip partial updates for unregistered textures" — turned the would-be panic into a silent no-op, masking the failure.)

**Fix:** apply egui texture deltas (`update_texture` for `set`) **unconditionally, before** calling `Renderer::render`, using `renderer.device()/queue()` — so the atlas upload can never be lost to a skipped present. The paint callback now only does `update_buffers` + `render` + `free_texture`. Removed the masking guard and all diagnostics.

**Verification:** debug *and* release `.app` now show the full shell — top bar (New/Open/Save), left tool strip (Sel…Eye), right inspector (Brush: Tip/Blend/Smudge/Symmetry/Wrap, Image→3D, + Adjustment Layer), bottom timeline — confirmed by screencapture. 61 tests green, clean build. Refreshed download in `~/Downloads/`.

**Lesson:** GPU resource uploads that a UI library emits **once** (font atlas, etc.) must never be gated behind a render path that can early-return. Apply them before any `Skipped`-able present.

---

## "GIMP/Photoshop ladder" — M1: real image open/export — 2026-06-27

Armon: "what can we do to at least get it to gimp/photoshop level." Honest assessment: SWEET was a 3D-first prototype with one fixed paint canvas — as a 2D editor it couldn't open an image, export a PNG, hold layers, select, transform, or place text. Agreed plan: a 5-milestone ladder to "a real basic raster editor" (GIMP is GPLv3 → reference only; full parity is a career). This is **M1 — image I/O**, the credibility floor.

### What shipped
- Added the `image` crate (MIT/Apache; png/jpeg/bmp/tga/gif/webp).
- `persistence::import_image_from(path, size)` — decode any supported format → fit (aspect-preserved, centered, white-padded) into a `size`² RGBA8 canvas buffer. Unit-tested.
- `persistence::import_image_dialog` / `export_png_dialog` — native rfd dialogs.
- `Renderer::paint_upload_rgba_undoable` + `RasterCanvas::upload_rgba_undoable` — replace the whole canvas as **one undoable edit** (⌘Z reverts to the prior canvas).
- Top-bar **Import Image** / **Export PNG** buttons (`FileAction::ImportImage` / `ExportPng`).
- `suite-visual <file>` — passing an image path on the command line imports it on launch (standard "open with…"; also the visual-test harness).

### Bugs found + fixed during verification
- **Undo clobber:** `upload_rgba` clears undo/redo at the end (correct for *Open*); the undoable variant was calling it and wiping the entry it just pushed. Split out a raw `write_texture_rgba`; `upload_rgba_undoable` uses that.
- **Queue-vs-encoder ordering:** `queue.write_texture` is flushed *before* the command buffers in a submit, so the snapshot copy (recorded in the encoder) captured the *new* pixels → undo restored the import instead of reverting it. Fixed by uploading via a staging buffer + `encoder.copy_buffer_to_texture`, ordered after the snapshot. GPU test (`upload_undoable_replaces_then_undo_restores`) proves it.

### Verification
- 63 workspace tests green; clean build.
- **Visually confirmed** (screencapture): launched `suite-visual <screenshot.png>` → the image appears centered on the artboard, undistorted, title shows `untitled •`. Earlier "white canvas" runs were operator error (pointed at scratchpad files the harness had GC'd between turns), not a code bug.

Next: **M2 — real raster layer stack + Layers panel.**

---

## GIMP ladder — M2: real raster layer stack + Layers panel — 2026-06-27

The soul of an image editor. Built as a renderer refactor in verifiable increments.

### Architecture
- `Renderer` now holds `layers: Vec<PaintLayer>` (each = its own `RasterCanvas` + name/visible/opacity) + `active_layer`. The brush, undo/redo, clear, import, and magic-wand all target `layers[active_layer]`.
- `self.raster` became the **flattened display cache** that `PaintCanvas` objects sample — unchanged on the display side, so the render path, picking, save/load all kept working.
- `record_composite` blends visible layers bottom→top into the display cache via a new full-screen `layer_blend_pipeline` (`LAYER_BLEND_WGSL`): alpha-over × per-layer opacity. UV uses the top-left-origin convention so it's a 1:1 match to the brush/texture layout. `render()` recomposites when `layers_dirty`.
- The **Background** layer is opaque paper; layers added on top start fully transparent so they composite over what's below.
- Public API: `layer_infos`, `active_layer`/`set_active_layer`, `add_layer`, `delete_layer`, `set_layer_visible`, `set_layer_opacity`, `move_layer`.
- **Layers panel** (shell.rs, Paint tool): list (top-first) with per-layer visibility checkbox, click-to-activate, opacity slider, Up/Dn reorder, Del, and "+ Add". UI→renderer via `ShellState::pending_layer_cmd` drained in main.rs. The right inspector now scrolls (`ScrollArea`) so the panel is always reachable.

### Verification (visual — the compositing is GPU-Renderer-bound, not headless-unit-testable)
- **Increment 1** (one layer): launched with an image import → it displays correctly through the new composite path, right orientation, no flip. Confirms the layer plumbing didn't break single-layer painting.
- **Increment 2** (multi-layer): a self-test painted a **red** stroke on layer 0, added a transparent layer, painted a **blue** stroke on it → screencapture shows **blue composited over red** at the intersection with **red showing through** the transparent areas. That's correct over-ordering + opacity + transparency — real raster layers.
- Layers panel renders (brush panel + "+ Add" + rows); 63 tests green; clean build; release `.app` refreshed in `~/Downloads/`.

### Decisions / deferred to M2.2
- **Display via a flattened cache, not re-pointing `PaintCanvas` at a new texture** — minimal blast radius; the entire display/picking/save path stayed identical.
- **Per-layer blend modes beyond Normal, layer masks, and multi-layer `.sweet` persistence are deferred.** For now Save/Export **flatten** the composite into the existing single PNG blob (Export PNG already reads the live composite, so the visible result is preserved); reopening loads one Background layer. Multi-layer save is the first M2.2 task.
- **No headless unit test** for the composite: a full `Renderer` needs a winit window, so the GPU layer path can't be exercised in `cargo test`. Covered by visual verification + the standalone `RasterCanvas`/compositor GPU tests. (A future refactor could split an offscreen renderer for testing.)

Next: **M3 — selections**, then M2.2 cleanups (blend modes, masks, layer persistence).

---

## M2.2 — multi-layer .sweet persistence — 2026-06-28

Closed the data-loss gap from M2 (saving flattened the layer stack to one PNG).

### What shipped
- `Renderer::layer_pixels(i)` (read back one layer's raw RGBA, not the composite) + `Renderer::replace_layers(Vec<LoadedLayer>)` (rebuild the stack on load). `LoadedLayer` is a new public type.
- `.sweet` format: each layer is a `main.layer.{i}.png` blob; `main.layers` is an ordered metadata doc (`name`/`visible`/`opacity`). `save_to(doc, &[LayerSave], path)` / `load_from` reconstruct the stack.
- **Back-compat:** a project with only the legacy `main.paint.png` (pre-layers builds) loads as one "Background" layer. Covered by a test.
- main.rs `current_layers()` gathers the stack for save; Open applies `replace_layers`.

### Verification
- `scene_and_layers_round_trip_through_a_bundle`: 2 layers with distinct pixels + metadata (name/visible/opacity) survive save→load.
- `legacy_single_paint_blob_loads_as_one_layer`: old single-blob projects still open.
- 64 workspace tests green; clean build; app launches without panic after the Open-path refactor.
- *Note:* the dialog-driven save→reopen can't be UI-automated; verified via the format round-trip test + the (proven-analogous) GPU readback/upload primitives.

### Deferred (M2.3): per-layer blend modes (needs the dest in-shader → ping-pong composite) and layer masks.

---

## M2.3 — per-layer blend modes — 2026-06-28

Layers now blend with **Normal / Multiply / Screen / Overlay / Soft Light / Hard Light / Add / Subtract** (the same `BlendMode` set the adjustment compositor uses).

### Implementation
- The single-pass alpha-over composite became a **ping-pong**: clear `comp_a` transparent, then for each visible layer blend `(running base, layer src)` → the other scratch texture with the layer's mode + opacity (Porter-Duff over), swap; copy the final scratch → the display cache. Two `Rgba8UnormSrgb` scratch textures (`comp_a`/`comp_b`).
- `LAYER_COMPOSITE_WGSL` mirrors the compositor's blend math (overlay/soft-light/hard-light/etc), so 2D layers and adjustment layers behave identically. `blend_mode_u32` maps the enum to the shader switch.
- The **bottom** visible layer is forced to Normal (it blends over a transparent base; other modes there would give wrong colours — matches Photoshop).
- `PaintLayer`/`LayerInfo`/`LoadedLayer` carry `blend`; `set_layer_blend` API; a **Blend dropdown** per layer in the panel; blend mode **persists** in `.sweet` (serde value in `main.layers`, with a round-trip test asserting Multiply survives).

### Verification
- **Visual:** a red layer + a blue **Multiply** layer → the overlap is clearly **darkened** (red×blue = dark purple) while blue over white paper stays blue. Correct Multiply.
- 64 workspace tests green (incl. blend-mode persistence round-trip); clean build.

Layers are now "real": multiple layers, visibility, opacity, **blend modes**, reorder, full persistence. Remaining layer polish (masks, duplicate/merge) is optional. Next: **M3 — selections**.

---

## GIMP ladder — M3: rectangle marquee selection — 2026-06-28

A selection that **constrains painting** is the credibility floor for "select" in a raster editor. Shipped the rectangle marquee; lasso/wand-as-selection/feather are deferred (they need a per-pixel mask texture, not representable as one rect).

### What shipped
- **`Tool::RectSelect`** ("Mrq" button, hotkey **M**). Drag on the canvas draws a selection rect in UV space; press records the drag start, cursor-move updates it live, release finalises (degenerate <0.001 UV rects are cancelled — guards stray clicks).
- **GPU scissor clipping** (not a shader mask): `RasterCanvas::stamp_segment` gained a `scissor: Option<[u32;4]>` param → `rpass.set_scissor_rect(...)`. `Renderer::selection_rect` (UV) + `selection_scissor()` converts UV→texel and feeds it to every brush stamp in `paint_stamp`. Efficient, no extra texture, no shader change. Mesh painting passes `None` (3D isn't 2D-selection-constrained).
- **State flow:** `ShellState` is immutable in the tool handlers, so the live rect lives in `InputState::select_rect` (which is `&mut` everywhere), synced `input → shell → renderer` before each frame and written back after `run_ui` (so the inspector's Select-All/Deselect buttons reach the GPU).
- **Marching-ants overlay** in the `CentralPanel`: white + phase-shifted black dashed perimeter via `egui::Shape::dashed_line`, animated off `ctx.input(|i| i.time)` with a continuous `request_repaint`.
- **Shortcuts:** ⌘A = select all (`[0,0,1,1]`), ⌘D = deselect; inspector shows the active rect % + Select All / Deselect / Invert.

### Verification
- Headless `scissor_clips_stroke_outside_rect`: a right-half scissor blocks a left-centre stroke — the pixel at u=0.25 stays paper. Green.
- 65 workspace tests green; clean build.

Next: **M4 — core 2D tools.**

---

## GIMP ladder — M4 (part 1): gradient tool + layer transforms — 2026-06-28

First slice of M4. Gradient fill and whole-layer flip/rotate — the two highest-leverage 2D ops after selection.

### What shipped
- **Gradient tool** (`Tool::Gradient`, "Grd", hotkey **G**). Drag start→end on the canvas fills the **active layer** from the brush colour to transparent. **Linear** (along the drag vector) or **Radial** (centred on the start) in the inspector. Source-over composite in **linear** space (decode sRGB → blend → re-encode, since the canvas is `Rgba8UnormSrgb`). Respects the active selection rect. A live white/black **guide line** with end-cap handles draws during the drag. Undoable.
- **Layer Transform** (inspector, shown for the paint-ish tools): **Flip H / Flip V / Rotate ⟳ (90° CW) / Rotate ⟲ (90° CCW) / 180°** on the active layer. Undoable.
- **Testability:** the pixel math is extracted into pure module fns `apply_gradient_fill(...)` and `apply_layer_transform(...)`; the GPU `Renderer` methods are thin (readback → pure fn → `upload_rgba_undoable`). 6 new headless unit tests (flip/rotate inverses + identities, gradient opacity falloff, selection-bounds clipping).
- **G is context-sensitive:** with a focused mesh edge it still bevels (modeling); otherwise it selects the Gradient tool. The "Grd" tool-strip button is the unambiguous path when a mesh is also selected.

### Design notes
- The right-most gradient texel never reaches pure background: its centre sits at t≈0.97, so ~3% of the source remains, and sRGB encoding lifts that to ~49/255 for dark colours. Expected, not a bug — the test asserts a hard falloff (right < mid/2), not "near zero".
- Both ops route through the active layer's existing undo stack, so they coalesce into the same `UndoKind::Paint` history the brush uses — ⌘Z just works.

### Verification
- 71 workspace tests green (16 in suite-gpu incl. the 6 new + the M3 scissor test); clean `suite-visual` build.
- **Live-verified in the running app** (computer-use): painted a stroke, dragged a linear gradient (grey→transparent, upper-left → lower-right) — the canvas shows the correct tonal fill — then hit **Flip H** and watched both the stroke and the gradient mirror horizontally.

### Regression caught + fixed during live verification (important)
The M3 selection overlay was an `egui::CentralPanel` covering the canvas. A panel registers an interactive area, so `ctx.is_pointer_over_area()` (hence `wants_pointer_input()`) returned **true** whenever the cursor was over the canvas, and `egui_winit::on_window_event` marked the pointer press/drag events **consumed** — silently swallowing every canvas-tool interaction (paint, gradient, marquee). It built and unit-tested fine but was dead on click. **Fix:** draw the canvas overlays (marching-ants + gradient guide) onto a **non-interactive foreground `layer_painter`** over `ui.available_rect_before_wrap()` instead of a panel. A layer painter allocates no widgets and registers no area, so raw winit pointer events flow through to the tool handlers. Lesson recorded in memory ([[sweet-egui-overlay-gotcha]]).

Next in M4: crop, bucket-fill polish, text tool. Then **M5 — arbitrary canvas size + sparse tiling**.

---

## GIMP ladder — M5 (core): arbitrary (non-square) canvas dimensions — 2026-07-06

Armon: "yes do all [ROADMAP tiers], really reference it [the real algorithms/prior art]" — a green light to keep working the roadmap autonomously. Started with M5 since it was flagged as the single highest-leverage fix ("Fixed square surface is the single most glaring limit — real images aren't square") and doesn't touch the [[feedback-sweet-clean-room-over-vendoring]] question (no new deps either way).

### What shipped
- **`RasterCanvas`** (`platform/gpu/src/raster.rs`): `size: u32` split into independent `width`/`height` everywhere — texture creation, brush stamping, undo/redo history regions, `clear_undoable`, `upload_rgba_undoable`, readback. `RasterCanvas::new(device, width, height, paper)`.
- **Two latent bugs the square-only assumption was hiding, found and fixed while doing this** (both invisible until width≠height was possible):
  - `note_dirty`'s y-bounds were clamped against `tw` (width) instead of a height bound — harmless when `tw==th`, a real corruption risk once they differ.
  - Brush dab vertex generation (`push_dab`) used an equal clip-space radius on both axes. Clip space `[-1,1]` always maps to the *full* render target regardless of its pixel aspect, so an equal clip-space radius only looks circular in texels when width==height. Fixed by scaling the clip-space y-radius by `width/height`, with `radius_uv` now documented as "fraction of canvas **width**" — a brush stays circular in texel space on any aspect ratio.
- **`Renderer::import_replace_canvas(width, height, pixels)`** (new): recreates the display cache, both compositor scratch textures, and collapses the layer stack to one Background layer at the new dimensions. "Import Image" now sizes the *whole canvas* to the image's own native aspect ratio (clamped to a 4096px max dimension, `Lanczos3` downscale only if needed) instead of always padding into the old fixed square with white bars.
- **`persistence.rs`**: `PaintImage`/`LayerSave` gained `width`/`height` (was one `size` field). `import_image_from` returns `(width, height, rgba)` — no padding, no forced square. `decode_png`'s old "must be square" hard error is gone (PNG decode just reads its own real dimensions) — meaning **old saved `.sweet` files load unchanged**, since each layer's dims were always derived from its own embedded PNG blob, never a separate stored field.
- **90° rotation redesigned as a whole-*document* op.** A per-layer 90° rotate swaps that one layer's width/height, which would desync it from every sibling layer's dims once the canvas isn't square (Photoshop draws the same distinction: "Image Rotation" affects the whole canvas; a per-layer "Transform → Rotate" doesn't change canvas bounds). Split `LayerTransform` down to `{FlipH, FlipV, Rotate180}` (always dimension-preserving, so always safe) and added a separate `CanvasRotate{Cw,Ccw}` + `Renderer::rotate_canvas_90` that rotates every layer + recreates the comp buffers together, keeping everything aligned. Inspector now shows "Layer Transform" (Flip/180°) and "Rotate Canvas" (90°) as two distinct groups.
- **`PaintCanvas` 3D object aspect**: `App::rescale_paint_canvases_to_aspect` (apps/visual/src/main.rs) rescales the artboard's world-space footprint non-uniformly (longer side pinned at the starter-scene's 3.0 units) after any canvas-dimension-changing op, so a landscape import renders as a wide rectangle in the 3D view instead of squished into the old 1:1 quad.
- Neither `import_replace_canvas` nor `rotate_canvas_90` is undoable (both are structural resizes, same posture as opening a project) — the app-level undo/redo queues are cleared alongside so a later ⌘Z can't land on a stale now-inert entry from before the resize.

### Testing
7 new tests: `raster::tests::non_square_canvas_paints_and_reports_correct_dimensions`; 5 in `m4_tests` retargeted at non-square buffers (`flip_h/flip_v` on a 6×3 buffer, `rotate_180` twice-is-identity, and two new `rotate_canvas_pixels` tests — CW-then-CCW round-trips through the dimension swap, and a hand-verified corner-mapping check); `persistence::tests::import_image_keeps_native_aspect_when_under_max_dim` + `..._downscales_when_over_max_dim` replacing the old "fits centered on white" test (that behavior no longer exists). 74 workspace tests green.

### Scope cut, stated plainly
"M5: Arbitrary canvas size + sparse tiling" in ROADMAP.md bundled two different features. This entry ships the first (non-square dimensions) — the `TileCanvas` sparse 256²-tile substrate (already built, `platform/gpu/src/tile_canvas.rs`, up to a 4096² extent) is **not** wired in as the main per-layer surface. That's a distinct, larger undertaking (sparse allocation, brush stamping across tile boundaries, compositor changes) properly scoped as its own future item, not silently folded into this one.

### Verification gap, stated plainly
Live GUI verification was blocked this session: both the computer-use screenshot tool and the native macOS `screencapture` CLI returned solid-black frames, and an `osascript`/TCC permission query hung outright — a system-level Screen Recording/Automation permission failure, not a code issue, and not something fixable from inside the sandbox. Packaged the app, loaded a hand-crafted 800×300 test PNG (`suite-visual wide_test.png`) via command-line launch to confirm the process runs without crashing, but could not visually confirm the aspect ratio renders correctly on screen. Substituted the automated test suite above as verification. **A real visual check (import a wide/tall image, confirm no squish; rotate canvas; flip/180°; save→reopen) is still owed** once the permission issue is resolved — flagging this rather than claiming a live check that didn't happen.

Next: remaining M4 (crop, text tool), then Tier 1 (selection mask + lasso/poly/wand/feather).

---

## GIMP ladder — M4: Crop — 2026-07-06

Directly unblocked by M5: crop is exactly "resize the canvas + offset layers", and M5 had just built that exact recreate-at-new-dims machinery for `import_replace_canvas`/`rotate_canvas_90`.

### What shipped
- **`crop_pixels(src, src_width, src_height, x0, y0, crop_w, crop_h)`** (pure fn, `platform/gpu/src/lib.rs`): extracts a sub-rectangle from an RGBA8 buffer, row by row. Clamps `crop_w`/`crop_h` if the requested rect overruns the source.
- **`Renderer::crop_to_rect(x0, y0, x1, y1)`** (UV space, 0..1): drains every layer (same 0-first drain pattern as `rotate_canvas_90`, for the same reason — reading `layer_pixels(i)` against a shrinking vec would desync pixels from the wrong layer once removal starts), crops each layer's pixels to the rect, then recreates `raster`/`comp_a`/`comp_b`/the whole layer stack at the cropped dimensions, uploading each layer's cropped pixels. No-op on a degenerate (zero-area) rect.
- **UI**: "Crop to Selection" button in the `RectSelect` inspector panel — reuses the M3 selection rect directly as the crop source. Disabled when there's no selection or the selection is the whole canvas (nothing to crop to). Wired through `ShellState::pending_crop` → `main.rs` drains it, calls `crop_to_rect`, rescales the `PaintCanvas` 3D object to the new aspect (via the same `rescale_paint_canvases_to_aspect` helper M5 built), clears the now-meaningless old selection, and clears the undo/redo queues (crop is a structural resize, not undoable — same posture as import/rotate).

### Testing
2 new tests: `crop_extracts_the_requested_sub_rect` (verifies output dims + that pixels come from the right source offset, not shifted), `crop_clamps_a_rect_that_overruns_the_source_bounds`. 76 workspace tests green.

### Verification gap continues
Same system-level Screen Recording/Automation permission issue as M5 — briefly cleared mid-session (one real screenshot went through) then reverted to black frames once another Claude session took the computer-use lock, confirmed by the returned image being byte-identical in size to the earlier failure. Relied on the automated suite again rather than claim an interactive check (drag a selection, click Crop, confirm the canvas actually shrinks) that didn't happen. Owed once the lock/permission situation is clear.

Next: remaining M4 (text tool, free transform), then Tier 1 (selection mask + lasso/poly/wand/feather).

---

## "Make sure we have all Photoshop tools" — audit + M4d: Move (2D) — 2026-07-06

Armon: "make sure we have all photoshop tools." Honest answer first, not a demo: **SWEET doesn't, and several missing families are each real, multi-day projects on their own** — pen/vector shapes, healing/clone/history-brush, dodge/burn/sponge, mixer brush, content-aware fill. Built a complete tool-by-tool checklist against SWEET's actual code (not memory) and put it in ROADMAP.md rather than make a vague claim either way — see "Photoshop tool-parity checklist" there for the full table. Then shipped the single highest-value, tractable gap: the **Move** tool, arguably Photoshop's most-used tool, which had zero SWEET equivalent (`Translate` only moves 3D scene objects).

### What shipped
- **Pure fns** (`platform/gpu/src/lib.rs`): `shift_pixels(src, w, h, dx, dy)` — whole-buffer shift, drops content pushed off-canvas, reveals transparent at the vacated edge (no wrap-around). `move_selection_pixels(src, w, h, sel_x0, sel_y0, sel_w, sel_h, dx, dy)` — cuts the selection region out (leaving a true transparent hole, not just a copy), then alpha-composites it back at the shifted position in **linear** space (reusing the same `srgb_to_linear`/`linear_to_srgb` helpers `apply_gradient_fill` established).
- **`Renderer::move_active_layer(dx, dy) -> Option<[f32;4]>`**: reads the active selection rect (if any), converts to texel bounds, dispatches to `move_selection_pixels` or `shift_pixels`, writes back as **one** undoable edit. Dimension-preserving, so it's safe on any canvas aspect (M5) with no special-casing needed. Returns the selection's new UV rect (if there was one) so the caller can move the marching-ants to follow the content, matching Photoshop's behaviour.
- **`Tool::MoveLayer`** ("MovL" in the tool strip, hotkey **V** — Photoshop's actual Move shortcut, context-sensitive like G: bevels a focused mesh corner if one exists, else picks Move). Drag-to-preview, **commit-on-release** — the same shape as the Gradient tool (a guide line + end-cap handles during the drag; the canvas itself doesn't change until release, so there's no need to juggle live per-frame GPU writes against the undo/redo-clearing behaviour `RasterCanvas::upload_rgba` has (a real footgun considered and avoided: plain `upload_rgba` unconditionally clears both undo AND redo, so a naive live-preview-every-frame design would have silently destroyed its own undo entry on the very next preview frame).

### Testing
2 new tests: `shift_pixels_moves_content_and_reveals_transparent_edges` (confirms drop-not-wrap + transparent reveal), `move_selection_pixels_cuts_a_hole_and_pastes_at_the_new_spot` (confirms the original spot becomes a genuine transparent hole, not stale background, and the destination shows the moved content). 78 workspace tests green.

### Verification gap continues
Same Screen Recording/Automation permission story as M5 and Crop: blocked by another Claude session holding the computer-use lock for the whole increment. Packaged the app anyway; a live check (drag to move a layer, then with a selection active, confirm the hole + the marching-ants follow the moved content) is still owed.

Next: remaining M4 (text tool, free transform), then Tier 1 (selection mask — the piece that unlocks Elliptical Marquee, Lasso, Quick Selection, Quick Mask, and layer masks all at once).

---

## Tier 1 S1 + S2: selection mask, Ellipse Select, Lasso — 2026-07-06

Armon: "let's do all that" — a green light to keep working the "make sure we have all Photoshop tools" checklist. Went straight for the highest-leverage item flagged in the previous entry: the selection-mask infrastructure, since it's the one piece that unlocks several tools at once rather than being one more one-off.

### Design decision: keep the rect fast-path completely untouched

`Renderer::selection_rect: Option<[f32;4]>` (the bounding box, driving the brush GPU scissor and Crop) is unchanged. A new `Renderer::selection_extra: Option<SelectionShape>` sits alongside it — `None` for the common case (`RectSelect`, deselect, select-all), so that path has zero new code running. `SelectionShape` is `Ellipse{cx,cy,rx,ry}` or `Polygon(Vec<[f32;2]>)`, all UV space. Deliberately **not** a standing canvas-sized mask buffer synced every frame — that would copy a full-resolution array 60 times a second for something that changes on the order of once per drag. Instead, `rasterize_selection_mask(width, height, shape)` runs on demand, exactly when Gradient or Move actually needs it (once per drag-release, not once per frame).

### What shipped
- **`SelectionShape`**, **`rasterize_selection_mask`** (antialiased ellipse via signed distance + smoothstep falloff; antialiased-free even-odd scanline polygon fill — both textbook CS graphics algorithms, clean-room), **`selection_shape_bounds`** (bounding box for the scissor/Crop fast path).
- **`apply_gradient_fill`** and **`move_selection_pixels`** both gained an `Option<&[u8]>` mask parameter: gradient multiplies the source alpha by the mask's coverage at each texel; Move's cut-and-composite only touches texels where the mask is nonzero (a rect-bound pixel with mask=0 is left completely alone — not cleared, not moved). Both fall back to their exact pre-existing rect-only behaviour when `mask` is `None`, so RectSelect users see no change at all.
- **`Tool::EllipseSelect`** ("Elps") — drag defines a bounding box; the ellipse is inscribed in it. **`Tool::Lasso`** ("Lso", hotkey **L** — Photoshop's real shortcut) — drag traces a freehand point path, implicitly closed on release. Both keep `select_rect` in sync as their bounding box throughout the drag (a bug caught and fixed before it shipped — see below), so the degenerate-selection cleanup and the scissor/crop fast paths work unmodified.
- **Marching-ants generalized** from "always 4 rect corners" to an arbitrary closed point path: an ellipse traces as a 48-segment polygon approximation; a lasso traces its own recorded points. The perimeter used for the animated dash-marching phase is now the actual summed segment length, not a rect-only `2*(w+h)` shortcut.
- **Move follows the shape**: after a Move, `Renderer::move_active_layer` shifts an active `SelectionShape`'s coordinates (an ellipse's centre, every polygon vertex) by the same UV delta as the pixels — Photoshop's marching ants track a moved selection the same way.

### Bug caught and fixed before it shipped
The Lasso cursor-move handler initially only appended to `lasso_points` and set `select_extra` — it never updated `select_rect`. Since `select_rect` stayed frozen at the single-point degenerate rect from mouse-press, **every** Lasso selection would have been silently cancelled by the existing "degenerate selection" cleanup on release (which checks `select_rect`'s width/height). Fixed by keeping `select_rect` synced to `selection_shape_bounds(&shape)` throughout the drag, same as Ellipse. Caught during implementation, before any build/test — the kind of thing that's obvious once you trace the data flow but easy to miss when adding a new tool that reuses an existing cleanup path.

### Testing
6 new pure-function tests (ellipse centre/corner selection, independent x/y radii — proving no accidental aspect-correction the way a round brush dab needs, a triangle polygon fill, bounding-box computation for both shapes, and a gradient-respects-exact-mask test that specifically checks a bounding-rect corner *outside* the ellipse stays untouched — the test that would have caught it if masking silently fell back to rect-only behaviour). 83 workspace tests green.

### Scope cut, stated plainly
**Brush painting is not yet mask-aware.** Painting inside an Ellipse/Lasso selection today is still bounded only by the selection's bounding-rect GPU scissor, same as before this entry — the exact shape is respected by Gradient and Move, not yet by the brush. Making Paint respect the exact shape needs either a bind-group/shader change to the brush pipeline (touches every stroke in the app) or a stroke-end CPU blend against the existing `pre_stroke` snapshot texture — deliberately deferred as its own tracked item (S1b in ROADMAP.md) rather than rushed into the shared brush pipeline under time pressure. Also not done this pass: Quick Selection (retargeting Magic Wand to produce a mask instead of directly flood-filling) and feathering (blurring the mask edge) — both real, both next.

### Verification gap continues
Screen Recording access flickered available again mid-session (`request_access` succeeded with no "another session" error) but the actual capture still returned the same byte-identical black frame as every prior attempt this session — confirms the block is at the OS capture layer, not the session-lock layer I'd been assuming. Relied on the automated suite once more; the geometric-correctness tests above (independent radii, mask-vs-bbox, shape tracking) are deliberately the closest a unit test can get to "did this actually draw right" without eyes on the screen.

Next: masked brush painting (S1b) or Quick Selection retargeting, then M4's remaining items (text tool, free transform), then layer masks (S3).

---

## Tier 1 S1b: masked brush painting — 2026-07-07

Armon: "do all" — closed the gap flagged at the end of the previous entry rather than move on with it outstanding.

### Approach chosen, and why the alternative was rejected
Two ways to make Paint respect an exact Ellipse/Lasso shape: (a) a mask-sampling brush fragment shader, which means adding a bind group to `RasterCanvas`'s brush pipelines — currently `bind_group_layouts: &[]`, i.e. every brush stroke in the app, square-canvas or not, selection or not, runs through this pipeline; or (b) a stroke-end CPU blend against the `pre_stroke` snapshot the canvas already captures for undo. Went with (b): it's additive (a new method, `end_stroke_masked`, sitting next to the existing `end_stroke` — nothing about plain painting changes) rather than a modification to code every stroke depends on.

### What shipped
- **`RasterCanvas::end_stroke_masked(device, queue, encoder, mask, mask_width)`**: on stroke end, reads back just the dirty bounding-box region (not the whole canvas) from both the live texture (post-stroke) and `pre_stroke` (pre-stroke, already captured), blends the two per-pixel by the mask's coverage — in **linear** space for RGB (the texture is sRGB-encoded), a direct lerp for alpha (not gamma-encoded) — writes the blended region back via a targeted `queue.write_texture`, then runs the normal undo-history capture unchanged.
- **`Renderer::paint_end_stroke`** branches on `self.selection_extra`: `Some(shape)` rasterizes the mask on demand and calls `end_stroke_masked`; `None` (the common case — no selection, or a plain RectSelect) calls the original `end_stroke`, byte-for-byte unchanged code path.
- **Documented, not hidden, approximation**: the blend is "stroke net pre→post change," not "each dab masked individually before compositing." For a typical single-pass stroke these are visually identical; they could diverge for a stroke that paints back over the same pixels multiple times with a non-Normal blend mode. Written into the method's doc comment, not left implicit.

### Bug caught by the test, before it shipped
`copy_texture_to_buffer` (used for the dirty-region readback) requires `bytes_per_row` to be a multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256) — a real wgpu/GPU constraint. The codebase's existing full-canvas readbacks satisfy this by accident (canvas sizes like 256, 1536 happen to be 64-texel-aligned); an arbitrary stroke's dirty-region **width** essentially never is. Wrote the test with a real GPU device (not just a CPU pure-function test) specifically because this was a genuinely new code path — the wgpu validation error fired on the very first run. Fixed by padding the readback buffer's row stride to the alignment and stripping the padding back out per row on the way out; confirmed `write_texture` (the write side) has no equivalent constraint by checking the existing full-canvas `write_texture_rgba` already writes arbitrary widths successfully (M5's non-square canvases). This is exactly the class of bug the CPU-only pure-function tests (crop/rotate/gradient/move) can't catch — worth the extra weight of a headless-GPU test for genuinely new GPU-touching code, not just for the pixel math.

### Testing
1 new GPU-backed test: a stroke painted across a mask boundary correctly keeps the masked-in half and reverts the masked-out half to paper, even though the live GPU dabs touched both sides (only the coarse bounding-box scissor constrained painting during the drag). 84 workspace tests green.

Next: Quick Selection retargeting (Magic Wand → produces a mask, not a direct fill) or M4's remaining items (text tool, free transform), then layer masks (S3).

---

## Tier 1 S2b: Quick Selection retargeting — 2026-07-07

Armon: "do all" — went straight from S1b's documentation into the next item on my own stated list rather than stopping to ask.

### What shipped
Magic Wand used to flood-fill the click point directly into painted pixels (`paint_magic_wand_fill`, mutating the active layer and re-uploading). Retargeted it to `Renderer::magic_wand_select`, which runs the same BFS but returns the result as a `SelectionShape::Mask { width, height, data }` instead — a new fourth variant alongside `Ellipse`/`Polygon`. The caller (`tools.rs`) assigns it to `selection_extra`/`select_rect`, exactly like an Ellipse or Lasso selection. Once it's a selection rather than a direct paint, everything Tier 1 already built keeps working for free: Paint (S1b), Gradient, and Move all respect the exact flood-filled region, not just its bounding box — and it composes with Crop, Select All/Deselect/Invert, and undo the normal way instead of being a single irreversible pixel write outside undo's per-stroke granularity.

`Mask` is a genuinely different kind of shape from `Ellipse`/`Polygon`: those two are UV-space and resolution-independent (a center+radii or a point list means the same thing at any canvas size); a flood-filled region has no compact vertex representation, so `Mask` stores the raw per-texel coverage instead. That makes it resolution-*coupled* — `rasterize_selection_mask`'s new `Mask` arm passes the data through unchanged when sizes match (the common case) and falls back to a nearest-neighbor resample if the canvas was resized (Crop/rotate) since the wand selection was made, rather than silently dropping it. `selection_shape_bounds` scans the mask for its tight nonzero extent. Move's shape-shift step got a `Mask` arm too: since there are no vertices to translate, it shifts the raw mask data by the same pixel delta as the pixels it's paired with.

Followed the project's established pure-function discipline: extracted the BFS itself into `flood_fill_mask(pixels, width, height, seed_uv, tolerance) -> (mask, count)`, a plain CPU function with no GPU dependency, and made `magic_wand_select` a thin GPU-readback wrapper around it — same shape as `apply_gradient_fill`/`crop_pixels`/`move_selection_pixels`/`apply_layer_transform`. This is what let me write real algorithmic tests (matching-region extraction, and specifically that 4-connectivity doesn't leak through a diagonal-only touch) without a GPU device, rather than only being able to exercise it through the app.

### Bug caught before it shipped — and it wasn't in the new code
While wiring the new selection into `handle_left_release`, I found that its existing Lasso-cancellation check was using `input.lasso_points.len() < 3` as a stand-in for "the user was dragging a Lasso and didn't close it." That's true for *every* tool that isn't Lasso — Ellipse Select's `select_extra` is set via a completely separate field (`ellipse_drag_start`) and never touches `lasso_points`. So the check `if lasso_points.len() < 3 { if select_extra.is_some() { cancel it } }` would have fired on every single Ellipse selection's release (0 lasso points, `Some` select_extra → cancelled), and would have done the same to every new Magic Wand selection. This function has no automated test (it's app-layer input-state plumbing, not something the GPU/pure-function test suite touches), and the verification gap has meant no one has watched Ellipse Select actually work on screen since it shipped — so this could plausibly have been silently broken in the shipped build already, not just a risk in the new code. Fixed by checking the shape variant directly (`matches!(&input.select_extra, Some(Polygon(points)) if points.len() < 3)`) instead of the side-channel point counter — this can't misfire for Ellipse or Mask since it only inspects `Polygon`.

### Explicitly not done
The marching-ants overlay for a Mask selection still draws its bounding-box rectangle, not the exact flood-filled outline (Ellipse/Lasso get their real shape traced; Mask doesn't, yet). A correct trace needs contour-following on the raster mask (Moore-neighbor / boundary tracing), which is a real, self-contained algorithm — but a subtly wrong version (wrong winding, infinite loop on a pathological mask, missed holes) is exactly the kind of bug that needs eyes on a screen to catch with confidence, and screen verification has been blocked all session. Rather than ship an unverified geometry algorithm blind, left it as a documented rectangle fallback (surfaced in the inspector's hint text, not silently wrong) and flagged it as a tracked follow-up in ROADMAP.md instead. Feather (blurring the mask edge) is also still open.

### Testing
3 new CPU-only tests in `platform/gpu`: flood fill correctly stops at a hard color boundary; flood fill's 4-connectivity does NOT leak across a diagonal-only touch (the property that distinguishes it from a plain color threshold); the `Mask` arm of `rasterize_selection_mask` passes through at matching size and nearest-neighbor-resamples otherwise. Plus a `Mask` case added to the existing `selection_shape_bounds` bbox test. 87 workspace tests green (up from 84).

Verification gap continues (Screen Recording still blocked at the OS capture layer this session) — same substitution as every prior entry: the automated suite, plus in this case extra care taken specifically *because* of the gap (declining to ship the contour-trace geometry code without being able to see it work).

Next: layer masks (S3, direct reuse of the S1 mask machinery) or M4's remaining items (text tool, free transform).

---

## Tier 1 S3 (layer masks): deferred, not skipped — 2026-07-07

Before picking the next item off my own list, I looked hard at layer masks (S3) since it was the natural next step and ROADMAP.md already estimated it at effort M ("direct reuse of the S1 mask machinery"). Worth recording why I didn't build it this session, since the ROADMAP entry alone wouldn't explain the reasoning.

A layer mask needs to affect the shared **compositor**, not a single tool's own code path. Every layer composite goes through one `wgpu` bind group + WGSL shader (`LAYER_COMPOSITE_WGSL`, `layer_composite_layout`, `record_composite`) with exactly 4 bindings (base texture, source texture, sampler, blend-params uniform) — that pipeline runs for every layer, on every recomposite, and `layers_dirty` is set (and consumed) on **every single brush stamp**, not just on stroke-end. So there were really only two ways to add masking:

1. **Extend the shared shader/bind-group** with a 5th (mask) texture binding, sampled and multiplied into source alpha. Correct and performant, but it touches the one pipeline every layer in the app depends on, on every frame of every drag. A subtly wrong bind-group layout or default-mask value would break rendering app-wide, not just masking — and I have no way to watch it render this session (Screen Recording still blocked).
2. **CPU pre-multiply before compositing** (readback a masked layer's pixels + mask, multiply alpha, reupload, composite normally) — avoids touching the shared shader at all, but since `layers_dirty`/full recomposite fires on every paint stamp, this would mean a blocking GPU→CPU readback (`device.poll(wait_indefinitely)`) on every frame of every stroke on any layer, as long as *any* layer in the document has a mask. That's a real, continuous frame-time regression, not a hypothetical — this app has its own stated `<8ms` frame-budget law (Phase 0), and this would blow through it.

Every masked feature shipped this session so far (S1b, S2b, Move, Gradient) deliberately avoided touching pipelines shared by *every* stroke/composite — that's the same reasoning that picked a CPU stroke-end blend over a brush-shader change for S1b. Layer masks don't have an equivalent "isolated" path: the thing that needs to change (the compositor) is the shared thing. Given that and the standing verification gap, I'm deferring it rather than shipping a shader/bind-group change I can't see run, or a CPU path with a real, predictable performance cost. Picked up M4c (Free Transform) instead — well-scoped, follows the proven pure-function pattern, and doesn't touch any shared pipeline. Flagged in ROADMAP.md so this isn't silently dropped.

---

## M4c: Free Transform (scale + rotate) — 2026-07-07

### What shipped
`Tool::FreeTransform` ("Xfrm", **T**): a transform box (the active selection, or the whole canvas with none — same convention as Move) with 4 corner handles and 1 rotate handle. Dragging a corner uniformly scales the box anchored at the *opposite* corner (Photoshop's default, no-modifier behavior); dragging the handle above the box rotates about its center. Same commit-on-release shape as Gradient/Move: the overlay shows a live guide (the box + handles always, plus a forward-mapped preview quad while dragging), and the actual pixel warp happens once, on release, as a single undo step.

Core math is `apply_free_transform` (pure fn, `platform/gpu/src/lib.rs`): forward-maps the region's corners to find the destination bbox to iterate, then for each destination pixel inverse-maps (undo rotation, undo scale, relative to the pivot) to find the nearest source texel, and composites it in linear space — same "cut a hole, paste back" shape and same composite math as `move_selection_pixels`, generalized from a plain shift to a full affine warp. Selection-aware and mask-aware the same way Move is: with a selection active, only that region transforms (leaving a transparent hole at the original spot); with none, the whole layer does (starting blank, since every destination pixel is re-sourced).

**Deliberate v1 scope cuts, both documented rather than silently missing:**
- **Nearest-neighbor sampling, not bilinear.** Bilinear would smooth scaled/rotated edges, but a subtly wrong resampling kernel (wrong weight, off-by-one in the 2x2 neighborhood) is exactly the kind of bug that needs eyes on a screen to catch with confidence, and this shipped during the same Screen Recording outage as S2b's contour-trace deferral. Nearest-neighbor's correctness is verifiable by construction with plain pixel-equality tests (see below) — the honest trade is aliased edges instead of smooth ones, stated in the tool's own inspector hint text, not hidden.
- **One drag, one commit — no multi-parameter staging session.** Photoshop's real Free Transform lets you adjust scale AND rotation across several handle drags before a single Enter-to-apply. This version commits each individual drag immediately (matching how every other tool in the app already works), so combining scale + rotate takes two separate undo steps instead of one. Simpler state machine, consistent mental model with the rest of the app, at the cost of an extra undo step for compound transforms.
- **No skew, no drag-inside-box-to-translate** — translation is already Move's job; skew wasn't built.

### Bug caught by the test, before it shipped
The first rotation test failed: I'd predicted a marker "2 texels above the pivot" would land 2 texels to its right after a +90° rotation, using a pivot at a grid *corner* (5.0, 5.0) and reasoning in whole-texel offsets. The code was right and my test's mental math was sloppy — the marker's texel *center* was actually (5.5, 3.5), an offset of (0.5, -1.5) from that corner-pivot, not the clean (0, -2) I'd assumed. Recomputed by hand (forward-mapping the actual texel center through the same rotation formula the code uses) and confirmed the code's output was correct; fixed the test to use a pivot that's itself a texel *center* (5.5, 5.5), which makes "2 texels above" an exact offset with no half-texel ambiguity. Worth recording because it's the inverse of most bugs caught this session: this time the implementation was right and the test's arithmetic was wrong — a reminder that a geometry test failing doesn't automatically mean the code is the thing that's broken.

### Testing
3 new CPU-only tests: scaling 2x about a corner moves a marker texel to the expected forward-mapped position (and spreads it across the 2x2 block nearest-neighbor now maps to it); a +90° rotation moves "above the pivot" to "right of the pivot" (confirmed as on-screen clockwise for positive angles in this y-down image convention — useful, since the UI's drag-angle computation depends on getting this sign right); a region-scoped transform leaves a transparent hole at the original spot once content shrinks away from it. 90 workspace tests green (up from 87).

Verification gap continues (Screen Recording still blocked at the OS capture layer this session) — same substitution as every prior entry: the automated suite, plus the nearest-neighbor scope cut above taken specifically because of it.

Next: M4a (text tool, effort L — clean-room font parsing + layout) is the last open Tier 0 item; Tier 1 S3 (layer masks) and Quick Selection feathering remain open pending either a compositor-shader change I can verify live, or a non-performance-regressing CPU path.

---

## M4a: Text tool (clean-room TrueType) + a HiDPI hit-test coordinate bug — 2026-07-07

Two things landed together: the Text tool (the last open Tier 0 item), and a canvas-coordinate bug I found *while trying to verify Free Transform's corner-drag* — the kind of latent bug that only a precise-target tool surfaces.

### M4a — Text tool

**What shipped.** `Tool::Text` ("Txt"): click the canvas to place a baseline, type a string + pick a size in the inspector, "Load Font…" a `.ttf`, and "Apply Text" composites it onto the active layer in the brush colour, as one undoable step. Selection/mask-aware the same way Paint/Gradient/Move/Free-Transform are.

The whole font stack is hand-written in `platform/gpu/src/font.rs` — no `ttf-parser`/`fontdue`/`rustybuzz`, per [[feedback-sweet-clean-room-over-vendoring]] (a font engine is product-differentiating logic, not plumbing):
- **Parser**: sfnt table directory → `head` (units-per-em, loca format), `maxp` (glyph count), `loca`/`glyf` (outlines), `hhea`/`hmtx` (advance widths), and a Unicode `cmap` format-4 subtable (the fiddly `idRangeOffset` indirection — a byte offset measured *from the field's own address* into a parallel glyph-id array — is the one genuinely error-prone part of format 4, and it's commented as such).
- **Rasterizer**: flattens each glyph contour (resolving TrueType's on/off-curve quadratic-Bezier convention, including the *implied* on-curve midpoint between two consecutive control points) into a polyline, then 4× supersampled even-odd scanline fill — the same fill algorithm already proven in `rasterize_selection_mask`'s polygon arm, reused rather than written fresh. Font space is y-up; the rasterizer flips to the app's y-down pixel convention.
- **Layout**: `layout_line` — left-to-right cmap-lookup + hmtx-advance accumulation. Single line, no kerning/GPOS/bidi/shaping.

**Verified live — the one feature this session I got real eyes on.** Screen Recording briefly came back mid-session, so I drove the actual packaged app: loaded the real system `Arial.ttf`, typed "SWEET", hit Apply, and confirmed correct, legible glyphs rendered on the canvas (screenshot-confirmed). That's the strongest verification any feature got this session — a genuine end-to-end path from a real font file's bytes through my parser and rasterizer to correct pixels on screen.

**Deliberate v1 scope, documented not hidden:** TrueType `glyf` outlines only (no OTF/CFF PostScript outlines); *simple* glyphs only (composite/compound glyphs render blank — this is why accented Latin like "é" won't show yet, stated in the inspector hint); `cmap` format 4 only (covers the whole BMP, i.e. all of ASCII/Latin-1); no hinting (parsed past, never executed — affects only small-size crispness); no kerning.

**Font files are never committed.** Arial (and every mainstream system font) is proprietary and not redistributable — only my *parsing code* is original and owned. The tests read the system font at test time from `/System/Library/Fonts/Supplemental/Arial.ttf` and skip gracefully (not fail) on any machine without it, so CI on a Linux box won't break. 4 font tests: parses Arial + maps ASCII to real glyph ids (and PUA → .notdef); space has metrics but no visible outline; a rasterized "H" shows ink in both vertical strokes and a gap between them at a sampled row above the crossbar; `layout_line` accumulates advances left-to-right with equal spacing for repeated glyphs.

### The coordinate bug — found chasing Free Transform, fixes every canvas tool

While trying to verify M4c's Free Transform corner-drag, the corner handles wouldn't grab — the box wouldn't scale. Root cause was **not** in Free Transform: `ShellState::canvas_rect()` (which every canvas tool uses to map a physical-pixel cursor into the canvas's UV space) was reconstructing the canvas rectangle by *summing hand-tracked panel dimensions* — `left_strip_w`, `right_panel_w`, `top_bar_h`, `bottom_strip_h` — and it had drifted:
1. **HiDPI**: an earlier version applied `scale = 1.0` where it needed the real `pixels_per_point` (2.0 on this Retina display), so on any HiDPI screen the whole mapping was off by 2×.
2. **A missed panel**: `bottom_strip_h` only measured *one* of the two stacked bottom panels — the timeline bar was added later and its height never got folded in — so the bottom edge was wrong by the timeline bar's height.

Loose gestures (Marquee) looked "close enough" and hid this for weeks; Free Transform's pixel-precise corner handles were the first thing that actually *needed* the rect to be exact, and exposed it.

**Fix — stop reconstructing, publish the authoritative value.** egui already computes exactly this rectangle every frame as `ui.available_rect_before_wrap()` ("what's left after every panel claimed its space"). Now `draw_shell` captures that directly (× `pixels_per_point`) into `canvas_rect_physical`, and `canvas_rect()` returns it verbatim. No more hand-summed reconstruction that silently rots when a panel is added — the one place egui knows the true answer is now the one place we read it. (The old sum survives only as a pre-first-frame fallback.) Also widened Free Transform's handle grab zone from a hardcoded 10 physical px to `16 * ui_scale` logical points, so the grab feels the same size at any DPI.

**Verification gap, honestly:** by the time I had this fix built, the Screen Recording outage had returned, so I could *not* re-drive the corner-drag interactively to watch it work end-to-end. What I *did* confirm: direct diagnostic prints showed `canvas_rect()` and the overlay's `available_rect_before_wrap()` now agree exactly (they diverged by the DPI factor + timeline-bar height before), which is the actual coordinate-math bug. So the fix is confirmed correct *at the math level*, not yet re-confirmed at the "watched the box scale" level. Same honesty posture as every prior entry: the substituted evidence is named, and what's still owed (an eyes-on pass once Screen Recording is stable) is stated rather than glossed. Note the diagnostic `eprintln!`s were removed before commit.

### Testing
94 workspace tests green (4 new font tests; the coordinate fix is app-layer UI plumbing with no unit-test surface, verified by the diagnostic-print method above). Clean `cargo build --workspace`, packaged `dist/SweetVisual.app`.

Next: Tier 0 is now clear (M5 canvas, crop, move, free-transform, text all shipped; M5b sparse-canvas remains explicitly deferred). Open: Tier 1 S3 (layer masks), Quick Selection feathering, and the eyes-on re-verification of the Free-Transform corner-drag once the OS Screen Recording capture is working again.

---

## Tier 1: Selection feathering — 2026-07-07

Armon: "lets do that" — took the fully-CPU-testable of the two open Tier 1 items (feathering vs. layer masks), since layer masks still need a compositor change I can't verify live under the ongoing Screen Recording outage, and feathering doesn't touch any shared pipeline.

### What shipped
A "Feather" slider (0–64 px) in the selection inspector softens the selection's edge, so Paint/Gradient/Move/Text fade out gradually across the boundary instead of a hard cut. `0` = a hard edge, byte-for-byte the prior behavior.

The whole feature is one pure function plus a routing change — no GPU/shader/scissor work:
- **`feather_mask(mask, w, h, radius)`** (pure fn): runs `box_blur_1ch` twice — a *tent* filter, which gives a smoother, more Gaussian-looking falloff than a single box pass while staying linear-time. `box_blur_1ch` is a separable (H then V), edge-clamped, sliding-window box blur — O(w·h) regardless of radius, so max-radius feather on a large canvas is still a one-shot commit, not a stall. Edge clamping (not zero-padding) is deliberate and tested: a selection touching the canvas border must *not* get a spurious dark fringe there.
- **`Renderer::current_selection_mask(w, h)`**: the single chokepoint. Rasterizes `selection_extra`, then applies `feather_mask` when `selection_feather > 0`. All five existing mask call sites (paint-end-stroke, gradient, move, free-transform, text) now go through it instead of calling `rasterize_selection_mask` directly — so feathering lands on every tool at once, and there's exactly one place the feather is applied.

### The one honest scope limit, documented in the UI
Because the blur is symmetric but every bbox-limited tool (Paint's scissor, Gradient/Move/Free-Transform's region) only *visits* the selection's existing bounds, the visible feather is **inset-style**: the falloff softens the interior side of the boundary; the selection's extent/scissor is not widened outward. (Text, which visits glyph pixels anywhere, sees the full symmetric falloff.) This is a real, well-defined behavior — not a bug — and it's stated in the inspector hint ("Applied on the interior side of the boundary — the selection's bounds don't grow") rather than left for a user to discover. A true both-directions feather would mean expanding each tool's processing region + Paint's GPU scissor by the feather radius, which is a larger, cross-tool change; deferred, not pretended-away.

### Testing
3 new CPU tests (feathering is pure, so it's fully unit-testable — no GPU device, no live check needed): `radius 0` is an exact byte-for-byte passthrough (this is what guarantees every tool's no-feather path is unchanged); a hard left/right split becomes a smooth, *monotonic* (no blur ringing/overshoot) ramp with the deep interior on each side untouched; an all-selected mask stays all-selected after feathering (the test that fails if the box blur zero-pads its borders instead of edge-clamping). 97 workspace tests green (up from 94). Clean `cargo build --workspace`, no warnings.

Verification gap: feathering needs no live check (its correctness is fully captured by the pure-fn tests above), but the *interactive* result — dragging the slider and watching an edge soften on screen — is still unwitnessed under the Screen Recording outage, same as everything else this session. The math is proven; the pixels-on-screen confirmation is owed once capture works.

Next: Tier 1 S3 (layer masks) remains the big open item, still gated on a live-verifiable compositor path; also owed — the eyes-on re-check of Free-Transform's corner-drag and this feather slider once OS Screen Recording is back.

---

## Tier 1 S3: Layer masks — 2026-07-07

Armon: "yes" — took on the one Tier 1 item I'd been holding back specifically because it needs a compositor change I can't watch on screen. The thing that unblocked it: the layer composite is a GPU shader, and a shader *can* be verified headlessly (run it on a test device, read the pixels back) — that's real verification even with Screen Recording down, unlike the interactive tools.

### What shipped
Non-destructive per-layer masks. A layer can carry an optional grayscale mask (white = reveal, black = hide, gray = partial) that modulates its alpha during compositing without touching the layer's actual pixels. v1 authoring is **"Mask from Selection"**: make any selection (rect/ellipse/lasso/wand, feather included), click it on the layer, and everything outside the selection is hidden — which cleanly closes the loop on all of Tier 1's selection work (every selection tool + feather feeds straight into masks). Plus "Clear" to drop the mask.

Design choices:
- **The mask is just another `RasterCanvas`.** `PaintLayer.mask: Option<RasterCanvas>` — reusing the paint substrate means the mask gets the entire brush/undo/texture machinery for free, and is exactly the right shape for the eventual "paint directly on the mask" follow-up.
- **The compositor change is one line, with a provably-identical no-mask path.** `LAYER_COMPOSITE_WGSL` gained `@binding(4)` (a mask texture) and `sa = src.a * opacity * m`. Layers with no mask bind a **1×1 opaque-white** texture → `m = 1.0` → the math is byte-for-byte the pre-S3 path. So adding masks cannot change how any existing (unmasked) document composites — the risk of touching the shared composite shader is contained to `* 1.0`.
- **From-selection reuses the feathering chokepoint.** `set_active_layer_mask_from_selection` builds the mask via `selection_coverage_mask`, which returns the *feathered* exact-shape mask (`current_selection_mask`) when there's an Ellipse/Lasso/Wand shape, or a plain rect fill otherwise — so a feathered selection makes a soft-edged mask with no extra code.

### Verification — a real GPU test, not just the math
This is the part I flagged to Armon as the reason S3 was blocked, so it's the part I most wanted to actually verify. Wrote a headless-GPU test (`layer_mask_gpu_tests`) that builds a pipeline from the **exact** `LAYER_COMPOSITE_WGSL` string the app composites with, runs it on a real test device, and reads the output back: a red top layer over a green base, through a half-black/half-white mask, correctly shows **green where the mask is black** (layer hidden, base shows through) and **red where it's white** (layer composited normally). A second test asserts an all-white mask is pixel-identical to red-over-green — i.e. proves the no-mask fallback is a true no-op on hardware, not just in my head. This is genuinely stronger than the "math is tested, shader mirrors it" posture I've had to settle for on the interactive tools this session — the actual shader ran and produced the right pixels.

### Explicitly deferred (documented in the Layers panel, not silently missing)
- **Painting directly on the mask** — the classic layer-mask interaction. The plumbing is ready (the mask is a `RasterCanvas`), but routing the brush/undo path to target the mask instead of the layer color is its own increment. From-selection is a complete, genuinely-useful authoring path on its own (it's one of the most common real mask workflows), so v1 ships without paint-on-mask and tracks it.
- **Persistence** — masks are session-only in v1; the `.sweet` `LayerSave` format doesn't carry them yet. Called out in the panel hint ("Not saved with the project yet") rather than letting a user lose work silently.
- **A cosmetic subtlety:** the mask `RasterCanvas` is sRGB-encoded, so a feathered mask edge's mid-gray samples through the sRGB→linear curve — the transition is a touch contrastier than a pure-linear ramp. Exact at black/white (the common case); a minor, monotonic bend only on soft edges. Noted, not fixed, for v1.

### Testing
2 new headless-GPU tests (real shader execution + readback) + everything above stayed green: 99 workspace tests. Clean `cargo build --workspace`, no warnings.

Next: paint-directly-on-mask and mask persistence are the natural S3 follow-ups; also still owed across the session — the eyes-on re-checks (Free-Transform corner-drag, feather slider, and now masks in the live app) once OS Screen Recording capture is back. Note that layer masks are the first of those with a real GPU-execution test behind them, so the live check is confirmation rather than first verification.

---

## Tier 1 S3 follow-ups: paint-on-mask + mask persistence — 2026-07-07

Armon: "do the natural follow up" — closed the two gaps S3 v1 had explicitly deferred (and flagged in the panel), so layer masks are now a complete feature rather than a create-from-selection-only stub.

### Paint directly on the mask
A `mask_edit` flag on the Renderer routes the brush — and its stroke-end selection-masking, undo, and redo — to the active layer's *mask* `RasterCanvas` instead of its colour, via a `painting_mask()` guard (`mask_edit && the layer actually has a mask`). Because the mask is itself a `RasterCanvas`, this is pure routing: the brush/undo machinery is exactly the tested code that already paints layers, just pointed at a different canvas. Paint black to hide, white to reveal.

The routing is inlined at each paint site (`paint_stamp`/`end_stroke`/`undo`/`redo`/`can_undo`/`can_redo`) rather than behind a `&mut self` helper, on purpose: a helper returning `&mut RasterCanvas` would borrow all of `self`, conflicting with the `&self.device`/`&self.queue` those calls also need — the disjoint-field borrow only type-checks when the target selection is inline. UI: a per-active-layer "Paint Layer / Paint Mask" toggle in the Layers panel, plus an "Add" button for a blank (white, reveal-all) mask to carve holes into. `mask_edit` is guarded so an out-of-sync `true` (e.g. after switching to an unmasked layer) harmlessly falls back to painting colour.

### Mask persistence
Masks now round-trip through `.sweet`. Each masked layer writes a sibling `main.layer.{i}.mask.png` blob next to its colour PNG; absence on load = unmasked (so old projects load unchanged, and it's forward/backward compatible without a version bump). Threaded through the whole chain: `LayerSave.mask`/`LoadedLayer.mask: Option<Vec<u8>>`, a `Renderer::layer_mask_pixels` readback (refactored the existing `layer_pixels` into a shared `readback_canvas` helper so there's one readback path, not two), and `replace_layers` rebuilding the mask canvas — with the same defensive "drop a wrong-size blob" guard the colour path already uses.

### Testing
The existing `scene_and_layers_round_trip_through_a_bundle` persistence test now gives a layer a half-black mask and asserts it survives save→load intact (and that an unmasked layer stays unmasked) — so mask persistence is covered by a real round-trip test, not just wired. The paint-on-mask routing is app-glue over the already-GPU-tested brush + already-GPU-tested mask compositor, so it leans on those rather than a new test. 99 workspace tests green, no warnings, packaged clean.

### State of S3
Layer masks are now feature-complete for v1: create from selection (feather-aware), create blank, paint directly (black hides / white reveals), clear, and save/load. The GPU compositor test from the core S3 entry still backs the "does masking actually work" claim on real hardware. Remaining polish (not blocking): a mask thumbnail in the panel, and the sRGB-sampling nonlinearity on soft mask edges noted in the core entry. Live eyes-on of the full author→paint→save→reload loop is still owed once Screen Recording is back, but every load-bearing piece has an automated test behind it now.

---

## Parity — blend modes (8 → 20) — 2026-07-07

Armon: "add all" — kicked off working through the Photoshop/GIMP parity matrix. Started with the cleanest high-value, fully-GPU-verifiable item: filling out the layer blend modes.

Added the twelve standard *separable* (per-channel) modes SWEET lacked — Darken, Lighten, Color Dodge, Color Burn, Linear Burn, Difference, Exclusion, Divide, Vivid/Linear/Pin Light, Hard Mix — taking the set from 8 to 20. Appended to `suite_doc::BlendMode` (serde tags by name, so old projects load unchanged), mapped in `blend_mode_u32`, and implemented as `switch` cases 8–19 in **both** shader copies (`LAYER_COMPOSITE_WGSL`, the app's real path, and `compositor.rs::BLEND_WGSL`, kept in sync) with shared `cdodge_ch`/`cburn_ch`/`vivid_ch`/`pin_ch` helpers. The Layers panel dropdown picks them all up automatically via `BlendMode::all()`.

**Verified on real hardware:** extended `layer_mask_gpu_tests` (parameterized `composite_row` by mode) to run Darken/Lighten/Difference/Exclusion through the actual shipped shader and assert the pixels — Darken(green,red)=black, Lighten=Difference=Exclusion=yellow. Real GPU execution, not math-on-paper.

**Documented consistency choice:** these blend in the compositor's **linear** space (sRGB textures auto-decode on sample), matching SWEET's existing eight — so they're internally consistent but not pixel-identical to Photoshop's gamma-space blends. Re-architecting the whole compositor to gamma-space blending would change the existing modes too; not worth it for parity-of-capability. The remaining PS modes are the non-separable HSL set (Hue/Saturation/Color/Luminosity) + Darker/Lighter Color, which need whole-pixel (not per-channel) math — a separate, later add.

101 workspace tests green (1 new GPU test). No warnings.
