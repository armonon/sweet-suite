//! Tool box — Select / Translate / Add{Cube,Sphere,Image} — and the per-tool
//! mouse handlers.
//!
//! Translate tool: if the cursor is near one of the selected object's gizmo axes,
//! a press locks the drag to that axis (single-DOF slide). Otherwise the press
//! falls back to a free drag along the camera-facing plane (the original Phase 1
//! lite behavior). Selection uses ray ↔ AABB; gizmo picking uses closest-point
//! between the cursor ray and each axis line.

use glam::Vec3;
use suite_doc::{Document, ObjectKind};
use suite_gpu::{GizmoAxis, GizmoOverlay, Renderer};

use crate::shell::ShellState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Select,
    Translate,
    Paint,
    RectSelect,
    EllipseSelect,
    Lasso,
    Gradient,
    MoveLayer,
    AddCube,
    AddSphere,
    AddImage,
    AddMesh,
    AddLathe,
    AddPipe,
    Sculpt,
    MagicWand,
    Eyedropper,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Translate => "Move",
            Self::Paint => "Paint",
            Self::RectSelect => "Rect Select",
            Self::EllipseSelect => "Ellipse Select",
            Self::Lasso => "Lasso",
            Self::Gradient => "Gradient",
            Self::MoveLayer => "Move Layer",
            Self::AddCube => "Add Cube",
            Self::AddSphere => "Add Sphere",
            Self::AddImage => "Add Image Plane",
            Self::AddLathe => "Add Lathe",
            Self::AddMesh => "Add Mesh",
            Self::AddPipe => "Add Pipe",
            Self::Sculpt => "Sculpt",
            Self::MagicWand => "Magic Wand",
            Self::Eyedropper => "Eyedropper",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::Select => "Click a primitive to select it. Backspace deletes the selection.",
            Self::Translate => "Drag a colored arrow to slide along one axis. Drag elsewhere to free-translate along the camera plane.",
            Self::Paint => "Drag on a Paint Canvas or directly on a 3D mesh to paint. Tune the brush in the inspector. Try the flat (ortho) view for a face-on artboard.",
            Self::RectSelect => "Drag to draw a selection rectangle on the canvas. Paint only affects pixels inside. ⌘A=all · ⌘D=deselect.",
            Self::EllipseSelect => "Drag to draw an elliptical selection (inscribed in the drag's bounding box). Gradient/Move respect the exact shape; Crop uses its bounding rect.",
            Self::Lasso => "Drag a freehand outline; it closes back to the start on release. Gradient/Move respect the exact shape; Crop uses its bounding rect.",
            Self::Gradient => "Drag on the canvas to fill the active layer with a gradient from the brush colour to transparent (or pick endpoints in the inspector). Linear or radial. Respects the active selection. G to activate.",
            Self::MoveLayer => "Drag on the canvas to move the active layer's pixels. With an active selection, only the selected pixels move (leaving a transparent hole). V to activate.",
            Self::AddCube => "Adds a unit cube where you click and selects it.",
            Self::AddSphere => "Adds a unit sphere where you click and selects it.",
            Self::AddImage => "Adds a textured image plane where you click and selects it.",
            Self::AddMesh => "Adds an editable mesh (a cube) where you click. Select it and press E to extrude its top face.",
            Self::AddLathe => "Adds a lathe/revolve mesh (vase profile) where you click. Shows as editable mesh.",
            Self::AddPipe => "Adds a path-extruded pipe (helix demo) where you click.",
            Self::Sculpt => "Drag to sculpt the selected mesh. Pick a brush in the inspector (Draw/Smooth/Flatten/Pinch). S to activate.",
            Self::MagicWand => "Click on the paint canvas to flood-fill that region with the brush color. Tune tolerance in the inspector. W to activate.",
            Self::Eyedropper => "Click the canvas to pick that color into the brush (Eye button).",
        }
    }
}

/// Rolling-average cursor stabilizer. Holds the last `window` positions; the smoothed
/// output is their centroid. A window of 1 means no smoothing (pass-through).
#[derive(Default)]
pub struct Stabilizer {
    history: std::collections::VecDeque<(f32, f32)>,
    pub window: usize,
}

impl Stabilizer {
    /// Push a new raw position and return the smoothed position.
    pub fn push(&mut self, pos: (f32, f32)) -> (f32, f32) {
        self.history.push_back(pos);
        while self.history.len() > self.window {
            self.history.pop_front();
        }
        let n = self.history.len() as f32;
        let sx = self.history.iter().map(|p| p.0).sum::<f32>() / n;
        let sy = self.history.iter().map(|p| p.1).sum::<f32>() / n;
        (sx, sy)
    }
    pub fn reset(&mut self) {
        self.history.clear();
    }
}

#[derive(Default)]
pub struct InputState {
    pub cursor: (f32, f32),
    pub left_pressed: bool,
    /// Set on cursor-move when the Translate tool is active, hovering near an axis.
    pub highlighted_axis: Option<GizmoAxis>,
    /// While dragging an axis, the locked axis + the original `t` (axis parameter)
    /// at press time and the original object translation.
    pub dragging_axis: Option<GizmoAxis>,
    pub drag_axis_anchor_t: f32,
    pub drag_object_start: Option<Vec3>,
    /// Free-plane drag fallback when no axis is locked.
    pub drag_plane_anchor: Option<Vec3>,
    /// Last painted UV on the active canvas, for stroke continuity between cursor moves.
    pub paint_last_uv: Option<[f32; 2]>,
    /// Last painted UV on a mesh (paint-on-3D path), keyed by ObjId.
    pub paint_mesh_last_uv: Option<(suite_doc::ObjId, [f32; 2])>,
    /// Cursor stabilizer for the Paint tool (rolling average of raw positions).
    pub stabilizer: Stabilizer,
    /// World-space snap target position — set while dragging near a snap point, cleared otherwise.
    pub snap_indicator: Option<Vec3>,
    /// Set by the Eyedropper on click (linear RGB); main.rs drains it into the brush colour.
    pub picked_color: Option<[f32; 3]>,
    /// RectSelect drag start UV `[u, v]` (0..1, origin top-left canvas corner).
    pub select_drag_start: Option<[f32; 2]>,
    /// The current active selection rectangle in UV space `[x0, y0, x1, y1]`. `None` = no
    /// selection (paint everywhere). Synced into `ShellState::selection_rect` each frame.
    /// Always the bounding box of the selection — even an Ellipse/Lasso (`select_extra`)
    /// keeps this in sync as its bounds, so the scissor/crop fast paths stay unchanged.
    pub select_rect: Option<[f32; 4]>,
    /// Tier 1: the selection's exact shape when it isn't a plain rectangle (Ellipse/Lasso).
    /// `None` means the selection (if any) is exactly `select_rect`. Synced into
    /// `ShellState::selection_extra` each frame, same as `select_rect`.
    pub select_extra: Option<suite_gpu::SelectionShape>,
    /// EllipseSelect drag start UV `[u, v]`, set on press (same shape as `select_drag_start`).
    pub ellipse_drag_start: Option<[f32; 2]>,
    /// Lasso: the freehand point path traced so far this drag, in UV space. Cleared on
    /// press, appended on every cursor move while dragging, rasterized into a `Polygon`
    /// selection on release.
    pub lasso_points: Vec<[f32; 2]>,
    /// Gradient tool drag: start UV `[u, v]` set on press; live endpoint follows the cursor.
    pub gradient_drag_start: Option<[f32; 2]>,
    /// Live gradient endpoints `[u0, v0, u1, v1]` in UV space while dragging — drawn as a
    /// guide line in the overlay. Cleared on release after the fill commits.
    pub gradient_preview: Option<[f32; 4]>,
    /// Move tool drag: start UV `[u, v]` set on press; live endpoint follows the cursor.
    /// Same commit-on-release shape as Gradient — the canvas doesn't change until release,
    /// only a guide line/handles draw during the drag.
    pub move_drag_start: Option<[f32; 2]>,
    /// Live move endpoints `[u0, v0, u1, v1]` in UV space while dragging — drawn as a guide
    /// line in the overlay. Cleared on release after the move commits.
    pub move_preview: Option<[f32; 4]>,
}

/// Krita-referenced symmetry painting: given a brush segment in UV space, return the
/// original plus its mirror images across the canvas centre (u=0.5 / v=0.5) for each
/// enabled axis. Enabling both axes yields 4-way radial symmetry.
fn symmetry_segments(
    from: [f32; 2],
    to: [f32; 2],
    mirror_x: bool,
    mirror_y: bool,
) -> Vec<([f32; 2], [f32; 2])> {
    let m = |p: [f32; 2], fx: bool, fy: bool| {
        [if fx { 1.0 - p[0] } else { p[0] }, if fy { 1.0 - p[1] } else { p[1] }]
    };
    let mut out = vec![(from, to)];
    if mirror_x {
        out.push((m(from, true, false), m(to, true, false)));
    }
    if mirror_y {
        out.push((m(from, false, true), m(to, false, true)));
    }
    if mirror_x && mirror_y {
        out.push((m(from, true, true), m(to, true, true)));
    }
    out
}

/// Full brush fan-out: symmetry images plus, when `wrap` is on, the 8 neighbour copies
/// offset by ±1 in UV (Krita-referenced wrap-around / seamless-texture mode). Segments
/// that land off-canvas are clipped by the GPU, so over-generating is cheap and correct.
fn paint_fanout(
    from: [f32; 2],
    to: [f32; 2],
    mirror_x: bool,
    mirror_y: bool,
    wrap: bool,
) -> Vec<([f32; 2], [f32; 2])> {
    let base = symmetry_segments(from, to, mirror_x, mirror_y);
    if !wrap {
        return base;
    }
    let mut out = Vec::with_capacity(base.len() * 9);
    for (a, b) in base {
        for ox in [-1.0_f32, 0.0, 1.0] {
            for oy in [-1.0_f32, 0.0, 1.0] {
                out.push(([a[0] + ox, a[1] + oy], [b[0] + ox, b[1] + oy]));
            }
        }
    }
    out
}

/// Where the cursor ray hits a paint canvas: the object + the canvas-local UV. Returns
/// the nearest hit if the ray crosses several canvases.
fn paint_uv_under_cursor(
    doc: &Document,
    renderer: &Renderer,
    canvas: (f32, f32, f32, f32),
    cursor: (f32, f32),
) -> Option<(suite_doc::ObjId, [f32; 2])> {
    let (origin, dir) = cursor_ray(renderer, canvas, cursor)?;
    let mut best: Option<(f32, suite_doc::ObjId, [f32; 2])> = None;
    for obj in doc.objects() {
        if !obj.kind.is_paintable() || !obj.visibility {
            continue;
        }
        let world = obj.world_matrix();
        // The canvas plane: local z=0 transformed to world.
        let normal = world.transform_vector3(Vec3::Z).normalize_or_zero();
        let point = world.transform_point3(Vec3::ZERO);
        let denom = normal.dot(dir);
        if denom.abs() < 1e-6 {
            continue;
        }
        let t = (point - origin).dot(normal) / denom;
        if t < 0.0 {
            continue;
        }
        let hit = origin + dir * t;
        let local = world.inverse().transform_point3(hit);
        // Quad spans local [-0.5, 0.5] in x/y. UV matches the textured-quad shader:
        // uv = (x + 0.5, 0.5 - y).
        if local.x < -0.5 || local.x > 0.5 || local.y < -0.5 || local.y > 0.5 {
            continue;
        }
        let uv = [local.x + 0.5, 0.5 - local.y];
        if best.map(|(bt, _, _)| t < bt).unwrap_or(true) {
            best = Some((t, obj.id, uv));
        }
    }
    best.map(|(_, id, uv)| (id, uv))
}

/// Möller–Trumbore ray-triangle intersection.
/// Returns `Some(t)` where t is the ray parameter (t > 0 means forward hit).
fn ray_triangle(origin: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    const EPS: f32 = 1e-7;
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h = dir.cross(e2);
    let a = e1.dot(h);
    if a.abs() < EPS {
        return None;
    }
    let f = 1.0 / a;
    let s = origin - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = f * dir.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * e2.dot(q);
    if t > EPS { Some(t) } else { None }
}

/// Find the nearest Mesh object under the cursor ray.
/// Returns `(obj_id, world_hit_point, world_face_normal)`.
fn mesh_hit_under_cursor(
    doc: &Document,
    renderer: &Renderer,
    canvas: (f32, f32, f32, f32),
    cursor: (f32, f32),
) -> Option<(suite_doc::ObjId, Vec3, Vec3)> {
    let (origin, dir) = cursor_ray(renderer, canvas, cursor)?;
    let mut best: Option<(f32, suite_doc::ObjId, Vec3, Vec3)> = None;
    for obj in doc.objects() {
        if obj.kind != ObjectKind::Mesh || !obj.visibility {
            continue;
        }
        let display = match obj.display_mesh() {
            Some(m) => m,
            None => continue,
        };
        let world = obj.world_matrix();
        for face in &display.faces {
            if face.indices.len() < 3 {
                continue;
            }
            let n_raw = display.face_normal(face);
            let wn = world.transform_vector3(Vec3::from(n_raw)).normalize_or_zero();
            let v0w = world.transform_point3(Vec3::from(display.vertex(face.indices[0])));
            for k in 1..face.indices.len() - 1 {
                let v1w = world.transform_point3(Vec3::from(display.vertex(face.indices[k])));
                let v2w = world.transform_point3(Vec3::from(display.vertex(face.indices[k + 1])));
                if let Some(t) = ray_triangle(origin, dir, v0w, v1w, v2w) {
                    if best.map(|(bt, _, _, _)| t < bt).unwrap_or(true) {
                        best = Some((t, obj.id, origin + dir * t, wn));
                    }
                }
            }
        }
    }
    best.map(|(_, id, hit, wn)| (id, hit, wn))
}

/// Triplanar (box) UV from a world-space point and a world-space face normal.
/// Projects the point onto the plane most aligned to the face normal.
fn triplanar_uv(world_hit: Vec3, face_normal: Vec3) -> [f32; 2] {
    let abs = face_normal.abs();
    if abs.x >= abs.y && abs.x >= abs.z {
        // YZ plane
        [world_hit.z * 0.5 + 0.5, world_hit.y * 0.5 + 0.5]
    } else if abs.y >= abs.x && abs.y >= abs.z {
        // XZ plane
        [world_hit.x * 0.5 + 0.5, world_hit.z * 0.5 + 0.5]
    } else {
        // XY plane
        [world_hit.x * 0.5 + 0.5, world_hit.y * 0.5 + 0.5]
    }
}

/// Collect snap candidate world positions from all objects except the excluded one.
/// Returns vertex positions + face/edge centers (limited to keep it cheap).
fn snap_candidates(doc: &Document, exclude: suite_doc::ObjId) -> Vec<Vec3> {
    let mut pts = Vec::new();
    for obj in doc.objects() {
        if obj.id == exclude || !obj.visibility {
            continue;
        }
        let world = obj.world_matrix();
        match obj.kind {
            ObjectKind::Mesh => {
                if let Some(m) = obj.mesh.as_ref() {
                    for &v in &m.vertices {
                        pts.push(world.transform_point3(Vec3::from(v)));
                    }
                    for face in &m.faces {
                        // Face centroid as a snap point.
                        let centroid: Vec3 = face
                            .indices
                            .iter()
                            .map(|&i| Vec3::from(m.vertex(i)))
                            .fold(Vec3::ZERO, |a, b| a + b)
                            / face.indices.len() as f32;
                        pts.push(world.transform_point3(centroid));
                    }
                }
            }
            // Cubes/spheres — snap to their origin.
            _ => {
                pts.push(world.transform_point3(Vec3::ZERO));
            }
        }
    }
    pts
}

/// Find the nearest snap point within `radius` of `pos`. Returns `Some(snap_pos)` when close enough.
fn nearest_snap(candidates: &[Vec3], pos: Vec3, radius: f32) -> Option<Vec3> {
    candidates
        .iter()
        .filter_map(|&p| {
            let d = (p - pos).length();
            if d < radius { Some((d, p)) } else { None }
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
        .map(|(_, p)| p)
}

/// Build the gizmo overlay for the renderer. Returns None when there's nothing
/// selected or the Translate tool isn't active.
pub fn gizmo_for_render(
    doc: &Document,
    renderer: &Renderer,
    shell: &ShellState,
    input: &InputState,
) -> Option<GizmoOverlay> {
    if shell.tool != Tool::Translate {
        return None;
    }
    let sel = doc.selection()?;
    let obj = doc.get(sel)?;
    let origin = obj.transform.translation;
    Some(GizmoOverlay {
        origin,
        world_scale: renderer.camera.gizmo_world_scale(origin),
        highlighted: input.dragging_axis.or(input.highlighted_axis),
    })
}

pub fn handle_cursor_moved(
    doc: &mut Document,
    renderer: &mut Renderer,
    shell: &ShellState,
    input: &mut InputState,
) {
    if !input.left_pressed {
        // Hover: when on the Translate tool with a selection, hit-test the gizmo
        // axes so the next click locks if we're near one.
        input.highlighted_axis = None;
        if shell.tool == Tool::Translate {
            if let Some(sel) = doc.selection() {
                if let Some(obj) = doc.get(sel) {
                    let canvas = shell.canvas_rect(renderer.size());
                    if let Some((origin, dir)) = cursor_ray(renderer, canvas, input.cursor) {
                        let world_scale =
                            renderer.camera.gizmo_world_scale(obj.transform.translation);
                        input.highlighted_axis = pick_gizmo_axis(
                            obj.transform.translation,
                            world_scale,
                            origin,
                            dir,
                            renderer.camera.gizmo_world_scale(obj.transform.translation) * 0.15,
                        );
                    }
                }
            }
        }
        return;
    }

    // Held drag — RectSelect: update the live selection rect from drag-start to cursor.
    if shell.tool == Tool::RectSelect && input.left_pressed {
        let canvas = shell.canvas_rect(renderer.size());
        let (l, t, r, b) = canvas;
        let cw = (r - l).max(1.0);
        let ch = (b - t).max(1.0);
        let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
        let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
        if let Some([u0, v0]) = input.select_drag_start {
            input.select_rect = Some([u.min(u0), v.min(v0), u.max(u0), v.max(v0)]);
        }
        return;
    }

    // Held drag — EllipseSelect: same bounding-box drag as RectSelect, but also records the
    // exact ellipse shape (Tier 1) so Gradient/Move can respect it, not just the bbox.
    if shell.tool == Tool::EllipseSelect && input.left_pressed {
        let canvas = shell.canvas_rect(renderer.size());
        let (l, t, r, b) = canvas;
        let cw = (r - l).max(1.0);
        let ch = (b - t).max(1.0);
        let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
        let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
        if let Some([u0, v0]) = input.ellipse_drag_start {
            let (x0, y0, x1, y1) = (u.min(u0), v.min(v0), u.max(u0), v.max(v0));
            input.select_rect = Some([x0, y0, x1, y1]);
            input.select_extra = Some(suite_gpu::SelectionShape::Ellipse {
                cx: (x0 + x1) * 0.5,
                cy: (y0 + y1) * 0.5,
                rx: (x1 - x0) * 0.5,
                ry: (y1 - y0) * 0.5,
            });
        }
        return;
    }

    // Held drag — Lasso: append the cursor's UV position to the traced point path. Also
    // sets `select_extra` live (not just on release) so the overlay can draw the growing
    // outline using the exact same Polygon-drawing path as the finalized selection.
    if shell.tool == Tool::Lasso && input.left_pressed {
        let canvas = shell.canvas_rect(renderer.size());
        let (l, t, r, b) = canvas;
        let cw = (r - l).max(1.0);
        let ch = (b - t).max(1.0);
        let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
        let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
        input.lasso_points.push([u, v]);
        if input.lasso_points.len() >= 2 {
            let shape = suite_gpu::SelectionShape::Polygon(input.lasso_points.clone());
            // Keep select_rect in sync as the shape's bounding box — the degenerate-size
            // check in handle_left_release, and the scissor/crop fast paths, both read it.
            input.select_rect = Some(suite_gpu::selection_shape_bounds(&shape));
            input.select_extra = Some(shape);
        }
        return;
    }

    // Held drag — Gradient: track the live endpoint for the overlay guide line.
    if shell.tool == Tool::Gradient && input.left_pressed {
        let (l, t, r, b) = shell.canvas_rect(renderer.size());
        let cw = (r - l).max(1.0);
        let ch = (b - t).max(1.0);
        let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
        let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
        if let Some([u0, v0]) = input.gradient_drag_start {
            input.gradient_preview = Some([u0, v0, u, v]);
        }
        return;
    }

    // Held drag — Move: track the live endpoint for the overlay guide line. Same
    // commit-on-release shape as Gradient — no canvas write happens until release.
    if shell.tool == Tool::MoveLayer && input.left_pressed {
        let (l, t, r, b) = shell.canvas_rect(renderer.size());
        let cw = (r - l).max(1.0);
        let ch = (b - t).max(1.0);
        let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
        let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
        if let Some([u0, v0]) = input.move_drag_start {
            input.move_preview = Some([u0, v0, u, v]);
        }
        return;
    }

    // Held drag — Paint: stamp from the last UV to the current one so fast strokes
    // stay continuous. Stabilizer smooths the cursor. Canvas path first, then mesh.
    if shell.tool == Tool::Paint {
        input.stabilizer.window = shell.brush_stabilize.max(1);
        let canvas = shell.canvas_rect(renderer.size());
        let smoothed = input.stabilizer.push(input.cursor);
        if let Some((_, uv)) = paint_uv_under_cursor(doc, renderer, canvas, smoothed) {
            let from = input.paint_last_uv.unwrap_or(uv);
            for (a, b) in paint_fanout(from, uv, shell.mirror_x, shell.mirror_y, shell.wrap_tiling) {
                renderer.paint_stamp(a, b, &shell.brush, shell.brush_pressure);
            }
            input.paint_last_uv = Some(uv);
            input.paint_mesh_last_uv = None;
        } else if let Some((id, hit, face_n)) = mesh_hit_under_cursor(doc, renderer, canvas, smoothed) {
            let uv = triplanar_uv(hit, face_n);
            let from = input.paint_mesh_last_uv
                .filter(|(last_id, _)| *last_id == id)
                .map(|(_, last_uv)| last_uv)
                .unwrap_or(uv);
            renderer.paint_on_mesh(id, from, uv, &shell.brush, shell.brush_pressure);
            input.paint_mesh_last_uv = Some((id, uv));
            input.paint_last_uv = None;
        } else {
            input.paint_last_uv = None;
            input.paint_mesh_last_uv = None;
        }
        return;
    }

    // Held drag — Sculpt.
    if shell.tool == Tool::Sculpt {
        let canvas = shell.canvas_rect(renderer.size());
        if let Some((hit, _face_n)) = mesh_hit_under_cursor(doc, renderer, canvas, input.cursor)
            .map(|(_, h, n)| (h, n))
        {
            doc.sculpt_stroke(hit, shell.sculpt_radius, shell.sculpt_strength, shell.sculpt_op);
        }
        return;
    }

    // Held drag — Translate.
    if shell.tool != Tool::Translate {
        return;
    }
    let Some(sel) = doc.selection() else { return };
    let canvas = shell.canvas_rect(renderer.size());
    let Some((origin, dir)) = cursor_ray(renderer, canvas, input.cursor) else {
        return;
    };
    let Some(start) = input.drag_object_start else {
        return;
    };

    // Collect snap candidates once per drag frame (only other visible objects).
    let candidates = snap_candidates(doc, sel);
    // Snap radius: 0.35 world units — feels tight enough to be intentional but loose
    // enough to engage while moving near surfaces.
    const SNAP_RADIUS: f32 = 0.35;

    let mut new_pos: Option<Vec3> = None;
    if let Some(axis) = input.dragging_axis {
        if let Some(t) = ray_axis_parameter(start, axis.unit(), origin, dir) {
            let delta_t = t - input.drag_axis_anchor_t;
            new_pos = Some(start + axis.unit() * delta_t);
        }
    } else if let Some(anchor) = input.drag_plane_anchor {
        let normal = (renderer.camera.eye() - anchor).normalize_or_zero();
        let denom = normal.dot(dir);
        if denom.abs() < 1e-5 {
            return;
        }
        let t = (anchor - origin).dot(normal) / denom;
        if t < 0.0 {
            return;
        }
        let hit = origin + dir * t;
        new_pos = Some(start + (hit - anchor));
    }

    if let Some(pos) = new_pos {
        if let Some(snap) = nearest_snap(&candidates, pos, SNAP_RADIUS) {
            input.snap_indicator = Some(snap);
            if let Some(obj) = doc.get_mut(sel) {
                obj.transform.translation = snap;
            }
        } else {
            input.snap_indicator = None;
            if let Some(obj) = doc.get_mut(sel) {
                obj.transform.translation = pos;
            }
        }
    }
}

pub fn handle_left_press(
    doc: &mut Document,
    renderer: &mut Renderer,
    shell: &ShellState,
    input: &mut InputState,
    canvas: (f32, f32, f32, f32),
) {
    input.left_pressed = true;
    let (l, t, r, b) = canvas;
    let (cx, cy) = input.cursor;
    if cx < l || cx > r || cy < t || cy > b {
        return;
    }
    match shell.tool {
        Tool::Select => {
            let Some((origin, dir)) = cursor_ray(renderer, canvas, input.cursor) else {
                return;
            };
            // Precise face-pick for meshes (so we know which face to focus), AABB for the
            // rest. Track the nearest hit across both.
            let mut best: Option<(f32, suite_doc::ObjId)> = None;
            for obj in doc.objects() {
                if !obj.visibility {
                    continue;
                }
                let hit = if let Some(mesh) = obj.mesh.as_ref() {
                    mesh.pick_face(origin, dir, obj.world_matrix())
                        .map(|(_, t)| t)
                } else {
                    suite_doc::ray_aabb_world(origin, dir, obj.world_matrix(), obj.local_aabb)
                };
                if let Some(hit_t) = hit {
                    if best.is_none() || hit_t < best.unwrap().0 {
                        best = Some((hit_t, obj.id));
                    }
                }
            }
            let picked = best.map(|(_, id)| id);
            doc.set_selection(picked);
            // If we landed on a mesh, also focus the face the ray hit.
            if picked.is_some() {
                doc.pick_selected_mesh_face(origin, dir);
            }
        }
        Tool::Translate => {
            if let Some(sel) = doc.selection() {
                let Some((origin, dir)) = cursor_ray(renderer, canvas, input.cursor) else {
                    return;
                };
                let anchor = doc
                    .get(sel)
                    .map(|o| o.transform.translation)
                    .unwrap_or(Vec3::ZERO);
                input.drag_object_start = Some(anchor);
                // If the cursor is on an axis, lock to it. Otherwise fall back to plane drag.
                if let Some(axis) = input.highlighted_axis {
                    if let Some(t) = ray_axis_parameter(anchor, axis.unit(), origin, dir) {
                        input.dragging_axis = Some(axis);
                        input.drag_axis_anchor_t = t;
                        input.drag_plane_anchor = None;
                        return;
                    }
                }
                input.dragging_axis = None;
                let normal = (renderer.camera.eye() - anchor).normalize_or_zero();
                let denom = normal.dot(dir);
                if denom.abs() > 1e-5 {
                    let t = (anchor - origin).dot(normal) / denom;
                    if t >= 0.0 {
                        input.drag_plane_anchor = Some(origin + dir * t);
                    }
                }
            }
        }
        Tool::RectSelect => {
            // Record the drag start in UV space.
            let (l, t, r, b) = canvas;
            let cw = (r - l).max(1.0);
            let ch = (b - t).max(1.0);
            let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
            let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
            input.select_drag_start = Some([u, v]);
            // A fresh drag replaces the old selection. Zero-size rect shows while dragging.
            input.select_rect = Some([u, v, u, v]);
            input.select_extra = None;
        }
        Tool::EllipseSelect => {
            let (l, t, r, b) = canvas;
            let cw = (r - l).max(1.0);
            let ch = (b - t).max(1.0);
            let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
            let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
            input.ellipse_drag_start = Some([u, v]);
            input.select_rect = Some([u, v, u, v]);
            input.select_extra = Some(suite_gpu::SelectionShape::Ellipse { cx: u, cy: v, rx: 0.0, ry: 0.0 });
        }
        Tool::Lasso => {
            let (l, t, r, b) = canvas;
            let cw = (r - l).max(1.0);
            let ch = (b - t).max(1.0);
            let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
            let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
            input.lasso_points.clear();
            input.lasso_points.push([u, v]);
            input.select_rect = Some([u, v, u, v]);
            input.select_extra = None; // needs >= 2 points before it's a real shape
        }
        Tool::Gradient => {
            // Record the gradient start point in UV; the drag sets the endpoint, release fills.
            let (l, t, r, b) = canvas;
            let cw = (r - l).max(1.0);
            let ch = (b - t).max(1.0);
            let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
            let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
            input.gradient_drag_start = Some([u, v]);
            input.gradient_preview = Some([u, v, u, v]);
        }
        Tool::MoveLayer => {
            // Record the move start point in UV; the drag previews the offset, release commits.
            let (l, t, r, b) = canvas;
            let cw = (r - l).max(1.0);
            let ch = (b - t).max(1.0);
            let u = ((input.cursor.0 - l) / cw).clamp(0.0, 1.0);
            let v = ((input.cursor.1 - t) / ch).clamp(0.0, 1.0);
            input.move_drag_start = Some([u, v]);
            input.move_preview = Some([u, v, u, v]);
        }
        Tool::Paint => {
            // Stamp a single dab where the press landed; the drag continues the stroke.
            input.stabilizer.reset();
            input.stabilizer.window = shell.brush_stabilize.max(1);
            let smoothed = input.stabilizer.push(input.cursor);
            if let Some((_, uv)) = paint_uv_under_cursor(doc, renderer, canvas, smoothed) {
                for (a, b) in paint_fanout(uv, uv, shell.mirror_x, shell.mirror_y, shell.wrap_tiling) {
                    renderer.paint_stamp(a, b, &shell.brush, shell.brush_pressure);
                }
                input.paint_last_uv = Some(uv);
                input.paint_mesh_last_uv = None;
            } else if let Some((id, hit, face_n)) = mesh_hit_under_cursor(doc, renderer, canvas, smoothed) {
                let uv = triplanar_uv(hit, face_n);
                renderer.paint_on_mesh(id, uv, uv, &shell.brush, shell.brush_pressure);
                input.paint_mesh_last_uv = Some((id, uv));
                input.paint_last_uv = None;
            }
        }
        Tool::AddCube => {
            let id = doc.add(
                ObjectKind::Cube,
                world_at_cursor_or_origin(renderer, canvas, input.cursor),
            );
            doc.set_selection(Some(id));
        }
        Tool::AddSphere => {
            let id = doc.add(
                ObjectKind::Sphere,
                world_at_cursor_or_origin(renderer, canvas, input.cursor),
            );
            doc.set_selection(Some(id));
        }
        Tool::AddImage => {
            let id = doc.add(
                ObjectKind::ImagePlane,
                world_at_cursor_or_origin(renderer, canvas, input.cursor),
            );
            doc.set_selection(Some(id));
        }
        Tool::AddMesh => {
            let id = doc.add(
                ObjectKind::Mesh,
                world_at_cursor_or_origin(renderer, canvas, input.cursor),
            );
            doc.set_selection(Some(id));
        }
        Tool::AddLathe => {
            let profile: &[[f32; 2]] = &[
                [0.0, -1.0],
                [0.6, -0.8],
                [0.7, -0.5],
                [0.5, 0.0],
                [0.7, 0.5],
                [0.4, 0.9],
                [0.15, 1.0],
                [0.0, 1.0],
            ];
            let id = doc.add_lathe(profile, 32, world_at_cursor_or_origin(renderer, canvas, input.cursor));
            doc.set_selection(Some(id));
        }
        Tool::Sculpt => {
            // Dab on press too so a click without drag still affects the mesh.
            if let Some((hit, _face_n)) = mesh_hit_under_cursor(doc, renderer, canvas, input.cursor)
                .map(|(_, h, n)| (h, n))
            {
                doc.sculpt_stroke(hit, shell.sculpt_radius, shell.sculpt_strength, shell.sculpt_op);
            }
        }
        Tool::MagicWand => {
            // Flood-fill the paint canvas region at the click point with the brush color.
            if let Some((_, uv)) = paint_uv_under_cursor(doc, renderer, canvas, input.cursor) {
                let fill = shell.brush.color; // [R, G, B, A] linear
                renderer.paint_magic_wand_fill(uv, shell.magic_wand_tolerance, fill);
            }
        }
        Tool::Eyedropper => {
            // Pick the canvas colour at the click; main.rs writes it into the brush.
            if let Some((_, uv)) = paint_uv_under_cursor(doc, renderer, canvas, input.cursor) {
                let picked = renderer.pick_paint_color(uv);
                input.picked_color = Some([picked[0], picked[1], picked[2]]);
            }
        }
        Tool::AddPipe => {
            let path: Vec<glam::Vec3> = (0..=32)
                .map(|i| {
                    let t = i as f32 / 32.0;
                    let angle = t * std::f32::consts::TAU * 2.0;
                    glam::Vec3::new(angle.cos() * 1.5, t * 2.0 - 1.0, angle.sin() * 1.5)
                })
                .collect();
            let shape: &[[f32; 2]] = &[
                [-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1],
            ];
            let pos = world_at_cursor_or_origin(renderer, canvas, input.cursor);
            let id = doc.add_pipe(&path, shape, pos);
            doc.set_selection(Some(id));
        }
    }
}

pub fn handle_left_release(input: &mut InputState) {
    input.left_pressed = false;
    input.dragging_axis = None;
    input.drag_axis_anchor_t = 0.0;
    input.drag_object_start = None;
    input.drag_plane_anchor = None;
    input.paint_last_uv = None;
    input.paint_mesh_last_uv = None;
    input.snap_indicator = None;
    input.stabilizer.reset();
    // RectSelect/EllipseSelect: clear the drag anchors — the finalized shape stays in
    // select_rect/select_extra.
    input.select_drag_start = None;
    input.ellipse_drag_start = None;
    // Lasso: fewer than 3 points can't close into a real polygon — cancel it. A valid
    // trace is already stored in select_extra (set live during the drag).
    if input.lasso_points.len() < 3 {
        if input.select_extra.is_some() {
            input.select_rect = None;
            input.select_extra = None;
        }
    }
    input.lasso_points.clear();
    // Degenerate selections (less than 2 texels wide or tall) are cancelled.
    if let Some([x0, y0, x1, y1]) = input.select_rect {
        if (x1 - x0) < 0.001 || (y1 - y0) < 0.001 {
            input.select_rect = None;
            input.select_extra = None;
        }
    }
}

// ----- Geometry helpers -----------------------------------------------------------

/// Closest-point parameter `t_axis` between the ray (`ray_origin`, `ray_dir`) and the
/// axis line through `origin` along `axis_unit`. Returns `None` if the lines are
/// parallel (or the result would be behind the camera along the ray).
fn ray_axis_parameter(
    origin: Vec3,
    axis_unit: Vec3,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> Option<f32> {
    let u_a = ray_dir;
    let u_b = axis_unit;
    let w0 = ray_origin - origin;
    let a = u_a.dot(u_a);
    let b = u_a.dot(u_b);
    let c = u_b.dot(u_b);
    let d = u_a.dot(w0);
    let e = u_b.dot(w0);
    let denom = a * c - b * b;
    if denom.abs() < 1e-6 {
        return None;
    }
    let t_a = (b * e - c * d) / denom;
    let t_b = (a * e - b * d) / denom;
    if t_a < 0.0 {
        return None;
    }
    Some(t_b)
}

/// Pick the axis (X/Y/Z) closest to the cursor ray, within `tolerance` world units of
/// approach. The axis has a finite length [0, world_scale], so the closest-approach
/// point must land inside that range.
fn pick_gizmo_axis(
    origin: Vec3,
    world_scale: f32,
    ray_origin: Vec3,
    ray_dir: Vec3,
    tolerance: f32,
) -> Option<GizmoAxis> {
    let mut best: Option<(f32, GizmoAxis)> = None;
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let Some(t_b) = ray_axis_parameter(origin, axis.unit(), ray_origin, ray_dir) else {
            continue;
        };
        if t_b < 0.0 || t_b > world_scale {
            continue;
        }
        // Distance between closest points on the two lines.
        let closest_axis = origin + axis.unit() * t_b;
        // Project that point back onto the ray to get t_a.
        let t_a = (closest_axis - ray_origin).dot(ray_dir);
        if t_a < 0.0 {
            continue;
        }
        let closest_ray = ray_origin + ray_dir * t_a;
        let dist = (closest_axis - closest_ray).length();
        if dist > tolerance {
            continue;
        }
        if best.map(|(d, _)| dist < d).unwrap_or(true) {
            best = Some((dist, axis));
        }
    }
    best.map(|(_, a)| a)
}

fn cursor_ray(
    renderer: &Renderer,
    canvas: (f32, f32, f32, f32),
    cursor: (f32, f32),
) -> Option<(Vec3, Vec3)> {
    let (l, t, r, b) = canvas;
    let w = (r - l).max(1.0);
    let h = (b - t).max(1.0);
    let nx = ((cursor.0 - l) / w).clamp(0.0, 1.0);
    let ny = ((cursor.1 - t) / h).clamp(0.0, 1.0);
    let canvas_aspect = w / h;
    Some(renderer.camera.ray_from_cursor(nx, ny, canvas_aspect))
}

fn world_at_cursor_or_origin(
    renderer: &Renderer,
    canvas: (f32, f32, f32, f32),
    cursor: (f32, f32),
) -> Vec3 {
    let Some((origin, dir)) = cursor_ray(renderer, canvas, cursor) else {
        return Vec3::ZERO;
    };
    if dir.y.abs() < 1e-5 {
        return renderer.camera.target;
    }
    let t = -origin.y / dir.y;
    if t < 0.0 {
        return renderer.camera.target;
    }
    origin + dir * t
}

#[cfg(test)]
mod tests {
    use super::symmetry_segments;

    fn approx(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
    }

    #[test]
    fn symmetry_none_is_just_the_original() {
        let segs = symmetry_segments([0.2, 0.3], [0.4, 0.6], false, false);
        assert_eq!(segs.len(), 1);
        assert!(approx(segs[0].0, [0.2, 0.3]) && approx(segs[0].1, [0.4, 0.6]));
    }

    #[test]
    fn symmetry_x_mirrors_horizontally_about_center() {
        let segs = symmetry_segments([0.2, 0.3], [0.4, 0.6], true, false);
        assert_eq!(segs.len(), 2);
        // Mirror of u across 0.5 → 1-u; v unchanged.
        assert!(approx(segs[1].0, [0.8, 0.3]) && approx(segs[1].1, [0.6, 0.6]));
    }

    #[test]
    fn symmetry_both_axes_is_four_way() {
        let segs = symmetry_segments([0.25, 0.25], [0.25, 0.25], true, true);
        assert_eq!(segs.len(), 4, "original + X + Y + XY");
        // The 4-way set should include the diagonal-opposite corner (0.75, 0.75).
        assert!(segs.iter().any(|(a, _)| approx(*a, [0.75, 0.75])));
    }
}
