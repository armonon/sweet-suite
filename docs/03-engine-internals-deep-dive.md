# The Engine Internals — Four Subsystems, Implementation-Grade

*The third companion doc. The first two answered **what** to build (the Unified Canvas) and **how to make three apps feel like one** (the Suite Platform). This one answers **exactly how** the four load-bearing subsystems are actually implemented: the renderer & compositor, the raster substrate & brush engine, the object model & undo, and the timeline & node-graph engines. This is the doc you hand an engineer and say "build this."*

---

## 0. Scope, and How These Four Interlock

The two architecture docs name a lot of subsystems. Four of them carry the weight of the whole product, because everything the user *feels* passes through them on every frame:

- **The renderer & compositor** turns the scene into pixels in under 8 ms.
- **The raster substrate & brush engine** is where paint, photo, and texture pixels live and move.
- **The object model & undo** is the spine every object hangs on and the reason "one canvas" is even possible.
- **The timeline & node-graph engines** are the two generic machines that, configured three ways, become animation/NLE/beat-grid and materials/compositing/DSP across the whole suite.

Get these four right and the rest of the product is domain logic bolted onto a sound skeleton. Get them wrong and no amount of feature work rescues the feel.

**They are not independent.** Here is how they stack — read top-to-bottom as ownership and data flow, with the renderer at the bottom consuming the state everything above it produces:

```
            ┌──────────────────────────────────────────────┐
            │              OBJECT MODEL & UNDO              │   ← the spine
            │  typed envelope · scene graph · mod stacks ·  │
            │     command/undo · serialization · format     │
            └───────┬───────────────┬───────────────┬───────┘
                    │ owns objects  │ owns tracks   │ owns graphs
                    ▼               ▼               ▼
           ┌────────────────┐ ┌──────────────┐ ┌──────────────┐
           │ RASTER SUBSTR. │ │   TIMELINE   │ │  NODE-GRAPH  │
           │ tiles · brush ·│ │ keyframes ·  │ │ ports · eval │
           │ live/committed │ │ playhead     │ │ · dirty/cache│
           └───────┬────────┘ └──────┬───────┘ └──────┬───────┘
                   │ produces tiles   │ drives values  │ produces
                   │                  │ each frame     │ textures/meshes
                   ▼                  ▼                ▼
            ┌──────────────────────────────────────────────┐
            │             RENDERER & COMPOSITOR             │   ← the sink
            │   Forward+ · 5 passes · compositor · color    │
            └──────────────────────────────────────────────┘
```

The mental model that makes this tractable: **the object model is the single source of truth; the timeline mutates it over time; the node-graph computes derived data for it; the raster substrate stores its pixel-typed payloads; and the renderer is a pure function of its state at one instant.** Every frame is `render(evaluate(scene, t))`. Hold that invariant and the architecture stays sane.

A note on conventions used throughout: code is Rust-flavored pseudocode (per the stack decision — Rust + wgpu). It is illustrative, not copy-paste; it exists to pin down data layout and algorithm, not to compile. Where a real library or paper is the right answer, it is named rather than reinvented — consistent with the "integrate, don't invent" rule from the master plan.

---

## 1. The Renderer & Compositor

The renderer is a **pure function of scene state at one instant**: `frame_pixels = render(scene_state, camera, t)`. It owns no truth and mutates no document. That single rule is what keeps it fast — it never waits on an edit, an undo, or a worker; it draws whatever the object model says is true *right now* and moves on. Everything below serves that.

### 1.1 The decision: clustered-forward (Forward+), restated in implementation terms

The master plan picked Forward+. Here is *why*, stated as the three properties that actually drive the code:

1. **Transparency is first-class, not a fight.** A design document is a deep stack of semi-transparent planes; washes, glass, and soft brushes are everywhere. Deferred shading stores one opaque surface per pixel in the G-buffer and has to bolt transparency back on as a second, awkward pipeline. Forward+ shades transparency in the same pass as everything else. For *this* content mix, that is decisive.
2. **Heterogeneous materials don't share a G-buffer.** A mesh with a PBR material, a vector fill with analytic coverage, and an SDF glyph want wildly different shading. Forward+ lets each object bring its own shader; deferred wants everything funneled through a fixed G-buffer layout.
3. **MSAA stays cheap and available.** Forward+ is MSAA-friendly (deferred forces you into expensive or approximate AA). For crisp 3D edges next to crisp vector edges, that matters.

The "+" is the light-culling step that makes forward scale to many lights. We divide the view frustum into a 3D grid of **clusters** (froxels) and, each frame, cull lights into per-cluster lists. A fragment looks up only the lights touching its cluster instead of looping over every light in the scene.

```
froxel grid:  16 (x) × 9 (y) × 24 (z)   →  3456 clusters
z-slicing:    logarithmic — depth of slice k:  z_k = near · (far/near)^(k / numZ)
              (fine clusters up close, coarse far away — matches perspective)
per cluster:  a u32 offset + count into a flat global light-index list
```

For a flat design document the light count is ~0–1 and this whole stage collapses to a constant; for a lit 3D scene in the same canvas it pays for itself. One renderer, both regimes, no branch in the app.

### 1.2 The frame graph and the five passes

wgpu (unlike a Vulkan render-graph) does not schedule passes or insert barriers/layout transitions for you, so we build a **thin frame graph**: each pass *declares* the resources it reads and writes; the graph orders them, allocates transient targets from a pool, and inserts the texture-state transitions. This is ~500 lines and pays off forever — it's also what lets the DAW/video apps add or drop passes without hand-managing barriers.

The pass order is the master plan's, made concrete:

```rust
// Declared once; the graph resolves dependencies & transitions.
fn build_frame_graph(g: &mut FrameGraph, scene: &Scene) {
    let depth  = g.create_depth("depth", HiZ);
    let hdr    = g.create_color("hdr", Rgba16Float);   // linear working space

    // 0. Cluster build (compute) — cull lights into froxels.
    g.add_compute("clusters", reads(scene.lights), writes(g.cluster_buffer));

    // 1. Opaque 3D — depth pre-pass then shade. Front-to-back, early-Z.
    g.add_pass("z-prepass", writes(depth), draw_opaque_depth_only);
    g.add_pass("opaque",    reads(depth, g.cluster_buffer), writes(hdr), draw_opaque);

    // 2. Transparent 3D — sorted back-to-front OR weighted-blended OIT (see 1.4).
    g.add_pass("transparent", reads(depth), writes(hdr), draw_transparent);

    // 3. Flat-snapped layer pass — screen-space 2D compositor when ortho (see 1.5).
    g.add_pass("flat", reads(scene.flat_stack), writes(hdr), composite_flat);

    // 4. Gizmos & overlays — selection outline, morphing gizmo, snap guides, grid.
    g.add_pass("overlays", reads(depth), writes(hdr), draw_overlays);

    // 5. Post / compositor — adjustment layers, bloom/DOF, tonemap, encode to display.
    g.add_pass("post", reads(hdr), writes(g.swapchain), composite_and_encode);
}
```

Two things to notice. First, **the main color target is `Rgba16Float` linear** the whole way through; we only encode to the 8-bit (or 10-bit) display-space swapchain in the final `post` pass. Everything blends in linear light. Second, **passes 2, 3, and 5 are where "this is a design tool, not a 3D viewport with text pasted on" is won or lost.**

### 1.3 The most important distinction: compositing order ≠ depth order

This trips up everyone who builds a 2D+3D hybrid, so state it loudly:

> **A design layer's position in the stack is authoritative and has nothing to do with its Z in the scene. Depth sorting is for 3D geometry that overlaps in space. Layer order is for 2D content the user explicitly stacked.**

So the engine runs **two different transparency strategies side by side**:

- **Flat content** (raster layers, vector shapes, text, image planes, adjustment layers) composites in **explicit stack order** — the order in the layers panel, full stop. This is deterministic, matches user intent exactly, and is handled by the flat pass (§1.5) as a compositor, not by depth sorting.
- **3D geometry** that is genuinely transparent and can interpenetrate in space uses **depth-based** transparency (§1.4), because there is no authored "order" — it's physical overlap.

Conflating these is the classic bug: design layers flicker or reorder as the camera moves because someone depth-sorted them. Don't. Tag every object `compositing: StackOrdered | DepthOrdered` and route accordingly.

### 1.4 Transparency for the 3D half: sorted, with OIT as the escape hatch

For depth-ordered transparency the default is **per-object back-to-front sort** by camera-space depth, which is correct for the common case (separated transparent objects). It fails on two things: mutually intersecting transparent surfaces, and large angular overlaps where a single sort order is wrong for different pixels.

Decision: **sorted by default; offer Weighted-Blended OIT (McGuire & Bavoil 2013) as a per-document toggle** for messy cases. WBOIT is order-independent, single-pass, and "good enough" for washes and glass where the user can't tell the exact compositing order anyway; it trades a little correctness for zero sorting and zero popping. We do *not* reach for depth-peeling (multi-pass, expensive) — it violates the frame budget for marginal gain. The honest line: exact OIT is not worth a frame; sorted handles intent, WBOIT handles chaos.

### 1.5 The flat-snapped orthographic pass — where the design tool lives

When the camera is orthographic *and* aligned to a work plane (the user "snapped to flat"), the flat content stops being quads in a perspective scene and becomes a **2D compositor graph evaluated in screen space at native resolution.** This is the single most important pass for the design/paint identity.

```
is_flat_mode = camera.ortho && aligned_to_workplane(camera, tol)

composite_flat(stack):                  // stack = flat objects in layer order
    target = screen_res HDR buffer
    for obj in stack (bottom → top):
        src = rasterize_obj_at_native_res(obj)   // tiles / SDF / coverage — never scaled
        if obj.is_adjustment:
            target = apply_adjustment_shader(obj, below = target)   // reads what's under it
        else:
            target = blend(target, src, obj.blend_mode, obj.opacity, obj.mask)
    return target
```

Three rules make it feel native:

- **Native-resolution rasterization, never scale-from-texture.** A vector or glyph is rasterized at the exact pixel size on screen *this frame*, so it's razor-sharp at 6400% zoom. (Contrast: sampling a pre-rasterized texture, which blurs.)
- **Adjustment layers read the composited result beneath them**, so curves/levels/hue affect everything below — exactly Photoshop semantics, but as one GPU pass with no "applying…" bar.
- **The perspective↔ortho transition is a continuous camera lerp.** As the camera approaches orthographic-and-aligned, we cross-fade from the 3D path to the flat path. The flat path is not a different *mode* the user toggles; it's an optimization+quality path the engine slips into when the geometry allows it. (Soul-of-the-product detail: the user never sees a "2D mode / 3D mode" switch — consistent with the master plan's "there are no modes.")

### 1.6 Crisp text and vectors: MSDF and analytic coverage

The crispness rule from the master plan — *render via distance fields / analytic coverage, never rasterize-then-scale* — implemented:

- **Text → multi-channel signed distance fields (MSDF, Chlumský).** Single-channel SDF rounds sharp corners; MSDF preserves them by encoding edges in three channels and reconstructing the true distance via median. Glyphs are baked into an **SDF atlas** on font load / on first use of a glyph, cached across the session. One small atlas serves every type size and rotation; the fragment shader reconstructs coverage analytically per pixel. Tiny memory, infinite scale, crisp corners.
- **Vector fills → tessellate once, AA analytically.** Bezier paths tessellate to triangles (the **Lyon** crate is the known-good Rust answer) and we compute edge coverage analytically in the shader rather than relying on MSAA alone, so curved edges stay smooth at any zoom. For strokes, an SDF of the stroke offset curve gives clean, scalable AA.
- **The shared trick:** in both cases the GPU computes *coverage as a function of distance to the true geometric edge* at the current pixel scale. That's why zoom never softens an edge — there's no fixed-resolution intermediate to soften.

### 1.7 The compositor: blend modes, groups, adjustment layers

Blend modes are pure per-pixel math in linear space — Porter-Duff `over` plus the separable set (multiply, screen, overlay, soft-light, …) and the non-separable HSL set (hue, saturation, color, luminosity). They live in one shader with a `blend_mode: u32` uniform switching the math; one implementation, used by the flat pass, the 3D transparent pass, and the per-app compositors in the video editor.

```rust
fn blend(cb: vec4, cs: vec4, mode: u32) -> vec4 {     // cb=backdrop, cs=source, premultiplied-linear
    let b = match mode {
        NORMAL   => cs.rgb,
        MULTIPLY => cb.rgb * cs.rgb,
        SCREEN   => cb.rgb + cs.rgb - cb.rgb*cs.rgb,
        OVERLAY  => hardlight(cs.rgb, cb.rgb),
        // … soft-light, color-dodge/burn, HSL set …
    };
    let a = cs.a + cb.a*(1.0 - cs.a);
    return vec4(mix(cs.rgb, b, cb.a)*cs.a + cb.rgb*(1.0-cs.a), a);
}
```

**Groups** composite into an offscreen target and then blend as a unit — this is what makes "isolated" groups and knockout work, and it's the same machine the renderer uses for any object whose `modifiers[]` need an intermediate buffer. **Adjustment layers** are full-screen (or dirty-rect-bounded) shader passes parameterized by their controls (a curves LUT, levels coefficients, an HSL matrix). They are cached: if neither the adjustment's parameters nor the pixels beneath it changed this frame, the cached result is reused — critical for deep stacks.

### 1.8 Color management — one layer, shared with video

Per the platform doc, color management is *one* shared layer (visual + video must match). The contract:

- **Working space is linear, wide-gamut.** Recommend **linear sRGB** for an SDR-first product, with a path to **ACEScg** for HDR/wide-gamut work. All blending, filtering, and compositing happen here, in linear light — this is non-negotiable and the reason gradients and edges look right where 8-bit-sRGB tools show banding and dark fringing.
- **Inputs convert in.** Imported images carry an ICC/embedded profile; we transform to working space on import. Untagged assets get an assumed-sRGB default.
- **Output converts out, once, last.** The final `post` pass applies the display transform (working-space → display profile, then the display's encoding transfer function / inverse-EOTF), plus dithering to hide 8-bit banding. An **OpenColorIO-style** config holds the transforms so the same config drives the video app's grade.
- HDR output is just a different output transform on the same linear pipeline — no architectural change.

### 1.9 Hitting the < 8 ms budget — the mechanics

The performance doctrine is platform law; here's how the renderer specifically holds it:

- **Measure every pass.** GPU timestamp queries bracket each frame-graph pass; a debug HUD shows per-pass ms. You cannot defend a budget you don't measure, and you measure from Phase 0 — with the *real* windowing/swapchain layer, because pen-to-photon latency hides in the OS compositor and present path, not in your shader.
- **Never read back in the hot path.** No `mapAsync`/GPU→CPU sync inside input→draw. Pixels stay on the GPU (the raster-substrate rule, §2). The one place readback is allowed is explicit, async, and off the input path (e.g., color-picker eyedropper, export).
- **Minimal swapchain depth + the right present mode.** Mailbox/Immediate for lowest latency where available; avoid deep FIFO queues that silently add frames of lag. Your *contribution* to latency must be a single sub-frame.
- **Persistent, pre-bound resources.** Dynamic per-frame data (camera, transforms) lives in persistently-mapped ring buffers; bind groups are built once and reused; large/static data (the cluster light list, object storage) is bindless-style indexed. Binding churn is a real frame cost — engineer it out.
- **Heavy work is on another queue.** Booleans, content-aware fill, ML, decimation, big blurs run on the compute/worker path and **stream results back atomically** (§2, §4). The draw thread sees a preview proxy until the real result lands. Nothing slow is ever between input and pixels.

### 1.10 The resource set (what actually lives on the GPU)

```
Persistent:
  • Tile atlas array textures (the raster substrate, §2) — Rgba16F or Rgba8 per layer policy
  • MSDF glyph atlas — R8 / Rgb8
  • Object storage buffer — transforms, material handles, blend/opacity (indexed by object id)
  • Light buffer + global light-index list + cluster grid buffer
Transient (frame-graph pool, recycled):
  • HDR color  (Rgba16Float, working space)
  • Depth / Hi-Z
  • Group/offscreen targets (allocated on demand for isolated groups & modifier intermediates)
  • OIT accumulation + revealage targets (only when WBOIT is on)
Final:
  • Swapchain (Bgra8Unorm / 10-bit), written once by `post`
```

The whole renderer is then: build clusters → draw opaque → draw transparent → composite flat → draw overlays → post-and-encode. Six steps, one HDR linear target, one final encode. Everything else is detail hanging off this skeleton.

---

## 2. The Raster Substrate & Brush Engine

This is the subsystem that makes the paint and photo halves feel faster than Photoshop. Everything here exists to honor four rules from the performance doctrine: **tiles always; pixels stay on the GPU; live vs committed; dirty-tile undo.** They are not independent features — they are one design, and below is the actual machine.

### 2.1 The tile substrate

A raster layer is **not** a big texture. It is a **sparse map of 256×256 tiles**:

```rust
struct RasterLayer {
    tiles: HashMap<TileCoord, TileHandle>,   // sparse — only tiles with content exist
    format: TileFormat,                      // Rgba8Unorm (default) | Rgba16Float (hi-bit)
}
struct TileCoord { x: i32, y: i32 }          // signed → canvas is infinite in all directions

enum TileResidency {
    Empty,                 // not in the map at all → reads as transparent, costs nothing
    Gpu(AtlasSlot),        // resident in a GPU atlas-array texture, ready to sample/write
    Spilled(CompressedBlob)// LZ4-compressed in RAM or on disk; paged back in on demand
}
```

Tiles live in **GPU array-textures (atlases)**: a few large `2048×2048` array textures, each holding an 8×8 grid of tiles per layer, with a free-list of slots. A tile is addressed by `(atlas_index, slot)`. Key consequences:

- **Infinite canvas is free.** `TileCoord` is signed and the map is sparse; an empty region is simply absent. You never allocate the bounding box of the canvas, only the tiles the user has actually touched.
- **A 100-megapixel canvas never gets processed whole.** Every operation — paint, filter, undo — works on the *set of tiles it overlaps*, typically a handful. This is the entire reason the app stays light where Photoshop spills to a scratch disk.
- **Residency is an LRU.** The GPU holds the working set; cold tiles spill compressed to RAM, then disk, and page back when scrolled into view. The compositor only ever samples resident tiles.

### 2.2 Live layer vs committed layer

A mid-stroke paint must not touch committed pixels — that would dirty (and have to undo-snapshot) tiles on every input event. Instead:

```
COMMITTED:  the layer's real, persisted tiles.
LIVE:       a scratch GPU texture (or a scratch tile set) holding only the current stroke.
DISPLAY:    composite( committed, live )  — done every frame, cheap.
ON PEN-UP:  flatten live → committed, but only for the tiles the stroke actually dirtied.
```

So during a stroke the committed tiles are read-only and the per-frame cost is "draw a few stamps into a scratch texture + composite it on top." In-progress strokes redraw at full framerate no matter how long the stroke or how big the canvas. The expensive event — flattening + undo capture — happens **once**, on pen-up, where a one-frame hitch is invisible.

### 2.3 Dirty-tile undo (the single biggest "feels light" lever)

Before the flatten on pen-up modifies a committed tile, we copy **just that tile** into the undo record. A brushstroke that dirties ~6 tiles produces an undo step of ~6 tiles, not a snapshot of the whole image.

```rust
struct TileEdit { coord: TileCoord, before: CompressedTile, after: CompressedTile }
struct UndoStep { layer: LayerId, edits: Vec<TileEdit>, label: &str }

fn undo(step):  for e in step.edits { swap(layer[e.coord], e.before) }   // restore before
fn redo(step):  for e in step.edits { swap(layer[e.coord], e.after)  }   // re-apply after
```

The arithmetic that makes this a non-issue: 6 tiles × 256×256×4 bytes ≈ **1.5 MB raw**, → a few hundred KB after LZ4. A hundred-step history is tens of MB, not the gigabytes a full-snapshot model burns. We **store both `before` and `after`** so undo/redo is a symmetric swap (no recompute), and we **spill compressed history tiles** under memory pressure — the same spill path as cold layer tiles. This is the mechanism the master plan calls out as the reason the app feels light where Photoshop hits the scratch disk; it generalizes to every tile-modifying op (filters, fills, transforms), not just brushes.

### 2.4 The stroke pipeline (input → pixels, end to end)

```
pen events (200–360 Hz)
   │  each: position, pressure, tilt(x,y), velocity (derived), bearing
   ▼
COALESCE + RESAMPLE        coalesce the burst that arrived since last frame;
   │                       resample to an even arc-length path (so spacing is uniform)
   ▼
STABILIZE                  pulled-string / catch-up smoothing (§2.7), per-brush
   ▼
SPACING                    emit stamps every `spacing · tip_size` along the path
   ▼
PER-STAMP PARAM EVAL       map inputs → brush params via response curves (§2.5)
   ▼
STAMP (compute)            for each stamp, dispatch a compute pass over the tiles under
   │                       its bounding box; blend tip·grain·flow into the LIVE layer
   ▼
COMPOSITE                  display = committed ⊕ live   (every frame, full rate)
   ▼ (pen-up)
FLATTEN + UNDO CAPTURE     snapshot dirtied committed tiles → flatten live into them
```

The hard real-time rule lives at the COALESCE step: pen input arrives faster than the display refreshes, so we **coalesce points and never let an input burst stall a frame** (master plan §2.6). The render loop runs at display refresh (120/144 Hz); the stroke path is consumed at whatever rate input arrives and decoupled from drawing.

### 2.5 The parametric (node-style) brush engine

A brush is a small **parameter graph**, not a fixed struct. Four stages compose:

```
TIP  ─────►  GRAIN  ─────►  DYNAMICS  ─────►  DUAL/WET
shape        texture        input→param        second tip,
(SDF or      (paper,        mappings           wet-mix mode
 stamp tex)   canvas)
```

The defining feature is **inputs → any parameter, through a response curve**:

```rust
struct InputMapping { input: Input, target: Param, curve: ResponseCurve, range: (f32,f32) }
enum Input  { Pressure, TiltX, TiltY, Velocity, Bearing, Random, StrokeT }
enum Param  { Size, Opacity, Flow, Rotation, Scatter, HueJitter, SatJitter, ValJitter, WetMix }
```

So "pressure → size and flow," "tilt → tip rotation," "velocity → scatter," "random → hue jitter" are all the same mechanism — a list of mappings evaluated per stamp. This is what makes one engine cover a hard pencil, a soft airbrush, and a textured charcoal without special-casing any of them. Each stamp's evaluated params drive the compute shader that blends the tip into the live layer.

### 2.6 Wet mixing and natural media

Procreate's engine is largely stamp-based; the master plan's "state-of-the-art" differentiator is **physically-inspired media**, which means tiles carry **more than RGBA**:

```
extended per-pixel channels (for natural-media layers):
   rgb · alpha · wetness · pigment_load · height
```

- **Wet mixing**: pigment blends *on the canvas*. A light advection/diffusion step (shallow-water-style, or simpler diffusion) moves pigment and water between neighboring pixels each step while the area is "wet." For color that mixes like paint (blue + yellow → green, not muddy gray), use **Kubelka–Munk** pigment mixing rather than naive RGB averaging. Rebelle is the high-end reference.
- **Oil impasto**: the `height` channel accumulates paint thickness; the compositor derives a normal from height so light catches the ridges — physical, not a texture overlay.
- **Pastel/charcoal grain**: the grain stage modulates deposition by a paper-texture height field, so strokes catch tooth.

All of this is **tile-local** — it only ever runs on wet tiles under and near the stroke — so it stays inside the frame budget by the same logic as everything else. It runs on compute; nothing here touches the whole canvas.

### 2.7 Stabilization

Smoothing happens on the coalesced path *before* stamping, so it's brush-tunable and never fights the renderer:

- **Pulled-string**: the brush tip trails the cursor as if on a string of fixed length; the tip only moves when the cursor pulls beyond that length. Long string = very smooth, laggy; short = responsive. (Procreate's StreamLine.)
- **Catch-up / weighted average**: the emitted point is a weighted average of recent input points; on pen-up the tip "catches up" to the final cursor position so strokes end where the user expects.
- **QuickShape**: hold at stroke end → fit the stroke to the nearest primitive (line/circle/rect/polygon) by least-squares and snap it clean.

### 2.8 Memory management & the spill ladder

One coherent LRU governs everything:

```
HOT   GPU atlas slots        ← tiles being drawn / on screen
WARM  RAM, LZ4-compressed    ← recently used tiles + recent undo history
COLD  disk, LZ4-compressed   ← scrolled-away tiles + deep undo history
```

Promotion/eviction is demand-driven: scrolling a tile into view pages it GPU-ward; memory pressure evicts the least-recently-used tile down a rung. Because undo records are *also* just compressed tiles, they ride the same ladder — deep history costs disk, not RAM, and never RAM you needed for painting. The net effect the user feels: the canvas size and history depth stop being things they have to think about.

---

## 3. The Object Model & Undo

This is the spine. The renderer, the raster substrate, the timeline, and the node-graph all hang off it. It is also the subsystem that *is* the "one canvas" idea — the master plan's "everything is a typed object in a single scene" is not a slogan, it's this data structure.

### 3.1 The typed-object envelope

Every object shares a uniform envelope and carries a type-specific payload. In Rust this is "common struct + payload enum," with the object owned in a generational arena:

```rust
struct Object {
    // ENVELOPE — every object has these, so every system treats every object uniformly
    id: ObjId,                       // generational index — stable across edits & saves
    name: String,
    parent: Option<ObjId>,
    children: Vec<ObjId>,
    transform: Trs,                  // translate · rotate(quat) · scale  (3D; 2D is a subset)
    world_cached: Cell<Option<Mat4>>,// memoized world matrix, dirty-flagged
    visibility: bool, lock: bool,
    blend_mode: BlendMode, opacity: f32,
    modifiers: Vec<Modifier>,        // non-destructive stack (§3.3)
    keyframe_tracks: Vec<TrackId>,   // animation on the shared timeline (§4)
    payload: Payload,                // ↓ the only type-specific part
}

enum Payload {
    Mesh(MeshHandle),                // half-edge / indexed geometry
    Sculpt(SculptHandle),            // dyntopo/voxel + multires
    Raster(RasterLayer),             // tile handle set (§2)
    Vector(VectorShape),             // bezier paths
    Text(TextBlock),                 // string + font + layout → MSDF
    ImagePlane(ImageHandle),
    Light(Light), Camera(Camera), Null,
    Group,
}
```

Two deliberate choices:

- **Generational IDs (`slotmap`-style), not pointers or bare indices.** An `ObjId` is `(slot, generation)`. Deleting an object frees its slot and bumps the generation, so any stale `ObjId` referring to it fails a lookup instead of silently aliasing a new object. This is what makes undo, cross-references (a modifier targeting another object, a snap referencing a face), and serialization safe across edits.
- **Envelope-as-struct, payload-as-enum — not a full ECS.** A scene editor with a *uniform* envelope and a *closed, small* set of payload types is simpler and more cache-coherent as one arena of `Object` than as an archetypal ECS. (ECS earns its keep when component combinations are open and many; here they aren't.) The uniformity — every object transformable, animatable, snappable, stackable, compositable "for free" — comes from the shared envelope, exactly as the master plan claims.

### 3.2 Scene graph & transforms

Hierarchy is `parent`/`children`; world transform is `parent.world * local`. World matrices are **cached and dirty-propagated**: mutating an object's local transform marks its subtree's `world_cached` dirty, and a world matrix is recomputed lazily on next read. Transforms are stored **decomposed (TRS)** rather than as raw matrices because the timeline interpolates translation/rotation/scale independently (slerp on the quaternion), and decomposition-from-matrix every frame is lossy and slow.

The flat/design world is not special-cased: a design layer is an object with an ortho-friendly transform on a work plane. "2D" is just "3D with the camera snapped flat" (§1.5), so the *same* transform math, gizmo, and snapping serve both — which is the entire reason a text layer can snap to a mesh face.

### 3.3 Non-destructive modifier / effect stacks

A `modifiers: Vec<Modifier>` is a **mini evaluation pipeline** cooked top-to-bottom: each modifier takes the previous result and produces the next. The base payload is never mutated; the renderer consumes the *cooked* output.

```
base mesh ─► [Mirror] ─► [Subdiv] ─► [Boolean(cutter)] ─► [Bevel] ─► cooked mesh → renderer
                 └─ each stage's output is cached; editing Subdiv re-cooks Subdiv→down only
```

The discipline that keeps this fast is the same one the node-graph uses (§4.5): **per-stage result cache + dirty-from-first-change-downward.** Tweak the Bevel and only Bevel re-cooks; tweak the Mirror and everything below it re-cooks. Heavy modifiers (Boolean, Remesh, Subdiv at high levels) cook on the worker pool with a live proxy, then swap their cached output atomically — so the stack stays editable at full framerate even when a stage is expensive (master plan §3.4). For raster objects the identical machine is an *effect* stack (adjustment layers, smart filters); "modifier stack" and "non-destructive filters" are one implementation wearing two hats.

### 3.4 The command pattern + per-type compact undo

**Every mutation is a `Command` with `apply` and `revert`.** Undo is not snapshotting — it is replaying inverses, and each command stores the *minimal* delta for its type.

```rust
trait Command {
    fn apply(&mut self, doc: &mut Document);
    fn revert(&mut self, doc: &mut Document);
    fn try_coalesce(&mut self, next: &dyn Command) -> bool;  // merge live drags
}

// Examples of how compact each delta is:
struct SetTransform { obj: ObjId, before: Trs, after: Trs }            // 2 small structs
struct SetParam     { obj: ObjId, key: ParamKey, before: Val, after: Val }
struct PaintStroke  { layer: LayerId, edits: Vec<TileEdit> }           // dirty tiles only (§2.3)
struct AddObject    { obj: ObjId, snapshot: ObjectData }               // inverse = remove
struct Reparent     { obj: ObjId, old_parent: Option<ObjId>, new_parent: Option<ObjId> }
```

Three properties make the undo system feel right:

- **Transactions.** One user action = one undo step, even when it's several commands. A drag-to-cut boolean is `{AddResultMesh, HideCutter, RecookStack}` bundled into a single transaction that undoes/redoes atomically.
- **Coalescing.** A slider drag emits hundreds of `SetParam`s; `try_coalesce` merges consecutive same-target commands while the gesture is live, so the user gets *one* undo step for "I changed the bevel width," not 300.
- **Compactness by type.** Every command is a delta, never a document snapshot — the same philosophy as dirty-tile undo, applied to structure and parameters. A deep history is cheap.

> **Precedent in your own code (different project, same pattern).** COMPOSITOR (`apps/compositor-native`) already ships a working undo/redo command model over its `Project → Composition → Layer` data model. It's a live proof that the command-delta approach holds up in a real C++/Qt timeline editor — and a sign that when the suite's *native* video app is built on this platform, its undo layer is a port of a proven design, not a leap.

### 3.5 Serialization — structured envelope, blob'd payloads, crash-safe

Split the document into two storage classes:

- **The scene structure** (envelopes, hierarchy, modifier params, keyframe tracks, references) — small, highly structured. Serialize as a compact, schema-versioned binary (CBOR or FlatBuffers/Cap'n Proto; CBOR if you want human-inspectable, FlatBuffers if you want zero-copy load of big scenes).
- **Heavy payloads** (committed tiles, mesh buffers, baked caches) — large binary blobs in a **content-addressed blob store** inside the bundle, referenced by id/hash from the structure. This deduplicates (two layers sharing a texture store it once) and lets you load the structure instantly and stream payloads lazily.

**Crash safety is non-negotiable** and cheap: atomic save (write to temp → `fsync` → rename), a rolling `.bak`, and a periodic auto-save journal so a crash costs seconds, not the session.

> **Precedent in your own code.** COMPOSITOR persists to **SQLite with crash-safe save and `.bak`/`.pre-restore.bak` recovery** — a proven, boring-in-the-best-way instance of exactly this robustness bar. SQLite-as-document-format is in fact a legitimate choice for the bundle's structure store (it gives you transactions, partial loads, and the recovery story for free); the blob store can live as SQLite BLOBs or as sidecar files referenced by row. Worth copying wholesale.

### 3.6 The universal project / bundle format

The platform doc is explicit and correct: **not one schema for a reverb and a sculpt — a shared container that references domain-specific documents, plus strong interchange.** Concretely:

```
project.suite   (a bundle — archive or directory)
├── manifest.json        # version, app, list of contained documents + asset refs
├── documents/
│   ├── visual.scene      # the typed-object scene graph (§3.1–3.5)
│   ├── video.timeline     # the NLE document (clips, tracks, transitions)
│   └── audio.session      # the DAW document (tracks, MIDI, plugin state)
├── assets/               # shared asset library: brushes, LUTs, fonts, materials, samples
└── blobs/                # content-addressed heavy payloads (tiles, meshes, media proxies)
```

The payoff — start in one app, finish in another — is **interchange by reference, not copy**: a visual animation embedded in the video timeline is a *reference* to `visual.scene` (Dynamic-Link style), re-rendered on demand, with a baked-export fallback for portability. Design the container shape and versioning early so app #1's files are forward-compatible; fill the domain payloads in over time (master plan §7).

> **Precedent in your own code.** This is the one place the video project is *directly* informative. ScenePilot/COMPOSITOR already runs this exact play: the **`scenepilot.editplan.v0` contract** is a minimal domain document (clips, cut points, overlays, audio, effects) that one half emits and the other renders, and COMPOSITOR exports **OpenTimelineIO (OTIO)** plus a `review-interchange.v0` JSON. OTIO is the industry-standard editorial-interchange spine and the natural format for the `video.timeline` document inside the universal container — so the suite's "shared container, domain payloads, strong interchange" thesis already has a working seam you can lift. (The reality-check's lesson applies here too: keep the contract *minimal* and let a real consumer shape it, rather than over-speccing the universal schema before its second app exists — which is precisely the master plan's "don't finalize a schema that spans a synth patch and a sculpt on day one.")

---

## 4. The Timeline & Node-Graph Engines — Two Spines, Six Faces

These are the platform doc's "one spine, three faces" engines. The trick to both is the same: **build the generic machine once — its data model, its editor, its evaluation discipline — and let each app supply only the item types and the execution context.** What's shared is the structure and the feel; what differs is what flows through.

### 4.1 The timeline spine

The generic model is **tracks of time-addressed items, plus a playhead, over a pluggable time base:**

```rust
struct Timeline { time_base: TimeBase, tracks: Vec<Track>, playhead: Time }

enum TimeBase {                      // the axis itself is configurable
    Seconds,                         // visual animation
    Frames { fps: Rational },        // NLE — deterministic, SMPTE-addressable
    Musical { tempo_map: TempoMap }, // DAW — bars/beats ↔ seconds via the tempo map
}

struct Track { target: Binding, items: Vec<Item> }   // items kept time-sorted

enum Item {                          // the per-app payload of the spine
    Keyframe { t: Time, value: Val, interp: Interp, tan_in: Tan, tan_out: Tan },
    Clip     { range: TimeRange, source: AssetId, src_in: Time },   // media region
    Note     { range: TimeRange, pitch: u8, velocity: u8 },         // MIDI
}
```

Three things are shared by *everything* built on this: the **playhead and transport** (play/scrub/loop), the **track/lane UI** (ruler, snapping, markers, drag), and the **evaluation contract** — "given time `t`, produce the state." That contract is what makes the playhead *feel* identical in all three apps, which is the whole point.

### 4.2 Values, interpolation, evaluation

For **property tracks** (the animation case), evaluation at time `t` is: binary-search the keyframe interval around `t`, then interpolate by the segment's mode.

```
sample(track, t):
    (k0, k1) = surrounding_keyframes(track, t)        // binary search, O(log n)
    u = (t - k0.t) / (k1.t - k0.t)
    match k0.interp:
        Constant => k0.value                          // stepped (frame-by-frame, holds)
        Linear   => lerp(k0.value, k1.value, u)
        Bezier   => bezier_ease(k0.tan_out, k1.tan_in, u) applied to the value
    // rotations interpolate as quaternion slerp, never per-euler-channel
```

Values are **typed** (`f32`, `Vec3`, `Quat`, `Color`, `bool`) and interpolation is per-type — the reason a curve editor with bezier tangents works for a position channel and slerp quietly does the right thing for rotation. Stepped/constant interpolation is what makes the *same* engine do frame-by-frame 2D animation: an onion-skinned drawing track is just a track of `Constant`-interp items holding raster-layer references.

### 4.3 The three faces of the timeline

| App | Time base | Dominant item | What the track binds to |
|---|---|---|---|
| **Visual** | Seconds | `Keyframe` curves (+ `Constant` for frame-by-frame) | object transforms, deformation, modifier/material params |
| **Video (NLE)** | Frames @ fps | `Clip` regions + transitions; keyframes for effects | media sources, effect parameters, the compositor graph |
| **DAW** | Musical (tempo map) | `Note` (MIDI) + audio `Clip` regions | instrument/effect nodes in the DSP graph |

Same spine, same playhead behavior, same snapping and ruler and feel — three configurations. The NLE's "ripple/roll" and the DAW's quantize are *operations on items* layered on the shared model, not different timelines.

> **Precedent in your own code.** The video project is a working instance of two of these faces at once. **ScenePilot** does the analysis that *feeds* a beat-grid/NLE timeline — `bpm`, `beats[]`, `silences[]`, `cuts[]`, and an `arrangement[]` of lanes (logo / voiceover / video-insert / effects) with **beat-locked cut points** — which is precisely the `Musical`/`Frames` time base driving `Clip` items. **COMPOSITOR** has the timeline *view* built — viewport, ruler, snapping, bookmarks, marker lane — over `Layer`s carrying `keyframes`. Between them you already have a real NLE face of this spine; building it on the shared platform is re-housing proven pieces, not inventing.

### 4.4 The node-graph spine

The generic model is a **typed dataflow DAG with a node editor:**

```rust
struct Graph { nodes: SlotMap<NodeId, Node>, wires: Vec<Wire> }
struct Node  { kind: NodeKind, params: ParamBlock, inputs: Vec<Port>, outputs: Vec<Port>,
               cache: Option<Cached>, dirty: bool }
struct Wire  { from: (NodeId, PortIdx), to: (NodeId, PortIdx) }   // output → input, type-checked
```

Built once: the **editor** (ports, wires, pan/zoom, box-select, group/collapse, type-checked connection) and the **evaluator** (below). Per app, only `NodeKind` (the library of operations) and the **type that flows on a wire** differ.

### 4.5 Evaluation: pull-based, dirty-propagated, cached

The evaluator is **lazy and pull-based**, the same discipline as the modifier stack (§3.3 is literally the linear special case of this):

```
eval(node):
    if not node.dirty and node.cache.is_some():
        return node.cache                      // memoized — the common case
    for each input wire: eval(upstream)        // pull dependencies (recurse), DAG-guaranteed
    out = node.kind.compute(inputs, node.params)
    node.cache = out;  node.dirty = false
    return out

on_change(node):                               // param edit OR upstream rewire
    mark node.dirty
    for each downstream dependent: on_change(dependent)   // propagate forward, transitively
```

So an edit dirties a node and everything downstream of it; the next pull re-evaluates **only the dirty sub-graph**, reusing every cached output that couldn't have changed. Connections are **type-checked at wire time** (you can't plug a mesh into a color port) and the graph is kept acyclic (cycle rejected on connect, except where an app explicitly models feedback with a delay node). For batch/whole-graph needs, a **topological sort** gives a correct evaluation order in one pass. This is the identical "dirty-from-first-change-downward + per-stage cache" rule the renderer, the modifier stack, and the adjustment-layer compositor all use — one evaluation philosophy across the engine.

### 4.6 The three faces of the node-graph

The structure, editor, and evaluation are shared; the **wire data type** and the **execution context** are what change — and that last difference is the load-bearing one:

| App | Node library | Wire carries | Executes where |
|---|---|---|---|
| **Visual** | materials, modifiers, procedural geometry | shaded surfaces / meshes / textures | GPU (graph compiles to a shader) + worker pool (geometry) |
| **Video** | composite, transform, key, color, blur | image textures | GPU, per output frame |
| **DAW** | instruments, effects, mixer routing | audio buffers + MIDI | **the real-time audio thread, per block, lock-free** |

The DAW is the one that proves the design's seriousness. Its graph *looks* like the others in the editor and obeys the same topology, but it evaluates under a **hard sub-millisecond deadline with no allocation and no locks**, pulling one audio block at a time. That's why the platform doc keeps the audio core process-isolated (its own binary) even as it wears the identical shell: the **graph structure is shared, the execution context is alien.** Build the editor and topology once; give audio its own ruthless real-time executor behind the same node abstraction.

> **Precedent — and an honest divergence.** The video project's compositing today is *not* a native node graph — per the reality-check it deliberately takes **Fork B**, driving FFmpeg as an embedded executor to retire the export blocker fast. That's the right call for shipping that product *now*. But it means the suite's "compositing node-graph face" is the *native* upgrade path (Fork A) the reality-check parks for later — "evolve toward A behind the same contract, justified only where it differentiates (live GPU preview, real-time effects)." So if COMPOSITOR becomes the suite's video pillar, this section is the spec for that eventual native compositor, with the FFmpeg executor as the pragmatic bridge until the GPU graph earns its place. The contract is the seam that lets you swap one for the other.

### 4.7 How the four interlock — one frame, end to end

Everything above resolves into the intro's invariant, `render(evaluate(scene, t))`, expanded:

```
1. ADVANCE     playhead → t
2. SAMPLE      timeline samples every track at t → writes values into:
                 · object transforms / deformation        (object model)
                 · modifier & material/node parameters     (node-graph inputs)
3. DIRTY       each write marks its modifier stack / node sub-graph dirty,
               propagating downstream  (§3.3 / §4.5)
4. COOK        pull-evaluate the dirty stacks & graphs — cached everywhere clean;
               heavy cooks run on the worker pool with a live proxy, swap atomically
5. RENDER      renderer reads the cooked, current scene state and draws it in <8 ms
               (build clusters → opaque → transparent → flat → overlays → post)
6. PRESENT     one encode to the swapchain; nothing in 1–5 ever blocked the UI thread
```

The object model is the truth; the timeline mutates it over time; the node-graph computes derived data for it; the raster substrate stores its pixel payloads; the renderer is a pure readout of it. Four subsystems, one loop, one invariant — and because every one of them obeys the same dirty/cache/never-block discipline, the whole thing stays inside the frame budget by construction rather than by heroics.

---

## 5. One Edit, All Four Subsystems — a Brushstroke's Journey

The four-subsystem story is easiest to feel through a single concrete edit: **the user paints one stroke on a raster layer that sits inside a design composition with an adjustment layer above it.** Follow it all the way to undo:

```
PEN DOWN
  └─ INTERACTION resolves the hit: the selected Object is a Raster payload (object model, §3.1)
  └─ a LIVE scratch layer is allocated next to that layer's COMMITTED tiles (§2.2)

PEN MOVE (×200/sec)
  └─ events coalesced & stabilized → stamps spaced along the path (§2.4, §2.7)
  └─ each stamp's params evaluated from pressure/tilt/velocity via response curves (§2.5)
  └─ stamps blend into the LIVE layer's dirty tiles on the GPU — pixels never leave it (§2.1)
  └─ EVERY FRAME: renderer composites committed ⊕ live, then the adjustment layer above
       reads the result beneath it, in the flat-snapped ortho pass, in linear space (§1.5, §1.7)
  └─ all of this finishes in <8 ms; the canvas stays live the whole stroke (§1.9)

PEN UP
  └─ dirtied COMMITTED tiles are snapshotted (before-image) → a PaintStroke command (§3.4, §2.3)
  └─ LIVE flattens into those committed tiles; live layer freed (§2.2)
  └─ the command is pushed as one transaction; the adjustment-layer cache above it invalidates
       only for the dirtied tile region, not the whole layer (§1.7)

CMD-Z
  └─ PaintStroke.revert swaps the ~6 before-tiles back; the affected node/adjustment caches
       re-evaluate only over that region; next frame redraws. A few hundred KB moved. (§2.3, §3.4)
```

Notice that no single subsystem "owns" the stroke — it is a relay: the object model says *what was hit*, the raster substrate *holds and moves the pixels*, the renderer *composites and shows it every frame*, and the command/undo system *makes it reversible for pocket change*. The same relay, with different payloads, is how a 2D→3D extrude (object model spawns a mesh, modifier stack cooks the bevel, renderer draws it) or a keyframed camera move (timeline samples → transform updates → renderer reads) flows. **One architecture, many edits, always the same five-step loop.**

---

## 6. Honest Caveats — the Hard Parts in These Four

Consistent with the source docs' honesty sections, here is where these subsystems will actually fight you:

- **The unified 2D/3D renderer is the genuine tightrope.** Keeping vector/text *razor-sharp* (analytic/MSDF, native-res) while keeping 3D *fast* (clustered forward) in **one** pipeline, with a seamless ortho↔perspective transition, is harder than either alone. The risk isn't any single pass — it's the seams between them (the flat-pass handoff, pixel-snapping at the ortho boundary, AA consistency between SDF edges and MSAA'd geometry). Build the flat pass and the transition in Phase 0–1 and stare at the seams early; they don't forgive being deferred.

- **Color management is a tar pit you must enter early, not late.** Retrofitting a linear, wide-gamut, ICC-correct pipeline after the fact means re-checking every blend, filter, and import in the product. Pick the working space and the OCIO-style config before the second object type exists, and share it with the video app from day one — the platform doc is right that visual+video color must be one layer, and it's far cheaper to hold that line than to reconcile two later.

- **The node-graph is three evaluators wearing one editor — don't pretend it's one.** The shared editor and topology are real wins, but the GPU image evaluator, the CPU/worker geometry evaluator, and the **lock-free real-time audio evaluator** are genuinely different execution engines. The trap is a too-clever unified evaluator that compromises the audio thread's hard deadline. Share structure; isolate execution; let the DAW's evaluator be ruthless and separate (and its process separate), exactly as the platform doc insists.

- **Undo across async cooks needs an explicit reconciliation rule.** When a heavy modifier/boolean cooks on the worker pool and the user undoes (or edits again) mid-cook, you must cancel the in-flight cook and reconcile its result against the new command state — or you'll commit a stale result over a newer edit. Decide the rule once, centrally (cancellation tokens + a generation stamp on each cook keyed to the command version), or this becomes a whack-a-mole of subtle history bugs.

- **The universal format must be designed shallow and grown, never speced deep up front.** Both source docs and the video reality-check converge on this: a container that *references* domain documents, with strong interchange, and a *minimal* contract shaped by a real second consumer — not a maximal schema invented before app #2 exists. The most likely way to waste a quarter here is to over-design the cross-app schema in the abstract. Design the container and versioning early; fill domain payloads progressively; let OTIO carry the video document rather than inventing your own editorial format.

- **Tiles vs. true-procedural raster is a boundary to draw deliberately.** Dirty-tile undo and the live/committed split assume *destructive-at-commit* pixels. Fully non-destructive, infinitely re-editable raster effects (the §4 compositing graph applied to paint) want a different caching model. Both are good; they meet at a defined seam (committed tiles feed the graph; the graph's output is itself cacheable). Name that seam explicitly so the two models compose instead of fighting.

---

## TL;DR

Four subsystems carry the product, and they are one design seen from four angles. The **object model** is the single source of truth — a uniform typed-object envelope with a payload enum, generational IDs, and command-delta undo that stores minimal inverses, never snapshots. The **raster substrate** keeps pixels on the GPU in sparse 256×256 tiles with a live/committed split and dirty-tile undo, so a 100-megapixel canvas and a deep history both stay light. The **renderer** is a pure readout of scene state — clustered-forward, five passes, with a screen-space flat pass and MSDF/analytic coverage that keep design content crisp at any zoom, all in linear color under an 8 ms budget. The **timeline and node-graph** are two generic spines — tracks/keyframes/playhead and a typed, pull-evaluated, dirty-cached DAG — each configured three ways to become animation/NLE/beat-grid and materials/compositing/DSP across the suite. They interlock as `render(evaluate(scene, t))`: the timeline mutates the model, the graph derives data for it, the substrate stores its pixels, the renderer draws it. Every one of them obeys the same three laws — **dirty-propagate, cache aggressively, never block the UI thread** — which is exactly why "instant" is a property the architecture gives you rather than a thing you chase. Build these four on that shared discipline and the apps on top are domain logic on a sound skeleton; skip the discipline and no feature work saves the feel.

