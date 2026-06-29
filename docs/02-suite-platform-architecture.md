# The Suite Platform — One Foundation, Consistent Apps

*How the visual tool, the video editor, and the DAW come to feel, run, and operate as one product — even though they do radically different things. Companion to the Unified Canvas architecture doc, which covers the visual app sitting on top of this platform.*

---

## 0. The Decision (since you asked me to make it)

**Build one shared platform — a core SDK that every app is built on. This is non-negotiable, and it is the entire answer to "make them feel, run, and operate the same."**

Two things follow from that, and they're the load-bearing decisions:

1. **Consistency comes from the platform, not from packaging.** Whether the apps ship as one binary or three changes almost nothing about whether they *feel* identical. What makes them identical is sharing the same UI framework, design system, interaction model, performance doctrine, and project fabric. Get those shared and three separate apps feel like one product; skip them and one giant app still feels incoherent.

2. **The platform is also the only thing that makes building three pro apps remotely feasible.** You build the hard foundation **once**. App #2 and #3 inherit the feel, the speed, and half the machinery for free. Without this you are building three apps from scratch — which is not realistic. With it, you're building one platform and three domain cores.

**My packaging recommendation:** ship the apps as **separate binaries on the shared platform** (the model Adobe uses, and old Affinity's StudioLink) — *skin-deep separate, bone-deep shared*. Optionally fuse the two visual-heavy apps (creation + video editor) into one shell the way Resolve fuses Edit/Fusion/Color. Keep the **DAW a sibling binary** regardless, because its real-time audio core is alien to a GPU frame loop and benefits from process isolation — but it wears the identical shell, design system, and shortcuts, so the user can't tell it's a different process. This is the one decision you can reverse later; the platform is the one you can't.

---

## 1. What Actually Makes Apps Feel the Same

Consistency is not a packaging question. It's five shared substrates. Share these and you've won, regardless of how many `.exe`s ship:

| Shared substrate | What it guarantees |
|---|---|
| **Design system** (tokens + component kit) | Same look — identical color, type, spacing, motion, identical buttons/sliders/pickers |
| **Interaction model** (input, gizmos, shortcuts, command palette) | Same moves — muscle memory transfers app to app; a gesture means the same thing everywhere |
| **Performance doctrine + GPU foundation** | Same speed — every app is equally instant, same frame budget, same never-block rule |
| **Document mechanics** (command/undo, non-destructive stacks, serialization) | Same behavior — undo, history, layers/effects work identically |
| **Project & asset fabric** (one format family, one asset library, interchange) | Same world — files and assets flow between apps; start in one, finish in another |

If all five are shared, the suite is coherent by construction. This table *is* the strategy.

---

## 2. The Industry Just Voted (precedents worth copying)

- **New Affinity (Oct 2025).** Canva collapsed the three standalone graphics apps into **one unified app** with Pixel / Vector / Layout "Studios" you switch between, plus a **universal `.af` file** holding all three document types. Native, not Electron. → Proof that *closely related* domains are better fused than separated.
- **DaVinci Resolve.** One application holding an NLE (Edit), color grading (Color), node compositing + motion graphics (Fusion), and **a complete DAW (Fairlight)** — unified by the **page model** (a bottom tab bar swapping entire workspaces). → The closest precedent to *your* suite: it already proves video + audio + compositing can live in one coherent app.
- **Adobe.** Separate apps (Premiere, Photoshop, After Effects, Audition) on shared internal frameworks, stitched by **Dynamic Link**. → Proof the separate-binaries-on-a-platform model scales to the biggest creative suite on earth.
- **Blender.** One app, **workspaces**, holding modeling, sculpt, a video sequence editor, and compositing. → Proof a single binary can hold wildly different tools via workspace switching.

**Takeaway:** both packagings are proven. Momentum for related domains is toward unification. Resolve specifically proves that even audio and video can share one binary if you want. So packaging is a preference you can satisfy either way — which is exactly why the *platform* is where you invest.

---

## 3. The Platform Layers (the core SDK)

Everything in this section is built once and used by all three apps. This is the foundation you extract and harden first.

**1. Shell & window framework.** The window chrome, panel docking, the thin top bar, the left tool strip, the contextual right panel (reskins by selection), the bottom timeline dock, multi-window/multi-monitor. Every app gets the same skeleton; only the contents differ.

**2. Design system.** One token set (the charcoal chrome + single accent, type scale, spacing, motion curves) and one component kit (buttons, sliders, numeric drags, color pickers, menus, dialogs). This is the single biggest lever for "looks the same." Build it as a real component library, not per-app CSS.

**3. GPU / render foundation + performance doctrine.** The wgpu abstraction, the render-loop architecture, the compositor, and the **performance doctrine from the app doc promoted to platform law**: input→on-screen under ~8 ms, pixels stay on the GPU, never block the UI thread, minimal swapchain buffering. Every app — *including the DAW's meters, waveforms, and spectral views* — renders through this and is equally instant.

**4. Interaction substrate.** Pen/tablet/mouse input handling, the **gizmo/manipulator framework** (the visual app and the video editor both need transform handles), snapping/magnet, the selection model, and the **shortcut + command system with a command palette**. Shared so that *how you do things* is identical across the suite.

**5. Document model + command/undo.** The typed-object envelope pattern, the **command pattern with per-type compact undo**, non-destructive stack evaluation, and serialization. Each app defines different object types, but the *machinery* — undo, history, effect stacks, keyframe tracks — is the platform's.

**6. Timeline / keyframe engine — one spine, three faces.** A generic track/keyframe/playhead engine, specialized per app:
   - Visual → an **animation** timeline (transform/deformation/motion-graphics/frame-by-frame).
   - Video → an **NLE** timeline (clips, tracks, transitions, ripple/roll).
   - DAW → a **beat-grid** timeline (bars/beats, MIDI, audio regions).
   Same engine, same playhead behavior, same feel — three configurations.

**7. Node-graph framework — one spine, three faces.** A generic node editor + evaluation graph, specialized per app:
   - Visual → materials / modifiers / procedural geometry.
   - Video → compositing / effects graph.
   - DAW → the DSP / instrument-and-effect signal graph.
   Build the node editor once (ports, wires, pan/zoom, grouping, evaluation) and every app's graph feels the same.

**8. Project & asset fabric.** A **universal project/bundle format** (a container that can reference domain-specific documents), a shared **asset library/browser** (brushes, materials, LUTs, presets, fonts, samples), and **cross-app embedding/interchange** — the StudioLink / Dynamic Link / `.af` equivalent. This is what lets a designed frame or a 3D render drop into the video editor, or a motion-graphic element come from the visual app, without round-tripping through export hell.

**9. Core services.** File I/O, **color management** (consistent color across visual + video is critical — one working-space and ICC layer), preferences/settings (synced so your setup follows you across apps), the **plugin/extension host** (third parties write once, extend the whole suite), licensing, auto-update, and crash/telemetry. Boring, shared, essential.

---

## 4. What Each App Adds (the domain cores)

On top of the platform, each app contributes only its domain-specific core:

| App | Its unique core |
|---|---|
| **Visual** | geometry/mesh kernel, the tiled raster substrate, sculpt (dyntopo/voxel), vector/text, the 3D viewport, the 2D→3D family |
| **Video editor** | codec decode/encode pipeline, NLE editing semantics, transitions, compositing, color grading |
| **DAW** | the **real-time audio thread** (lock-free, sub-millisecond, strict timing), DSP, MIDI, instruments/effects, plugin hosting (VST/AU/CLAP), the mixer |

**Notice the reuse hiding in plain sight:** the video compositor and the visual material system are the *same node framework*. All three timelines are the *same engine*. Color grading and the design app's adjustment layers are the *same compositor*. Much of what looks app-specific is a platform primitive wearing a different hat — which is exactly why the apps feel related: they literally are, underneath.

---

## 5. Repo & Code Structure (concrete, assuming Rust)

A single monorepo workspace. Platform crates everyone links; thin app binaries on top.

```
suite/
├── platform/
│   ├── gpu/         # wgpu abstraction, renderer, compositor
│   ├── ui/          # shell, panels, docking, command palette
│   ├── design/      # tokens + component kit (the look)
│   ├── input/       # pen/mouse, gizmos, snapping, shortcuts
│   ├── doc/         # object model, command/undo, serialization
│   ├── timeline/    # keyframe/track engine (3 faces)
│   ├── nodes/       # node-graph framework (3 faces)
│   ├── assets/      # universal project format, asset library, interchange
│   └── services/    # io, color mgmt, prefs, plugin host, licensing, update
├── apps/
│   ├── visual/      # binary: thin shell + visual domain core + platform/*
│   ├── video/       # binary: thin shell + video domain core + platform/*
│   └── audio/       # binary: thin shell + audio domain core + platform/*
└── design-tokens/   # shared source of truth for the design system
```

Each app binary is a thin shell wiring its domain core to the platform crates. The platform is the product's center of gravity; the apps are comparatively small.

---

## 6. How "Feel / Run / Operate the Same" Is Guaranteed

Direct mapping from your three words to the mechanism that delivers each:

- **Feel the same** ← `platform/design` + `platform/ui`. Identical components, spacing, motion, color, chrome. There is physically one button implementation; every app uses it.
- **Operate the same** ← `platform/input` + the shortcut/command system + the command palette + the shared gizmo and selection model. The same gesture, shortcut, and manipulator mean the same thing everywhere; muscle memory transfers instantly.
- **Run the same** ← `platform/gpu` + the performance doctrine as platform law. Every app hits the same frame budget and never-block rule, so all three feel equally instant — the DAW's UI stays buttery while its audio thread runs isolated underneath.
- **Work together (the bonus)** ← `platform/assets` universal format + cross-app embedding. Start a thing in one app, finish it in another, no export round-trip. This is the payoff no loosely-coupled suite can match.

---

## 7. Build Sequence — Don't Boil the Ocean

The single most important discipline, and the #1 failure mode to avoid:

**Extract the platform *from* the first app. Never build a speculative framework first.**

1. **Build the visual app** (per the Unified Canvas doc). As you build, the moment something is obviously reusable — the component kit, the GPU loop, the timeline engine, the node editor, the command/undo system — factor it down into a `platform/*` crate. The platform grows by extraction from real, working code.
2. **App #2 (video editor)** is built *on the now-real platform.* It inherits the shell, the look, the speed, the timeline, and the node graph for free. Its real new work is the codec pipeline and NLE semantics. This is dramatically faster than app #1 — that acceleration is the whole point of the platform.
3. **App #3 (DAW)** likewise. Its one big new subsystem is the real-time audio engine; everything around it is already built.
4. **The universal project format and asset library:** design the *shape* early (so app #1's files are forward-compatible and assets are shareable from day one), implement progressively. Don't try to finalize a schema that spans a synth patch and a sculpt on day one — design the container, fill it in over time.

**The trap:** spending two years building "the grand engine" with nothing shippable. Build app-first; let the platform crystallize out of it. You always have a working app, and the platform is real because it's already in use.

---

## 8. Honest Caveats

- **Don't over-abstract before the second use exists.** A platform designed before its second consumer is usually the *wrong* platform. Extract when you have two real uses, not before.
- **Keep the audio real-time core ruthlessly isolated.** A DAW's audio thread runs on a hard sub-millisecond deadline with no allocation and no locks. It must never share the GPU/UI thread or block on it. Process isolation (separate binary) is the safe default here — a strong reason the DAW stays its own binary even as it wears the shared shell.
- **One literal file format for everything is a mirage.** Realistic is a shared *container/project* format that references domain-specific documents, plus strong interchange — not a single schema describing both a reverb and a rigged character. Be pragmatic: shared container, domain payloads, great import/export.
- **Coupling release cycles is real.** One binary means an audio bug can't ship a fix without re-shipping the whole suite. Separate binaries let each app update independently — another point in favor of separate binaries on a shared platform.
- **Design the plugin host into the platform earlier than feels necessary.** If third parties (and you) extend through one shared extension API, every plugin can potentially work across the suite — a genuine ecosystem advantage that's painful to retrofit.

---

## TL;DR

Build **one platform**, three thin apps on top. The platform owns everything the user feels (look, moves, speed, document behavior, project fabric); each app owns only its domain core (geometry/raster, codecs/NLE, audio DSP). Ship them as **separate binaries that are bone-deep shared** — optionally fusing the two visual apps Resolve-style, keeping the DAW its own process for real-time safety. **Grow the platform by extracting it from the first app**, never by building a framework in a vacuum. Do this and "feel, run, and operate the same" isn't a goal you chase — it's a property the architecture gives you for free.
