# The Unified Canvas — Architecture & Feature Master Plan

*A standalone creative tool where 3D modeling/sculpting, graphic design, and natural-media painting live on one canvas, with a custom engine and "instant feel" as the prime directive.*

---

## 0. The One Idea Everything Hangs On

**Everything is a typed object in a single 3D scene, rendered by one engine, edited by tools that morph to fit what's selected.**

There are no "modes" — no 3D mode, design mode, paint mode. There is one canvas (a 3D scene with a camera that can snap to flat orthographic). A text layer is a quad in space. An artboard is a plane. A photo is a textured plane. A painting is a tiled raster plane. A model is a mesh. They share coordinate space, camera, renderer, timeline, and undo system.

This single decision is what lets you build *one* coherent app instead of three apps in a trench coat. Every feature below falls out of it.

The thing that sells "one tool" to the user is not a feature — it's two pieces of craft:
- **The morphing gizmo** (the transform handle changes shape based on selection: full 3D handles on a mesh, flat handles on a design layer). It silently tells the user what kind of thing they're holding.
- **The contextual right panel** (same screen real estate, contents reskin by selection type). One panel, not nine.

---

## 1. System Architecture

### 1.1 The layered stack

```
┌─────────────────────────────────────────────────────────────┐
│  SHELL (UI)   thin top bar · left tool strip · contextual    │
│               right panel · universal bottom timeline        │
├─────────────────────────────────────────────────────────────┤
│  INTERACTION  selection · the morphing gizmo · snapping/     │
│               magnet · tool state machine (morphs by type)   │
├─────────────────────────────────────────────────────────────┤
│  DOCUMENT MODEL   scene graph of typed objects · transforms ·│
│                   modifier/effect stacks · the timeline ·    │
│                   the command/undo system                    │
├──────────────┬───────────────┬───────────────┬──────────────┤
│ MESH KERNEL  │ RASTER SUBSTR.│ VECTOR/TEXT   │ IMAGE I/O     │
│ poly edit,   │ tiled GPU     │ bezier,       │ image I/O,    │
│ sculpt,      │ pixel buffers,│ tessellation, │ import &      │
│ booleans,    │ masks,        │ SDF text      │ export        │
│ subdivision  │ brush engine  │               │               │
├──────────────┴───────────────┴───────────────┴──────────────┤
│  RENDERER   clustered-forward (Forward+) · depth-sorted      │
│             transparency · analytic AA for vectors/text ·    │
│             the compositor (blend modes, adjustments) · post │
├─────────────────────────────────────────────────────────────┤
│  GPU ABSTRACTION   wgpu (Vulkan / Metal / DX12 / WebGPU)     │
├─────────────────────────────────────────────────────────────┤
│  CONCURRENCY   UI/render thread (never blocks) · GPU queues ·│
│                worker pool (booleans, ML, decimation, I/O)   │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 The object model (the heart)

Every scene object shares a common envelope and carries type-specific payload:

| Field | Every object has it |
|---|---|
| `id`, `name`, `parent`, `children` | scene graph membership |
| `transform` | position / rotation / scale in 3D space |
| `visibility`, `lock`, `blend_mode`, `opacity` | compositing |
| `modifiers[]` / `effects[]` | non-destructive stack |
| `keyframe_tracks[]` | animation on the shared timeline |
| `payload` | the type-specific data ↓ |

**Object types and their payloads:**

- **Mesh** — half-edge or indexed geometry; the poly-modeling target.
- **Sculpt body** — high-res dynamic-topology / voxel mesh + multires displacement.
- **Raster layer** — a tile handle into the raster substrate (lives as a textured plane).
- **Vector shape** — bezier paths; tessellated to triangles or rendered via SDF.
- **Text** — string + font + layout; rendered from an SDF glyph atlas.
- **Image plane** — a textured quad (photos, references, texture sources).
- **Light / Camera / Null** — transform helpers and scene rig.
- **Group / Collection** — organizational + transform inheritance.

Because the envelope is uniform, *every* object can be transformed by the gizmo, animated on the timeline, snapped by the magnet, stacked with non-destructive effects, and composited with blend modes — for free, regardless of type. That uniformity is the whole trick.

### 1.3 The renderer

A **clustered-forward (Forward+) renderer** is the sweet spot for this mixed content — it handles many lights, plays nicely with transparency (which deferred fights), and stays flexible for heterogeneous object types.

Pass order:
1. **Opaque 3D** — meshes, depth pre-pass, shaded.
2. **Transparent** — back-to-front sorted (design layers, glass, washes).
3. **Flat-snapped layer** — when the camera is orthographic, design/paint planes composite in screen space with analytic antialiasing so vector and text edges stay crisp at any zoom.
4. **Gizmos & overlays** — selection outlines, the morphing gizmo, snapping guides, grid.
5. **Post / compositor** — blend modes, adjustment layers, color management, optional bloom/DOF for 3D.

**Crispness rule:** vectors and text render via signed-distance fields or analytic coverage, *not* by rasterizing-then-scaling. This is what makes the "graphic design" half feel like a design tool and not a 3D viewport with text pasted on.

### 1.4 The performance doctrine (because speed is priority #1)

This is its own section below (§2) because you ranked it first. The one-line version that governs the architecture: **the thread that draws the cursor never waits for anything.**

---

## 2. Performance Doctrine — "Instant or it didn't ship"

Photoshop lags on modest hardware for fixable, legacy reasons: too much CPU↔GPU round-tripping, memory-heavy full-snapshot undo that spills to the scratch disk, single-threaded filters that freeze the UI. You start clean in 2026 on wgpu and beat it *by default* if you hold these lines:

**The non-negotiables:**

1. **Input → on-screen result completes in < ~8 ms, every frame, no exceptions.** Anything that can't finish in budget runs async and streams its result back. The canvas stays live the entire time.

2. **Pixels live on the GPU and stay there.** The CPU rarely touches pixel data. No per-stroke round-trips across the bus.

3. **Tiles, always.** The canvas is split into 256×256 tiles. Only tiles under the brush are touched per stroke. This is how you paint on a 100-megapixel canvas without choking — you never process the whole image, only the few tiles the cursor is over.

4. **Live layer vs committed layer.** A mid-stroke paint goes into a scratch GPU texture composited on top; on stroke-end it flattens into committed tiles. In-progress strokes cost almost nothing and redraw at full framerate.

5. **Dirty-tile undo, not full snapshots.** Before a stroke modifies a tile, copy *just that tile* to history. A brushstroke dirties ~6 tiles → one undo step is a few hundred KB, not hundreds of MB. This is the single biggest reason your app feels light where Photoshop hits the scratch disk. Spill compressed tiles only if memory pressure demands it.

6. **Render loop decoupled from edit rate.** Redraw at display refresh (120/144 Hz). Pen input arrives at 200+ Hz — coalesce points, but never let an input burst stall a frame.

7. **Minimal swapchain buffering.** Pen-to-photon latency includes the OS stack, compositor, and display, which you don't fully control — so make *your* contribution a single sub-frame. Avoid deep swapchains that silently add frames of lag. Measure this early with the real windowing layer, not late.

**Where heavy work goes** (so it never causes lag): booleans, content-aware fill, large blurs, ML selection, mesh decimation, file I/O — all dispatched to compute shaders or the worker pool, previewed live where possible, committed atomically when done. **Nothing slow is ever in the input→draw path.**

Krita and Procreate already feel snappier than Photoshop on modest hardware for exactly these reasons. None of this is exotic — it's just work Photoshop can't retrofit and you can build clean.

---

## 3. The 3D Toolset — *make modelers love it (Blender / Cinema 4D feel)*

### 3.1 Poly modeling (the box-modeling core)

The non-negotiable mesh-edit toolkit modelers expect, all operating on a half-edge mesh:

- Selection: vertex / edge / face, **loop & ring** select, grow/shrink, select-by-angle, linked.
- Core ops: extrude, inset, bevel (edge + vertex), **loop cut**, knife, bridge, merge/weld, dissolve, spin, fill/grid-fill.
- Symmetry editing (mirror across an axis live).
- Soft selection / proportional editing (Blender's "O" — falloff-weighted moves) for organic adjustments.

### 3.2 The modifier stack (the C4D/Blender magic)

This is where modelers fall in love: **a non-destructive generator stack** evaluated top-to-bottom, re-cookable at any time.

Subdivision surface · Mirror · Array (linear/radial) · Solidify · **Boolean** · Bevel · Displace (texture-driven) · Lattice/cage deform · Shrinkwrap · Remesh. Each is a node in the object's `modifiers[]`. The user stacks, reorders, toggles, and tweaks parameters forever — the base mesh is never destroyed.

### 3.3 Sculpting (ZBrush/Blender-sculpt "ease")

The thing that makes sculpting feel *free* is **not fighting topology**:

- **Dynamic topology** (subdivide where the brush adds detail) and **voxel remesh on demand** (rebuild uniform topology in one click) — so users add detail anywhere without manual edge-loop work. This is the single most important "ease" feature.
- **Multiresolution**: sculpt on a subdivided cage, bake the fine detail to displacement/normal maps for a light final mesh.
- Brush set: draw, clay, **clay strips**, smooth, grab, snake hook, inflate, pinch, crease, flatten, scrape, mask, and trim.
- **Auto-retopology** (QuadriFlow-style quad remesh) to turn a sculpt into clean animation-ready topology — bundle or integrate a remesher; this is a known hard problem you should *lean on existing research for*, not invent.

### 3.4 Booleans — *easy boolean (your stated priority)*

What "easy" means in practice:
- **Live, non-destructive preview**: drop a cutter object, drag it, watch union/difference/intersect update in real time.
- **Auto-cleanup** of the resulting topology so the output isn't garbage.
- **Robustness is the hard part.** Naive booleans produce broken meshes at coplanar faces and near-misses. Use an **exact-arithmetic / mesh-arrangement** solver (the approach Blender's "exact" boolean uses) to avoid the classic boolean mess. Evaluate heavy booleans on the worker pool with a live preview proxy so the canvas never stalls.

This is a place you can genuinely out-feel Blender by making the *interaction* (drag-to-cut, magnetic placement, instant preview) effortless even though the math underneath is serious.

### 3.5 Magnet & snapping — *objects snap together (your stated priority)*

Blender's snapping is powerful but fiddly. **A predictive, visual, magnetic system is a real differentiator.**

- Snap targets: vertex, edge midpoint, face center, grid, increment, object pivot, bounding-box corners.
- **Surface snapping**: drop an object onto another's surface and auto-orient to the surface normal (place a book on a table, a barnacle on a hull).
- **Face-to-face mating**: lightweight CAD-style assembly — select two faces, they snap flush and aligned. Great for hard-surface and product work.
- **The magnet feel**: as you drag near a valid target, candidate snap points *glow and pull* the object in, with a clear visual guide and a satisfying click. Predictive (shows the snap before you commit), forgiving (generous capture radius that tightens as you slow down), and overridable (hold a key to free-move).

### 3.6 Rigging — *automated rig (your stated priority)*

- **Armature/skeleton** system with FK and **IK** chains for posing.
- **Automated weighting**: bone-heat / heat-map skinning so binding is one click, not hours of weight painting.
- **One-click humanoid auto-rig** (Mixamo-style): detect a humanoid mesh, fit a standard skeleton, bind, done. This is the "automated" dream and a strong candidate for an ML-assisted skeleton-fit (see §7).
- Pose library + weight-paint touch-up for when the automation needs a nudge.

### 3.7 2D → 3D — *make 2D objects & images 3D with ease (your stated priority)*

This is several distinct, high-delight features — ship them as a family:

- **Vector/text extrusion** — a shape or text → extrude + bevel into solid 3D (C4D's classic Extrude). Easy, instant, huge value for logos and titles.
- **Lathe/revolve** — spin a 2D profile around an axis into a solid (vases, bottles, turned forms).
- **Image → displacement** — a grayscale image displaces a plane into relief.
- **Sketch-to-3D "inflate"** — draw a closed shape and *puff* it into a rounded 3D form (Monster Mash / sketch-based modeling). Delightful, approachable, makes non-modelers feel powerful.
- **SVG/logo → 3D** — import vector art, extrude, material, done.
- **ML image-to-3D** — single image → mesh via a bundled or called model (now genuinely viable). Flag as a heavier feature (§7), but it's the headline "wow."

---

## 4. The Graphic Design Toolset — *an upgraded Photoshop*

Built on the tiled GPU raster substrate from §2, plus:

### 4.1 Non-destructive everything
- **Adjustment layers** (curves, levels, hue/sat, color balance, exposure) as GPU shaders — instant, re-editable, stackable, maskable.
- **Smart filters** — filters applied non-destructively with live re-tweak.
- **Smart objects / linked layers** — edit once, update everywhere.

### 4.2 The tools
- **Paint/erase** (the brush engine — see §5; it's shared with the paint side).
- **Clone stamp** (sample-and-paint, trivial).
- **Healing / spot heal / patch** — Poisson seamless cloning: blend a region by matching gradients instead of copying pixels. Published math, real but unmysterious work, runs on a compute shader.
- **Content-aware fill** — PatchMatch on the worker pool with a live preview.
- **Pen tool** (full bezier), **shape tools**, **type tool** with real typographic control.
- **Gradient** (incl. modern free-form/mesh gradients as the "upgrade").

### 4.3 Selection
- Marquee, lasso, polygonal lasso, magic wand (color threshold), intelligent-scissors edge-snapping.
- **Select Subject (ML)** — the one feature that's a project unto itself; covered in §7. Classic selection is hand-buildable; Adobe-quality one-click selection means bundling a segmentation model.
- **Feather** = a gaussian on the selection edge; **refine edge** for hair/fur.

### 4.4 The "upgrade" — what Photoshop structurally can't do
- **Non-destructive by default**, everywhere.
- **Infinite, instant canvas** (the tiled architecture) with multiple **artboards** on one surface.
- **Vector + raster coexist seamlessly** because both are just objects in the scene.
- **Live 3D in your composition** — drop in a model, re-light and re-pose it without leaving the document. This is the upgrade Photoshop fakes and you do natively.
- **Real-time GPU pipeline** — no "applying filter…" progress bars.
- **Proper color management** — ICC profiles, linear working space, wide-gamut output for modern displays. Pros notice this immediately.

---

## 5. The Paint & Draw Toolset — *a state-of-the-art Procreate*

The shared GPU-tiled substrate already gives you Procreate's defining quality: **low latency**. On top of that:

### 5.1 The brush engine
- A **deeply parametric (node-style) brush engine**: shape + grain/texture + **dual brush** + dynamics + wet-mix.
- **Inputs → any parameter**: pressure, tilt, velocity, and bearing can each drive size, opacity, flow, rotation, scatter, hue/sat/value jitter.
- **Stabilization**: predictive stroke smoothing — pulled-string / catch-up modes (Procreate's StreamLine), tunable per brush.
- **QuickShape / shape-snap**: hold at the end of a stroke and a rough circle/line/rect snaps clean.

### 5.2 Natural media (the "state-of-the-art" part)
- **Real wet mixing** — pigment that blends on the canvas (Rebelle-style fluid sim is the high end).
- **Watercolor bleed**, **oil impasto** (height-based so light catches the ridges), **pastel/charcoal grain**.
- Physically-inspired media is your differentiator over Procreate's (excellent but largely stamp-based) engine.

### 5.3 The drawing environment
- **Symmetry / radial / mandala** modes.
- **Drawing assists**: 2-point/3-point perspective, isometric, and other guides that constrain strokes.
- **Reference layers**, alpha lock, clipping masks, full blend modes.
- **Color**: HSB picker, harmony wheels, palettes, and **flood-fill drop** (Procreate's ColorDrop).
- **Animation assist** — frame-by-frame with onion skinning, native because you already own a timeline (§1.2).

---

## 6. The Unifiers — *the features that exist ONLY because it's one canvas*

These are your moat. No incumbent can do all of them because none share one substrate:

1. **Paint directly on 3D surfaces** with the same brush engine — texture/projection painting on a model (Substance Painter / 3D-Coat territory), using the exact brushes you draw flat illustrations with.
2. **Live 3D inside a 2D composition** — re-pose and re-light a model without leaving your design.
3. **Any 2D → 3D in a click** — drawings, vectors, photos become geometry (§3.7).
4. **One timeline animates everything** — mesh deformation, camera moves, motion graphics, a paint-on reveal, and frame-by-frame drawing, all on the same tracks. (This is an *animation* timeline — audio and video editing live in the sibling suite apps.)
5. **Magnet & snap across every object type** — a text layer snaps to a mesh face; a photo plane snaps to the grid; a sculpt snaps onto a surface.

When you demo this tool, you demo *these five*. Everything else is table stakes; these are why someone switches.

---

## 7. The Honest "Hard Bucket" (ML & robustness)

Three things are genuinely separate engineering projects, not weekend features. Plan for them as commitments:

- **Select Subject at Adobe quality** → bundle an on-device segmentation model (SAM-style) and run inference on the worker pool; mask streams in while the user keeps working. Classic selection ships first; ML selection is a milestone.
- **Image → 3D** → an on-device or called image-to-mesh model. Real in 2026, but a model-ops commitment (size, inference time, quality variance).
- **Robust mesh booleans & auto-retopology** → use exact-arithmetic solvers and existing remeshing research (QuadriFlow et al.). Do **not** invent these from scratch; integrate and wrap them in a delightful interaction.

ML runtime: an on-device inference engine (ONNX Runtime or equivalent) loaded off the UI thread. Everything ML is async, previewed, and committed atomically — same doctrine as §2.

---

## 8. Recommended Tech Stack

| Layer | Recommendation | Why |
|---|---|---|
| Language | **Rust** (or C++) | memory-safe, fast, modern; C++ if you want the mature geometry-lib ecosystem |
| GPU | **wgpu** | one API over Vulkan/Metal/DX12/WebGPU; native backend is proven for "3D viewport + UI" |
| UI framework | **GPUI** (polish-first) or **egui** (fastest to move) | GPUI gives a high starting floor of polished, GPU-native components; egui is immediate-mode and integrates with a 3D viewport trivially but needs heavy custom styling to not look like an engineering tool |
| Geometry kernel | integrate, don't invent | mesh booleans, remeshing, decimation are solved-but-hard; wrap existing research/libs |
| ML | **ONNX Runtime** (or equiv.) | on-device inference for selection, image-to-3D, auto-rig, off the UI thread |
| Images | platform image codecs | photo / texture import & export; **no video or audio — those are separate apps in the suite** |

**The honest framework call:** if the engine is the priority and you'll style from scratch, **egui + wgpu** is the proven path for "dominant 3D viewport with surrounding panels" and moves fastest. If visual polish is core to your identity — and for a design tool, it is — invest in **GPUI**. Either way the renderer is **wgpu**, which composes cleanly with both.

---

## 9. Build Roadmap (ruthless sequencing)

Sequence so visual design and "feel" always have something real to live on. Each phase is shippable-as-a-demo.

- **Phase 0 — Engine spine.** wgpu renderer that draws a mesh, a textured quad, and a grid, with one camera that toggles perspective ↔ orthographic. *This is the single-canvas foundation.*
- **Phase 1 — The gizmo, early.** Build the morphing transform gizmo *before* panels. It's the soul of the unified feel and it teaches you what your interaction model actually needs.
- **Phase 2 — Raster substrate.** Tiles, brush→mask, live/committed layers, dirty-tile undo. → a paint MVP that already feels faster than Photoshop.
- **Phase 3 — The shell.** Monochrome charcoal chrome, one accent color, the contextual right panel, the universal timeline. Build *one* panel that reskins by selection type — never a panel per discipline.
- **Phase 4 — 3D modeling.** Poly-edit toolkit + the modifier stack + parametric primitives.
- **Phase 5 — Sculpting.** Dynamic topology / voxel remesh + the brush set.
- **Phase 6 — Graphic design.** Adjustment layers, vector + text objects, blend modes, classic selection, healing/clone.
- **Phase 7 — The unifiers.** 2D→3D family, paint-on-3D, magnet snapping. *This is where it stops being three tools and becomes the thing.*
- **Phase 8 — Rigging + animation** across the shared timeline.
- **Phase 9 — ML milestones.** Select Subject, image→3D, auto-rig.
- **Throughout** — the timeline grows from simple keyframes (Phase 1) to full character animation (Phase 8). It stays an *animation* timeline, not a media editor — audio and video are separate apps in the suite.

---

## 10. Scope Reality — and How to Actually Win

This app is the **visual-creation pillar of a suite** — the DAW and the video editor are separate standalone apps. That focus is a gift. This one app "only" has to be Blender + Photoshop + Procreate + a texture-painter fused on a single canvas. Still enormously ambitious — each of those represents a large specialized team — but it's a *coherent* ambition around one substrate, not three unrelated engineering fronts (audio DSP, video codecs/NLE, visual creation) opened at once. The hardest categories in the suite live in their *own* binaries, where they belong.

**The winning strategy:**
1. **Pick the one axis that's your real differentiator** — the *unified visual canvas* where paint, design, and 3D coexist and 2D becomes 3D effortlessly. Make that genuinely great.
2. **Keep this app's timeline an *animation* timeline, not a media editor.** Keyframes for transforms, deformation, motion graphics, and frame-by-frame drawing — not audio tracks or video clips. Those belong to the sibling apps. This keeps the scope honest and the substrate clean, and it's why the object model has no audio/video citizens.
3. **Design for the suite, not just the app.** Since audio and video are siblings, decide the connective tissue early: one shared visual design language across every app, a common project/asset format, and clean interchange — this app renders stills and animation the video editor imports; the video editor pulls motion-graphic elements and 3D renders from here. Separate binaries can still feel like one product when they share a spine.
4. **Lead every demo with the five unifiers (§6).** That's the story no incumbent can tell.
5. **Hold the performance doctrine (§2) as sacred from Phase 0.** "Instant" is a property you build in from the first frame, not one you optimize in later. It's also, conveniently, the thing you wanted most — and the thing that makes every other feature feel professional.

Build the substrate right, and feather, smooth-erase, healing, sculpting, booleans, and live 3D-in-design all feel inevitable. Build it wrong, and even perfect algorithms feel janky. The substrate *is* the product.
