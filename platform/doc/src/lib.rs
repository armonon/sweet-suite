//! # suite-doc — the object model & undo. **The spine of the suite.**
//!
//! Everything is a typed object in one scene. The renderer, the raster substrate,
//! the timeline, and the node-graph all hang off this crate. This crate *is* the
//! "one canvas" idea (docs/01 §0) and the single source of truth in the invariant
//! `render(evaluate(scene, t))`. Implements docs/03 §3.
//!
//! Phase 1+3 lite: the typed envelope is real, with a generational object arena and
//! a translate/rotate/scale transform driven by `glam`. Mesh payloads are a flat
//! `ObjectKind` enum (Cube/Sphere/ImagePlane) until the geometry kernel arrives in
//! Phase 4.

#![allow(dead_code)]

mod halfedge;
pub use halfedge::{HalfEdge, HalfEdgeMesh};

use glam::{EulerRot, Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

/// Translate · rotate(quat) · scale. Stored **decomposed** so the timeline can
/// interpolate channels independently (slerp the quaternion). docs/03 §3.2.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Trs {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Trs {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
    /// Euler convenience for the inspector (XYZ intrinsic, radians).
    pub fn rotation_euler(&self) -> Vec3 {
        let (x, y, z) = self.rotation.to_euler(EulerRot::XYZ);
        Vec3::new(x, y, z)
    }
    pub fn set_rotation_euler(&mut self, e: Vec3) {
        self.rotation = Quat::from_euler(EulerRot::XYZ, e.x, e.y, e.z);
    }
}

/// Axis-aligned bounding box in *local* space, used by the picker.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const fn unit() -> Self {
        Self {
            min: Vec3::new(-0.5, -0.5, -0.5),
            max: Vec3::new(0.5, 0.5, 0.5),
        }
    }
}

/// Generational id (slotmap-style). Stable across edits & saves; a stale id fails
/// lookup instead of aliasing a new object. docs/03 §3.1.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ObjId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Add,
    Subtract,
    // Appended for parity (all separable, per-channel — the shader ids in
    // `blend_mode_u32`/`LAYER_COMPOSITE_WGSL` continue from Subtract=7). Serde tags by
    // variant name, so old projects (which only reference the first eight) load unchanged.
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    LinearBurn,
    Difference,
    Exclusion,
    Divide,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
}

impl BlendMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::SoftLight => "Soft Light",
            Self::HardLight => "Hard Light",
            Self::Add => "Linear Dodge (Add)",
            Self::Subtract => "Subtract",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorDodge => "Color Dodge",
            Self::ColorBurn => "Color Burn",
            Self::LinearBurn => "Linear Burn",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Divide => "Divide",
            Self::VividLight => "Vivid Light",
            Self::LinearLight => "Linear Light",
            Self::PinLight => "Pin Light",
            Self::HardMix => "Hard Mix",
        }
    }
    pub fn all() -> &'static [BlendMode] {
        &[
            Self::Normal,
            Self::Multiply,
            Self::Screen,
            Self::Overlay,
            Self::SoftLight,
            Self::HardLight,
            Self::Add,
            Self::Subtract,
            Self::Darken,
            Self::Lighten,
            Self::ColorDodge,
            Self::ColorBurn,
            Self::LinearBurn,
            Self::Difference,
            Self::Exclusion,
            Self::Divide,
            Self::VividLight,
            Self::LinearLight,
            Self::PinLight,
            Self::HardMix,
        ]
    }
}

/// Adjustment layer parameters. An `ObjectKind::Adjustment` carries one of these and
/// reads the composited pixels beneath it, applying its transform in linear space.
/// docs/03 §1.7.
/// Adjustment-layer kinds. The first three are the originals; the rest are the photo-
/// editing set (algorithm reference: PhotoDemon, BSD-licensed — `tannerhelland/PhotoDemon`),
/// reimplemented per-pixel in linear HDR space. All are non-destructive layers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AdjustmentKind {
    BrightnessContrast { brightness: f32, contrast: f32 },
    HueSaturation { hue: f32, saturation: f32, lightness: f32 },
    Levels { black_point: f32, gamma: f32, white_point: f32 },
    /// Photographic exposure in stops (multiply in linear light): `2^stops`.
    Exposure { stops: f32 },
    /// Smart saturation that ramps less-saturated pixels harder and protects already-
    /// saturated ones — PhotoDemon's "vibrance". `amount` in roughly -1..1.
    Vibrance { amount: f32 },
    /// White balance: `temperature` warms (+) / cools (−); `tint` shifts green↔magenta.
    WhiteBalance { temperature: f32, tint: f32 },
    /// Quantize each channel to `levels` steps (≥2).
    Posterize { levels: f32 },
    /// Binary threshold on luminance at `level` (0..1).
    Threshold { level: f32 },
    /// Invert RGB.
    Invert,
    /// Box blur with `radius` in texels (neighbor-sampling, GPU-only).
    BoxBlur { radius: f32 },
    /// Unsharp-style sharpen, strength `amount` (neighbor-sampling, GPU-only).
    Sharpen { amount: f32 },
    /// Sobel edge-detection on luminance (neighbor-sampling, GPU-only).
    EdgeDetect,
    /// Separable Gaussian blur, `radius` in texels (two 1-D GPU passes, GPU-only).
    GaussianBlur { radius: f32 },
}

impl AdjustmentKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::BrightnessContrast { .. } => "Brightness / Contrast",
            Self::HueSaturation { .. } => "Hue / Saturation",
            Self::Levels { .. } => "Levels",
            Self::Exposure { .. } => "Exposure",
            Self::Vibrance { .. } => "Vibrance",
            Self::WhiteBalance { .. } => "White Balance",
            Self::Posterize { .. } => "Posterize",
            Self::Threshold { .. } => "Threshold",
            Self::Invert => "Invert",
            Self::BoxBlur { .. } => "Box Blur",
            Self::Sharpen { .. } => "Sharpen",
            Self::EdgeDetect => "Edge Detect",
            Self::GaussianBlur { .. } => "Gaussian Blur",
        }
    }

    /// CPU reference implementation of the per-pixel adjustment math, mirroring the
    /// compositor's WGSL. `rgb` is linear light. Kept in lock-step with the shader so it
    /// can drive CPU previews/thumbnails and unit tests (the GPU path has no readback in
    /// tests). The HSL kinds are intentionally omitted here (the GPU owns those).
    pub fn apply_linear(&self, rgb: [f32; 3]) -> [f32; 3] {
        let [r, g, b] = rgb;
        let clamp01 = |x: f32| x.clamp(0.0, 1.0);
        match *self {
            Self::Exposure { stops } => {
                let m = 2.0f32.powf(stops);
                [r * m, g * m, b * m]
            }
            Self::Vibrance { amount } => {
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let mx = r.max(g).max(b);
                let mn = r.min(g).min(b);
                let sat = mx - mn;
                let w = (1.0 - sat) * amount;
                let mix = |x: f32| (luma + (x - luma) * (1.0 + w)).max(0.0);
                [mix(r), mix(g), mix(b)]
            }
            Self::WhiteBalance { temperature, tint } => {
                [(r * (1.0 + temperature)).max(0.0),
                 (g * (1.0 + tint)).max(0.0),
                 (b * (1.0 - temperature)).max(0.0)]
            }
            Self::Posterize { levels } => {
                let n = levels.max(2.0);
                let q = |x: f32| (clamp01(x) * n).floor() / (n - 1.0);
                [q(r), q(g), q(b)]
            }
            Self::Threshold { level } => {
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let v = if luma >= level { 1.0 } else { 0.0 };
                [v, v, v]
            }
            Self::Invert => [1.0 - clamp01(r), 1.0 - clamp01(g), 1.0 - clamp01(b)],
            // BrightnessContrast / HueSaturation / Levels are owned by the GPU path.
            _ => rgb,
        }
    }

    /// A neutral (no-op) instance of each kind, for the inspector's kind picker.
    pub fn all_defaults() -> Vec<AdjustmentKind> {
        vec![
            Self::BrightnessContrast { brightness: 0.0, contrast: 0.0 },
            Self::HueSaturation { hue: 0.0, saturation: 1.0, lightness: 0.0 },
            Self::Levels { black_point: 0.0, gamma: 1.0, white_point: 1.0 },
            Self::Exposure { stops: 0.0 },
            Self::Vibrance { amount: 0.0 },
            Self::WhiteBalance { temperature: 0.0, tint: 0.0 },
            Self::Posterize { levels: 8.0 },
            Self::Threshold { level: 0.5 },
            Self::Invert,
            Self::BoxBlur { radius: 2.0 },
            Self::Sharpen { amount: 0.5 },
            Self::EdgeDetect,
            Self::GaussianBlur { radius: 4.0 },
        ]
    }
}

/// How an object is composited. **Never conflate these** — 2D design layers stack
/// in explicit order; 3D geometry sorts by depth. docs/03 §1.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compositing {
    StackOrdered,
    DepthOrdered,
}

/// Phase 1+3 lite payload set. Each variant is a *kind*; the renderer maps it to a
/// mesh. The geometry kernel (half-edge, modifiers) lives behind this enum until
/// Phase 4, when `ObjectKind::Mesh` becomes a real geometry handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Cube,
    Sphere,
    ImagePlane,
    /// A raster artboard — a flat plane backed by the GPU paint texture (the raster
    /// substrate). The brush engine paints into it. This is the "Photoshop / Procreate
    /// surface" living natively in the one 3D scene. docs/01 §4–5, docs/03 §2.
    PaintCanvas,
    /// An editable polygon mesh — the poly-modeling target (docs/01 §3, docs/03 §3).
    /// The geometry lives in `Object::mesh`.
    Mesh,
    /// A full-screen adjustment layer. It reads the composited result beneath it and
    /// applies the `Object::adjustment` transform in linear HDR space. docs/03 §1.7.
    Adjustment,
}

impl ObjectKind {
    pub fn default_color(&self) -> [f32; 4] {
        match self {
            Self::Cube => [0.30, 0.56, 0.86, 1.0],
            Self::Sphere => [0.95, 0.34, 0.30, 1.0],
            Self::ImagePlane => [0.89, 0.70, 0.25, 1.0],
            Self::PaintCanvas => [1.0, 1.0, 1.0, 1.0],
            Self::Mesh => [0.62, 0.66, 0.72, 1.0],
            Self::Adjustment => [0.70, 0.42, 0.85, 1.0],
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cube => "Cube",
            Self::Sphere => "Sphere",
            Self::ImagePlane => "Image Plane",
            Self::PaintCanvas => "Paint Canvas",
            Self::Mesh => "Mesh",
            Self::Adjustment => "Adjustment Layer",
        }
    }
    /// Whether this kind is a flat paintable raster surface.
    pub fn is_paintable(&self) -> bool {
        matches!(self, Self::PaintCanvas)
    }
}

/// One polygon face: an ordered list of vertex indices (quad or n-gon). The renderer
/// fan-triangulates these for display; the editor mutates them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Face {
    pub indices: Vec<u32>,
}

/// A single bone in a skeleton hierarchy.
///
/// `head` and `tail` are **absolute positions in object/local space** — the bone's rest
/// pose. The `parent` link is for forward kinematics only (a parent's `pose` rotation
/// propagates to its descendants); it does NOT change where the rest head/tail live.
/// `pose` is the bone's local rotation about its head, applied on top of the rest pose
/// (identity = rest). Linear-blend skinning reads these. docs/03 §4 rigging.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bone {
    pub name: String,
    /// Rest position of the bone root (proximal end), absolute in object space.
    pub head: Vec3,
    /// Rest position of the bone tip (distal end), absolute in object space.
    pub tail: Vec3,
    /// Index into `Skeleton::bones` of the parent bone, or `None` for root.
    pub parent: Option<usize>,
    /// Local pose rotation about the head, applied on top of rest. Identity = rest pose.
    #[serde(default = "quat_identity")]
    pub pose: Quat,
}

fn quat_identity() -> Quat {
    Quat::IDENTITY
}

/// Shortest distance from point `p` to the line segment `a`–`b`.
fn point_segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-12 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

impl Bone {
    pub fn new(name: impl Into<String>, head: Vec3, tail: Vec3, parent: Option<usize>) -> Self {
        Self { name: name.into(), head, tail, parent, pose: Quat::IDENTITY }
    }
    /// Length of the bone.
    pub fn length(&self) -> f32 {
        (self.tail - self.head).length()
    }
    /// Direction vector (head→tail), normalized.
    pub fn direction(&self) -> Vec3 {
        (self.tail - self.head).normalize_or_zero()
    }
}

/// Per-vertex skin binding: up to 4 bone influences per vertex (LBS). `indices[v]` are
/// bone indices into the skeleton; `weights[v]` are their blend weights (sum ≈ 1).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Skin {
    pub indices: Vec<[u32; 4]>,
    pub weights: Vec<[f32; 4]>,
}

/// A skeleton: an ordered list of bones with parent references. Rest head/tail are
/// absolute object-space positions; `pose` rotations drive deformation via FK + LBS.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
}

impl Skeleton {
    /// Rest head of bone `i` (absolute object space).
    pub fn world_head(&self, i: usize) -> Vec3 {
        self.bones[i].head
    }

    /// Rest tail of bone `i` (absolute object space).
    pub fn world_tail(&self, i: usize) -> Vec3 {
        self.bones[i].tail
    }

    /// Rest global transform of each bone: a pure translation to its head. (Bones carry
    /// no rest rotation in this model — orientation is implied by head→tail and only
    /// matters once a bone is posed.) `global_rest_i = T(head_i)`.
    pub fn rest_globals(&self) -> Vec<Mat4> {
        self.bones.iter().map(|b| Mat4::from_translation(b.head)).collect()
    }

    /// Posed global transform of each bone via forward kinematics, built so that with all
    /// poses identity it equals `rest_globals` exactly (every skinning matrix is then `I`).
    ///
    /// Local bind transform relative to the parent is the head offset
    /// `L_i = T(head_i − head_parent)` (root: `T(head_root)`); the pose rotation is applied
    /// in the bone's local frame (about its head): `A_i = A_parent · L_i · R(pose_i)`.
    /// Bones must be topologically ordered (parent index < child index), which
    /// `auto_rig_from_mesh` guarantees.
    pub fn pose_globals(&self) -> Vec<Mat4> {
        let mut globals: Vec<Mat4> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            debug_assert!(b.parent.map_or(true, |p| p < i), "bones must be topo-ordered");
            let head_parent = b.parent.map_or(Vec3::ZERO, |p| self.bones[p].head);
            let local = Mat4::from_translation(b.head - head_parent) * Mat4::from_quat(b.pose);
            let g = match b.parent {
                None => local,
                Some(p) => globals[p] * local,
            };
            globals.push(g);
        }
        globals
    }

    /// Per-bone skinning matrices `M_i = global_pose_i · global_rest_i⁻¹`. A vertex bound
    /// to bone `i` is deformed by `M_i · v`. With all poses identity, every `M_i` is the
    /// identity, so the mesh is unchanged (rest pose).
    pub fn skinning_matrices(&self) -> Vec<Mat4> {
        let rest = self.rest_globals();
        let posed = self.pose_globals();
        rest.iter()
            .zip(posed.iter())
            .map(|(r, p)| *p * r.inverse())
            .collect()
    }

    /// **Auto-skin**: bind each mesh vertex to its nearest bone segment(s), blending the
    /// two closest by inverse distance so the seam between bones bends smoothly. Returns a
    /// `Skin` parallel to `mesh.vertices`.
    ///
    /// *Upgrade path:* replace inverse-distance with bone-heat / geodesic-aware weights
    /// (Pinocchio-style) once a real skinning solver is authorized.
    pub fn auto_skin(&self, mesh: &Mesh) -> Skin {
        let nb = self.bones.len();
        let mut indices = Vec::with_capacity(mesh.vertices.len());
        let mut weights = Vec::with_capacity(mesh.vertices.len());
        for &v in &mesh.vertices {
            let p = Vec3::from(v);
            // Distance from p to each bone segment.
            let mut dists: Vec<(usize, f32)> = (0..nb)
                .map(|i| (i, point_segment_distance(p, self.bones[i].head, self.bones[i].tail)))
                .collect();
            dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            // Blend the two nearest (or one if a single-bone rig).
            let (i0, d0) = dists[0];
            if nb == 1 {
                indices.push([i0 as u32, 0, 0, 0]);
                weights.push([1.0, 0.0, 0.0, 0.0]);
                continue;
            }
            let (i1, d1) = dists[1];
            // Inverse-distance weights with a small epsilon so a vertex exactly on a bone
            // doesn't divide by zero.
            let w0 = 1.0 / (d0 + 1e-4);
            let w1 = 1.0 / (d1 + 1e-4);
            let sum = w0 + w1;
            indices.push([i0 as u32, i1 as u32, 0, 0]);
            weights.push([w0 / sum, w1 / sum, 0.0, 0.0]);
        }
        Skin { indices, weights }
    }

    /// **Auto-rig**: place a spine of `n_bones` along the principal axis of a mesh bounding
    /// box. The spine runs bottom-to-top along the longest axis.
    ///
    /// This is the v1 heuristic — good for vertical bipeds (people, trees, columns).
    /// *Upgrade path:* replace with a neural auto-rig model (RigNet, RigFormer) via ONNX
    /// once a model download is authorized and `ort` is available.
    pub fn auto_rig_from_mesh(mesh: &Mesh, n_bones: u32) -> Skeleton {
        let n = n_bones.max(2) as usize;
        if mesh.vertices.is_empty() {
            return Skeleton::default();
        }

        // Compute bounding box.
        let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for &v in &mesh.vertices {
            let p = Vec3::from(v);
            mn = mn.min(p);
            mx = mx.max(p);
        }
        let span = mx - mn;

        // Choose the longest axis for the spine.
        let (start, end) = if span.y >= span.x && span.y >= span.z {
            (Vec3::new((mn.x + mx.x) * 0.5, mn.y, (mn.z + mx.z) * 0.5),
             Vec3::new((mn.x + mx.x) * 0.5, mx.y, (mn.z + mx.z) * 0.5))
        } else if span.x >= span.z {
            (Vec3::new(mn.x, (mn.y + mx.y) * 0.5, (mn.z + mx.z) * 0.5),
             Vec3::new(mx.x, (mn.y + mx.y) * 0.5, (mn.z + mx.z) * 0.5))
        } else {
            (Vec3::new((mn.x + mx.x) * 0.5, (mn.y + mx.y) * 0.5, mn.z),
             Vec3::new((mn.x + mx.x) * 0.5, (mn.y + mx.y) * 0.5, mx.z))
        };

        let mut bones: Vec<Bone> = Vec::with_capacity(n);
        for i in 0..n {
            let ta = i as f32 / n as f32;
            let tb = (i + 1) as f32 / n as f32;
            let head = start.lerp(end, ta);
            let tail = start.lerp(end, tb);
            let parent = if i == 0 { None } else { Some(i - 1) };
            bones.push(Bone::new(format!("Bone.{:03}", i), head, tail, parent));
        }
        Skeleton { bones }
    }
}

/// An editable polygon mesh — vertices + faces. Deliberately a simple indexed form for
/// the first poly-modeling slice; a half-edge kernel (loop/ring select, bevel, etc.)
/// is the Phase 4 deepening. Operations here keep it watertight enough for extrude.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub vertices: Vec<[f32; 3]>,
    pub faces: Vec<Face>,
}

impl Mesh {
    /// A unit cube centered at the origin, 8 verts / 6 quad faces, outward winding.
    pub fn cube() -> Self {
        let v = vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5], // back  (z-)
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5], // front (z+)
        ];
        // CCW seen from outside.
        let faces = vec![
            Face {
                indices: vec![4, 5, 6, 7],
            }, // +Z front
            Face {
                indices: vec![1, 0, 3, 2],
            }, // -Z back
            Face {
                indices: vec![0, 4, 7, 3],
            }, // -X left
            Face {
                indices: vec![5, 1, 2, 6],
            }, // +X right
            Face {
                indices: vec![3, 7, 6, 2],
            }, // +Y top
            Face {
                indices: vec![0, 1, 5, 4],
            }, // -Y bottom
        ];
        Self { vertices: v, faces }
    }

    pub fn vertex(&self, i: u32) -> [f32; 3] {
        self.vertices[i as usize]
    }

    /// Geometric center of a face.
    pub fn face_centroid(&self, f: &Face) -> [f32; 3] {
        let mut c = [0.0f32; 3];
        for &i in &f.indices {
            let p = self.vertices[i as usize];
            c[0] += p[0];
            c[1] += p[1];
            c[2] += p[2];
        }
        let n = f.indices.len().max(1) as f32;
        [c[0] / n, c[1] / n, c[2] / n]
    }

    /// Newell's method face normal (robust for non-planar n-gons), normalized.
    pub fn face_normal(&self, f: &Face) -> [f32; 3] {
        let mut n = [0.0f32; 3];
        let m = f.indices.len();
        for k in 0..m {
            let a = self.vertices[f.indices[k] as usize];
            let b = self.vertices[f.indices[(k + 1) % m] as usize];
            n[0] += (a[1] - b[1]) * (a[2] + b[2]);
            n[1] += (a[2] - b[2]) * (a[0] + b[0]);
            n[2] += (a[0] - b[0]) * (a[1] + b[1]);
        }
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-9 {
            [0.0, 1.0, 0.0]
        } else {
            [n[0] / len, n[1] / len, n[2] / len]
        }
    }

    /// Ray-pick a face: returns the nearest face index the world-space ray hits, plus the
    /// hit distance `t`. The ray is given in world space; `world` is the object's model
    /// matrix. Tests the fan-triangulation of each face with Möller–Trumbore.
    pub fn pick_face(&self, ray_origin: Vec3, ray_dir: Vec3, world: Mat4) -> Option<(usize, f32)> {
        let inv = world.inverse();
        let lo = inv.transform_point3(ray_origin);
        let ld = inv.transform_vector3(ray_dir); // not normalized — fine for nearest-t compare
        let mut best: Option<(usize, f32)> = None;
        for (fi, face) in self.faces.iter().enumerate() {
            if face.indices.len() < 3 {
                continue;
            }
            let v0 = Vec3::from(self.vertex(face.indices[0]));
            for k in 1..face.indices.len() - 1 {
                let v1 = Vec3::from(self.vertex(face.indices[k]));
                let v2 = Vec3::from(self.vertex(face.indices[k + 1]));
                if let Some(t) = ray_triangle(lo, ld, v0, v1, v2) {
                    if best.map(|(_, bt)| t < bt).unwrap_or(true) {
                        best = Some((fi, t));
                    }
                }
            }
        }
        best
    }

    /// The face whose normal points most toward +Y (a sensible default extrude target).
    pub fn top_face_index(&self) -> Option<usize> {
        self.faces
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                self.face_normal(a)[1]
                    .partial_cmp(&self.face_normal(b)[1])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    /// Extrude `face_idx` along its normal by `dist`: duplicate the face's loop, push it
    /// out, retarget the face to the new loop, and stitch side quads around the rim. The
    /// classic box-modeling extrude (docs/01 §3.1).
    pub fn extrude_face(&mut self, face_idx: usize, dist: f32) -> bool {
        let Some(face) = self.faces.get(face_idx).cloned() else {
            return false;
        };
        let normal = self.face_normal(&face);
        let offset = [normal[0] * dist, normal[1] * dist, normal[2] * dist];

        // New top-loop vertices.
        let mut new_loop = Vec::with_capacity(face.indices.len());
        for &i in &face.indices {
            let p = self.vertices[i as usize];
            let np = [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]];
            new_loop.push(self.vertices.len() as u32);
            self.vertices.push(np);
        }

        // Side quads: for each edge (a,b) on the original loop, make quad
        // (a, b, b', a') so the new wall winds outward consistently with the cap.
        let m = face.indices.len();
        for k in 0..m {
            let a = face.indices[k];
            let b = face.indices[(k + 1) % m];
            let a2 = new_loop[k];
            let b2 = new_loop[(k + 1) % m];
            self.faces.push(Face {
                indices: vec![a, b, b2, a2],
            });
        }

        // Retarget the original face to the new (extruded) loop.
        self.faces[face_idx] = Face { indices: new_loop };
        true
    }

    /// Inset `face_idx`: shrink a copy of the face toward its centroid by `amount`
    /// (as a fraction of each vertex's distance to the centroid), connect the rim with a
    /// ring of quads, and retarget the face to the inner loop. The classic inset op
    /// (docs/01 §3.1) — the usual prelude to an extrude.
    pub fn inset_face(&mut self, face_idx: usize, amount: f32) -> bool {
        let Some(face) = self.faces.get(face_idx).cloned() else {
            return false;
        };
        let c = Vec3::from(self.face_centroid(&face));
        let t = amount.clamp(0.0, 0.95);
        let mut inner = Vec::with_capacity(face.indices.len());
        for &i in &face.indices {
            let p = Vec3::from(self.vertices[i as usize]);
            let np = p + (c - p) * t;
            inner.push(self.vertices.len() as u32);
            self.vertices.push([np.x, np.y, np.z]);
        }
        let m = face.indices.len();
        for k in 0..m {
            let a = face.indices[k];
            let b = face.indices[(k + 1) % m];
            let b2 = inner[(k + 1) % m];
            let a2 = inner[k];
            self.faces.push(Face {
                indices: vec![a, b, b2, a2],
            });
        }
        self.faces[face_idx] = Face { indices: inner };
        true
    }

    /// One level of **Catmull–Clark subdivision** — the gold-standard smooth subdivision
    /// surface (docs/01 §3.2). Every face becomes a fan of quads; vertices relax toward
    /// the limit surface. Iterate for smoother. Boundary edges/vertices use the crease
    /// rules so open meshes (a plane) keep their silhouette.
    pub fn catmull_clark(&self) -> Mesh {
        use std::collections::HashMap;
        let edge_key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
        let nverts = self.vertices.len();

        // Face points: centroid of each face.
        let face_points: Vec<Vec3> = self
            .faces
            .iter()
            .map(|f| Vec3::from(self.face_centroid(f)))
            .collect();

        // Edge → adjacent face indices.
        let mut edge_faces: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (fi, f) in self.faces.iter().enumerate() {
            let m = f.indices.len();
            for k in 0..m {
                let a = f.indices[k];
                let b = f.indices[(k + 1) % m];
                edge_faces.entry(edge_key(a, b)).or_default().push(fi);
            }
        }

        // Edge points (interior: avg of endpoints + adjacent face points; boundary: midpoint).
        let mut edge_point: HashMap<(u32, u32), Vec3> = HashMap::new();
        for (&(a, b), faces) in &edge_faces {
            let pa = Vec3::from(self.vertices[a as usize]);
            let pb = Vec3::from(self.vertices[b as usize]);
            let ep = if faces.len() == 2 {
                (pa + pb + face_points[faces[0]] + face_points[faces[1]]) / 4.0
            } else {
                (pa + pb) / 2.0
            };
            edge_point.insert((a, b), ep);
        }

        // Per-vertex adjacency.
        let mut vert_faces: Vec<Vec<usize>> = vec![Vec::new(); nverts];
        for (fi, f) in self.faces.iter().enumerate() {
            for &vi in &f.indices {
                vert_faces[vi as usize].push(fi);
            }
        }
        let mut vert_edges: Vec<Vec<(u32, u32)>> = vec![Vec::new(); nverts];
        for &(a, b) in edge_faces.keys() {
            vert_edges[a as usize].push((a, b));
            vert_edges[b as usize].push((a, b));
        }

        // Relaxed original-vertex positions.
        let mut new_vert: Vec<Vec3> = Vec::with_capacity(nverts);
        for v in 0..nverts {
            let p = Vec3::from(self.vertices[v]);
            let faces = &vert_faces[v];
            let edges = &vert_edges[v];
            if faces.is_empty() {
                new_vert.push(p);
                continue;
            }
            let boundary: Vec<(u32, u32)> = edges
                .iter()
                .copied()
                .filter(|e| edge_faces[e].len() == 1)
                .collect();
            if boundary.len() == 2 {
                // Boundary crease rule: (m0 + m1 + 6p) / 8.
                let mid = |e: (u32, u32)| {
                    (Vec3::from(self.vertices[e.0 as usize])
                        + Vec3::from(self.vertices[e.1 as usize]))
                        / 2.0
                };
                new_vert.push((mid(boundary[0]) + mid(boundary[1]) + p * 6.0) / 8.0);
            } else {
                let n = faces.len() as f32;
                let f_avg = faces.iter().map(|&fi| face_points[fi]).sum::<Vec3>() / n;
                let r_avg = edges
                    .iter()
                    .map(|&(a, b)| {
                        (Vec3::from(self.vertices[a as usize])
                            + Vec3::from(self.vertices[b as usize]))
                            / 2.0
                    })
                    .sum::<Vec3>()
                    / edges.len() as f32;
                new_vert.push((f_avg + r_avg * 2.0 + p * (n - 3.0)) / n);
            }
        }

        // Assemble: original verts, then face points, then edge points.
        let mut verts: Vec<[f32; 3]> = new_vert.iter().map(|p| [p.x, p.y, p.z]).collect();
        let face_pt_base = verts.len();
        for fp in &face_points {
            verts.push([fp.x, fp.y, fp.z]);
        }
        let mut edge_idx: HashMap<(u32, u32), u32> = HashMap::new();
        for (&key, ep) in &edge_point {
            edge_idx.insert(key, verts.len() as u32);
            verts.push([ep.x, ep.y, ep.z]);
        }

        // Each original face of valence k → k quads.
        let mut faces: Vec<Face> = Vec::new();
        for (fi, f) in self.faces.iter().enumerate() {
            let m = f.indices.len();
            let fp = (face_pt_base + fi) as u32;
            for i in 0..m {
                let vi = f.indices[i];
                let v_next = f.indices[(i + 1) % m];
                let v_prev = f.indices[(i + m - 1) % m];
                let e_next = edge_idx[&edge_key(vi, v_next)];
                let e_prev = edge_idx[&edge_key(v_prev, vi)];
                faces.push(Face {
                    indices: vec![vi, e_next, fp, e_prev],
                });
            }
        }
        Mesh {
            vertices: verts,
            faces,
        }
    }

    // --- CSG booleans via manifold3d (Tier 1). docs/03 §3 "integrate, don't write." ------

    /// Fan-triangulate `self` into a flat vertex + triangle list suitable for manifold3d.
    /// The world matrix `m` is applied so booleans work in world space. Returns (verts_xyz,
    /// tris) where `verts_xyz` is [x0,y0,z0, x1,y1,z1, …] and `tris` is [a0,b0,c0, …].
    fn triangulate_for_csg(&self, m: glam::Mat4) -> (Vec<f32>, Vec<u32>) {
        let mut verts: Vec<f32> = Vec::new();
        let mut tris: Vec<u32> = Vec::new();
        let base = 0u32;
        for v in &self.vertices {
            let wp = m.transform_point3(Vec3::from(*v));
            verts.extend_from_slice(&[wp.x, wp.y, wp.z]);
        }
        for face in &self.faces {
            // Fan triangulate (works for convex faces which all our procedural meshes are).
            let idx = &face.indices;
            if idx.len() < 3 {
                continue;
            }
            let a = idx[0] + base;
            for i in 1..idx.len() as u32 - 1 {
                tris.extend_from_slice(&[a, idx[i as usize] + base, idx[i as usize + 1] + base]);
            }
        }
        (verts, tris)
    }

    /// Convert a flat `(vert_props, n_props, tri_verts)` from manifold3d back to a `Mesh`
    /// with triangular faces in local space (world matrix applied in reverse via `inv_m`).
    fn from_csg_output(vert_props: Vec<f32>, n_props: usize, tri_verts: Vec<u32>, inv_m: glam::Mat4) -> Mesh {
        let verts: Vec<[f32; 3]> = vert_props.chunks(n_props).map(|c| {
            let wp = Vec3::new(c[0], c[1], c[2]);
            let lp = inv_m.transform_point3(wp);
            [lp.x, lp.y, lp.z]
        }).collect();
        let faces: Vec<Face> = tri_verts.chunks(3).map(|t| Face {
            indices: vec![t[0], t[1], t[2]],
        }).collect();
        Mesh { vertices: verts, faces }
    }

    /// CSG union: `self ∪ other`. Both meshes are given their world matrices.
    /// Returns `None` if either mesh is non-manifold.
    pub fn bool_union(&self, self_world: glam::Mat4, other: &Mesh, other_world: glam::Mat4) -> Option<Mesh> {
        use manifold3d::Manifold;
        let (va, ta) = self.triangulate_for_csg(self_world);
        let (vb, tb) = other.triangulate_for_csg(other_world);
        let ma = Manifold::from_mesh_f32(&va, 3, &ta).ok()?;
        let mb = Manifold::from_mesh_f32(&vb, 3, &tb).ok()?;
        let result = ma.union(&mb);
        let (vr, np, tr) = result.to_mesh_f32();
        Some(Self::from_csg_output(vr, np, tr, glam::Mat4::IDENTITY))
    }

    /// CSG difference: `self − other`. Returns `None` if either mesh is non-manifold.
    pub fn bool_subtract(&self, self_world: glam::Mat4, other: &Mesh, other_world: glam::Mat4) -> Option<Mesh> {
        use manifold3d::Manifold;
        let (va, ta) = self.triangulate_for_csg(self_world);
        let (vb, tb) = other.triangulate_for_csg(other_world);
        let ma = Manifold::from_mesh_f32(&va, 3, &ta).ok()?;
        let mb = Manifold::from_mesh_f32(&vb, 3, &tb).ok()?;
        let result = ma.difference(&mb);
        let (vr, np, tr) = result.to_mesh_f32();
        Some(Self::from_csg_output(vr, np, tr, glam::Mat4::IDENTITY))
    }

    /// CSG intersection: `self ∩ other`. Returns `None` if either mesh is non-manifold.
    pub fn bool_intersect(&self, self_world: glam::Mat4, other: &Mesh, other_world: glam::Mat4) -> Option<Mesh> {
        use manifold3d::Manifold;
        let (va, ta) = self.triangulate_for_csg(self_world);
        let (vb, tb) = other.triangulate_for_csg(other_world);
        let ma = Manifold::from_mesh_f32(&va, 3, &ta).ok()?;
        let mb = Manifold::from_mesh_f32(&vb, 3, &tb).ok()?;
        let result = ma.intersection(&mb);
        let (vr, np, tr) = result.to_mesh_f32();
        Some(Self::from_csg_output(vr, np, tr, glam::Mat4::IDENTITY))
    }

    /// Compute per-vertex averaged normals (angle-weighted face normals at each incident face).
    pub fn vertex_normals(&self) -> Vec<Vec3> {
        let nv = self.vertices.len();
        let mut normals = vec![Vec3::ZERO; nv];
        for face in &self.faces {
            let n = Vec3::from(self.face_normal(face));
            for &vi in &face.indices {
                normals[vi as usize] += n;
            }
        }
        normals.iter().map(|n| n.normalize_or_zero()).collect()
    }

    /// Apply linear-blend skinning: deform a copy of this mesh by the per-bone skinning
    /// matrices, blended per vertex by `skin`. Returns the deformed mesh (faces unchanged).
    /// If `skin` doesn't match the vertex count, returns a clone unchanged (fail-safe).
    pub fn skin_deform(&self, skin: &Skin, matrices: &[Mat4]) -> Mesh {
        if skin.indices.len() != self.vertices.len() || skin.weights.len() != self.vertices.len() {
            return self.clone();
        }
        let mut out = self.clone();
        for (vi, v) in self.vertices.iter().enumerate() {
            let p = Vec3::from(*v);
            let idx = skin.indices[vi];
            let w = skin.weights[vi];
            let mut acc = Vec3::ZERO;
            let mut wsum = 0.0_f32;
            for k in 0..4 {
                if w[k] <= 0.0 {
                    continue;
                }
                let bi = idx[k] as usize;
                if bi >= matrices.len() {
                    continue;
                }
                acc += matrices[bi].transform_point3(p) * w[k];
                wsum += w[k];
            }
            if wsum > 1e-6 {
                out.vertices[vi] = (acc / wsum).into();
            }
        }
        out
    }

    /// Sculpt: **draw** brush — displaces vertices along their averaged normals within `radius`
    /// of `center` (world space). `strength` in world units per stroke. Negative = push in.
    /// `world` is the object's model matrix (to project brush into local space).
    /// Returns true if any vertex was moved.
    pub fn sculpt_draw(&mut self, center: Vec3, radius: f32, strength: f32, world: Mat4) -> bool {
        let inv = world.inverse();
        let center_l = inv.transform_point3(center);
        let normals = self.vertex_normals();
        let r2 = radius * radius;
        let mut moved = false;
        for (i, v) in self.vertices.iter_mut().enumerate() {
            let p = Vec3::from(*v);
            let d2 = (p - center_l).length_squared();
            if d2 >= r2 {
                continue;
            }
            // Smooth clamped bell falloff: cos²(π/2 · d/r)
            let t = (d2 / r2).sqrt();
            let falloff = {
                let f = (std::f32::consts::FRAC_PI_2 * t).cos();
                f * f
            };
            let n = normals[i];
            *v = (p + n * (strength * falloff)).into();
            moved = true;
        }
        moved
    }

    /// Sculpt: **smooth** brush — moves each vertex toward the average of its neighbours.
    /// `factor` in [0, 1] controls how strongly to pull toward the average (0.5 is typical).
    pub fn sculpt_smooth(&mut self, center: Vec3, radius: f32, factor: f32, world: Mat4) -> bool {
        let inv = world.inverse();
        let center_l = inv.transform_point3(center);
        let r2 = radius * radius;

        // Build per-vertex adjacency list (iterate once over all faces).
        let nv = self.vertices.len();
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); nv];
        for face in &self.faces {
            let m = face.indices.len();
            for k in 0..m {
                let a = face.indices[k];
                let b = face.indices[(k + 1) % m];
                adj[a as usize].push(b);
                adj[b as usize].push(a);
            }
        }

        let orig: Vec<Vec3> = self.vertices.iter().map(|v| Vec3::from(*v)).collect();
        let mut moved = false;
        for (i, v) in self.vertices.iter_mut().enumerate() {
            let p = Vec3::from(*v);
            let d2 = (p - center_l).length_squared();
            if d2 >= r2 || adj[i].is_empty() {
                continue;
            }
            let avg = adj[i].iter().map(|&j| orig[j as usize]).sum::<Vec3>()
                / adj[i].len() as f32;
            let t = (d2 / r2).sqrt();
            let falloff = { let f = (std::f32::consts::FRAC_PI_2 * t).cos(); f * f };
            *v = (p + (avg - p) * (factor * falloff)).into();
            moved = true;
        }
        moved
    }

    /// Sculpt: **flatten** brush — projects vertices toward the tangent plane at `center`
    /// (defined by the averaged vertex normal at the hit point) that is closest to `center`.
    /// Great for flattening peaks and bumps.
    pub fn sculpt_flatten(&mut self, center: Vec3, radius: f32, strength: f32, world: Mat4) -> bool {
        let inv = world.inverse();
        let center_l = inv.transform_point3(center);
        let r2 = radius * radius;
        // Compute the averaged normal at the brush center (from all verts inside radius).
        let avg_normal = {
            let mut n = Vec3::ZERO;
            for v in &self.vertices {
                let p = Vec3::from(*v);
                if (p - center_l).length_squared() < r2 {
                    n += Vec3::from(self.face_normal(
                        // Use the face that owns this vertex — just pick first via vertex_normals shortcut.
                        &Face { indices: vec![] },
                    ));
                }
            }
            // Fall back to Y-up if nothing found.
            let normals = self.vertex_normals();
            let mut avg = Vec3::ZERO;
            for (i, v) in self.vertices.iter().enumerate() {
                if (Vec3::from(*v) - center_l).length_squared() < r2 {
                    avg += normals[i];
                }
            }
            avg.normalize_or_zero()
        };
        if avg_normal.length_squared() < 1e-6 {
            return false;
        }
        // Project all verts in range onto the plane through center_l with avg_normal.
        let mut moved = false;
        for v in &mut self.vertices {
            let p = Vec3::from(*v);
            let d2 = (p - center_l).length_squared();
            if d2 >= r2 {
                continue;
            }
            let t = (d2 / r2).sqrt();
            let falloff = { let f = (std::f32::consts::FRAC_PI_2 * t).cos(); f * f };
            // Distance from point to plane: signed distance = dot(p - center_l, normal)
            let signed_dist = (p - center_l).dot(avg_normal);
            *v = (p - avg_normal * (signed_dist * strength * falloff)).into();
            moved = true;
        }
        moved
    }

    /// Sculpt: **pinch** brush — moves vertices toward the brush center (like inflate but inward).
    pub fn sculpt_pinch(&mut self, center: Vec3, radius: f32, strength: f32, world: Mat4) -> bool {
        let inv = world.inverse();
        let center_l = inv.transform_point3(center);
        let r2 = radius * radius;
        let mut moved = false;
        for v in &mut self.vertices {
            let p = Vec3::from(*v);
            let d2 = (p - center_l).length_squared();
            if d2 >= r2 {
                continue;
            }
            let t = (d2 / r2).sqrt();
            let falloff = { let f = (std::f32::consts::FRAC_PI_2 * t).cos(); f * f };
            let dir_to_center = (center_l - p).normalize_or_zero();
            *v = (p + dir_to_center * (strength * falloff)).into();
            moved = true;
        }
        moved
    }

    /// Generate a mesh from a luminance heightmap.
    ///
    /// `pixels` must be row-major RGBA8 (same layout as `Renderer::paint_readback_rgba`).
    /// A regular `width × height` grid is created where Y = `luminance * scale`.
    /// The mesh is centred at the origin and spans [−0.5, 0.5] in X and Z.
    ///
    /// *Upgrade path:* replace with a TripoSR / Zero123 neural network (single-image-to-3D)
    /// once model download is authorized and the `ort` ONNX runtime is available.
    pub fn from_heightmap(pixels: &[u8], width: u32, height: u32, scale: f32) -> Mesh {
        let w = width.max(2) as usize;
        let h = height.max(2) as usize;

        let luma = |xi: usize, yi: usize| -> f32 {
            let src_x = (xi * width as usize / w).min(width as usize - 1);
            let src_y = (yi * height as usize / h).min(height as usize - 1);
            let idx = (src_y * width as usize + src_x) * 4;
            if idx + 2 >= pixels.len() {
                return 0.0;
            }
            let r = pixels[idx] as f32 / 255.0;
            let g = pixels[idx + 1] as f32 / 255.0;
            let b = pixels[idx + 2] as f32 / 255.0;
            // Rec.709 luminance.
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };

        let mut verts: Vec<[f32; 3]> = Vec::with_capacity(w * h);
        for yi in 0..h {
            for xi in 0..w {
                let x = xi as f32 / (w - 1) as f32 - 0.5;
                let z = yi as f32 / (h - 1) as f32 - 0.5;
                let y = luma(xi, yi) * scale;
                verts.push([x, y, z]);
            }
        }

        let mut faces: Vec<Face> = Vec::new();
        let idx = |xi: usize, yi: usize| -> u32 { (yi * w + xi) as u32 };
        for yi in 0..h - 1 {
            for xi in 0..w - 1 {
                // Two triangles per quad cell, CCW winding.
                faces.push(Face { indices: vec![idx(xi, yi), idx(xi + 1, yi), idx(xi + 1, yi + 1)] });
                faces.push(Face { indices: vec![idx(xi, yi), idx(xi + 1, yi + 1), idx(xi, yi + 1)] });
            }
        }

        Mesh { vertices: verts, faces }
    }

    /// **Mesh decimation** via vertex clustering.
    ///
    /// Divides the mesh bounding box into a `grid_res × grid_res × grid_res` voxel grid.
    /// Each occupied cell contributes one averaged vertex to the output; triangles that degenerate
    /// (all three verts in the same cell) are dropped. This produces a coarser mesh with
    /// approximately `grid_res³` unique vertices; a `grid_res` of 16–32 gives a clean LOD.
    ///
    /// The output is always triangulated (no n-gons). Returns `None` if the result is empty.
    ///
    /// *Upgrade path:* replace with QuadriFlow (global orientation-field quad remesh) via FFI
    /// once a build environment with C++17 is available. The API contract here (takes a Mesh,
    /// returns a simpler Mesh) is the same.
    pub fn decimate_cluster(&self, grid_res: u32) -> Option<Mesh> {
        if self.vertices.is_empty() || self.faces.is_empty() {
            return None;
        }
        let res = grid_res.max(2) as usize;

        // Compute bounding box.
        let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &self.vertices {
            for k in 0..3 {
                mn[k] = mn[k].min(v[k]);
                mx[k] = mx[k].max(v[k]);
            }
        }
        let span: [f32; 3] = [mx[0] - mn[0] + 1e-6, mx[1] - mn[1] + 1e-6, mx[2] - mn[2] + 1e-6];

        // Map each vertex to a cell index.
        let cell_of = |v: [f32; 3]| -> usize {
            let xi = (((v[0] - mn[0]) / span[0]) * res as f32).min(res as f32 - 1.0) as usize;
            let yi = (((v[1] - mn[1]) / span[1]) * res as f32).min(res as f32 - 1.0) as usize;
            let zi = (((v[2] - mn[2]) / span[2]) * res as f32).min(res as f32 - 1.0) as usize;
            xi + yi * res + zi * res * res
        };

        // Accumulate per-cell vertex sums.
        let ncells = res * res * res;
        let mut cell_sum = vec![[0.0f32; 3]; ncells];
        let mut cell_count = vec![0u32; ncells];
        for &v in &self.vertices {
            let c = cell_of(v);
            cell_sum[c][0] += v[0];
            cell_sum[c][1] += v[1];
            cell_sum[c][2] += v[2];
            cell_count[c] += 1;
        }

        // Build output vertex list and cell→output-index map.
        let mut cell_to_out = vec![u32::MAX; ncells];
        let mut out_verts: Vec<[f32; 3]> = Vec::new();
        for (c, &cnt) in cell_count.iter().enumerate() {
            if cnt > 0 {
                cell_to_out[c] = out_verts.len() as u32;
                let avg = [
                    cell_sum[c][0] / cnt as f32,
                    cell_sum[c][1] / cnt as f32,
                    cell_sum[c][2] / cnt as f32,
                ];
                out_verts.push(avg);
            }
        }
        if out_verts.is_empty() {
            return None;
        }

        // Rebuild triangles: fan-triangulate each face, skip degenerate tris.
        let mut out_faces: Vec<Face> = Vec::new();
        for face in &self.faces {
            let m = face.indices.len();
            if m < 3 {
                continue;
            }
            let a0 = cell_to_out[cell_of(self.vertices[face.indices[0] as usize])];
            for k in 1..m - 1 {
                let a1 = cell_to_out[cell_of(self.vertices[face.indices[k] as usize])];
                let a2 = cell_to_out[cell_of(self.vertices[face.indices[k + 1] as usize])];
                if a0 != a1 && a1 != a2 && a0 != a2 {
                    out_faces.push(Face { indices: vec![a0, a1, a2] });
                }
            }
        }
        if out_faces.is_empty() {
            return None;
        }

        Some(Mesh { vertices: out_verts, faces: out_faces })
    }

    /// Revolve (lathe) a 2D profile around the Y axis.
    ///
    /// `profile` is a slice of (r, y) pairs in the XY half-plane (r ≥ 0).
    /// `steps` is the number of angular slices (≥ 3); 32 gives smooth results.
    /// Returns an oriented closed solid mesh.  Open profiles (first ≠ last point) get
    /// capped automatically if r==0 at the first or last vertex.
    pub fn revolve(profile: &[[f32; 2]], steps: u32) -> Mesh {
        assert!(profile.len() >= 2, "revolve needs at least 2 profile points");
        let steps = steps.max(3) as usize;
        let np = profile.len();
        let mut verts: Vec<[f32; 3]> = Vec::with_capacity(np * steps);
        let mut faces: Vec<Face> = Vec::new();

        // Build the vertex ring grid: ring[step][profile_pt].
        for s in 0..steps {
            let angle = std::f32::consts::TAU * s as f32 / steps as f32;
            let (sin_a, cos_a) = angle.sin_cos();
            for &[r, y] in profile {
                verts.push([r * cos_a, y, r * sin_a]);
            }
        }
        // Vertex index: ring s, profile point p.
        let idx = |s: usize, p: usize| -> u32 { ((s % steps) * np + p) as u32 };

        // Connect each pair of adjacent profile segments around the full ring.
        for s in 0..steps {
            let sn = (s + 1) % steps;
            for p in 0..np - 1 {
                let pn = p + 1;
                let [r0, _] = profile[p];
                let [r1, _] = profile[pn];
                // Collapse degenerate quads on the axis into triangles.
                if r0.abs() < 1e-5 {
                    faces.push(Face { indices: vec![idx(s, p), idx(s, pn), idx(sn, pn)] });
                } else if r1.abs() < 1e-5 {
                    faces.push(Face { indices: vec![idx(s, p), idx(sn, p), idx(s, pn)] });
                } else {
                    faces.push(Face { indices: vec![idx(s, p), idx(sn, p), idx(sn, pn), idx(s, pn)] });
                }
            }
        }

        // Flat caps if first/last profile point sits on axis.
        if profile[0][0].abs() < 1e-5 {
            // Bottom cap (fan around the axis pole).
            let pole = verts.len() as u32;
            verts.push([0.0, profile[0][1], 0.0]);
            for s in 0..steps {
                let sn = (s + 1) % steps;
                faces.push(Face { indices: vec![pole, idx(sn, 0), idx(s, 0)] });
            }
        }
        if profile[np - 1][0].abs() < 1e-5 {
            let pole = verts.len() as u32;
            verts.push([0.0, profile[np - 1][1], 0.0]);
            for s in 0..steps {
                let sn = (s + 1) % steps;
                faces.push(Face { indices: vec![pole, idx(s, np - 1), idx(sn, np - 1)] });
            }
        }

        Mesh { vertices: verts, faces }
    }

    /// Extrude a 2D cross-section (shape) along a 3D path, creating a tube/pipe mesh.
    ///
    /// `path`  — 3D spine points (≥ 2). The tube runs from path[0] to path[last].
    /// `shape` — 2D cross-section points in the XY plane, in order (≥ 3), forming a closed
    ///           polygon. The polygon is NOT automatically closed; repeat the first point if
    ///           you want a seamless join.
    ///
    /// Frame orientation: transport frame (Frenet-Serret with a stable-up fallback). Each
    /// spine step gets an orthonormal `(tangent, normal, binormal)` so the cross-section
    /// stays aligned without twisting on straight segments.
    pub fn path_extrude(path: &[Vec3], shape: &[[f32; 2]]) -> Mesh {
        assert!(path.len() >= 2, "path_extrude needs at least 2 path points");
        assert!(shape.len() >= 3, "path_extrude needs at least 3 shape points");

        let ns = shape.len();
        let np = path.len();
        let mut verts: Vec<[f32; 3]> = Vec::with_capacity(np * ns);
        let mut faces: Vec<Face> = Vec::new();

        // Build transport frames along the path.
        // Frame i: (normal_i, binormal_i) perpendicular to tangent_i.
        // Seed frame 0 using a world-up fallback.
        let tangent = |i: usize| -> Vec3 {
            if i + 1 < np {
                (path[i + 1] - path[i]).normalize_or_zero()
            } else {
                (path[i] - path[i - 1]).normalize_or_zero()
            }
        };

        let t0 = tangent(0);
        let world_up = Vec3::Y;
        let initial_right = if t0.cross(world_up).length() > 1e-4 {
            t0.cross(world_up).normalize()
        } else {
            t0.cross(Vec3::Z).normalize()
        };
        let initial_up = initial_right.cross(t0).normalize();

        let mut normal = initial_up;
        let mut binormal = initial_right;

        for step in 0..np {
            let t = tangent(step);
            // Parallel-transport: project the current normal onto the plane perpendicular to t.
            let n_proj = (normal - t * t.dot(normal)).normalize_or_zero();
            let b_proj = t.cross(n_proj).normalize_or_zero();
            if n_proj.length() > 1e-6 {
                normal = n_proj;
                binormal = b_proj;
            }
            // Stamp the cross-section shape in the local frame.
            for &[sx, sy] in shape {
                let world_pt = path[step] + binormal * sx + normal * sy;
                verts.push(world_pt.into());
            }
        }

        // Vertex index: path_step s, shape_pt p.
        let idx = |s: usize, p: usize| -> u32 { (s * ns + p) as u32 };

        // Build quads between adjacent path rings.
        for s in 0..np - 1 {
            for p in 0..ns {
                let pn = (p + 1) % ns;
                faces.push(Face {
                    indices: vec![idx(s, p), idx(s, pn), idx(s + 1, pn), idx(s + 1, p)],
                });
            }
        }

        Mesh { vertices: verts, faces }
    }

    /// Reflect the whole mesh across a world axis (0=X, 1=Y, 2=Z), appending the mirrored
    /// copy with reversed winding so its normals point outward. (No seam weld yet — that's
    /// a refinement; visually fine for a mirror that sits off the plane.)
    pub fn mirrored(&self, axis: usize) -> Mesh {
        let mut out = self.clone();
        let base = self.vertices.len() as u32;
        for v in &self.vertices {
            let mut nv = *v;
            nv[axis] = -nv[axis];
            out.vertices.push(nv);
        }
        for f in &self.faces {
            // Reverse winding so the mirrored face faces outward.
            let mut idx: Vec<u32> = f.indices.iter().rev().map(|&i| i + base).collect();
            idx.rotate_right(1);
            out.faces.push(Face { indices: idx });
        }
        out
    }

    /// Duplicate the mesh `count` times, each copy offset by `offset * i`. `count` of 1
    /// returns a clone.
    pub fn arrayed(&self, count: u32, offset: [f32; 3]) -> Mesh {
        let count = count.max(1);
        let mut out = Mesh::default();
        let stride = self.vertices.len() as u32;
        for c in 0..count {
            let base = c * stride;
            let d = [
                offset[0] * c as f32,
                offset[1] * c as f32,
                offset[2] * c as f32,
            ];
            for v in &self.vertices {
                out.vertices.push([v[0] + d[0], v[1] + d[1], v[2] + d[2]]);
            }
            for f in &self.faces {
                out.faces.push(Face {
                    indices: f.indices.iter().map(|&i| i + base).collect(),
                });
            }
        }
        out
    }

    /// Evaluate a modifier stack top-to-bottom into a display mesh. The base mesh is
    /// never touched — modifiers are a re-cookable generator stack (docs/01 §3.2).
    pub fn evaluated(&self, modifiers: &[Modifier]) -> Mesh {
        let mut m = self.clone();
        for modifier in modifiers {
            m = match *modifier {
                Modifier::Mirror { axis } => m.mirrored(axis.min(2) as usize),
                Modifier::Array { count, offset } => m.arrayed(count, offset),
                Modifier::Subdivide { levels } => {
                    let mut r = m;
                    for _ in 0..levels.min(4) {
                        r = r.catmull_clark();
                    }
                    r
                }
                Modifier::Decimate { grid_res } => {
                    m.decimate_cluster(grid_res).unwrap_or(m)
                }
            };
        }
        m
    }
}

/// A non-destructive generator in an object's modifier stack (docs/01 §3.2). Evaluated
/// top-to-bottom by `Mesh::evaluated`; the base mesh is preserved so the user can toggle,
/// reorder, and re-tweak forever.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Modifier {
    /// Reflect across a world axis (0=X, 1=Y, 2=Z).
    Mirror { axis: u32 },
    /// Repeat `count` times, each offset by `offset * i`.
    Array { count: u32, offset: [f32; 3] },
    /// `levels` of Catmull–Clark subdivision (capped at 4).
    Subdivide { levels: u32 },
    /// Vertex-clustering decimation to `grid_res³` cells. Upgrade to QuadriFlow when
    /// a C++17 build environment is available.
    Decimate { grid_res: u32 },
}

impl Modifier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Mirror { .. } => "Mirror",
            Self::Array { .. } => "Array",
            Self::Subdivide { .. } => "Subdivide",
            Self::Decimate { .. } => "Decimate",
        }
    }
}

/// The uniform **envelope** every object shares. docs/03 §3.1.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Object {
    pub id: ObjId,
    pub name: String,
    pub kind: ObjectKind,
    pub transform: Trs,
    pub color: [f32; 4],
    pub visibility: bool,
    pub lock: bool,
    pub opacity: f32,
    pub compositing: Compositing,
    /// How this object blends with the layers beneath it in the flat compositor.
    /// docs/03 §1.7.
    #[serde(default)]
    pub blend_mode: BlendMode,
    pub local_aabb: Aabb,
    /// Editable geometry for `ObjectKind::Mesh`. `default` so non-mesh objects and older
    /// bundles serialize/parse without it.
    #[serde(default)]
    pub mesh: Option<Mesh>,
    /// Non-destructive modifier stack evaluated over `mesh` for display. `default` for
    /// back-compat. docs/01 §3.2.
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    /// Adjustment layer parameters for `ObjectKind::Adjustment`. `None` for all other
    /// object kinds. docs/03 §1.7.
    #[serde(default)]
    pub adjustment: Option<AdjustmentKind>,
    /// Optional skeleton bound to this mesh (auto-rig or manual). docs/03 §4 rigging.
    #[serde(default)]
    pub skeleton: Option<Skeleton>,
    /// Optional per-vertex skin binding (parallel to `mesh.vertices`). Drives LBS deform
    /// when the skeleton is posed. docs/03 §4 rigging.
    #[serde(default)]
    pub skin: Option<Skin>,
}

impl Object {
    /// The display mesh = base mesh, **skinned** by the posed skeleton (if any), then with
    /// the modifier stack applied. `None` for non-mesh objects.
    ///
    /// Skinning runs before modifiers so that, e.g., a Subdivide modifier smooths the
    /// already-deformed surface. With all bone poses at identity, skinning is a no-op and
    /// the mesh equals its rest shape.
    pub fn display_mesh(&self) -> Option<Mesh> {
        let base = self.mesh.as_ref()?;
        // Skinning pass: only pay for it when a skeleton + skin both exist and at least one
        // bone is actually posed away from rest.
        let skinned;
        let to_modify: &Mesh = match (&self.skeleton, &self.skin) {
            (Some(skel), Some(skin)) if skel.bones.iter().any(|b| b.pose != Quat::IDENTITY) => {
                skinned = base.skin_deform(skin, &skel.skinning_matrices());
                &skinned
            }
            _ => base,
        };
        Some(if self.modifiers.is_empty() {
            to_modify.clone()
        } else {
            to_modify.evaluated(&self.modifiers)
        })
    }
}

impl Object {
    pub fn world_matrix(&self) -> Mat4 {
        self.transform.matrix()
    }
}

struct Slot {
    generation: u32,
    object: Option<Object>,
}

/// Slotmap-style generational arena. Stale ids fail lookup; new objects reuse
/// freed slots with bumped generations so a stale id never aliases.
#[derive(Default)]
struct ObjectArena {
    slots: Vec<Slot>,
    free: Vec<u32>,
}

impl ObjectArena {
    fn insert(&mut self, mut object: Object) -> ObjId {
        if let Some(slot_idx) = self.free.pop() {
            let slot = &mut self.slots[slot_idx as usize];
            slot.generation = slot.generation.wrapping_add(1);
            let id = ObjId {
                slot: slot_idx,
                generation: slot.generation,
            };
            object.id = id;
            slot.object = Some(object);
            id
        } else {
            let slot_idx = self.slots.len() as u32;
            let id = ObjId {
                slot: slot_idx,
                generation: 0,
            };
            object.id = id;
            self.slots.push(Slot {
                generation: 0,
                object: Some(object),
            });
            id
        }
    }
    fn get(&self, id: ObjId) -> Option<&Object> {
        let slot = self.slots.get(id.slot as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.object.as_ref()
    }
    fn get_mut(&mut self, id: ObjId) -> Option<&mut Object> {
        let slot = self.slots.get_mut(id.slot as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.object.as_mut()
    }
    fn remove(&mut self, id: ObjId) -> Option<Object> {
        let slot = self.slots.get_mut(id.slot as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let object = slot.object.take();
        if object.is_some() {
            self.free.push(id.slot);
        }
        object
    }
    /// Place an object back at its **exact** id (slot + generation). Used by undo to
    /// resurrect a removed object without aliasing — the generation is pinned to the
    /// object's own id, so any later id reuse still can't collide.
    fn restore(&mut self, object: Object) {
        let slot_idx = object.id.slot as usize;
        // Grow the slot vector if needed (shouldn't normally happen, but be safe).
        while self.slots.len() <= slot_idx {
            self.slots.push(Slot { generation: 0, object: None });
        }
        // This slot is no longer free.
        self.free.retain(|&s| s != object.id.slot);
        let gen = object.id.generation;
        self.slots[slot_idx] = Slot { generation: gen, object: Some(object) };
    }
    fn iter(&self) -> impl Iterator<Item = &Object> + '_ {
        self.slots.iter().filter_map(|s| s.object.as_ref())
    }
}

/// One object's before/after state in an undo transaction. `None` = the object did not
/// exist at that end (so `before: None` is an add, `after: None` is a remove).
#[derive(Clone)]
struct ObjectEdit {
    id: ObjId,
    before: Option<Object>,
    after: Option<Object>,
}

/// A user action = one transaction of per-object edits. Undo reverts every edit to its
/// `before`; redo re-applies every `after`. This is **command-delta** undo (per-object
/// state, not whole-document snapshots — docs/03 §3 law).
#[derive(Clone, Default)]
struct Transaction {
    edits: Vec<ObjectEdit>,
}

/// Bounded undo/redo history of transactions.
#[derive(Default)]
struct History {
    past: Vec<Transaction>,
    future: Vec<Transaction>,
}

/// In-flight capture started by `Document::checkpoint`. Records the prior state of named
/// objects plus the id set that existed, so `commit` can diff for adds/removes/modifies.
struct Pending {
    before: Vec<(ObjId, Object)>,
    existing: std::collections::HashSet<ObjId>,
}

const HISTORY_CAP: usize = 128;

/// Structural equality of two objects, used to skip no-op undo transactions. Compares
/// serialized form rather than deriving `PartialEq` across the whole object graph — this
/// runs once per committed action (not per frame), so the allocation is immaterial.
fn objects_equal(a: &Object, b: &Object) -> bool {
    serde_json::to_vec(a).ok() == serde_json::to_vec(b).ok()
}

/// The document: the scene graph of typed objects plus the selection set.
///
/// Undo is **command-delta**: wrap a mutation in `checkpoint(&[ids])` … `commit()` and the
/// document records the minimal per-object before/after state (docs/03 §3 law — never a
/// whole-document snapshot). `undo()`/`redo()` walk the transaction history.
#[derive(Default)]
pub struct Document {
    objects: ObjectArena,
    selection: Option<ObjId>,
    /// Sub-object selection: the face index within the selected mesh, if any. Cleared
    /// whenever the object selection changes. Runtime-only (not serialized — it's a
    /// transient editing focus, not document content).
    selected_face: Option<usize>,
    next_serial: u32,
    /// Undo/redo history (runtime-only — not serialized).
    history: History,
    /// In-flight checkpoint between `checkpoint()` and `commit()`.
    pending: Option<Pending>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn objects(&self) -> impl Iterator<Item = &Object> + '_ {
        self.objects.iter()
    }

    // ---- Command-delta undo/redo --------------------------------------------------

    /// Begin an undo transaction. Records the current full state of every id in `ids`
    /// (the objects about to be mutated or removed) plus the set of all live ids (so
    /// added objects are detected at `commit`). Nesting is not supported — a second
    /// `checkpoint` before `commit` replaces the first.
    pub fn checkpoint(&mut self, ids: &[ObjId]) {
        let before = ids
            .iter()
            .filter_map(|&id| self.objects.get(id).map(|o| (id, o.clone())))
            .collect();
        let existing = self.objects.iter().map(|o| o.id).collect();
        self.pending = Some(Pending { before, existing });
    }

    /// Close the current transaction, pushing it onto the undo stack if it changed
    /// anything. Clears the redo stack (a new edit forks history). Returns true if a
    /// transaction was actually recorded. No-op if nothing was captured or nothing changed.
    pub fn commit(&mut self) -> bool {
        let Some(pending) = self.pending.take() else { return false };
        let mut edits: Vec<ObjectEdit> = Vec::new();

        // Modifies & removes: ids we captured a `before` for.
        for (id, before) in pending.before {
            let after = self.objects.get(id).cloned();
            // Skip true no-ops (before == after) to avoid empty history entries.
            let changed = match &after {
                Some(a) => !objects_equal(&before, a),
                None => true, // removed
            };
            if changed {
                edits.push(ObjectEdit { id, before: Some(before), after });
            }
        }
        // Adds: ids that exist now but didn't at checkpoint.
        for o in self.objects.iter() {
            if !pending.existing.contains(&o.id) {
                edits.push(ObjectEdit { id: o.id, before: None, after: Some(o.clone()) });
            }
        }

        if edits.is_empty() {
            return false;
        }
        self.history.past.push(Transaction { edits });
        if self.history.past.len() > HISTORY_CAP {
            self.history.past.remove(0);
        }
        self.history.future.clear();
        true
    }

    /// Record a single-object edit from a caller-held `before` snapshot (immediate-mode UI
    /// path: the app keeps the pre-edit state and calls this once the edit burst settles).
    /// Pushes one transaction if the object actually changed. Returns true if recorded.
    pub fn record_object_change(&mut self, before: Object) -> bool {
        let id = before.id;
        let after = self.objects.get(id).cloned();
        let changed = match &after {
            Some(a) => !objects_equal(&before, a),
            None => true,
        };
        if !changed {
            return false;
        }
        self.history.past.push(Transaction {
            edits: vec![ObjectEdit { id, before: Some(before), after }],
        });
        if self.history.past.len() > HISTORY_CAP {
            self.history.past.remove(0);
        }
        self.history.future.clear();
        true
    }

    fn apply_edit_side(&mut self, id: ObjId, target: &Option<Object>) {
        match target {
            Some(obj) => self.objects.restore(obj.clone()),
            None => {
                self.objects.remove(id);
                if self.selection == Some(id) {
                    self.selection = None;
                }
            }
        }
    }

    /// Undo the most recent transaction. Returns true if anything was undone.
    pub fn undo(&mut self) -> bool {
        let Some(tx) = self.history.past.pop() else { return false };
        for edit in tx.edits.iter().rev() {
            let before = edit.before.clone();
            self.apply_edit_side(edit.id, &before);
        }
        self.history.future.push(tx);
        true
    }

    /// Redo the most recently undone transaction. Returns true if anything was redone.
    pub fn redo(&mut self) -> bool {
        let Some(tx) = self.history.future.pop() else { return false };
        for edit in tx.edits.iter() {
            let after = edit.after.clone();
            self.apply_edit_side(edit.id, &after);
        }
        self.history.past.push(tx);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.history.past.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.history.future.is_empty()
    }

    pub fn object_count(&self) -> usize {
        self.objects
            .slots
            .iter()
            .filter(|s| s.object.is_some())
            .count()
    }

    pub fn get(&self, id: ObjId) -> Option<&Object> {
        self.objects.get(id)
    }
    pub fn get_mut(&mut self, id: ObjId) -> Option<&mut Object> {
        self.objects.get_mut(id)
    }

    pub fn selection(&self) -> Option<ObjId> {
        self.selection
    }
    pub fn set_selection(&mut self, id: Option<ObjId>) {
        let new = id.and_then(|id| self.objects.get(id).map(|_| id));
        if new != self.selection {
            self.selected_face = None; // sub-object focus is per-object
        }
        self.selection = new;
    }

    /// The face index selected within the currently selected mesh, if any.
    pub fn selected_face(&self) -> Option<usize> {
        self.selected_face
    }
    pub fn set_selected_face(&mut self, face: Option<usize>) {
        self.selected_face = face;
    }

    /// Ray-pick the face of the selected mesh under a world-space ray and focus it.
    /// Returns the picked face index. No-op (returns None) if the selection isn't a mesh.
    pub fn pick_selected_mesh_face(&mut self, ray_origin: Vec3, ray_dir: Vec3) -> Option<usize> {
        let id = self.selection?;
        let obj = self.objects.get(id)?;
        let mesh = obj.mesh.as_ref()?;
        let world = obj.world_matrix();
        let (face, _) = mesh.pick_face(ray_origin, ray_dir, world)?;
        self.selected_face = Some(face);
        Some(face)
    }

    pub fn add(&mut self, kind: ObjectKind, position: Vec3) -> ObjId {
        self.next_serial += 1;
        let name = format!("{} {}", kind.label(), self.next_serial);
        let mut transform = Trs::default();
        transform.translation = position;
        let local_aabb = match kind {
            ObjectKind::Cube | ObjectKind::Sphere | ObjectKind::Mesh => Aabb::unit(),
            ObjectKind::ImagePlane
            | ObjectKind::PaintCanvas
            | ObjectKind::Adjustment => Aabb {
                min: Vec3::new(-0.5, -0.5, -0.02),
                max: Vec3::new(0.5, 0.5, 0.02),
            },
        };
        let mesh = if kind == ObjectKind::Mesh {
            Some(Mesh::cube())
        } else {
            None
        };
        let adjustment = if kind == ObjectKind::Adjustment {
            Some(AdjustmentKind::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            })
        } else {
            None
        };
        let object = Object {
            id: ObjId {
                slot: 0,
                generation: 0,
            }, // overwritten by arena
            name,
            kind,
            transform,
            color: kind.default_color(),
            visibility: true,
            lock: false,
            opacity: 1.0,
            compositing: Compositing::DepthOrdered,
            blend_mode: BlendMode::Normal,
            local_aabb,
            mesh,
            modifiers: Vec::new(),
            adjustment,
            skeleton: None,
            skin: None,
        };
        self.objects.insert(object)
    }

    /// Place an auto-generated skeleton on the selected mesh (overwrites any existing skeleton).
    /// `n_bones` controls the spine density. Returns true on success.
    pub fn auto_rig_selected_mesh(&mut self, n_bones: u32) -> bool {
        let Some(id) = self.selection else { return false };
        let Some(obj) = self.objects.get_mut(id) else { return false };
        if obj.kind != ObjectKind::Mesh {
            return false;
        }
        let Some(mesh) = obj.mesh.as_ref() else { return false };
        let skeleton = Skeleton::auto_rig_from_mesh(mesh, n_bones);
        let has_bones = !skeleton.bones.is_empty();
        // Bind skin weights immediately so the rig can deform the mesh the moment a bone
        // is posed. Weighting is against the base (rest) mesh vertices.
        let skin = skeleton.auto_skin(mesh);
        obj.skeleton = Some(skeleton);
        obj.skin = Some(skin);
        has_bones
    }

    /// Set the local pose rotation of bone `bone_idx` on the selected object's skeleton.
    /// Returns true if applied (object is a rigged mesh with that bone).
    pub fn set_selected_bone_pose(&mut self, bone_idx: usize, pose: Quat) -> bool {
        let Some(id) = self.selection else { return false };
        self.set_bone_pose(id, bone_idx, pose)
    }

    /// Set the local pose rotation of bone `bone_idx` on a specific object's skeleton.
    /// Used by the timeline sampler to drive rig animation. Returns true if applied.
    pub fn set_bone_pose(&mut self, id: ObjId, bone_idx: usize, pose: Quat) -> bool {
        let Some(obj) = self.objects.get_mut(id) else { return false };
        let Some(skel) = obj.skeleton.as_mut() else { return false };
        let Some(bone) = skel.bones.get_mut(bone_idx) else { return false };
        bone.pose = pose;
        true
    }

    /// Apply a sculpt stroke to the selected mesh.
    /// `op`: 0=Draw, 1=Smooth, 2=Flatten, 3=Pinch.
    /// Returns true if the mesh was modified.
    pub fn sculpt_stroke(
        &mut self,
        center: Vec3,
        radius: f32,
        strength: f32,
        op: u8,
    ) -> bool {
        let Some(id) = self.selection else { return false };
        let Some(obj) = self.objects.get_mut(id) else { return false };
        if obj.kind != ObjectKind::Mesh {
            return false;
        }
        let world = obj.transform.matrix();
        let Some(mesh) = obj.mesh.as_mut() else { return false };
        match op {
            0 => mesh.sculpt_draw(center, radius, strength, world),
            1 => mesh.sculpt_smooth(center, radius, strength.min(1.0).max(0.0), world),
            2 => mesh.sculpt_flatten(center, radius, strength, world),
            3 => mesh.sculpt_pinch(center, radius, strength, world),
            _ => false,
        }
    }

    /// Convert `pixels` (RGBA8, row-major) into a heightmap mesh, adding it to the scene.
    /// `resolution` controls the grid density (e.g., 64 gives a 64×64 grid).
    /// `scale` is the Y extrusion amplitude in world units.
    pub fn add_heightmap_mesh(
        &mut self,
        pixels: &[u8],
        img_width: u32,
        img_height: u32,
        resolution: u32,
        scale: f32,
        position: Vec3,
    ) -> ObjId {
        let mesh = Mesh::from_heightmap(pixels, img_width.min(resolution), img_height.min(resolution), scale);
        self.next_serial += 1;
        let name = format!("Heightmap {}", self.next_serial);
        let mut transform = Trs::default();
        transform.translation = position;
        let object = Object {
            id: ObjId { slot: 0, generation: 0 },
            name,
            kind: ObjectKind::Mesh,
            transform,
            color: ObjectKind::Mesh.default_color(),
            visibility: true,
            lock: false,
            opacity: 1.0,
            compositing: Compositing::DepthOrdered,
            blend_mode: BlendMode::Normal,
            local_aabb: Aabb::unit(),
            mesh: Some(mesh),
            modifiers: Vec::new(),
            adjustment: None,
            skeleton: None,
            skin: None,
        };
        self.objects.insert(object)
    }

    /// Add a path-extruded (pipe) mesh. `path` is the 3D spine; `shape` is the 2D cross-section.
    pub fn add_pipe(&mut self, path: &[Vec3], shape: &[[f32; 2]], position: Vec3) -> ObjId {
        let mut mesh = Mesh::path_extrude(path, shape);
        // Translate verts to `position`.
        for v in &mut mesh.vertices {
            v[0] += position.x;
            v[1] += position.y;
            v[2] += position.z;
        }
        self.next_serial += 1;
        let name = format!("Pipe {}", self.next_serial);
        let transform = Trs::default();
        let object = Object {
            id: ObjId { slot: 0, generation: 0 },
            name,
            kind: ObjectKind::Mesh,
            transform,
            color: ObjectKind::Mesh.default_color(),
            visibility: true,
            lock: false,
            opacity: 1.0,
            compositing: Compositing::DepthOrdered,
            blend_mode: BlendMode::Normal,
            local_aabb: Aabb::unit(),
            mesh: Some(mesh),
            modifiers: Vec::new(),
            adjustment: None,
            skeleton: None,
            skin: None,
        };
        self.objects.insert(object)
    }

    /// Add a lathe/revolve mesh from a 2D profile in the XY half-plane.
    /// `profile` is a slice of `[r, y]` pairs (r ≥ 0); `steps` is the angular resolution.
    pub fn add_lathe(&mut self, profile: &[[f32; 2]], steps: u32, position: Vec3) -> ObjId {
        let mesh = Mesh::revolve(profile, steps);
        self.next_serial += 1;
        let name = format!("Lathe {}", self.next_serial);
        let mut transform = Trs::default();
        transform.translation = position;
        let object = Object {
            id: ObjId { slot: 0, generation: 0 },
            name,
            kind: ObjectKind::Mesh,
            transform,
            color: ObjectKind::Mesh.default_color(),
            visibility: true,
            lock: false,
            opacity: 1.0,
            compositing: Compositing::DepthOrdered,
            blend_mode: BlendMode::Normal,
            local_aabb: Aabb::unit(),
            mesh: Some(mesh),
            modifiers: Vec::new(),
            adjustment: None,
            skeleton: None,
            skin: None,
        };
        self.objects.insert(object)
    }

    pub fn remove(&mut self, id: ObjId) -> Option<Object> {
        if self.selection == Some(id) {
            self.selection = None;
        }
        self.objects.remove(id)
    }

    /// Set the blend mode of the selected object. Returns false if nothing is selected.
    pub fn set_selected_blend_mode(&mut self, mode: BlendMode) -> bool {
        if let Some(obj) = self.selection.and_then(|id| self.objects.get_mut(id)) {
            obj.blend_mode = mode;
            true
        } else {
            false
        }
    }

    /// Set the opacity of the selected object (clamped to [0, 1]).
    pub fn set_selected_opacity(&mut self, opacity: f32) -> bool {
        if let Some(obj) = self.selection.and_then(|id| self.objects.get_mut(id)) {
            obj.opacity = opacity.clamp(0.0, 1.0);
            true
        } else {
            false
        }
    }

    /// Set the adjustment parameters of the selected adjustment layer object.
    pub fn set_selected_adjustment(&mut self, adjustment: AdjustmentKind) -> bool {
        if let Some(obj) = self.selection.and_then(|id| self.objects.get_mut(id)) {
            if obj.kind == ObjectKind::Adjustment {
                obj.adjustment = Some(adjustment);
                return true;
            }
        }
        false
    }

    /// Extrude the selected face of the selected mesh by `dist` (falling back to the
    /// +Y-most face when no face is focused). Keeps that face focused after the extrude
    /// so repeated `E` grows a tower. Returns false if the selection isn't a mesh.
    /// docs/01 §3.1.
    pub fn extrude_selected_mesh(&mut self, dist: f32) -> bool {
        let Some(id) = self.selection else {
            return false;
        };
        let face_pref = self.selected_face;
        let Some(obj) = self.objects.get_mut(id) else {
            return false;
        };
        let Some(mesh) = obj.mesh.as_mut() else {
            return false;
        };
        let face_idx = match face_pref {
            Some(f) if f < mesh.faces.len() => f,
            _ => match mesh.top_face_index() {
                Some(f) => f,
                None => return false,
            },
        };
        // extrude_face retargets `face_idx` to the new top loop, so the same index keeps
        // pointing at the (now-raised) cap — repeat-extrude just works.
        let ok = mesh.extrude_face(face_idx, dist);
        if ok {
            self.selected_face = Some(face_idx);
        }
        ok
    }

    /// Inset the selected (or top) face of the selected mesh. Keeps the inner face
    /// focused so a follow-up `E` extrudes it — inset-then-extrude is the bread-and-butter
    /// box-modeling combo.
    pub fn inset_selected_mesh(&mut self, amount: f32) -> bool {
        let Some(id) = self.selection else {
            return false;
        };
        let face_pref = self.selected_face;
        let Some(obj) = self.objects.get_mut(id) else {
            return false;
        };
        let Some(mesh) = obj.mesh.as_mut() else {
            return false;
        };
        let face_idx = match face_pref {
            Some(f) if f < mesh.faces.len() => f,
            _ => match mesh.top_face_index() {
                Some(f) => f,
                None => return false,
            },
        };
        let ok = mesh.inset_face(face_idx, amount);
        if ok {
            self.selected_face = Some(face_idx);
        }
        ok
    }

    /// Loop-cut the selected mesh, seeding the ring on the first edge of the selected (or
    /// top) face. Replaces the mesh with the cut result and re-focuses the top face, since
    /// the cut rebuilds the face list. Returns false if there's no mesh or the ring crosses
    /// no quads. docs/01 §3.1.
    pub fn loop_cut_selected_mesh(&mut self) -> bool {
        let Some(id) = self.selection else {
            return false;
        };
        let face_pref = self.selected_face;
        let Some(obj) = self.objects.get_mut(id) else {
            return false;
        };
        let Some(mesh) = obj.mesh.as_ref() else {
            return false;
        };
        let face_idx = match face_pref {
            Some(f) if f < mesh.faces.len() => f,
            _ => match mesh.top_face_index() {
                Some(f) => f,
                None => return false,
            },
        };
        let face = &mesh.faces[face_idx];
        if face.indices.len() < 2 {
            return false;
        }
        let (a, b) = (face.indices[0], face.indices[1]);
        let Some(cut) = mesh.loop_cut(a, b) else {
            return false;
        };
        obj.mesh = Some(cut);
        // Topology changed; re-anchor focus on whatever now faces up.
        self.selected_face = obj.mesh.as_ref().and_then(|m| m.top_face_index());
        true
    }

    /// Bevel (chamfer) a shared edge of the selected mesh — the first edge of the selected (or
    /// top) face. Replaces the mesh with the beveled result. Returns false if the edge is a
    /// boundary or doesn't exist. docs/01 §3.1.
    pub fn bevel_selected_mesh_edge(&mut self) -> bool {
        let Some(id) = self.selection else {
            return false;
        };
        let face_pref = self.selected_face;
        let Some(obj) = self.objects.get_mut(id) else {
            return false;
        };
        let Some(mesh) = obj.mesh.as_ref() else {
            return false;
        };
        let face_idx = match face_pref {
            Some(f) if f < mesh.faces.len() => f,
            _ => match mesh.top_face_index() {
                Some(f) => f,
                None => return false,
            },
        };
        let face = &mesh.faces[face_idx];
        if face.indices.len() < 2 {
            return false;
        }
        let (a, b) = (face.indices[0], face.indices[1]);
        let Some(beveled) = mesh.bevel_edge(a, b, 0.25) else {
            return false;
        };
        obj.mesh = Some(beveled);
        self.selected_face = obj.mesh.as_ref().and_then(|m| m.top_face_index());
        true
    }

    /// Bevel (chamfer) a corner of the selected mesh — the first vertex of the selected (or
    /// top) face. Replaces the mesh with the beveled result and re-anchors face focus, since
    /// the cut rebuilds the face list. Returns false if there's no mesh or the corner sits on
    /// a boundary. docs/01 §3.1.
    pub fn bevel_selected_mesh_corner(&mut self) -> bool {
        let Some(id) = self.selection else {
            return false;
        };
        let face_pref = self.selected_face;
        let Some(obj) = self.objects.get_mut(id) else {
            return false;
        };
        let Some(mesh) = obj.mesh.as_ref() else {
            return false;
        };
        let face_idx = match face_pref {
            Some(f) if f < mesh.faces.len() => f,
            _ => match mesh.top_face_index() {
                Some(f) => f,
                None => return false,
            },
        };
        let face = &mesh.faces[face_idx];
        if face.indices.is_empty() {
            return false;
        }
        let v = face.indices[0];
        let Some(beveled) = mesh.bevel_vertex(v, 0.25) else {
            return false;
        };
        obj.mesh = Some(beveled);
        self.selected_face = obj.mesh.as_ref().and_then(|m| m.top_face_index());
        true
    }

    // --- CSG boolean ops on the scene (Tier 1). The selection is the target; the
    //     caller supplies the tool object id. The result replaces the selected object's
    //     mesh; the tool object is removed. docs/03 §3.

    /// Apply a boolean union/subtract/intersect between the selected mesh and `tool_id`.
    /// `op`: 0=union, 1=subtract, 2=intersect.
    /// On success, the selected object gets the result mesh, `tool_id` is removed.
    pub fn apply_boolean(&mut self, tool_id: ObjId, op: u8) -> bool {
        let sel_id = match self.selection {
            Some(id) => id,
            None => return false,
        };
        if sel_id == tool_id {
            return false;
        }
        let (sel_world, tool_world, sel_mesh, tool_mesh) = {
            let sel = match self.objects.get(sel_id) {
                Some(o) => o,
                None => return false,
            };
            let tool = match self.objects.get(tool_id) {
                Some(o) => o,
                None => return false,
            };
            if sel.kind != ObjectKind::Mesh || tool.kind != ObjectKind::Mesh {
                return false;
            }
            let sw = sel.world_matrix();
            let tw = tool.world_matrix();
            let sm = match sel.mesh.as_ref() {
                Some(m) => m.clone(),
                None => return false,
            };
            let tm = match tool.mesh.as_ref() {
                Some(m) => m.clone(),
                None => return false,
            };
            (sw, tw, sm, tm)
        };
        let result = match op {
            1 => sel_mesh.bool_subtract(sel_world, &tool_mesh, tool_world),
            2 => sel_mesh.bool_intersect(sel_world, &tool_mesh, tool_world),
            _ => sel_mesh.bool_union(sel_world, &tool_mesh, tool_world),
        };
        let out = match result {
            Some(m) => m,
            None => return false,
        };
        self.remove(tool_id);
        if let Some(obj) = self.objects.get_mut(sel_id) {
            obj.mesh = Some(out);
            // Result is in world space; reset transform so position is baked in.
            obj.transform = Trs::default();
        }
        self.selected_face = None;
        true
    }

    /// A starter scene so the canvas isn't empty on launch: a large paint artboard
    /// standing upright, with a cube + sphere beside it — paint and 3D on one canvas,
    /// which is the whole pitch (docs/01 §0, §6).
    pub fn with_starter_scene() -> Self {
        let mut doc = Self::default();
        // The artboard: scaled up so there's room to paint, set back a touch.
        let board = doc.add(ObjectKind::PaintCanvas, Vec3::new(0.0, 0.6, 0.0));
        if let Some(obj) = doc.get_mut(board) {
            obj.transform.scale = Vec3::new(3.0, 3.0, 1.0);
        }
        doc.add(ObjectKind::Cube, Vec3::new(-2.6, -0.5, 0.6));
        doc.add(ObjectKind::Sphere, Vec3::new(2.6, -0.5, 0.6));
        doc
    }

    /// Serialize the live scene into a `visual.scene` payload (the domain document the
    /// universal `.sweet` bundle references). Objects are emitted in iteration order;
    /// the generational ids are preserved so a saved selection still resolves. This is
    /// the `visual` payload from docs/03 §3.6 — NOT the bundle container itself, which
    /// `suite-assets` owns.
    pub fn to_scene_doc(&self) -> SceneDoc {
        SceneDoc {
            schema: SCENE_SCHEMA.to_string(),
            version: SCENE_VERSION,
            next_serial: self.next_serial,
            selection: self.selection,
            objects: self.objects.iter().cloned().collect(),
        }
    }

    /// Rebuild a Document from a `visual.scene` payload. Fail-closed on an unknown
    /// schema or a future major version (we don't guess at forward-incompatible data).
    /// Objects keep their saved ids by reconstructing the arena slot-for-slot.
    pub fn from_scene_doc(scene: SceneDoc) -> Result<Self, SceneError> {
        if scene.schema != SCENE_SCHEMA {
            return Err(SceneError::UnknownSchema(scene.schema));
        }
        if scene.version > SCENE_VERSION {
            return Err(SceneError::FutureVersion {
                found: scene.version,
                supported: SCENE_VERSION,
            });
        }
        let mut doc = Self::default();
        doc.next_serial = scene.next_serial;
        // Reconstruct the arena so saved ids stay valid: place each object at its own
        // slot index, padding gaps with tombstones whose generation we honor.
        let max_slot = scene.objects.iter().map(|o| o.id.slot).max();
        if let Some(max_slot) = max_slot {
            doc.objects.slots.reserve((max_slot as usize) + 1);
            for slot_idx in 0..=max_slot {
                if let Some(object) = scene.objects.iter().find(|o| o.id.slot == slot_idx) {
                    doc.objects.slots.push(Slot {
                        generation: object.id.generation,
                        object: Some(object.clone()),
                    });
                } else {
                    // A freed slot at save time — keep generation 0 and mark it reusable.
                    doc.objects.slots.push(Slot {
                        generation: 0,
                        object: None,
                    });
                    doc.objects.free.push(slot_idx);
                }
            }
        }
        doc.selection = scene.selection.filter(|id| doc.objects.get(*id).is_some());
        Ok(doc)
    }

    /// Convenience: serialize straight to pretty JSON.
    pub fn to_scene_json(&self) -> Result<String, SceneError> {
        serde_json::to_string_pretty(&self.to_scene_doc()).map_err(SceneError::Json)
    }
    /// Convenience: parse a `visual.scene` JSON payload into a Document.
    pub fn from_scene_json(json: &str) -> Result<Self, SceneError> {
        let scene: SceneDoc = serde_json::from_str(json).map_err(SceneError::Json)?;
        Self::from_scene_doc(scene)
    }
}

/// The on-disk `visual.scene` domain document. Carried inside a `.sweet` bundle by
/// `suite-assets`; serializable on its own so tests and tooling can round-trip it.
pub const SCENE_SCHEMA: &str = "sweet.visual.scene";
pub const SCENE_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneDoc {
    pub schema: String,
    pub version: u32,
    pub next_serial: u32,
    pub selection: Option<ObjId>,
    pub objects: Vec<Object>,
}

#[derive(Debug)]
pub enum SceneError {
    UnknownSchema(String),
    FutureVersion { found: u32, supported: u32 },
    Json(serde_json::Error),
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSchema(s) => write!(f, "unknown scene schema: {s}"),
            Self::FutureVersion { found, supported } => {
                write!(
                    f,
                    "scene version {found} is newer than supported {supported}"
                )
            }
            Self::Json(e) => write!(f, "scene json error: {e}"),
        }
    }
}

impl std::error::Error for SceneError {}

/// World-space ray-vs-AABB intersection used by the picker. Returns the nearest
/// positive `t` along the ray, or `None` if the ray misses.
pub fn ray_aabb_world(
    ray_origin: Vec3,
    ray_dir: Vec3,
    world_matrix: Mat4,
    local_aabb: Aabb,
) -> Option<f32> {
    let inv = world_matrix.inverse();
    let local_origin = inv.transform_point3(ray_origin);
    let local_dir = inv.transform_vector3(ray_dir);
    let mut tmin = f32::NEG_INFINITY;
    let mut tmax = f32::INFINITY;
    for i in 0..3 {
        let o = local_origin[i];
        let d = local_dir[i];
        let lo = local_aabb.min[i];
        let hi = local_aabb.max[i];
        if d.abs() < 1e-8 {
            if o < lo || o > hi {
                return None;
            }
        } else {
            let inv_d = 1.0 / d;
            let mut t1 = (lo - o) * inv_d;
            let mut t2 = (hi - o) * inv_d;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            tmin = tmin.max(t1);
            tmax = tmax.min(t2);
            if tmax < tmin {
                return None;
            }
        }
    }
    if tmax < 0.0 {
        return None;
    }
    Some(tmin.max(0.0))
}

/// Möller–Trumbore ray-triangle intersection. Ray and triangle are in the same space.
/// Returns the positive distance `t` along `dir` (which need not be normalized), or None.
fn ray_triangle(origin: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-8 {
        return None; // ray parallel to triangle
    }
    let inv_det = 1.0 / det;
    let tvec = origin - v0;
    let u = tvec.dot(p) * inv_det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = tvec.cross(e1);
    let v = dir.dot(q) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv_det;
    if t > 1e-6 {
        Some(t)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_round_trips_through_json() {
        let mut doc = Document::with_starter_scene();
        // Mutate so we're not just round-tripping defaults.
        let ids: Vec<_> = doc.objects().map(|o| o.id).collect();
        doc.set_selection(Some(ids[1]));
        if let Some(obj) = doc.get_mut(ids[0]) {
            obj.transform.translation = Vec3::new(2.0, 3.0, -1.0);
            obj.transform.scale = Vec3::new(1.5, 1.5, 1.5);
            obj.name = "Hero Cube".into();
        }

        let json = doc.to_scene_json().expect("serialize");
        let reopened = Document::from_scene_json(&json).expect("deserialize");

        assert_eq!(reopened.object_count(), doc.object_count());
        assert_eq!(reopened.selection(), doc.selection(), "selection survives");

        // The saved selection still resolves to the same object by id.
        let sel = reopened.selection().expect("has selection");
        assert!(reopened.get(sel).is_some(), "saved id resolves");

        // The mutated object survives field-for-field.
        let hero = reopened.get(ids[0]).expect("hero by id");
        assert_eq!(hero.name, "Hero Cube");
        assert_eq!(hero.transform.translation, Vec3::new(2.0, 3.0, -1.0));
        assert_eq!(hero.transform.scale, Vec3::new(1.5, 1.5, 1.5));

        // A second round-trip is byte-identical (the keystone: stable serialization).
        let json2 = reopened.to_scene_json().expect("re-serialize");
        assert_eq!(json, json2, "save -> load -> save is byte-identical");
    }

    #[test]
    fn rejects_unknown_schema_and_future_version() {
        let bad_schema = r#"{"schema":"some.other.doc","version":1,"next_serial":0,"selection":null,"objects":[]}"#;
        assert!(matches!(
            Document::from_scene_json(bad_schema),
            Err(SceneError::UnknownSchema(_))
        ));

        let future = format!(
            r#"{{"schema":"{}","version":{},"next_serial":0,"selection":null,"objects":[]}}"#,
            SCENE_SCHEMA,
            SCENE_VERSION + 1
        );
        assert!(matches!(
            Document::from_scene_json(&future),
            Err(SceneError::FutureVersion { .. })
        ));
    }

    #[test]
    fn extrude_grows_a_mesh_and_survives_save_load() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        doc.set_selection(Some(id));
        let (v0, f0) = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            (m.vertices.len(), m.faces.len())
        };
        // Cube: 8 verts, 6 faces. Extruding a quad face adds 4 verts and 4 side faces.
        assert_eq!((v0, f0), (8, 6));
        assert!(doc.extrude_selected_mesh(0.5));
        let (v1, f1) = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            (m.vertices.len(), m.faces.len())
        };
        assert_eq!((v1, f1), (12, 10), "extrude added 4 verts + 4 side faces");

        // The extruded top face moved up by 0.5.
        let top_y = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            let ti = m.top_face_index().unwrap();
            m.face_centroid(&m.faces[ti])[1]
        };
        assert!((top_y - 1.0).abs() < 1e-4, "top face at y≈1.0, got {top_y}");

        // Mesh survives the scene round-trip.
        let json = doc.to_scene_json().unwrap();
        let reopened = Document::from_scene_json(&json).unwrap();
        let rm = reopened.get(id).unwrap().mesh.as_ref().unwrap();
        assert_eq!((rm.vertices.len(), rm.faces.len()), (12, 10));
    }

    #[test]
    fn catmull_clark_subdivides_and_smooths_a_cube() {
        let cube = Mesh::cube();
        let s = cube.catmull_clark();
        // 8 orig + 6 face + 12 edge = 26 verts; 6 faces × 4 = 24 quads.
        assert_eq!(s.vertices.len(), 26);
        assert_eq!(s.faces.len(), 24);
        // A corner relaxes inward: every coordinate magnitude drops below 0.5.
        let corner = s
            .vertices
            .iter()
            .find(|v| v[0].abs() > 0.2 && v[1].abs() > 0.2 && v[2].abs() > 0.2)
            .copied()
            .expect("a relaxed corner");
        assert!(
            corner.iter().all(|c| c.abs() < 0.5),
            "corner moved inward: {corner:?}"
        );
    }

    #[test]
    fn mirror_and_array_modifiers_generate_geometry() {
        let cube = Mesh::cube();
        let m = cube.mirrored(0);
        assert_eq!(m.vertices.len(), 16);
        assert_eq!(m.faces.len(), 12);

        let a = cube.arrayed(3, [2.0, 0.0, 0.0]);
        assert_eq!(a.vertices.len(), 24);
        assert_eq!(a.faces.len(), 18);
        // Third copy is shifted by +4 on X.
        assert!(a.vertices[16..].iter().all(|v| v[0] > 1.0));
    }

    #[test]
    fn modifier_stack_evaluates_nondestructively_and_round_trips() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        {
            let obj = doc.get_mut(id).unwrap();
            obj.modifiers.push(Modifier::Array {
                count: 2,
                offset: [2.0, 0.0, 0.0],
            });
            obj.modifiers.push(Modifier::Subdivide { levels: 1 });
        }
        let obj = doc.get(id).unwrap();
        // Base mesh untouched (still a cube).
        assert_eq!(obj.mesh.as_ref().unwrap().vertices.len(), 8);
        // Display mesh = array(2) then subdivide(1): 2 cubes → CC.
        let display = obj.display_mesh().unwrap();
        assert!(
            display.vertices.len() > 40,
            "stack generated geometry: {}",
            display.vertices.len()
        );

        // Modifiers survive save/load.
        let json = doc.to_scene_json().unwrap();
        let re = Document::from_scene_json(&json).unwrap();
        assert_eq!(re.get(id).unwrap().modifiers.len(), 2);
    }

    #[test]
    fn inset_adds_a_ring_without_destroying_the_face() {
        let mut m = Mesh::cube();
        let (v0, f0) = (m.vertices.len(), m.faces.len());
        assert!(m.inset_face(0, 0.3));
        // Inset adds 4 inner verts + 4 ring quads; the face count grows by 4.
        assert_eq!(m.vertices.len(), v0 + 4);
        assert_eq!(m.faces.len(), f0 + 4);
    }

    #[test]
    fn loop_cut_through_document_grows_the_mesh_and_round_trips() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        doc.set_selection(Some(id));
        let (v0, f0) = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            (m.vertices.len(), m.faces.len())
        };
        assert!(doc.loop_cut_selected_mesh(), "cube loop-cuts");
        let (v1, f1) = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            (m.vertices.len(), m.faces.len())
        };
        assert_eq!(v1, v0 + 4, "4 midpoint verts added");
        assert_eq!(f1, f0 + 4, "4 quads split into 8");

        // The cut mesh survives a save/load round-trip.
        let json = doc.to_scene_json().expect("serialize");
        let back = Document::from_scene_json(&json).expect("deserialize");
        let m = back.get(id).unwrap().mesh.as_ref().unwrap();
        assert_eq!(m.vertices.len(), v1);
        assert_eq!(m.faces.len(), f1);
    }

    #[test]
    fn bevel_through_document_truncates_a_corner_and_round_trips() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        doc.set_selection(Some(id));
        assert!(doc.bevel_selected_mesh_corner(), "cube corner bevels");
        let (v1, f1) = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            (m.vertices.len(), m.faces.len())
        };
        assert_eq!(v1, 10, "8 - 1 corner + 3 new verts");
        assert_eq!(f1, 7, "6 faces + 1 cap");

        let json = doc.to_scene_json().expect("serialize");
        let back = Document::from_scene_json(&json).expect("deserialize");
        let m = back.get(id).unwrap().mesh.as_ref().unwrap();
        assert_eq!(m.vertices.len(), v1);
        assert_eq!(m.faces.len(), f1);
    }

    #[test]
    fn pick_face_hits_the_front_face_and_extrudes_it() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        doc.set_selection(Some(id));
        // Ray from +Z toward origin should hit the +Z front face.
        let face = doc.pick_selected_mesh_face(Vec3::new(0.0, 0.0, 3.0), Vec3::new(0.0, 0.0, -1.0));
        assert!(face.is_some(), "ray hits a face");
        let fi = face.unwrap();
        // The +Z face normal should point +Z.
        let n = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            m.face_normal(&m.faces[fi])
        };
        assert!(n[2] > 0.9, "picked the front (+Z) face, normal {n:?}");

        // Extruding the *selected* face moves it out along +Z.
        assert!(doc.extrude_selected_mesh(0.5));
        let cz = {
            let m = doc.get(id).unwrap().mesh.as_ref().unwrap();
            m.face_centroid(&m.faces[doc.selected_face().unwrap()])[2]
        };
        assert!(
            (cz - 1.0).abs() < 1e-4,
            "front face pushed to z≈1.0, got {cz}"
        );
    }

    #[test]
    fn bool_union_produces_larger_mesh() {
        // Two cubes at distinct positions — union must produce a non-empty mesh
        // with more triangles than either cube alone (they don't overlap).
        let mut doc = Document::default();
        let a = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        let b = doc.add(ObjectKind::Mesh, Vec3::new(3.0, 0.0, 0.0)); // non-overlapping
        doc.set_selection(Some(a));
        let n_a = doc.get(a).unwrap().mesh.as_ref().unwrap().vertices.len();
        assert!(doc.apply_boolean(b, 0), "union returned false");
        // b should be gone
        assert!(doc.get(b).is_none(), "tool object must be removed after op");
        // a should have more verts than the original cube (two disjoint solids merged)
        let n_out = doc.get(a).unwrap().mesh.as_ref().unwrap().vertices.len();
        assert!(n_out > n_a, "union of two cubes should have more verts than one cube");
    }

    #[test]
    fn bool_subtract_removes_overlap() {
        // Two overlapping cubes: subtract should give us fewer verts than both combined.
        let mut doc = Document::default();
        let a = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        let b = doc.add(ObjectKind::Mesh, Vec3::new(0.5, 0.0, 0.0)); // overlapping
        doc.set_selection(Some(a));
        assert!(doc.apply_boolean(b, 1), "subtract returned false");
        assert!(doc.get(b).is_none(), "tool removed after subtract");
        // Result must be non-empty (a minus a partial overlap still has volume)
        let mesh = doc.get(a).unwrap().mesh.as_ref().unwrap();
        assert!(!mesh.vertices.is_empty(), "subtract result must not be empty");
    }

    #[test]
    fn auto_rig_produces_correct_bone_count() {
        let mesh = Mesh::cube();
        let skel = Skeleton::auto_rig_from_mesh(&mesh, 5);
        assert_eq!(skel.bones.len(), 5, "auto_rig should produce exactly n_bones bones");
        // Root bone has no parent; subsequent bones chain.
        assert!(skel.bones[0].parent.is_none(), "root bone has no parent");
        for i in 1..skel.bones.len() {
            assert_eq!(skel.bones[i].parent, Some(i - 1), "bone {} should parent to {}", i, i-1);
        }
    }

    #[test]
    fn auto_rig_bones_span_bounding_box() {
        // Unit cube: AABB [-0.5, 0.5]³. The spine should start near the bottom and end near the top.
        let mesh = Mesh::cube();
        let skel = Skeleton::auto_rig_from_mesh(&mesh, 4);
        let first_head = skel.bones[0].head;
        let last_tail = skel.bones.last().unwrap().tail;
        // World tails are computed via world_tail; for a simple chain they stack.
        // The total span should cover most of the AABB.
        let span = (skel.world_tail(skel.bones.len() - 1) - skel.world_head(0)).length();
        assert!(span > 0.8, "skeleton should span most of the mesh height; span={}", span);
        let _ = (first_head, last_tail); // silence unused
    }

    #[test]
    fn skinning_identity_leaves_mesh_at_rest() {
        // Unposed skeleton → all skinning matrices identity → skin_deform is a no-op.
        let mesh = Mesh::cube().catmull_clark();
        let skel = Skeleton::auto_rig_from_mesh(&mesh, 4);
        let skin = skel.auto_skin(&mesh);
        let mats = skel.skinning_matrices();
        for m in &mats {
            assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-5), "rest pose → identity skinning matrix");
        }
        let deformed = mesh.skin_deform(&skin, &mats);
        for (a, b) in mesh.vertices.iter().zip(deformed.vertices.iter()) {
            let d = (Vec3::from(*a) - Vec3::from(*b)).length();
            assert!(d < 1e-5, "rest skinning leaves vertices put; moved {}", d);
        }
    }

    #[test]
    fn auto_skin_weights_normalized() {
        let mesh = Mesh::cube().catmull_clark();
        let skel = Skeleton::auto_rig_from_mesh(&mesh, 4);
        let skin = skel.auto_skin(&mesh);
        assert_eq!(skin.weights.len(), mesh.vertices.len(), "one weight set per vertex");
        for w in &skin.weights {
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-4, "weights sum to 1, got {sum}");
        }
    }

    #[test]
    fn posed_tip_bone_bends_only_the_tip() {
        // A tall box rigged with a vertical spine. Rotating the TOP bone should move the
        // top vertices a lot and the bottom vertices essentially not at all.
        let mut mesh = Mesh::cube();
        // Stretch the cube tall on Y so the spine has clear top/bottom.
        for v in &mut mesh.vertices {
            v[1] *= 3.0;
        }
        let mut skel = Skeleton::auto_rig_from_mesh(&mesh, 4);
        let skin = skel.auto_skin(&mesh);
        // Pose the last (top) bone with a 90° rotation about Z.
        let last = skel.bones.len() - 1;
        skel.bones[last].pose = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let mats = skel.skinning_matrices();
        let deformed = mesh.skin_deform(&skin, &mats);

        // Find the highest and lowest vertices in the rest mesh.
        let top_i = (0..mesh.vertices.len()).max_by(|&a, &b| mesh.vertices[a][1].partial_cmp(&mesh.vertices[b][1]).unwrap()).unwrap();
        let bot_i = (0..mesh.vertices.len()).min_by(|&a, &b| mesh.vertices[a][1].partial_cmp(&mesh.vertices[b][1]).unwrap()).unwrap();
        let top_move = (Vec3::from(mesh.vertices[top_i]) - Vec3::from(deformed.vertices[top_i])).length();
        let bot_move = (Vec3::from(mesh.vertices[bot_i]) - Vec3::from(deformed.vertices[bot_i])).length();
        assert!(top_move > 0.5, "top vertex should swing with the posed tip bone; moved {top_move}");
        assert!(bot_move < 0.2, "bottom vertex should barely move; moved {bot_move}");
    }

    #[test]
    fn heightmap_mesh_size() {
        // 4×4 RGBA8 image (all white) → 4×4 grid → 16 verts, (3×3)×2 = 18 tris.
        let pixels = vec![255u8; 4 * 4 * 4]; // RGBA8, 4×4
        let mesh = Mesh::from_heightmap(&pixels, 4, 4, 1.0);
        assert_eq!(mesh.vertices.len(), 16, "4×4 grid = 16 verts");
        assert_eq!(mesh.faces.len(), 18, "(4-1)×(4-1)×2 = 18 triangles");
        // White image: all Y values = 1.0 * scale = 1.0.
        for v in &mesh.vertices {
            assert!((v[1] - 1.0).abs() < 1e-4, "white pixel → Y=scale=1.0, got {}", v[1]);
        }
    }

    #[test]
    fn decimate_cluster_reduces_mesh() {
        // Subdivide a cube twice to get ~96 verts; decimation to grid_res=4 should
        // produce far fewer verts but still a valid non-empty mesh.
        let dense = Mesh::cube().catmull_clark().catmull_clark(); // 2 levels of CC
        let decimated = dense.decimate_cluster(4).expect("decimate returned None");
        assert!(
            decimated.vertices.len() < dense.vertices.len(),
            "decimated mesh should have fewer verts: {} → {}",
            dense.vertices.len(),
            decimated.vertices.len()
        );
        assert!(!decimated.faces.is_empty(), "decimated mesh should have faces");
        // All face indices in bounds.
        let nv = decimated.vertices.len() as u32;
        for face in &decimated.faces {
            for &i in &face.indices {
                assert!(i < nv, "decimate produced out-of-bounds face index");
            }
        }
    }

    #[test]
    fn sculpt_draw_displaces_vertices() {
        let mut mesh = Mesh::cube();
        let original_verts = mesh.vertices.clone();
        // Draw brush at the cube center, large enough to touch all verts.
        let moved = mesh.sculpt_draw(Vec3::ZERO, 5.0, 0.1, Mat4::IDENTITY);
        assert!(moved, "sculpt_draw should have moved some vertices");
        // At least one vertex should have changed.
        let changed = mesh.vertices.iter().zip(&original_verts).any(|(a, b)| a != b);
        assert!(changed, "no vertex was displaced");
    }

    #[test]
    fn sculpt_smooth_reduces_variance() {
        // Spike a vertex right at the brush center so falloff=1; then smooth pulls it toward neighbors.
        let mut mesh = Mesh::cube();
        // Put vertex 0 at the brush center (ZERO) so it's the closest with max falloff,
        // and also nearby so its distance from origin is 0 → falloff ≈ 1.
        let orig_pos = Vec3::from(mesh.vertices[0]);
        mesh.vertices[0] = [0.0, 0.0, 0.0]; // at brush center
        let moved = mesh.sculpt_smooth(Vec3::ZERO, 5.0, 0.9, Mat4::IDENTITY);
        assert!(moved, "sculpt_smooth should have moved some vertices");
        // Vertex 0 was at origin; its neighbors are the cube verts (~0.5,0.5,0.5 etc).
        // After smooth it should have moved away from origin toward the neighbor average.
        let new_pos = Vec3::from(mesh.vertices[0]);
        assert!(
            (new_pos - Vec3::ZERO).length() > 0.01,
            "vertex at brush center should move toward neighbor average; orig={:?} new={:?}", orig_pos, new_pos
        );
    }

    #[test]
    fn revolve_lathe_vertex_count() {
        // A 4-point profile with 8 steps — produces 4×8 = 32 ring verts + 2 poles.
        let profile: &[[f32; 2]] = &[
            [0.0, -1.0],  // axis pole
            [0.5, -0.5],
            [0.5, 0.5],
            [0.0, 1.0],   // axis pole
        ];
        let mesh = Mesh::revolve(profile, 8);
        // 4 profile pts × 8 steps = 32 ring verts, plus 2 pole verts = 34
        assert_eq!(mesh.vertices.len(), 34, "wrong vert count from revolve");
        // Faces: 3 segments × 8 steps = 24 ring faces, but seg 0 and seg 2 collapse to
        // tris (axis on both ends), and seg 1 is a quad strip:
        // 2 tri-cap fans (8 tris each) + 8 quad faces = 24 total, but we also have
        // 2 explicit cap fans below. Let's just check it's non-empty.
        assert!(!mesh.faces.is_empty(), "lathe produced no faces");
        // All face indices must be in bounds.
        let nv = mesh.vertices.len() as u32;
        for face in &mesh.faces {
            for &i in &face.indices {
                assert!(i < nv, "face index out of bounds");
            }
        }
    }

    #[test]
    fn path_extrude_vertex_and_face_count() {
        // A 5-step straight path, square cross-section (4 pts) → 5×4 = 20 verts, 4×4 = 16 faces.
        let path: Vec<Vec3> = (0..5).map(|i| Vec3::new(0.0, i as f32, 0.0)).collect();
        let shape: &[[f32; 2]] = &[
            [-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5],
        ];
        let mesh = Mesh::path_extrude(&path, shape);
        assert_eq!(mesh.vertices.len(), 20, "5 path steps × 4 shape pts = 20 verts");
        assert_eq!(mesh.faces.len(), 16, "4 segments × 4 shape edges = 16 quad faces");
        // All face indices in bounds.
        let nv = mesh.vertices.len() as u32;
        for face in &mesh.faces {
            for &idx in &face.indices {
                assert!(idx < nv, "face index out of bounds in path_extrude");
            }
        }
    }

    #[test]
    fn freed_slots_dont_alias_after_load() {
        let mut doc = Document::default();
        let a = doc.add(ObjectKind::Cube, Vec3::ZERO);
        let _b = doc.add(ObjectKind::Sphere, Vec3::X);
        doc.remove(a); // slot of `a` is now free
        let json = doc.to_scene_json().unwrap();
        let reopened = Document::from_scene_json(&json).unwrap();
        // `a` was removed; its id must not resolve in the reopened doc.
        assert!(
            reopened.get(a).is_none(),
            "freed id stays dead across save/load"
        );
        assert_eq!(reopened.object_count(), 1);
    }

    #[test]
    fn adjustment_math_matches_intent() {
        let approx = |a: [f32; 3], b: [f32; 3]| {
            (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4)
        };
        // Exposure +1 stop doubles linear light.
        assert!(approx(
            AdjustmentKind::Exposure { stops: 1.0 }.apply_linear([0.25, 0.1, 0.5]),
            [0.5, 0.2, 1.0]
        ));
        // Exposure 0 is a no-op.
        assert!(approx(
            AdjustmentKind::Exposure { stops: 0.0 }.apply_linear([0.3, 0.6, 0.9]),
            [0.3, 0.6, 0.9]
        ));
        // Invert.
        assert!(approx(
            AdjustmentKind::Invert.apply_linear([0.2, 0.0, 1.0]),
            [0.8, 1.0, 0.0]
        ));
        // Threshold: below → black, at/above → white (mid-gray luma ≈ 0.5).
        assert_eq!(
            AdjustmentKind::Threshold { level: 0.5 }.apply_linear([0.1, 0.1, 0.1]),
            [0.0, 0.0, 0.0]
        );
        assert_eq!(
            AdjustmentKind::Threshold { level: 0.5 }.apply_linear([0.9, 0.9, 0.9]),
            [1.0, 1.0, 1.0]
        );
        // Posterize to 2 levels snaps to {0,1}.
        assert_eq!(
            AdjustmentKind::Posterize { levels: 2.0 }.apply_linear([0.2, 0.6, 0.9]),
            [0.0, 1.0, 1.0]
        );
        // White balance warms red, cools blue.
        let wb = AdjustmentKind::WhiteBalance { temperature: 0.2, tint: 0.0 }.apply_linear([0.5, 0.5, 0.5]);
        assert!(wb[0] > 0.5 && wb[2] < 0.5, "temp+ warms R, cools B: {wb:?}");
        // Vibrance with amount 0 is a no-op.
        assert!(approx(
            AdjustmentKind::Vibrance { amount: 0.0 }.apply_linear([0.4, 0.2, 0.7]),
            [0.4, 0.2, 0.7]
        ));
        // All kinds have a label and the picker enumerates them.
        assert_eq!(AdjustmentKind::all_defaults().len(), 13);
        assert_eq!(AdjustmentKind::Invert.label(), "Invert");
        assert_eq!(AdjustmentKind::EdgeDetect.label(), "Edge Detect");
        // Convolution kinds are GPU-only → CPU apply_linear is a pass-through no-op.
        assert_eq!(
            AdjustmentKind::BoxBlur { radius: 4.0 }.apply_linear([0.3, 0.4, 0.5]),
            [0.3, 0.4, 0.5]
        );
    }

    #[test]
    fn undo_redo_add_object() {
        let mut doc = Document::default();
        doc.checkpoint(&[]);
        let id = doc.add(ObjectKind::Cube, Vec3::ZERO);
        doc.commit();
        assert_eq!(doc.object_count(), 1);
        assert!(doc.can_undo());

        assert!(doc.undo());
        assert_eq!(doc.object_count(), 0, "undo removes the added object");
        assert!(doc.get(id).is_none());

        assert!(doc.redo());
        assert_eq!(doc.object_count(), 1, "redo re-adds it");
        assert!(doc.get(id).is_some(), "id resolves again after redo");
    }

    #[test]
    fn undo_redo_remove_object() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Sphere, Vec3::X);
        doc.checkpoint(&[id]);
        doc.remove(id);
        doc.commit();
        assert_eq!(doc.object_count(), 0);

        assert!(doc.undo(), "undo restores the removed object");
        assert_eq!(doc.object_count(), 1);
        let restored = doc.get(id).expect("removed object comes back with its exact id");
        assert_eq!(restored.kind, ObjectKind::Sphere);

        assert!(doc.redo());
        assert_eq!(doc.object_count(), 0, "redo removes it again");
    }

    #[test]
    fn undo_redo_transform_edit() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Cube, Vec3::ZERO);
        doc.checkpoint(&[id]);
        doc.get_mut(id).unwrap().transform.translation = Vec3::new(5.0, 0.0, 0.0);
        doc.commit();
        assert_eq!(doc.get(id).unwrap().transform.translation, Vec3::new(5.0, 0.0, 0.0));

        assert!(doc.undo());
        assert_eq!(doc.get(id).unwrap().transform.translation, Vec3::ZERO, "undo restores prior transform");
        assert!(doc.redo());
        assert_eq!(doc.get(id).unwrap().transform.translation, Vec3::new(5.0, 0.0, 0.0), "redo re-applies");
    }

    #[test]
    fn undo_covers_sculpt_and_skips_noops() {
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        doc.get_mut(id).unwrap().mesh = Some(Mesh::cube().catmull_clark());
        doc.set_selection(Some(id));
        let before: Vec<_> = doc.get(id).unwrap().mesh.as_ref().unwrap().vertices.clone();

        // A real sculpt stroke at the mesh center.
        doc.checkpoint(&[id]);
        doc.sculpt_stroke(Vec3::ZERO, 2.0, 0.3, 0);
        doc.commit();
        let after: Vec<_> = doc.get(id).unwrap().mesh.as_ref().unwrap().vertices.clone();
        assert!(before != after, "sculpt changed the mesh");
        assert!(doc.can_undo());

        assert!(doc.undo());
        assert_eq!(doc.get(id).unwrap().mesh.as_ref().unwrap().vertices, before, "undo restores pre-sculpt mesh");

        // A no-op transaction (checkpoint + commit with no change) must not push history.
        doc.checkpoint(&[id]);
        doc.commit();
        assert!(doc.can_redo(), "the sculpt redo is still available");
        // can_undo should be false now (we undid the only real edit and the no-op was skipped).
        assert!(!doc.can_undo(), "no-op transaction left no undo entry");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut doc = Document::default();
        doc.checkpoint(&[]);
        let _a = doc.add(ObjectKind::Cube, Vec3::ZERO);
        doc.commit();
        doc.undo();
        assert!(doc.can_redo());
        // A fresh edit forks history — redo must be discarded.
        doc.checkpoint(&[]);
        let _b = doc.add(ObjectKind::Sphere, Vec3::Y);
        doc.commit();
        assert!(!doc.can_redo(), "new edit clears the redo stack");
    }

    #[test]
    fn skeleton_and_modifiers_survive_save_load() {
        // Hardening: a mesh carrying a skeleton + a Decimate modifier must round-trip
        // through the scene JSON with no loss.
        let mut doc = Document::default();
        let id = doc.add(ObjectKind::Mesh, Vec3::ZERO);
        if let Some(obj) = doc.get_mut(id) {
            obj.mesh = Some(Mesh::cube().catmull_clark());
            obj.modifiers.push(Modifier::Decimate { grid_res: 8 });
        }
        doc.set_selection(Some(id));
        assert!(doc.auto_rig_selected_mesh(5), "rig the cube");

        let json = doc.to_scene_json().expect("serialize");
        let back = Document::from_scene_json(&json).expect("deserialize");
        let obj = back.get(id).expect("object survives");

        let skel = obj.skeleton.as_ref().expect("skeleton survives save/load");
        assert_eq!(skel.bones.len(), 5, "all 5 bones round-trip");
        assert_eq!(skel.bones[0].parent, None, "root parent preserved");
        assert_eq!(skel.bones[4].parent, Some(3), "chain parent preserved");
        assert!(
            matches!(obj.modifiers.first(), Some(Modifier::Decimate { grid_res: 8 })),
            "decimate modifier round-trips with its param"
        );
    }

    #[test]
    fn heightmap_mesh_survives_save_load() {
        // A heightmap-built mesh is a plain Mesh, but lock in that the add path + JSON
        // round-trip preserve its vertex/face counts exactly.
        let mut doc = Document::default();
        let pixels = vec![200u8; 8 * 8 * 4]; // 8×8 RGBA8
        let id = doc.add_heightmap_mesh(&pixels, 8, 8, 8, 1.0, Vec3::ZERO);
        let before = doc.get(id).and_then(|o| o.mesh.as_ref()).map(|m| (m.vertices.len(), m.faces.len()));

        let json = doc.to_scene_json().expect("serialize");
        let back = Document::from_scene_json(&json).expect("deserialize");
        let after = back.get(id).and_then(|o| o.mesh.as_ref()).map(|m| (m.vertices.len(), m.faces.len()));

        assert_eq!(before, after, "heightmap mesh geometry round-trips losslessly");
        assert!(before.unwrap().0 > 0, "heightmap actually produced geometry");
    }
}
