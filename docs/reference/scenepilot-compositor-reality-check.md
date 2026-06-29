# ScenePilot + COMPOSITOR — Merge Plan Reality-Check

*Engineering reality-check against the actual repo, June 17 2026. Read alongside the 5-step merge plan.*

## Verdict

The merge thesis is right: each product fixes the other's fatal flaw, and "AI drafts, you refine on a real timeline, one click renders" is a real wedge that CapCut doesn't own. The branding cleanup (ScenePilot = product, COMPOSITOR = engine inside it) is free and worth taking.

But the plan's risk ordering is inverted. It names the plan contract as "the keystone… do it before anything" and files the render work under "honest risk: real integration work." The code says the opposite. The contract is the *cheap, low-risk* part — two schemas already exist, they just disagree. The render path is the *expensive, unproven, zero-today* part: **COMPOSITOR cannot render a single frame, by design.** Step 2 is not integration. It is "build (or borrow) a media engine," and everything user-facing in the merged product depends on it.

So: define a deliberately minimal contract fast (a day, not a sprint), then immediately go prove pixels come out. Let the renderer's real constraints drive the contract's final shape rather than over-designing the schema in the abstract.

## What each half actually is (grounded in the code)

**ScenePilot** (`sites/auto-cut`) is browser-local analysis. `app.js → buildAndRenderPlan()` produces a rich plan object (`version: "0.1.0-standalone-rc"`) containing `source`, `creativeKit{intent, assets}`, `controls`, `analysis{bpm…}`, `beats[]`, `silences[]`, `cuts[]`, `segments[{start,end,keep,duration}]`, and `arrangement[{lane, role, asset, start, end, placement}]` with lanes for logo / voiceover / video-insert / effects. It exports `cutPlan.json`, Premiere XMEML, and a marker CSV.

The catch is in `tools/render-from-plan.mjs`: the only thing that turns a plan into an MP4 filters `plan.segments` (keep ≠ false, end > start) against a **single** `--source` file and concatenates with FFmpeg. **It ignores `arrangement` entirely** — the overlays, voiceover, inserts, and beat-synced effects ScenePilot so carefully plans are dropped at render time. That gap, plus "you must run a node command with a `--source` path the browser never knew," *is* the export blocker. ScenePilot's own README lists "wire the FFmpeg render helper into the UI as one-click rendered export" as the #1 next task.

**COMPOSITOR** (`apps/compositor-native`) is the serious half: a C++/Qt native foundation with a real model (`src/core/ProjectModel.hpp`: `Project → Composition → Layer` with `sourceAssetId`, `startSeconds`, `durationSeconds`, `transform`, `keyframes`, `masks`, `blendMode`, `opacity`), SQLite persistence with crash-safe save and `.bak`/`.pre-restore.bak` recovery, an undo/redo command model, extensive timeline-view scaffolding (viewport, ruler, snapping, bookmarks, marker lane), and two interchange exporters (`compositor.review-interchange.v0` JSON and an OTIO draft).

And the load-bearing fact: **every media surface is explicitly inert.** The structs carry `decodesMedia = false`, `rendersFrames = false`, `outputsAudio = false`, `writesExternalFiles = false` as invariants. The interchange is `metadataOnly: true`; the importer (`importProjectReviewJson`) *fail-closes* if an artifact claims `rendersMedia: true` (see the `unsafe-renders-review.json` negative-test fixture). The CLI (`src/cli/main.cpp`) exposes save/inspect/validate/import/keyframe/queue/backup commands; its only `--export-*` verbs emit interchange JSON, and there is **no render or media-export command anywhere** (no `avcodec`/`libav`/encode path exists in `src/` at all). The playback strip models play/pause/scrub as transport metadata only. COMPOSITOR is a beautifully safe timeline *that has never touched a pixel of video.* Its README names the intended engine (FFmpeg/libav + a custom render graph, GPU later) as direction, not built.

## Plan vs. reality, step by step

**Step 1 — Plan contract.** *Status: undefined, but easy.* Three schemas are in play and none is "the contract": ScenePilot's flat `segments` + loose `arrangement`; COMPOSITOR's `review-interchange.v0` full layer model; and the old web sketch's `sample-project.json`. The plan is correct that a single contract is needed and that everything hangs off it structurally. It is wrong that this is the hard part — it's mostly a naming-and-mapping exercise between models that already exist. Real risk here is *over*-design: speccing a maximal schema before the renderer exists.

**Step 2 — Headless render path.** *Status: this is the whole project.* "COMPOSITOR takes a plan and renders an MP4 with no UI and no FFmpeg script" assumes a render engine that does not exist in any form. There is no decode, no compositing, no encode, and hard-coded invariants asserting as much. This is the step that retires the blocker, the step the differentiation depends on, and the step with all the risk. It deserves to be attacked first and smallest, not treated as a downstream integration.

**Step 3 — Timeline view + manual override.** *Status: half-built, blocked on Step 2.* COMPOSITOR has more timeline *view* machinery than most v1 editors ship — but no rendered preview, so "see the AI's draft, trim a clip, re-render" can't close its loop until there's a renderer to preview and re-run. ScenePilot's `arrangement` isn't editable at all today. The good news: the undo/redo command model and inspector edits are already real, so the *refine* half is closer than the *render* half.

**Step 4 — One installable app (Mac first).** *Status: tractable.* The Qt shell builds (Qt 6.11.1 via Homebrew per `DECISIONS.md`), and Mac-first is the right call — it keeps you on the one platform where the audio path and `ffprobe` already work. Bundling/signing/notarization is real work but well-trodden, and it's downstream of having something worth bundling.

**Step 5 — Wedge demo + launch.** *Status: downstream of all of the above*, and note `DECISIONS.md` flags external/public launches as an "L4 gate" requiring explicit human sign-off — keep that in the sequence.

## The dependency the plan inverts

The plan's logic is "contract → render → timeline → app → launch," justified by the contract being the keystone. Structurally true. But *risk-wise* the order should be driven by what's unproven, and the single unproven thing is **can the merged tool turn a plan into a watchable file at all.** Until that's answered, the contract is speculative — you'd be designing the exact shape of a message for a consumer that can't yet read it. Design the contract minimally, then let the first real render tell you what the contract actually needs (frame vs. seconds, how an "effect" is expressed, how audio is addressed). Lock it *after* the first pixels, not before.

## Step 2 has two honest forks

The plan says "no FFmpeg." That word is doing a lot of work, and it forces a real fork:

**Fork A — Native render engine (literal reading of the plan).** Give COMPOSITOR libav decode + a compositing/render graph + an encoder, so it renders the timeline itself. This is the durable, differentiated, GPU-future version. It is also precisely the from-scratch-NLE-engine work the merge thesis was trying to *avoid* — months of effort, landing exactly where Resolve and Premiere are strongest, before you can demo anything.

**Fork B — Contract-driven FFmpeg executor (pragmatic).** The merged app reads the contract and drives FFmpeg as an *embedded library/process* under the hood: no user-facing command line, one button, deterministic output. This contradicts "no FFmpeg" only if you read it as "no FFmpeg anywhere" rather than "no FFmpeg *script the user has to run*." It retires the actual blocker (no command line, honors the full arrangement the current helper drops) in days, not months. It's also what most shipping editors do, and COMPOSITOR's own README already names FFmpeg/libav as the engine.

**Recommendation: Fork B now, evolve toward A later, and keep the contract as the seam between them.** Ship the executor that turns the contract into MP4s and unblocks the demo; treat the native render graph as a later upgrade behind the same contract, justified only where it differentiates (live GPU preview, real-time effects). The whole point of the contract is that the executor is swappable. "No FFmpeg" as a v1 purity goal is the thing most likely to sink the timeline — name it explicitly and let it go for now.

## Revised build sequence (smallest provable slice first)

1. **Slice 0 — prove the seam end-to-end.** One real source clip + a *hand-written* minimal contract file → merged executor → finished MP4, one command, deterministic, that honors at least: two kept segments, one overlay (logo), one audio track, and one beat-synced effect at a cut point. This is the keystone *proof* — it validates contract + render + the arrangement-that-the-old-helper-drops, all at once, before polishing either side. If this is hard, everything downstream is harder; better to learn it in week one.
2. **Minimal contract, locked against Slice 0.** Promote the hand-written file into a versioned schema once a real render has shaped it. Keep it tiny: clips with in/out, beat-locked cut points, exactly one effect type, one audio track. Resist adding masks/trackers/multi-comp until v2.
3. **ScenePilot emits the contract.** Replace its bespoke `cutPlan.json` export with the contract; route its `arrangement` (not just `segments`) into it so overlays/VO/effects finally survive to render.
4. **COMPOSITOR consumes the contract on the timeline.** Read-only first (draft appears on the existing timeline view), then editable (the undo/redo model already exists), then re-emit + re-render. This is where "AI drafts, human refines" becomes real — and it's mostly wiring existing pieces to the renderer from step 1.
5. **Bundle the Mac app.** One window: drop assets → see plan → tweak → export. No command line surfaced anywhere.
6. **Wedge demo + launch** (one creator niche, one before/after), gated on the L4 human sign-off `DECISIONS.md` already requires.

## What to reuse, retire, and throw away

Reuse COMPOSITOR's model, SQLite/crash-recovery, timeline view, and undo/redo; reuse ScenePilot's analysis and arrangement logic — these are the genuinely valuable assets. Retire the user-facing `render-from-plan.mjs` *command* (keep FFmpeg itself as the embedded executor). Collapse the three competing JSON shapes (`segments`+`arrangement`, `review-interchange.v0`, `sample-project.json`) into the single contract; let the OTIO draft stay a side-export, not the spine. Drop the disposable web sketches (`sites/compositor`, and `sites/context-compositor-mvp` once its context-compositing idea is captured) per the native-first decision already on record.

## Illustrative minimal contract (not yet locked)

Shown only to demonstrate how small Step 1 can be — the real shape should be confirmed by Slice 0's first render.

```json
{
  "contract": "scenepilot.editplan.v0",
  "fps": 30,
  "resolution": [1920, 1080],
  "sources": [
    { "id": "main", "path": "media/take01.mp4", "hasAudio": true }
  ],
  "clips": [
    { "source": "main", "in": 0.0, "out": 1.2, "track": "video" },
    { "source": "main", "in": 2.0, "out": 3.4, "track": "video" }
  ],
  "cutPoints": [ { "t": 1.2, "beatLocked": true } ],
  "overlays": [
    { "source": "logo", "in": 0.0, "out": 6.0, "x": 0.82, "y": 0.85, "scale": 0.2 }
  ],
  "audioTracks": [
    { "source": "music", "in": 0.0, "gain": -6.0 }
  ],
  "effects": [
    { "type": "punch-zoom", "at": 1.2, "amount": 1.08, "beatLocked": true }
  ]
}
```

One effect type, one overlay concept, one audio track. ScenePilot's `arrangement` lanes map onto `overlays`/`audioTracks`/`effects`; COMPOSITOR's `Layer.transform` + `keyframes` already express `punch-zoom`, and its `Layer{sourceAssetId,startSeconds,durationSeconds}` already expresses `clips`. The mapping is short on both sides — which is exactly why the contract is the easy keystone, not the risky one.

## Open decisions to settle before Slice 0

Adopt the ScenePilot-product / COMPOSITOR-engine naming (low-cost, recommended). Pick the Step 2 fork explicitly (recommend B). Decide the contract's time base — seconds is friendlier to ScenePilot, frames is friendlier to deterministic render; pick frames-with-fps and convert once at the edge. Confirm Mac-first and a single audio track for v1. And confirm the determinism bar (same contract in → byte-stable or just frame-stable MP4 out), since "deterministic" is claimed in the plan and should be tested, not assumed.
