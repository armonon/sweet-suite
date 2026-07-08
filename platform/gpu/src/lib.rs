//! # suite-gpu — wgpu abstraction, the renderer, and the compositor.
//!
//! The renderer is a **pure function of scene state at one instant**:
//! `frame = render(scene, camera, t)`. It owns no truth and mutates no document;
//! it draws whatever the object model says is true *right now* and moves on.
//! Forward+ (clustered), five passes, linear HDR working space, <8 ms budget. docs/03 §1.
//!
//! Phase 1+3 lite: the renderer reads from a `suite_doc::Document`, draws every
//! visible object with its world matrix, and highlights the selected one. A
//! procedural infinite grid sits behind everything. The frame graph is still a
//! single pass — the multi-pass Forward+ graph arrives at Phase 4.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use suite_doc::{Document, ObjId, ObjectKind};
use wgpu::util::DeviceExt;
use winit::window::Window;

mod compositor;
pub mod font;
mod raster;
mod shaders;
mod tile_canvas;

pub use compositor::{AdjustmentEntry, CompEntry, Compositor, LayerEntry};
pub use raster::{Brush, BrushBlend, BrushTip, RasterCanvas};
pub use tile_canvas::{TileCanvas, CANVAS_TILES, DISPLAY_SIZE, TILE_SIZE};
pub use suite_doc; // re-export so apps don't need a separate import

/// The frame-graph passes, in order. docs/03 §1.2 (not all used yet in Phase 1+3 lite).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    ClusterBuild,
    OpaqueZPrepassThenShade,
    Transparent,
    FlatComposite,
    Overlays,
    Post,
}

pub const PASS_ORDER: [Pass; 6] = [
    Pass::ClusterBuild,
    Pass::OpaqueZPrepassThenShade,
    Pass::Transparent,
    Pass::FlatComposite,
    Pass::Overlays,
    Pass::Post,
];

/// Input → on-screen in under this many ms, every frame. docs/01 §2, docs/03 §1.9.
pub const FRAME_BUDGET_MS: f32 = 8.0;

/// `design-tokens/tokens.toml`'s `color.chrome.bg-0` in linear RGB.
const CHROME_BG0_LINEAR: [f32; 4] = [0.0033, 0.0035, 0.0040, 1.0];
/// `color.accent.base` linearized — used as the selection highlight tint.
const ACCENT_BASE_LINEAR: [f32; 4] = [0.0466, 0.2122, 0.7605, 1.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    Perspective,
    Orthographic,
}

impl Projection {
    pub fn toggle(self) -> Self {
        match self {
            Self::Perspective => Self::Orthographic,
            Self::Orthographic => Self::Perspective,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Perspective => "perspective",
            Self::Orthographic => "orthographic",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub projection: Projection,
    pub target: Vec3,
    pub distance: f32,
    pub yaw_radians: f32,
    pub pitch_radians: f32,
    pub fov_y_radians: f32,
    pub ortho_height: f32,
    pub z_near: f32,
    pub z_far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            projection: Projection::Perspective,
            target: Vec3::ZERO,
            distance: 6.0,
            yaw_radians: std::f32::consts::FRAC_PI_4,
            pitch_radians: -std::f32::consts::FRAC_PI_6,
            fov_y_radians: std::f32::consts::FRAC_PI_4,
            ortho_height: 5.0,
            z_near: 0.1,
            z_far: 100.0,
        }
    }
}

impl Camera {
    pub fn eye(&self) -> Vec3 {
        let cp = self.pitch_radians.cos();
        let sp = self.pitch_radians.sin();
        let cy = self.yaw_radians.cos();
        let sy = self.yaw_radians.sin();
        self.target
            + Vec3::new(
                self.distance * cp * sy,
                self.distance * sp,
                self.distance * cp * cy,
            )
    }
    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }
    pub fn proj(&self, aspect: f32) -> Mat4 {
        match self.projection {
            Projection::Perspective => {
                Mat4::perspective_rh(self.fov_y_radians, aspect, self.z_near, self.z_far)
            }
            Projection::Orthographic => {
                let h = self.ortho_height * 0.5;
                let w = h * aspect;
                Mat4::orthographic_rh(-w, w, -h, h, self.z_near, self.z_far)
            }
        }
    }
    /// World-space ray for a normalized cursor (in 0..1, top-left origin) — used by
    /// the picker (suite_doc::ray_aabb_world).
    pub fn ray_from_cursor(
        &self,
        cursor_x_norm: f32,
        cursor_y_norm: f32,
        aspect: f32,
    ) -> (Vec3, Vec3) {
        let ndc_x = cursor_x_norm * 2.0 - 1.0;
        let ndc_y = 1.0 - cursor_y_norm * 2.0;
        let view_proj = self.proj(aspect) * self.view();
        let inv = view_proj.inverse();
        let near = inv.project_point3(Vec3::new(ndc_x, ndc_y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc_x, ndc_y, 1.0));
        let dir = (far - near).normalize_or_zero();
        (near, dir)
    }
    /// Gizmo scale that stays roughly screen-constant as the camera moves. Returns the
    /// world-space length the gizmo arms should be so they always project to a similar
    /// size in pixels. Tuned for a 1280×800 default window; refine when measuring
    /// real screen sizes (Phase 1 polish vs. a real `platform/input::Gizmo`).
    pub fn gizmo_world_scale(&self, world_pos: Vec3) -> f32 {
        match self.projection {
            Projection::Perspective => {
                let d = (self.eye() - world_pos).length().max(0.1);
                d * 0.18
            }
            Projection::Orthographic => self.ortho_height * 0.18,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

impl GizmoAxis {
    pub fn unit(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }
}

/// Whole-layer pixel transforms (M4). Applied as a single undoable edit on the active
/// layer's canvas. Deliberately dimension-preserving (safe on any aspect ratio, M5) — a
/// 90° rotation is a **document**-level operation instead (`CanvasRotate` + `rotate_canvas_90`)
/// since it must rotate every layer + the comp buffers together to keep them aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerTransform {
    FlipH,
    FlipV,
    Rotate180,
}

/// Whole-**document** 90° rotation (M5): swaps width↔height, rotating every layer plus the
/// comp/display buffers together so they stay aligned. See `Renderer::rotate_canvas_90`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasRotate {
    Cw,
    Ccw,
}

/// **Tier 1**: a selection's exact shape when it isn't a plain rectangle. Coordinates are
/// UV space (0..1, top-left origin) — same convention as `Renderer::selection_rect`.
/// Rasterized into a per-pixel mask on demand by `rasterize_selection_mask` wherever a
/// gradient/move needs exact-shape correctness, rather than kept as a standing canvas-sized
/// buffer synced every frame (selections change far less often than frames render).
#[derive(Clone, Debug, PartialEq)]
pub enum SelectionShape {
    /// Centre `(cx, cy)` and radii `(rx, ry)`, all UV. A plain ellipse inscribed in the
    /// drag's bounding box — `rx`/`ry` are independent per-axis, so (unlike a round brush
    /// dab) no aspect correction is needed: an ellipse already has separate x/y extents.
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32 },
    /// A closed polygon (implicitly closed — the last point connects back to the first).
    /// UV-space vertices, in drag order.
    Polygon(Vec<[f32; 2]>),
    /// A raw per-texel coverage mask (row-major, 0 = unselected, 255 = selected) captured at
    /// `width`×`height` — produced by Magic Wand's flood fill, which (unlike Ellipse/Lasso)
    /// has no compact analytic or vertex representation. Unlike the other two variants this
    /// one is resolution-coupled, not UV-resolution-independent: `rasterize_selection_mask`
    /// resamples it (nearest-neighbor) if asked for a different size than it was captured at,
    /// which only happens if the canvas is resized (Crop/rotate) after the wand selection.
    Mask { width: u32, height: u32, data: Vec<u8> },
}

/// **Pure**: rasterize a `SelectionShape` into a full-canvas mask (row-major, one byte per
/// texel: 0 = unselected, 255 = fully selected). The boundary is antialiased over roughly
/// one texel so edges aren't jagged.
pub fn rasterize_selection_mask(width: usize, height: usize, shape: &SelectionShape) -> Vec<u8> {
    let mut mask = vec![0u8; width * height];
    match shape {
        SelectionShape::Ellipse { cx, cy, rx, ry } => {
            let (cx, cy) = (cx * width as f32, cy * height as f32);
            let rx = (rx * width as f32).max(0.5);
            let ry = (ry * height as f32).max(0.5);
            for y in 0..height {
                for x in 0..width {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let nx = (px - cx) / rx;
                    let ny = (py - cy) / ry;
                    let d = (nx * nx + ny * ny).sqrt();
                    // Antialias over roughly one texel's worth of normalized distance.
                    let aa = (1.0 / rx.min(ry).max(1.0)).max(0.02);
                    let coverage = (1.0 - ((d - 1.0) / aa).clamp(0.0, 1.0)).clamp(0.0, 1.0);
                    mask[y * width + x] = (coverage * 255.0).round() as u8;
                }
            }
        }
        SelectionShape::Polygon(points) => {
            if points.len() < 3 {
                return mask;
            }
            let px: Vec<f32> = points.iter().map(|p| p[0] * width as f32).collect();
            let py: Vec<f32> = points.iter().map(|p| p[1] * height as f32).collect();
            // Even-odd scanline fill: for each row, find edge crossings, sort, fill between pairs.
            for y in 0..height {
                let sy = y as f32 + 0.5;
                let mut xs: Vec<f32> = Vec::new();
                let n = px.len();
                for i in 0..n {
                    let j = (i + 1) % n;
                    let (y0, y1) = (py[i], py[j]);
                    if (y0 <= sy && y1 > sy) || (y1 <= sy && y0 > sy) {
                        let t = (sy - y0) / (y1 - y0);
                        xs.push(px[i] + t * (px[j] - px[i]));
                    }
                }
                xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for pair in xs.chunks_exact(2) {
                    let (x0, x1) = (pair[0], pair[1]);
                    let start = x0.floor().max(0.0) as usize;
                    let end = (x1.ceil() as usize).min(width);
                    for x in start..end {
                        mask[y * width + x] = 255;
                    }
                }
            }
        }
        SelectionShape::Mask { width: mw, height: mh, data } => {
            if *mw as usize == width && *mh as usize == height {
                return data.clone();
            }
            // Resolution mismatch (canvas resized since the wand selection was made):
            // nearest-neighbor resample rather than dropping the selection outright.
            let (mw, mh) = (*mw as usize, (*mh).max(1) as usize);
            for y in 0..height {
                let sy = (y * mh / height.max(1)).min(mh.saturating_sub(1));
                for x in 0..width {
                    let sx = (x * mw / width.max(1)).min(mw.saturating_sub(1));
                    mask[y * width + x] = data[sy * mw + sx];
                }
            }
        }
    }
    mask
}

/// Bounding box (UV space, `[x0, y0, x1, y1]`) of a `SelectionShape` — used as the fast-path
/// `selection_rect` (GPU scissor bound) alongside the exact mask.
pub fn selection_shape_bounds(shape: &SelectionShape) -> [f32; 4] {
    match shape {
        SelectionShape::Ellipse { cx, cy, rx, ry } => {
            [(cx - rx).max(0.0), (cy - ry).max(0.0), (cx + rx).min(1.0), (cy + ry).min(1.0)]
        }
        SelectionShape::Polygon(points) => {
            let (mut x0, mut y0, mut x1, mut y1) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
            for p in points {
                x0 = x0.min(p[0]);
                y0 = y0.min(p[1]);
                x1 = x1.max(p[0]);
                y1 = y1.max(p[1]);
            }
            [x0.max(0.0), y0.max(0.0), x1.min(1.0), y1.min(1.0)]
        }
        SelectionShape::Mask { width, height, data } => {
            let (w, h) = (*width as usize, *height as usize);
            let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
            let mut any = false;
            for y in 0..h {
                for x in 0..w {
                    if data[y * w + x] > 0 {
                        any = true;
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x + 1);
                        y1 = y1.max(y + 1);
                    }
                }
            }
            if !any {
                return [0.0, 0.0, 0.0, 0.0];
            }
            [x0 as f32 / w as f32, y0 as f32 / h as f32, x1 as f32 / w as f32, y1 as f32 / h as f32]
        }
    }
}

/// Separable single-channel box blur (horizontal then vertical), edge-clamped, sliding-window
/// O(width·height) regardless of `radius`. Helper for `feather_mask`; edge clamping (not
/// zero-padding) is deliberate so a selection touching the canvas border doesn't get a
/// spurious dark fringe there.
fn box_blur_1ch(src: &[u8], width: usize, height: usize, radius: usize) -> Vec<u8> {
    let window = (2 * radius + 1) as u32;
    let (w, h) = (width as isize, height as isize);

    // Horizontal pass: src -> tmp.
    let mut tmp = vec![0u8; src.len()];
    for y in 0..height {
        let row = y * width;
        let r = radius as isize;
        let mut sum: u32 = 0;
        for k in -r..=r {
            sum += src[row + k.clamp(0, w - 1) as usize] as u32;
        }
        for x in 0..width {
            tmp[row + x] = (sum / window) as u8;
            let remove = (x as isize - r).clamp(0, w - 1) as usize;
            let add = (x as isize + r + 1).clamp(0, w - 1) as usize;
            sum = sum - src[row + remove] as u32 + src[row + add] as u32;
        }
    }

    // Vertical pass: tmp -> dst.
    let mut dst = vec![0u8; src.len()];
    for x in 0..width {
        let r = radius as isize;
        let mut sum: u32 = 0;
        for k in -r..=r {
            sum += tmp[k.clamp(0, h - 1) as usize * width + x] as u32;
        }
        for y in 0..height {
            dst[y * width + x] = (sum / window) as u8;
            let remove = (y as isize - r).clamp(0, h - 1) as usize;
            let add = (y as isize + r + 1).clamp(0, h - 1) as usize;
            sum = sum - tmp[remove * width + x] as u32 + tmp[add * width + x] as u32;
        }
    }
    dst
}

/// **Pure**: soften a coverage mask's edges (selection *feathering*). Runs `box_blur_1ch`
/// twice — a tent filter, which gives a smoother, more Gaussian-looking falloff than a single
/// box pass while staying linear-time. `radius == 0` returns the mask unchanged, byte-for-byte
/// (the hard-edge default). Used so Paint/Gradient/Move/Text fade out gradually across a
/// selection boundary instead of a hard cut. Note this softens the mask *in place over the
/// canvas*; a tool that only visits the selection's bounding box sees the interior half of the
/// falloff (an inset-style feather), which is the intended, documented v1 behavior — the
/// selection's extent/scissor is not widened.
pub fn feather_mask(mask: &[u8], width: usize, height: usize, radius: usize) -> Vec<u8> {
    if radius == 0 || width == 0 || height == 0 {
        return mask.to_vec();
    }
    let once = box_blur_1ch(mask, width, height, radius);
    box_blur_1ch(&once, width, height, radius)
}

// sRGB <-> linear transfer (the paint texture is `Rgba8UnormSrgb`, so direct CPU pixel
// edits must encode/decode to composite correctly in linear space).
#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// **Pure**: source-over composite one already-linear `(sr, sg, sb, sa)` sample onto the
/// sRGB-encoded destination pixel at byte offset `di`, in place. The same per-pixel blend
/// used inline by `move_selection_pixels`/`apply_free_transform`, pulled out as its own
/// function for `Renderer::apply_text` (M4a) rather than duplicated a fourth time.
#[inline]
fn composite_over(dst: &mut [u8], di: usize, sr: f32, sg: f32, sb: f32, sa: f32) {
    if sa <= 0.0 {
        return;
    }
    let da = dst[di + 3] as f32 / 255.0;
    let dr = srgb_to_linear(dst[di] as f32 / 255.0);
    let dg = srgb_to_linear(dst[di + 1] as f32 / 255.0);
    let db = srgb_to_linear(dst[di + 2] as f32 / 255.0);
    let oa = sa + da * (1.0 - sa);
    let (or_, og, ob) = if oa > 1e-6 {
        (
            (sr * sa + dr * da * (1.0 - sa)) / oa,
            (sg * sa + dg * da * (1.0 - sa)) / oa,
            (sb * sa + db * da * (1.0 - sa)) / oa,
        )
    } else {
        (0.0, 0.0, 0.0)
    };
    dst[di] = (linear_to_srgb(or_).clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[di + 1] = (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[di + 2] = (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[di + 3] = (oa.clamp(0.0, 1.0) * 255.0).round() as u8;
}

/// **Pure** gradient fill over an RGBA8 (sRGB-encoded) buffer. Interpolates `color_a`→
/// `color_b` (linear RGBA) along the from→to vector (`radial=false`) or radially from
/// `from_uv` (`radial=true`), then source-over composites in linear space. `selection`
/// (UV `[x0,y0,x1,y1]`) clips the affected region. Endpoints are in UV (0..1). Extracted
/// from the GPU method so the math is unit-testable without a window/device.
#[allow(clippy::too_many_arguments)]
pub fn apply_gradient_fill(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    from_uv: [f32; 2],
    to_uv: [f32; 2],
    color_a: [f32; 4],
    color_b: [f32; 4],
    radial: bool,
    selection: Option<[f32; 4]>,
    mask: Option<&[u8]>,
) {
    let (sx0, sy0, sx1, sy1) = match selection {
        Some([x0, y0, x1, y1]) if x1 > x0 && y1 > y0 => (
            (x0 * width as f32).floor().max(0.0) as usize,
            (y0 * height as f32).floor().max(0.0) as usize,
            (x1 * width as f32).ceil().min(width as f32) as usize,
            (y1 * height as f32).ceil().min(height as f32) as usize,
        ),
        _ => (0, 0, width, height),
    };

    // Gradient direction/radius in texel space. Using `width` for both axes here (not a
    // separate per-axis scale) means the from/to UVs already encode the visual direction
    // the user dragged on screen — the gradient itself has no "shape" to correct for aspect
    // (unlike a round brush dab), so no aspect correction is needed here.
    let fx = from_uv[0] * width as f32;
    let fy = from_uv[1] * height as f32;
    let dx = (to_uv[0] - from_uv[0]) * width as f32;
    let dy = (to_uv[1] - from_uv[1]) * height as f32;
    let len2 = (dx * dx + dy * dy).max(1e-6);
    let radius = len2.sqrt().max(1e-6);

    for y in sy0..sy1 {
        for x in sx0..sx1 {
            let pxf = x as f32 + 0.5;
            let pyf = y as f32 + 0.5;
            let t = if radial {
                (((pxf - fx).powi(2) + (pyf - fy).powi(2)).sqrt() / radius).clamp(0.0, 1.0)
            } else {
                (((pxf - fx) * dx + (pyf - fy) * dy) / len2).clamp(0.0, 1.0)
            };
            let sr = color_a[0] + (color_b[0] - color_a[0]) * t;
            let sg = color_a[1] + (color_b[1] - color_a[1]) * t;
            let sb = color_a[2] + (color_b[2] - color_a[2]) * t;
            let mut sa = color_a[3] + (color_b[3] - color_a[3]) * t;
            // Tier 1: an exact-shape selection (Ellipse/Lasso) attenuates alpha by the
            // mask's coverage at this texel — the rect above is only the coarse bound.
            if let Some(m) = mask {
                sa *= m[y * width + x] as f32 / 255.0;
            }
            if sa <= 0.0 {
                continue;
            }
            let idx = (y * width + x) * 4;
            let dr = srgb_to_linear(pixels[idx] as f32 / 255.0);
            let dg = srgb_to_linear(pixels[idx + 1] as f32 / 255.0);
            let db = srgb_to_linear(pixels[idx + 2] as f32 / 255.0);
            let da = pixels[idx + 3] as f32 / 255.0;
            let oa = sa + da * (1.0 - sa);
            let (or, og, ob) = if oa > 1e-6 {
                (
                    (sr * sa + dr * da * (1.0 - sa)) / oa,
                    (sg * sa + dg * da * (1.0 - sa)) / oa,
                    (sb * sa + db * da * (1.0 - sa)) / oa,
                )
            } else {
                (0.0, 0.0, 0.0)
            };
            pixels[idx] = (linear_to_srgb(or).clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels[idx + 1] = (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels[idx + 2] = (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels[idx + 3] = (oa.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

/// **Pure** whole-layer flip/180°-rotate of an RGBA8 buffer. Returns a same-size,
/// same-dimensions buffer — these three ops never change width/height, so they're always
/// safe on a non-square (M5) canvas. (90° rotation swaps width↔height for the WHOLE
/// document, not one layer — see `rotate_canvas_pixels` — so it isn't a `LayerTransform`.)
pub fn apply_layer_transform(src: &[u8], width: usize, height: usize, op: LayerTransform) -> Vec<u8> {
    let mut dst = vec![0u8; src.len()];
    for y in 0..height {
        for x in 0..width {
            let (nx, ny) = match op {
                LayerTransform::FlipH => (width - 1 - x, y),
                LayerTransform::FlipV => (x, height - 1 - y),
                LayerTransform::Rotate180 => (width - 1 - x, height - 1 - y),
            };
            let si = (y * width + x) * 4;
            let di = (ny * width + nx) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    dst
}

/// **Pure** 90° rotation of an RGBA8 buffer, swapping width↔height in the output — used for
/// a whole-canvas rotate (every layer + the doc's overall dimensions rotate together, so
/// they all stay aligned). `cw=true` rotates clockwise.
pub fn rotate_canvas_pixels(src: &[u8], width: usize, height: usize, cw: bool) -> Vec<u8> {
    let mut dst = vec![0u8; src.len()];
    for y in 0..height {
        for x in 0..width {
            // CW: (x,y) -> (height-1-y, x) in a (height x width) output.
            // CCW: (x,y) -> (y, width-1-x) in a (height x width) output.
            let (nx, ny) = if cw { (height - 1 - y, x) } else { (y, width - 1 - x) };
            let si = (y * width + x) * 4;
            let new_w = height; // output buffer is height×width
            let di = (ny * new_w + nx) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    dst
}

/// **Pure** crop of an RGBA8 buffer to the `(x0, y0, crop_w, crop_h)` sub-rectangle (texel
/// space, top-left origin). `x0+crop_w` and `y0+crop_h` are clamped to the source bounds —
/// used for `Renderer::crop_to_rect` (M4).
pub fn crop_pixels(src: &[u8], src_width: usize, src_height: usize, x0: usize, y0: usize, crop_w: usize, crop_h: usize) -> Vec<u8> {
    let x0 = x0.min(src_width);
    let y0 = y0.min(src_height);
    let crop_w = crop_w.min(src_width - x0).max(1);
    let crop_h = crop_h.min(src_height - y0).max(1);
    let mut dst = vec![0u8; crop_w * crop_h * 4];
    for y in 0..crop_h {
        let src_row_start = ((y0 + y) * src_width + x0) * 4;
        let dst_row_start = y * crop_w * 4;
        dst[dst_row_start..dst_row_start + crop_w * 4]
            .copy_from_slice(&src[src_row_start..src_row_start + crop_w * 4]);
    }
    dst
}

/// **Pure** BFS flood fill over an RGBA8 buffer: starting at the texel under `seed_uv`,
/// grows to every 4-connected neighbor within `tolerance` (0–1, per channel including alpha)
/// of the seed's color. Returns a coverage mask (row-major, 0/255) plus the selected texel
/// count (0 if `seed_uv` is degenerate, though the seed always matches itself in practice) —
/// used by `Renderer::magic_wand_select` (Tier 1 Quick Selection) after a GPU readback.
pub fn flood_fill_mask(pixels: &[u8], width: usize, height: usize, seed_uv: [f32; 2], tolerance: f32) -> (Vec<u8>, usize) {
    let px = (seed_uv[0].clamp(0.0, 0.9999) * width as f32) as usize;
    let py = (seed_uv[1].clamp(0.0, 0.9999) * height as f32) as usize;
    let seed_idx = (py * width + px) * 4;

    let (seed_r, seed_g, seed_b, seed_a) = (
        pixels[seed_idx] as f32 / 255.0,
        pixels[seed_idx + 1] as f32 / 255.0,
        pixels[seed_idx + 2] as f32 / 255.0,
        pixels[seed_idx + 3] as f32 / 255.0,
    );

    let mut mask = vec![0u8; width * height];
    let mut visited = vec![false; width * height];
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((px as isize, py as isize));
    let mut count = 0usize;

    while let Some((x, y)) = queue.pop_front() {
        if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
            continue;
        }
        let (xi, yi) = (x as usize, y as usize);
        let flat = yi * width + xi;
        if visited[flat] {
            continue;
        }
        let idx = flat * 4;
        let matches = (pixels[idx] as f32 / 255.0 - seed_r).abs() <= tolerance
            && (pixels[idx + 1] as f32 / 255.0 - seed_g).abs() <= tolerance
            && (pixels[idx + 2] as f32 / 255.0 - seed_b).abs() <= tolerance
            && (pixels[idx + 3] as f32 / 255.0 - seed_a).abs() <= tolerance;
        if !matches {
            continue;
        }
        visited[flat] = true;
        mask[flat] = 255;
        count += 1;
        queue.push_back((x - 1, y));
        queue.push_back((x + 1, y));
        queue.push_back((x, y - 1));
        queue.push_back((x, y + 1));
    }

    (mask, count)
}

/// **Pure** whole-layer shift by `(dx, dy)` texels. Texels that would land off-canvas are
/// dropped; the newly-revealed edge is transparent. Used by `Renderer::move_active_layer`
/// (M4) when there's no active selection — Photoshop's Move tool with nothing selected
/// moves the whole layer the same way.
pub fn shift_pixels(src: &[u8], width: usize, height: usize, dx: i32, dy: i32) -> Vec<u8> {
    let mut dst = vec![0u8; src.len()];
    for y in 0..height {
        let ny = y as i32 + dy;
        if ny < 0 || ny >= height as i32 {
            continue;
        }
        for x in 0..width {
            let nx = x as i32 + dx;
            if nx < 0 || nx >= width as i32 {
                continue;
            }
            let si = (y * width + x) * 4;
            let di = (ny as usize * width + nx as usize) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    dst
}

/// **Pure** selection-aware move: cuts the `(sel_x0, sel_y0, sel_w, sel_h)` region out of
/// `src` (leaving it transparent), then alpha-composites (in **linear** space, since the
/// buffer is sRGB-encoded) that region back at `(sel_x0+dx, sel_y0+dy)`, clipped to canvas
/// bounds. Used by `Renderer::move_active_layer` (M4) when a selection is active —
/// Photoshop's Move tool with an active selection only moves the selected pixels, leaving
/// a transparent hole behind.
pub fn move_selection_pixels(
    src: &[u8],
    width: usize,
    height: usize,
    sel_x0: usize,
    sel_y0: usize,
    sel_w: usize,
    sel_h: usize,
    dx: i32,
    dy: i32,
    mask: Option<&[u8]>,
) -> Vec<u8> {
    // Tier 1: `mask` (full-canvas) narrows the moved region to an exact shape (Ellipse/
    // Lasso) within the `sel_*` bounding rect — a texel with mask=0 is left untouched
    // (not cleared, not moved), same as Photoshop moving only the selected pixels.
    let mask_at = |x: usize, y: usize| -> f32 {
        match mask {
            Some(m) => m[(sel_y0 + y) * width + (sel_x0 + x)] as f32 / 255.0,
            None => 1.0,
        }
    };

    let region = crop_pixels(src, width, height, sel_x0, sel_y0, sel_w, sel_h);
    let mut dst = src.to_vec();

    // Clear the original selection region to transparent (only where the mask selects it).
    let clear_w = sel_w.min(width.saturating_sub(sel_x0));
    for y in 0..sel_h {
        let row = sel_y0 + y;
        if row >= height {
            break;
        }
        let row_start = (row * width + sel_x0) * 4;
        for i in 0..clear_w {
            if mask_at(i, y) <= 0.0 {
                continue;
            }
            let p = row_start + i * 4;
            dst[p..p + 4].copy_from_slice(&[0, 0, 0, 0]);
        }
    }

    // Composite the cut region back at the shifted position, source-over in linear space.
    for y in 0..sel_h {
        let ny = sel_y0 as i32 + y as i32 + dy;
        if ny < 0 || ny >= height as i32 {
            continue;
        }
        for x in 0..sel_w {
            let nx = sel_x0 as i32 + x as i32 + dx;
            if nx < 0 || nx >= width as i32 {
                continue;
            }
            let si = (y * sel_w + x) * 4;
            let sa = (region[si + 3] as f32 / 255.0) * mask_at(x, y);
            if sa <= 0.0 {
                continue;
            }
            let sr = srgb_to_linear(region[si] as f32 / 255.0);
            let sg = srgb_to_linear(region[si + 1] as f32 / 255.0);
            let sb = srgb_to_linear(region[si + 2] as f32 / 255.0);
            let di = (ny as usize * width + nx as usize) * 4;
            let da = dst[di + 3] as f32 / 255.0;
            let dr = srgb_to_linear(dst[di] as f32 / 255.0);
            let dg = srgb_to_linear(dst[di + 1] as f32 / 255.0);
            let db = srgb_to_linear(dst[di + 2] as f32 / 255.0);
            let oa = sa + da * (1.0 - sa);
            let (or_, og, ob) = if oa > 1e-6 {
                (
                    (sr * sa + dr * da * (1.0 - sa)) / oa,
                    (sg * sa + dg * da * (1.0 - sa)) / oa,
                    (sb * sa + db * da * (1.0 - sa)) / oa,
                )
            } else {
                (0.0, 0.0, 0.0)
            };
            dst[di] = (linear_to_srgb(or_).clamp(0.0, 1.0) * 255.0).round() as u8;
            dst[di + 1] = (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8;
            dst[di + 2] = (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8;
            dst[di + 3] = (oa.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    dst
}

/// **Pure** free transform (M4c): uniform scale then rotation about a fixed `pivot` texel,
/// applied to `region` (texel `(x0, y0, w, h)`, or `None` for the whole layer) and
/// composited back — same "cut a hole, paste back" shape as `move_selection_pixels`, but
/// with an inverse-mapped affine warp instead of a plain shift. Nearest-neighbor sampling
/// (not bilinear): a deliberate scope cut for v1 — bilinear would smooth scaled/rotated
/// edges, but a subtly wrong resampling kernel is exactly the kind of bug that needs eyes
/// on a screen to catch, and this shipped during a session-long Screen Recording outage
/// (see DECISIONS.md). Nearest-neighbor's correctness is easy to verify by construction and
/// with plain pixel-equality tests, at the cost of aliased (not smoothed) transformed edges.
/// `region.is_some()` leaves a transparent hole at the original spot (like Move); `None`
/// (whole layer) starts from blank since every destination pixel is re-sourced by the warp.
#[allow(clippy::too_many_arguments)]
pub fn apply_free_transform(
    src: &[u8],
    width: usize,
    height: usize,
    region: Option<(usize, usize, usize, usize)>,
    pivot: (f32, f32),
    scale: f32,
    rotation: f32,
    mask: Option<&[u8]>,
) -> Vec<u8> {
    let (rx0, ry0, rw, rh) = region.unwrap_or((0, 0, width, height));
    let (rx1, ry1) = (rx0 + rw, ry0 + rh);
    let mask_at = |x: usize, y: usize| -> f32 {
        match mask {
            Some(m) => m[y * width + x] as f32 / 255.0,
            None => 1.0,
        }
    };

    let mut dst = if region.is_some() {
        let mut d = src.to_vec();
        for y in ry0..ry1.min(height) {
            for x in rx0..rx1.min(width) {
                let cov = mask_at(x, y);
                if cov <= 0.0 {
                    continue;
                }
                let idx = (y * width + x) * 4;
                d[idx + 3] = (d[idx + 3] as f32 * (1.0 - cov)).round() as u8;
            }
        }
        d
    } else {
        vec![0u8; src.len()]
    };

    let scale = if scale.abs() > 1e-6 { scale } else { 1e-6 };
    let (sin_r, cos_r) = rotation.sin_cos();

    // Forward-map the region's 4 corners to find the destination bbox to iterate — cheaper
    // than scanning the whole canvas, and correct for any scale/rotation combination.
    let corners = [
        (rx0 as f32, ry0 as f32),
        (rx1 as f32, ry0 as f32),
        (rx0 as f32, ry1 as f32),
        (rx1 as f32, ry1 as f32),
    ];
    let (mut dx0, mut dy0, mut dx1, mut dy1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (cx, cy) in corners {
        let (ox, oy) = (cx - pivot.0, cy - pivot.1);
        let (sx, sy) = (ox * scale, oy * scale);
        let (fx, fy) = (sx * cos_r - sy * sin_r + pivot.0, sx * sin_r + sy * cos_r + pivot.1);
        dx0 = dx0.min(fx);
        dy0 = dy0.min(fy);
        dx1 = dx1.max(fx);
        dy1 = dy1.max(fy);
    }
    let bx0 = (dx0.floor().max(0.0) as usize).min(width);
    let by0 = (dy0.floor().max(0.0) as usize).min(height);
    let bx1 = (dx1.ceil().max(0.0) as usize).min(width);
    let by1 = (dy1.ceil().max(0.0) as usize).min(height);

    for y in by0..by1 {
        for x in bx0..bx1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Inverse map: undo rotation then scale, relative to the pivot.
            let (ox, oy) = (px - pivot.0, py - pivot.1);
            let (rxp, ryp) = (ox * cos_r + oy * sin_r, -ox * sin_r + oy * cos_r);
            let (srcx, srcy) = (rxp / scale + pivot.0, ryp / scale + pivot.1);
            if srcx < rx0 as f32 || srcx >= rx1 as f32 || srcy < ry0 as f32 || srcy >= ry1 as f32 {
                continue;
            }
            let (sxi, syi) = (srcx as usize, srcy as usize);
            let cov = mask_at(sxi, syi);
            let si = (syi * width + sxi) * 4;
            let sa = (src[si + 3] as f32 / 255.0) * cov;
            if sa <= 0.0 {
                continue;
            }
            let sr = srgb_to_linear(src[si] as f32 / 255.0);
            let sg = srgb_to_linear(src[si + 1] as f32 / 255.0);
            let sb = srgb_to_linear(src[si + 2] as f32 / 255.0);
            let di = (y * width + x) * 4;
            let da = dst[di + 3] as f32 / 255.0;
            let dr = srgb_to_linear(dst[di] as f32 / 255.0);
            let dg = srgb_to_linear(dst[di + 1] as f32 / 255.0);
            let db = srgb_to_linear(dst[di + 2] as f32 / 255.0);
            let oa = sa + da * (1.0 - sa);
            let (or_, og, ob) = if oa > 1e-6 {
                (
                    (sr * sa + dr * da * (1.0 - sa)) / oa,
                    (sg * sa + dg * da * (1.0 - sa)) / oa,
                    (sb * sa + db * da * (1.0 - sa)) / oa,
                )
            } else {
                (0.0, 0.0, 0.0)
            };
            dst[di] = (linear_to_srgb(or_).clamp(0.0, 1.0) * 255.0).round() as u8;
            dst[di + 1] = (linear_to_srgb(og).clamp(0.0, 1.0) * 255.0).round() as u8;
            dst[di + 2] = (linear_to_srgb(ob).clamp(0.0, 1.0) * 255.0).round() as u8;
            dst[di + 3] = (oa.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    dst
}

/// Drives the translate-gizmo draw. The renderer reads this once per frame; nothing is
/// retained across frames.
#[derive(Clone, Copy, Debug)]
pub struct GizmoOverlay {
    pub origin: Vec3,
    pub world_scale: f32,
    pub highlighted: Option<GizmoAxis>,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    proj_kind_and_pad: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    _pad0: f32,
    color: [f32; 4],
}

/// Vertex for editable Mesh objects with a per-mesh paint texture.
/// Box-projected UV is computed per-triangle in `tessellate_mesh_painted`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshVertex {
    position: [f32; 3],
    _pad0: f32,
    color: [f32; 4],
    uv: [f32; 2],
    _pad1: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
struct ObjectUniform {
    model: [[f32; 4]; 4],
    color: [f32; 4],
    selected_pad: [f32; 4], // x: 0.0 = unselected, 1.0 = selected; rest reserved
}

/// 256-byte aligned object uniform slot. Matches the minimum uniform-buffer offset
/// alignment on all wgpu targets so we can use dynamic offset addressing.
const OBJECT_SLOT_BYTES: u64 = 256;

pub struct FrameBudget {
    last_overrun_log: Instant,
    pub last_frame_ms: f32,
    pub peak_frame_ms: f32,
    pub frames: u64,
}

impl Default for FrameBudget {
    fn default() -> Self {
        Self {
            last_overrun_log: Instant::now(),
            last_frame_ms: 0.0,
            peak_frame_ms: 0.0,
            frames: 0,
        }
    }
}

impl FrameBudget {
    pub fn record(&mut self, elapsed_ms: f32) {
        self.last_frame_ms = elapsed_ms;
        if elapsed_ms > self.peak_frame_ms {
            self.peak_frame_ms = elapsed_ms;
        }
        self.frames += 1;
        if elapsed_ms > FRAME_BUDGET_MS && self.last_overrun_log.elapsed().as_secs() >= 1 {
            eprintln!(
                "frame budget overrun: {:.2} ms > {:.2} ms (peak {:.2} ms over {} frames)",
                elapsed_ms, FRAME_BUDGET_MS, self.peak_frame_ms, self.frames
            );
            self.last_overrun_log = Instant::now();
        }
    }
}

struct SurfaceState {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,
}

struct MeshAsset {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

/// One layer in the 2D paint stack: its own paintable texture + compositing metadata.
struct PaintLayer {
    canvas: RasterCanvas,
    name: String,
    visible: bool,
    opacity: f32,
    blend: suite_doc::BlendMode,
}

/// Read-only snapshot of a layer's metadata for the UI (the panel can't touch the GPU).
#[derive(Clone, Debug)]
pub struct LayerInfo {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend: suite_doc::BlendMode,
}

/// Map a blend mode to the shader's `mode` id (matches LAYER_COMPOSITE_WGSL's switch).
fn blend_mode_u32(m: suite_doc::BlendMode) -> u32 {
    use suite_doc::BlendMode::*;
    match m {
        Normal => 0,
        Multiply => 1,
        Screen => 2,
        Overlay => 3,
        SoftLight => 4,
        HardLight => 5,
        Add => 6,
        Subtract => 7,
    }
}

/// A layer's pixels + metadata, for loading a saved project into the stack. `width`/`height`
/// (M5) are this layer's own decoded dimensions — `replace_layers` uses the first layer's
/// dims as the document's canonical canvas size.
#[derive(Clone, Debug)]
pub struct LoadedLayer {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend: suite_doc::BlendMode,
}

/// Composites one layer (`src`) over the running result (`base`) with a blend mode +
/// opacity, writing the Porter-Duff "over" result. Mirrors the compositor's blend math so
/// 2D layers and adjustment layers behave identically. UV uses the top-left-origin
/// convention (1:1 with the brush/texture layout).
const LAYER_COMPOSITE_WGSL: &str = r#"
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 1.0, (vi & 1u) != 0u);
    let y = select(-1.0, 1.0, (vi & 2u) != 0u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv  = vec2<f32>(x * 0.5 + 0.5, 0.5 - y * 0.5);
    return out;
}
@group(0) @binding(0) var base_tex: texture_2d<f32>;
@group(0) @binding(1) var src_tex:  texture_2d<f32>;
@group(0) @binding(2) var samp:     sampler;
struct BlendParams { mode: u32, opacity: f32, _p0: u32, _p1: u32 }
@group(0) @binding(3) var<uniform> params: BlendParams;

fn overlay_ch(b: f32, s: f32) -> f32 {
    if b < 0.5 { return 2.0 * b * s; }
    return 1.0 - 2.0 * (1.0 - b) * (1.0 - s);
}
fn hard_light_ch(b: f32, s: f32) -> f32 { return overlay_ch(s, b); }
fn soft_light_ch(b: f32, s: f32) -> f32 {
    if s < 0.5 { return b - (1.0 - 2.0 * s) * b * (1.0 - b); }
    var d: f32;
    if b < 0.25 { d = ((16.0 * b - 12.0) * b + 4.0) * b; }
    else        { d = sqrt(b); }
    return b + (2.0 * s - 1.0) * (d - b);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(base_tex, samp, in.uv);
    let src  = textureSample(src_tex,  samp, in.uv);
    var rgb: vec3<f32>;
    switch params.mode {
        case 1u { rgb = base.rgb * src.rgb; }
        case 2u { rgb = 1.0 - (1.0 - base.rgb) * (1.0 - src.rgb); }
        case 3u { rgb = vec3(overlay_ch(base.r, src.r), overlay_ch(base.g, src.g), overlay_ch(base.b, src.b)); }
        case 4u { rgb = vec3(soft_light_ch(base.r, src.r), soft_light_ch(base.g, src.g), soft_light_ch(base.b, src.b)); }
        case 5u { rgb = vec3(hard_light_ch(base.r, src.r), hard_light_ch(base.g, src.g), hard_light_ch(base.b, src.b)); }
        case 6u { rgb = clamp(base.rgb + src.rgb, vec3(0.0), vec3(1.0)); }
        case 7u { rgb = clamp(base.rgb - src.rgb, vec3(0.0), vec3(1.0)); }
        default { rgb = src.rgb; }
    }
    let sa = src.a * params.opacity;
    let out_a = sa + base.a * (1.0 - sa);
    let out_rgb = select(
        (rgb * sa + base.rgb * base.a * (1.0 - sa)) / out_a,
        vec3(0.0),
        out_a < 0.0001
    );
    return vec4<f32>(out_rgb, out_a);
}
"#;

pub struct Renderer {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_state: SurfaceState,
    surface_format: wgpu::TextureFormat,

    camera_uniform_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    object_uniform_buffer: wgpu::Buffer,
    object_bind_group: wgpu::BindGroup,
    object_slots: u32,

    quad_bind_group: wgpu::BindGroup,

    /// Flattened display cache: the visible composite of `layers`. `PaintCanvas` objects
    /// sample this. Painting goes to `layers[active_layer]`, then `composite_layers` blits
    /// the stack into here (bottom→top).
    raster: RasterCanvas,
    paint_bind_group: wgpu::BindGroup,
    /// The 2D layer stack (bottom = index 0). The brush, undo, import, etc. target
    /// `layers[active_layer]`.
    layers: Vec<PaintLayer>,
    active_layer: usize,
    /// Set when a layer's pixels/metadata change; `render` recomposites before drawing.
    layers_dirty: bool,
    /// Blends one layer (src) over the running composite (base) with its blend mode +
    /// opacity, ping-ponging between `comp_a`/`comp_b`. docs/03 §1.7.
    layer_composite_pipeline: wgpu::RenderPipeline,
    layer_composite_layout: wgpu::BindGroupLayout,
    comp_a: wgpu::Texture,
    comp_a_view: wgpu::TextureView,
    comp_b: wgpu::Texture,
    comp_b_view: wgpu::TextureView,

    /// Per-mesh paint textures (paint-on-3D). Created lazily on first brush stroke.
    mesh_textures: std::collections::HashMap<suite_doc::ObjId, (RasterCanvas, wgpu::BindGroup)>,
    /// Sampler shared by all mesh paint textures.
    paint_sampler: wgpu::Sampler,
    /// Bind-group layout for texture + sampler, stored so we can build mesh-paint BGs.
    texture_bgl: wgpu::BindGroupLayout,

    scene_pipeline: wgpu::RenderPipeline,
    quad_pipeline: wgpu::RenderPipeline,
    grid_pipeline: wgpu::RenderPipeline,
    outline_pipeline: wgpu::RenderPipeline,
    /// Pipeline for Mesh objects that have a per-mesh paint texture.
    mesh_paint_pipeline: wgpu::RenderPipeline,
    /// Pipeline layout for mesh_paint (camera + object + texture).
    mesh_paint_layout: wgpu::PipelineLayout,

    cube_mesh: MeshAsset,
    sphere_mesh: MeshAsset,
    plane_mesh: MeshAsset,

    pub camera: Camera,
    pub budget: FrameBudget,
    /// World-space position of the magnetic snap indicator (drawn as a small bright sphere).
    /// Set by the app while a snapped drag is active; cleared when not snapping.
    pub snap_indicator: Option<[f32; 3]>,
    /// World-space bone segments `(head, tail)` for the selected object's skeleton.
    /// Drawn as bright lines with a joint cross at each head. Empty = no rig shown.
    pub skeleton_segments: Vec<([f32; 3], [f32; 3])>,
    /// Active selection rectangle in UV space `[x0, y0, x1, y1]` (0..1, top-left origin).
    /// When `Some`, brush stamps are clipped to this region via a GPU scissor rect. Set by
    /// the app from `InputState::select_rect`; `None` means "paint anywhere" (no selection).
    /// Always the selection's bounding box — even when `selection_extra` narrows the actual
    /// selected shape further (Ellipse/Lasso), this stays the rect that bounds it, so the
    /// existing scissor/crop fast paths don't need to change.
    pub selection_rect: Option<[f32; 4]>,
    /// Tier 1: the selection's exact shape when it's *not* a plain rectangle. `None` means
    /// the selection (if any) is exactly `selection_rect` — the common case (`RectSelect`),
    /// left completely untouched by this. `Some` means gradient/move should mask by the
    /// exact shape instead of just the bounding rect; brush painting still only respects the
    /// bounding-rect scissor for now (masked *painting* is a tracked follow-up — see
    /// ROADMAP.md Tier 1).
    pub selection_extra: Option<SelectionShape>,
    /// Tier 1: feather radius in **texels**. `0.0` = a hard selection edge (the default, and
    /// byte-for-byte the pre-feather behavior). When > 0, `current_selection_mask` softens the
    /// rasterized mask's edge by this much before any tool applies it. Kept in sync from the
    /// shell's inspector slider.
    pub selection_feather: f32,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Self {
        pollster::block_on(Self::new_async(window))
    }

    async fn new_async(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("suite-gpu device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                experimental_features: Default::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request device");

        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let depth_view = create_depth_view(&device, width, height);

        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<CameraUniform>() as u64
                    ),
                },
                count: None,
            }],
        });
        let camera_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera uniform buffer"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_uniform_buffer.as_entire_binding(),
            }],
        });

        // Per-object uniform — a single big buffer sliced into 256-byte chunks. Each
        // draw binds it with the dynamic offset of the object's slot.
        let object_slots: u32 = 256;
        let object_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("object bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<ObjectUniform>() as u64
                    ),
                },
                count: None,
            }],
        });
        let object_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("object uniform buffer"),
            size: OBJECT_SLOT_BYTES * object_slots as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let object_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("object bind group"),
            layout: &object_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &object_uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<ObjectUniform>() as u64),
                }),
            }],
        });

        // Image-plane checker texture, bound by both the image-plane pipeline and the
        // standalone textured-quad pipeline (kept around as an example).
        let (checker_texture, checker_view, checker_sampler) =
            build_checker_texture(&device, &queue);
        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let quad_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("textured quad bind group"),
            layout: &texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&checker_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&checker_sampler),
                },
            ],
        });
        drop(checker_texture);

        // The raster paint substrate + a bind group so PaintCanvas objects sample it.
        let mut raster = RasterCanvas::new(&device, 1536, 1536, [0.97, 0.96, 0.94, 1.0]);
        let paint_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("paint/mesh sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let paint_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paint bind group"),
            layout: &texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(raster.texture_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&paint_sampler),
                },
            ],
        });
        // The 2D layer stack starts with one opaque paper layer (the "Background"). Upper
        // layers, added later, start transparent so they composite over what's below.
        let mut background = RasterCanvas::new(&device, 1536, 1536, [0.97, 0.96, 0.94, 1.0]);
        // Paint both the display cache and the background layer to paper up-front so they
        // aren't undefined memory.
        {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint init clear"),
            });
            raster.clear(&mut enc);
            background.clear(&mut enc);
            queue.submit(Some(enc.finish()));
        }
        let layers = vec![PaintLayer {
            canvas: background,
            name: "Background".to_string(),
            visible: true,
            opacity: 1.0,
            blend: suite_doc::BlendMode::Normal,
        }];

        // Layer-composite pipeline: blends one layer (src) over the running composite
        // (base) with its blend mode + opacity (Porter-Duff over). Ping-pongs comp_a/comp_b.
        let comp_desc = |label| wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: 1536, height: 1536, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        let comp_a = device.create_texture(&comp_desc("layer comp a"));
        let comp_b = device.create_texture(&comp_desc("layer comp b"));
        let comp_a_view = comp_a.create_view(&Default::default());
        let comp_b_view = comp_b.create_view(&Default::default());

        let layer_composite_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("layer composite layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let layer_composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("layer composite shader"),
            source: wgpu::ShaderSource::Wgsl(LAYER_COMPOSITE_WGSL.into()),
        });
        let layer_composite_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer composite pl"),
            bind_group_layouts: &[Some(&layer_composite_layout)],
            immediate_size: 0,
        });
        let layer_composite_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("layer composite pipeline"),
                layout: Some(&layer_composite_pl),
                vertex: wgpu::VertexState {
                    module: &layer_composite_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &layer_composite_shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        blend: None, // shader writes the final Porter-Duff result directly
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let cube_mesh = build_mesh(&device, &cube_vertices());
        let sphere_mesh = build_mesh(&device, &uv_sphere_vertices(24, 16));
        let plane_mesh = build_mesh(&device, &plane_vertices());

        let depth_format = wgpu::TextureFormat::Depth32Float;

        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&object_bgl)],
            immediate_size: 0,
        });
        let quad_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("textured quad pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&object_bgl), Some(&texture_bgl)],
            immediate_size: 0,
        });
        let mesh_paint_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh paint pipeline layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&object_bgl), Some(&texture_bgl)],
            immediate_size: 0,
        });

        let scene_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene shader"),
            source: wgpu::ShaderSource::Wgsl(shaders::SCENE_WGSL.into()),
        });
        let grid_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grid shader"),
            source: wgpu::ShaderSource::Wgsl(shaders::GRID_WGSL.into()),
        });
        let quad_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured quad shader"),
            source: wgpu::ShaderSource::Wgsl(shaders::TEXTURED_QUAD_WGSL.into()),
        });
        let outline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("outline shader"),
            source: wgpu::ShaderSource::Wgsl(shaders::OUTLINE_WGSL.into()),
        });

        let scene_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };

        let scene_pipeline = make_render_pipeline(
            &device,
            &scene_layout,
            &scene_shader,
            "vs_main",
            "fs_main",
            &[scene_vertex_layout.clone()],
            surface_format,
            Some(depth_format),
            Some(wgpu::Face::Back),
            wgpu::PrimitiveTopology::TriangleList,
            true,
            wgpu::CompareFunction::Less,
        );
        let quad_pipeline = make_render_pipeline(
            &device,
            &quad_layout,
            &quad_shader,
            "vs_main",
            "fs_main",
            &[scene_vertex_layout.clone()],
            surface_format,
            Some(depth_format),
            None,
            wgpu::PrimitiveTopology::TriangleList,
            true,
            wgpu::CompareFunction::Less,
        );
        let grid_pipeline = make_render_pipeline_no_vbo(
            &device,
            &scene_layout,
            &grid_shader,
            surface_format,
            Some(depth_format),
        );
        let outline_pipeline = make_render_pipeline(
            &device,
            &scene_layout,
            &outline_shader,
            "vs_main",
            "fs_main",
            &[scene_vertex_layout],
            surface_format,
            Some(depth_format),
            None,
            wgpu::PrimitiveTopology::LineList,
            false,
            wgpu::CompareFunction::LessEqual,
        );

        let mesh_paint_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh paint shader"),
            source: wgpu::ShaderSource::Wgsl(shaders::MESH_PAINT_WGSL.into()),
        });
        let mesh_paint_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        };
        let mesh_paint_pipeline = make_render_pipeline(
            &device,
            &mesh_paint_layout,
            &mesh_paint_shader,
            "vs_main",
            "fs_main",
            &[mesh_paint_vertex_layout],
            surface_format,
            Some(depth_format),
            Some(wgpu::Face::Back),
            wgpu::PrimitiveTopology::TriangleList,
            true,
            wgpu::CompareFunction::Less,
        );

        Self {
            window,
            device,
            queue,
            surface_state: SurfaceState {
                surface,
                config,
                depth_view,
            },
            surface_format,
            camera_uniform_buffer,
            camera_bind_group,
            object_uniform_buffer,
            object_bind_group,
            object_slots,
            quad_bind_group,
            raster,
            paint_bind_group,
            layers,
            active_layer: 0,
            layers_dirty: true,
            layer_composite_pipeline,
            layer_composite_layout,
            comp_a,
            comp_a_view,
            comp_b,
            comp_b_view,
            mesh_textures: std::collections::HashMap::new(),
            paint_sampler,
            texture_bgl,
            scene_pipeline,
            quad_pipeline,
            grid_pipeline,
            outline_pipeline,
            mesh_paint_pipeline,
            mesh_paint_layout,
            cube_mesh,
            sphere_mesh,
            plane_mesh,
            camera: Camera::default(),
            budget: FrameBudget::default(),
            snap_indicator: None,
            skeleton_segments: Vec::new(),
            selection_rect: None,
            selection_extra: None,
            selection_feather: 0.0,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if self.surface_state.config.width == w && self.surface_state.config.height == h {
            return;
        }
        self.surface_state.config.width = w;
        self.surface_state.config.height = h;
        self.surface_state
            .surface
            .configure(&self.device, &self.surface_state.config);
        self.surface_state.depth_view = create_depth_view(&self.device, w, h);
    }

    pub fn aspect(&self) -> f32 {
        self.surface_state.config.width as f32 / self.surface_state.config.height.max(1) as f32
    }

    pub fn window(&self) -> &Window {
        &self.window
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }
    pub fn size(&self) -> (u32, u32) {
        (
            self.surface_state.config.width,
            self.surface_state.config.height,
        )
    }

    /// Convert a UV-space selection rect `[x0, y0, x1, y1]` to a texel-space scissor
    /// `[x, y, w, h]` for `RasterCanvas::stamp_segment`. Returns `None` when the selection
    /// is degenerate (zero area) or absent.
    fn selection_scissor(&self, width: u32, height: u32) -> Option<[u32; 4]> {
        let [x0, y0, x1, y1] = self.selection_rect?;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (w, h) = (width as f32, height as f32);
        let px = (x0 * w).floor() as u32;
        let py = (y0 * h).floor() as u32;
        let pw = ((x1 * w).ceil() as u32).saturating_sub(px).max(1);
        let ph = ((y1 * h).ceil() as u32).saturating_sub(py).max(1);
        Some([px, py, pw, ph])
    }

    /// Paint a brush stroke segment into the raster substrate. `from_uv`/`to_uv` are in
    /// 0..1 canvas space (origin top-left). Records its own encoder and submits, so paint
    /// lands immediately and independently of the frame loop. Pixels never leave the GPU.
    /// Stamp a brush segment from `from_uv` to `to_uv`. `pressure` in [0,1] scales radius
    /// (by sqrt) and flow linearly — 1.0 is full/mouse pressure.
    /// When `self.selection_rect` is set, stamps are clipped to that region.
    pub fn paint_stamp(
        &mut self,
        from_uv: [f32; 2],
        to_uv: [f32; 2],
        brush: &Brush,
        pressure: f32,
    ) {
        let pressure = pressure.clamp(0.01, 1.0);
        let effective = Brush {
            radius_uv: brush.radius_uv * pressure.sqrt(),
            flow: brush.flow * pressure,
            ..*brush
        };
        let (width, height) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width(), c.height())
        };
        let scissor = self.selection_scissor(width, height);
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint stamp enc"),
            });
        self.layers[self.active_layer]
            .canvas
            .stamp_segment(&self.device, &mut enc, from_uv, to_uv, &effective, scissor);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
    }

    /// Paint a brush stroke directly onto a 3D mesh's per-mesh texture.
    /// `from_uv` / `to_uv` are box-projected UV coordinates (computed by the caller
    /// from the world-space hit point + face normal via `triplanar_uv`).
    /// Creates the per-mesh `RasterCanvas` on first call (starts transparent — the
    /// mesh-paint shader composites paint over the flat-shaded surface, so alpha 0 =
    /// untouched surface).
    pub fn paint_on_mesh(
        &mut self,
        obj_id: suite_doc::ObjId,
        from_uv: [f32; 2],
        to_uv: [f32; 2],
        brush: &Brush,
        pressure: f32,
    ) {
        let pressure = pressure.clamp(0.01, 1.0);
        let effective = Brush {
            radius_uv: brush.radius_uv * pressure.sqrt(),
            flow: brush.flow * pressure,
            ..*brush
        };
        // Create the per-mesh canvas lazily. Transparent start means unpainted
        // faces show the flat-shaded surface color normally.
        if !self.mesh_textures.contains_key(&obj_id) {
            let canvas = RasterCanvas::new(&self.device, 1024, 1024, [0.0, 0.0, 0.0, 0.0]);
            // Build the bind group while we still own `canvas` and can reference its view.
            let bg = {
                let view = canvas.texture_view();
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mesh paint bg"),
                    layout: &self.texture_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.paint_sampler),
                        },
                    ],
                })
            };
            self.mesh_textures.insert(obj_id, (canvas, bg));
        }
        let (canvas, _) = self.mesh_textures.get_mut(&obj_id).unwrap();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mesh paint stamp enc"),
        });
        canvas.stamp_segment(&self.device, &mut enc, from_uv, to_uv, &effective, None);
        self.queue.submit(Some(enc.finish()));
    }

    /// Tier 1: the active selection's exact per-texel coverage mask (full canvas, `w`×`h`),
    /// or `None` when there's no exact shape (plain `RectSelect`, or no selection at all — the
    /// common case, where the rect scissor already does the clipping). Rasterizes
    /// `selection_extra` and, when `selection_feather > 0`, softens its edge — the single place
    /// feathering is applied, so every tool (Paint/Gradient/Move/Text) picks it up uniformly.
    fn current_selection_mask(&self, w: usize, h: usize) -> Option<Vec<u8>> {
        self.selection_extra.as_ref().map(|shape| {
            let mask = rasterize_selection_mask(w, h, shape);
            let r = self.selection_feather.round().max(0.0) as usize;
            if r > 0 { feather_mask(&mask, w, h, r) } else { mask }
        })
    }

    /// Finish the current paint stroke — commits its undo entry. Call on mouse-up.
    pub fn paint_end_stroke(&mut self) {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint end stroke enc"),
            });
        // Tier 1 (S1b): an exact-shape selection (Ellipse/Lasso) masks the just-finished
        // stroke against its precise boundary — the GPU scissor during painting only
        // constrained dabs to the selection's bounding box.
        let (w, h) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width() as usize, c.height() as usize)
        };
        match self.current_selection_mask(w, h) {
            Some(mask) => {
                self.layers[self.active_layer].canvas.end_stroke_masked(
                    &self.device, &self.queue, &mut enc, &mask, w,
                );
            }
            None => {
                self.layers[self.active_layer].canvas.end_stroke(&self.device, &mut enc);
            }
        }
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
    }

    /// Undo the last paint stroke / clear on the active layer. Returns whether changed.
    pub fn paint_undo(&mut self) -> bool {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint undo enc"),
            });
        let changed = self.layers[self.active_layer].canvas.undo(&self.device, &mut enc);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
        changed
    }
    /// Redo the last undone paint change on the active layer.
    pub fn paint_redo(&mut self) -> bool {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint redo enc"),
            });
        let changed = self.layers[self.active_layer].canvas.redo(&self.device, &mut enc);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
        changed
    }
    pub fn paint_can_undo(&self) -> bool {
        self.layers[self.active_layer].canvas.can_undo()
    }
    pub fn paint_can_redo(&self) -> bool {
        self.layers[self.active_layer].canvas.can_redo()
    }

    /// Wipe the active layer back to paper (undoable).
    pub fn paint_clear(&mut self) {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint clear enc"),
            });
        self.layers[self.active_layer].canvas.clear_undoable(&self.device, &mut enc);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
    }

    /// Replace the active layer with `pixels` (`canvas_width()×canvas_height()×4` RGBA8) as
    /// a single undoable edit (used when an import doesn't change canvas dimensions).
    /// `⌘Z` reverts to the prior layer.
    pub fn paint_upload_rgba_undoable(&mut self, pixels: &[u8]) {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint import enc"),
            });
        self.layers[self.active_layer]
            .canvas
            .upload_rgba_undoable(&self.device, &self.queue, &mut enc, pixels);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
    }

    /// The paint canvas's width/height in texels (M5: no longer assumed square).
    pub fn canvas_width(&self) -> u32 {
        self.raster.width()
    }
    pub fn canvas_height(&self) -> u32 {
        self.raster.height()
    }

    /// GPU→CPU readback of the whole paint texture as row-major RGBA8 (top-left origin).
    /// Blocking — used on save, not in the paint loop.
    pub fn paint_readback_rgba(&self) -> Vec<u8> {
        let (w, h) = (self.raster.width(), self.raster.height());
        let bpr = w * 4;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("paint readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("paint readback enc"),
            });
        // Flatten the layer stack into the display cache first, so the readback (used by
        // save + Export PNG) captures the *visible composite*, not a stale cache.
        self.record_composite(&mut enc);
        enc.copy_texture_to_buffer(
            self.raster.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| {
            let _ = r;
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range();
        data.to_vec()
    }

    /// Eyedropper: read the paint-canvas colour at `uv` (0..1, top-left origin) as a
    /// **linear** RGBA suitable for `Brush::color`. The stored texture is sRGB-encoded, so
    /// this sRGB-decodes the sampled texel. Blocking readback — used on a single click.
    pub fn pick_paint_color(&self, uv: [f32; 2]) -> [f32; 4] {
        let (w, h) = (self.raster.width(), self.raster.height());
        let pixels = self.paint_readback_rgba();
        let x = (uv[0].clamp(0.0, 0.999) * w as f32) as u32;
        let y = (uv[1].clamp(0.0, 0.999) * h as f32) as u32;
        let i = ((y * w + x) * 4) as usize;
        if i + 3 >= pixels.len() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        let srgb_to_linear = |c: f32| {
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        [
            srgb_to_linear(pixels[i] as f32 / 255.0),
            srgb_to_linear(pixels[i + 1] as f32 / 255.0),
            srgb_to_linear(pixels[i + 2] as f32 / 255.0),
            pixels[i + 3] as f32 / 255.0,
        ]
    }

    /// Upload a full-canvas RGBA8 image into the canvas (used on project load). Opening a
    /// `.sweet` loads a *flattened* painting, so this collapses to one background layer.
    pub fn paint_upload_rgba(&mut self, pixels: &[u8]) {
        self.active_layer = 0;
        self.layers.truncate(1);
        self.layers[0].name = "Background".to_string();
        self.layers[0].visible = true;
        self.layers[0].opacity = 1.0;
        self.layers[0].canvas.upload_rgba(&self.queue, pixels);
        self.layers_dirty = true;
    }

    // ---- Layer stack (2D editing) -------------------------------------------------

    /// Composite all visible layers (bottom→top, alpha-over × opacity) into the display
    /// cache `self.raster` that `PaintCanvas` objects sample. Recorded into `encoder`.
    fn record_composite(&self, encoder: &mut wgpu::CommandEncoder) {
        let (width, height) = (self.raster.width(), self.raster.height());
        let visible: Vec<usize> =
            (0..self.layers.len()).filter(|&i| self.layers[i].visible).collect();

        // Start with a transparent base in comp_a.
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.comp_a_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        let mut base_is_a = true;
        for (k, &i) in visible.iter().enumerate() {
            // The bottom layer blends over nothing → force Normal (other modes over a
            // transparent base would give wrong colours).
            let mode = if k == 0 { 0u32 } else { blend_mode_u32(self.layers[i].blend) };
            let op = self.layers[i].opacity.clamp(0.0, 1.0);
            let bytes: [u8; 16] = bytemuck::cast([mode, op.to_bits(), 0u32, 0u32]);
            let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("layer blend params"),
                contents: &bytes,
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let (base_view, target_view) = if base_is_a {
                (&self.comp_a_view, &self.comp_b_view)
            } else {
                (&self.comp_b_view, &self.comp_a_view)
            };
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer composite bg"),
                layout: &self.layer_composite_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(base_view) },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(self.layers[i].canvas.texture_view()),
                    },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.paint_sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: ubuf.as_entire_binding() },
                ],
            });
            {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("layer composite pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(&self.layer_composite_pipeline);
                rp.set_bind_group(0, &bg, &[]);
                rp.draw(0..4, 0..1);
            }
            base_is_a = !base_is_a;
        }

        // The final result is in whichever buffer is the current base. Copy → display cache.
        let final_tex = if base_is_a { &self.comp_a } else { &self.comp_b };
        encoder.copy_texture_to_texture(
            final_tex.as_image_copy(),
            self.raster.texture().as_image_copy(),
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    /// Recomposite the layer stack into the display cache (own encoder + submit).
    fn composite_layers(&mut self) {
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("composite enc") });
        self.record_composite(&mut enc);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = false;
    }

    /// Metadata snapshot for the Layers panel (index 0 = bottom).
    pub fn layer_infos(&self) -> Vec<LayerInfo> {
        self.layers
            .iter()
            .map(|l| LayerInfo { name: l.name.clone(), visible: l.visible, opacity: l.opacity, blend: l.blend })
            .collect()
    }
    pub fn set_layer_blend(&mut self, i: usize, blend: suite_doc::BlendMode) {
        if let Some(l) = self.layers.get_mut(i) {
            l.blend = blend;
            self.layers_dirty = true;
        }
    }
    pub fn active_layer(&self) -> usize {
        self.active_layer
    }
    pub fn set_active_layer(&mut self, i: usize) {
        if i < self.layers.len() {
            self.active_layer = i;
        }
    }
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
    /// Add a transparent layer above the active one and make it active.
    pub fn add_layer(&mut self) {
        let (w, h) = (self.layers[0].canvas.width(), self.layers[0].canvas.height());
        let mut canvas = RasterCanvas::new(&self.device, w, h, [0.0, 0.0, 0.0, 0.0]);
        {
            let mut enc = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("new layer clear") });
            canvas.clear(&mut enc);
            self.queue.submit(Some(enc.finish()));
        }
        let idx = self.active_layer + 1;
        let name = format!("Layer {}", self.layers.len() + 1);
        self.layers.insert(idx, PaintLayer { canvas, name, visible: true, opacity: 1.0, blend: suite_doc::BlendMode::Normal });
        self.active_layer = idx;
        self.layers_dirty = true;
    }
    /// Remove layer `i` (keeps at least one layer).
    pub fn delete_layer(&mut self, i: usize) {
        if self.layers.len() <= 1 || i >= self.layers.len() {
            return;
        }
        self.layers.remove(i);
        if self.active_layer >= self.layers.len() {
            self.active_layer = self.layers.len() - 1;
        }
        self.layers_dirty = true;
    }
    pub fn set_layer_visible(&mut self, i: usize, visible: bool) {
        if let Some(l) = self.layers.get_mut(i) {
            l.visible = visible;
            self.layers_dirty = true;
        }
    }
    pub fn set_layer_opacity(&mut self, i: usize, opacity: f32) {
        if let Some(l) = self.layers.get_mut(i) {
            l.opacity = opacity.clamp(0.0, 1.0);
            self.layers_dirty = true;
        }
    }
    /// Move layer `i` one step toward the top (`up`) or bottom; updates `active_layer`.
    pub fn move_layer(&mut self, i: usize, up: bool) {
        let n = self.layers.len();
        if n < 2 || i >= n {
            return;
        }
        let j = if up { i + 1 } else { i.wrapping_sub(1) };
        if j >= n {
            return;
        }
        self.layers.swap(i, j);
        if self.active_layer == i {
            self.active_layer = j;
        } else if self.active_layer == j {
            self.active_layer = i;
        }
        self.layers_dirty = true;
    }

    /// Read back layer `i`'s raw RGBA8 pixels (the layer itself, not the composite) — used
    /// by save. Blocking. Empty if `i` is out of range.
    pub fn layer_pixels(&self, i: usize) -> Vec<u8> {
        let canvas = match self.layers.get(i) {
            Some(l) => &l.canvas,
            None => return Vec::new(),
        };
        let (w, h) = (canvas.width(), canvas.height());
        let bpr = w * 4;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("layer readback enc") });
        enc.copy_texture_to_buffer(
            canvas.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        slice.get_mapped_range().to_vec()
    }

    /// Build a fresh comp_a/comp_b-style scratch texture + view at `width`×`height`.
    fn make_scratch_texture(device: &wgpu::Device, width: u32, height: u32, label: &str) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        (tex, view)
    }

    /// Replace the whole layer stack (used on project load). The **first** loaded layer's
    /// dimensions become the document's canonical canvas size (M5: no longer forced square);
    /// `raster`/`comp_a`/`comp_b` are recreated to match. A layer whose buffer doesn't match
    /// its own recorded dims is left transparent (defends against a corrupt blob). Resets the
    /// active layer to 0.
    pub fn replace_layers(&mut self, loaded: Vec<LoadedLayer>) {
        if loaded.is_empty() {
            return;
        }
        let (doc_w, doc_h) = (loaded[0].width.max(1), loaded[0].height.max(1));

        // Recreate the display cache + comp buffers to match the loaded document's canvas.
        self.raster = RasterCanvas::new(&self.device, doc_w, doc_h, [0.97, 0.96, 0.94, 1.0]);
        let (comp_a, comp_a_view) = Self::make_scratch_texture(&self.device, doc_w, doc_h, "layer comp a");
        let (comp_b, comp_b_view) = Self::make_scratch_texture(&self.device, doc_w, doc_h, "layer comp b");
        self.comp_a = comp_a;
        self.comp_a_view = comp_a_view;
        self.comp_b = comp_b;
        self.comp_b_view = comp_b_view;
        {
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("load canvas clear") });
            self.raster.clear(&mut enc);
            self.queue.submit(Some(enc.finish()));
        }
        // The paint bind group holds a view into `self.raster`'s old texture — rebuild it.
        self.paint_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paint bind group"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(self.raster.texture_view()) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.paint_sampler) },
            ],
        });

        let mut layers = Vec::with_capacity(loaded.len());
        for l in loaded {
            let expected = (l.width as usize) * (l.height as usize) * 4;
            let mut canvas = RasterCanvas::new(&self.device, doc_w, doc_h, [0.0, 0.0, 0.0, 0.0]);
            if l.rgba.len() == expected && l.width == doc_w && l.height == doc_h {
                canvas.upload_rgba(&self.queue, &l.rgba);
            } else {
                let mut enc = self
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("load layer clear") });
                canvas.clear(&mut enc);
                self.queue.submit(Some(enc.finish()));
            }
            layers.push(PaintLayer { canvas, name: l.name, visible: l.visible, opacity: l.opacity, blend: l.blend });
        }
        self.layers = layers;
        self.active_layer = 0;
        self.layers_dirty = true;
    }

    /// **M5**: replace the entire paint substrate (display cache, comp buffers, and the
    /// layer stack — collapsed to one Background layer) with `pixels` at `width`×`height`.
    /// Used by "Import Image" so the canvas takes the image's own aspect ratio instead of
    /// being forced into the old fixed square. Not undoable (like opening a project, this
    /// is a hard reset of the paint substrate — the prior canvas's dimensions are gone).
    pub fn import_replace_canvas(&mut self, width: u32, height: u32, pixels: &[u8]) {
        let (w, h) = (width.max(1), height.max(1));
        self.raster = RasterCanvas::new(&self.device, w, h, [0.97, 0.96, 0.94, 1.0]);
        let (comp_a, comp_a_view) = Self::make_scratch_texture(&self.device, w, h, "layer comp a");
        let (comp_b, comp_b_view) = Self::make_scratch_texture(&self.device, w, h, "layer comp b");
        self.comp_a = comp_a;
        self.comp_a_view = comp_a_view;
        self.comp_b = comp_b;
        self.comp_b_view = comp_b_view;
        self.paint_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paint bind group"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(self.raster.texture_view()) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.paint_sampler) },
            ],
        });

        let mut canvas = RasterCanvas::new(&self.device, w, h, [0.97, 0.96, 0.94, 1.0]);
        canvas.upload_rgba(&self.queue, pixels);
        self.layers = vec![PaintLayer {
            canvas,
            name: "Background".to_string(),
            visible: true,
            opacity: 1.0,
            blend: suite_doc::BlendMode::Normal,
        }];
        self.active_layer = 0;
        self.layers_dirty = true;
    }

    /// **M5**: rotate the whole document 90° (every layer + the comp/display buffers
    /// together), swapping the canvas's width↔height. A per-layer rotate can't do this
    /// safely once the canvas may be non-square — see `LayerTransform`'s doc comment.
    /// Not undoable (a structural resize, same posture as `import_replace_canvas`).
    pub fn rotate_canvas_90(&mut self, dir: CanvasRotate) {
        let cw = matches!(dir, CanvasRotate::Cw);
        let (old_w, old_h) = (self.raster.width() as usize, self.raster.height() as usize);
        let (new_w, new_h) = (old_h as u32, old_w as u32);

        // Drain layers 0-first: each iteration reads layer 0's pixels, rotates them, THEN
        // removes layer 0 — reading `layer_pixels(i)` against the shrinking vec instead
        // would desync the pixels from the wrong layer's metadata after the first removal.
        let n = self.layers.len();
        let mut rotated_layers: Vec<(Vec<u8>, PaintLayer)> = Vec::with_capacity(n);
        for _ in 0..n {
            let src = self.layer_pixels(0);
            let rotated = rotate_canvas_pixels(&src, old_w, old_h, cw);
            rotated_layers.push((rotated, self.layers.remove(0)));
        }

        self.raster = RasterCanvas::new(&self.device, new_w, new_h, [0.97, 0.96, 0.94, 1.0]);
        let (comp_a, comp_a_view) = Self::make_scratch_texture(&self.device, new_w, new_h, "layer comp a");
        let (comp_b, comp_b_view) = Self::make_scratch_texture(&self.device, new_w, new_h, "layer comp b");
        self.comp_a = comp_a;
        self.comp_a_view = comp_a_view;
        self.comp_b = comp_b;
        self.comp_b_view = comp_b_view;
        self.paint_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paint bind group"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(self.raster.texture_view()) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.paint_sampler) },
            ],
        });

        self.layers = rotated_layers
            .into_iter()
            .map(|(pixels, old)| {
                let mut canvas = RasterCanvas::new(&self.device, new_w, new_h, [0.0, 0.0, 0.0, 0.0]);
                canvas.upload_rgba(&self.queue, &pixels);
                PaintLayer { canvas, name: old.name, visible: old.visible, opacity: old.opacity, blend: old.blend }
            })
            .collect();
        self.layers_dirty = true;
    }

    /// **M4: Move.** Drag-offset the active layer's pixels by `(dx, dy)` texels. With an
    /// active selection, only the selected pixels move (leaving a transparent hole, same as
    /// Photoshop's Move tool with a selection active); with none, the whole layer shifts.
    /// Dimension-preserving, so it's always safe regardless of canvas aspect (M5) — a single
    /// undoable edit (commit-on-release; see `apps/visual`'s Move tool for the drag UX).
    ///
    /// Returns the new selection rect in UV space (clamped to canvas bounds) if a selection
    /// was active, so the caller can update the on-screen marching-ants immediately.
    pub fn move_active_layer(&mut self, dx: i32, dy: i32) -> Option<[f32; 4]> {
        let (w, h) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width() as usize, c.height() as usize)
        };
        let src = self.read_active_layer_rgba();

        // Tier 1: an exact-shape selection rasterizes into a mask on demand (see
        // `paint_gradient_fill`) so Move only cuts the pixels actually inside it, not the
        // whole bounding rect.
        let mask = self.current_selection_mask(w, h);

        let sel_texel = self.selection_rect.and_then(|[x0, y0, x1, y1]| {
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let sx0 = ((x0 * w as f32).floor() as usize).min(w.saturating_sub(1));
            let sy0 = ((y0 * h as f32).floor() as usize).min(h.saturating_sub(1));
            let sw = (((x1 * w as f32).ceil() as usize).saturating_sub(sx0)).max(1);
            let sh = (((y1 * h as f32).ceil() as usize).saturating_sub(sy0)).max(1);
            Some((sx0, sy0, sw, sh))
        });

        let (dst, new_selection_uv) = match sel_texel {
            Some((sx0, sy0, sw, sh)) => {
                let moved = move_selection_pixels(&src, w, h, sx0, sy0, sw, sh, dx, dy, mask.as_deref());
                let nx0 = (sx0 as i32 + dx).clamp(0, w as i32) as f32 / w as f32;
                let ny0 = (sy0 as i32 + dy).clamp(0, h as i32) as f32 / h as f32;
                let nx1 = ((sx0 + sw) as i32 + dx).clamp(0, w as i32) as f32 / w as f32;
                let ny1 = ((sy0 + sh) as i32 + dy).clamp(0, h as i32) as f32 / h as f32;
                (moved, Some([nx0, ny0, nx1, ny1]))
            }
            None => (shift_pixels(&src, w, h, dx, dy), None),
        };
        self.write_active_layer_undoable(&dst);

        // Shift the exact shape by the same UV delta so it stays aligned with the moved
        // content (an Ellipse/Lasso selection should follow what it selected, same as
        // Photoshop's marching ants tracking a moved selection).
        if let Some(shape) = self.selection_extra.as_mut() {
            let (udx, udy) = (dx as f32 / w as f32, dy as f32 / h as f32);
            match shape {
                SelectionShape::Ellipse { cx, cy, .. } => {
                    *cx += udx;
                    *cy += udy;
                }
                SelectionShape::Polygon(points) => {
                    for p in points.iter_mut() {
                        p[0] += udx;
                        p[1] += udy;
                    }
                }
                SelectionShape::Mask { width: mw, height: mh, data } => {
                    // A raw mask has no vertices to offset — shift its pixel data instead
                    // (same one-channel logic as `shift_pixels`, just 1 byte/texel).
                    let (mw, mh) = (*mw as usize, *mh as usize);
                    let mut shifted = vec![0u8; data.len()];
                    for sy in 0..mh {
                        let ny = sy as i32 + dy;
                        if ny < 0 || ny >= mh as i32 {
                            continue;
                        }
                        for sx in 0..mw {
                            let nx = sx as i32 + dx;
                            if nx < 0 || nx >= mw as i32 {
                                continue;
                            }
                            shifted[ny as usize * mw + nx as usize] = data[sy * mw + sx];
                        }
                    }
                    *data = shifted;
                }
            }
        }
        new_selection_uv
    }

    /// **M4c: Free Transform.** Uniform scale then rotate the active layer's pixels about
    /// `pivot_uv`, selection-aware like Move (only the selected region moves, leaving a
    /// transparent hole, when a selection is active). `pivot_uv`/`scale`/`rotation` are
    /// supplied by the caller (`tools.rs`), derived from whichever drag handle the user
    /// grabbed — a corner scales anchored at the opposite corner; the rotate handle rotates
    /// about the box's center. Thin GPU-readback/write wrapper around the pure
    /// `apply_free_transform`. A single commit, same shape as `move_active_layer`.
    pub fn free_transform_active_layer(&mut self, pivot_uv: [f32; 2], scale: f32, rotation: f32) {
        let (w, h) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width() as usize, c.height() as usize)
        };
        let src = self.read_active_layer_rgba();
        let mask = self.current_selection_mask(w, h);
        let region = self.selection_rect.and_then(|[x0, y0, x1, y1]| {
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let sx0 = ((x0 * w as f32).floor() as usize).min(w.saturating_sub(1));
            let sy0 = ((y0 * h as f32).floor() as usize).min(h.saturating_sub(1));
            let sw = (((x1 * w as f32).ceil() as usize).saturating_sub(sx0)).max(1);
            let sh = (((y1 * h as f32).ceil() as usize).saturating_sub(sy0)).max(1);
            Some((sx0, sy0, sw, sh))
        });
        let pivot = (pivot_uv[0] * w as f32, pivot_uv[1] * h as f32);
        let dst = apply_free_transform(&src, w, h, region, pivot, scale, rotation, mask.as_deref());
        self.write_active_layer_undoable(&dst);
    }

    /// **M4a: Text.** Lay out `text` as a single line (see `font::layout_line`) starting at
    /// `anchor_uv`, rasterize each glyph (see `font::rasterize_glyph`), and composite them
    /// onto the active layer in `color` (linear RGBA) — one undoable write for the whole
    /// string, same "readback, mutate in CPU memory, write back once" shape as every other
    /// M4 tool. Respects the active selection (mask-aware, same convention as Paint/Gradient/
    /// Move/Free Transform).
    pub fn apply_text(&mut self, font: &font::Font, text: &str, anchor_uv: [f32; 2], point_size: f32, color: [f32; 4]) {
        let (w, h) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width() as usize, c.height() as usize)
        };
        let mut pixels = self.read_active_layer_rgba();
        let sel_mask = self.current_selection_mask(w, h);
        let scale = point_size / font.units_per_em().max(1) as f32;
        let pen_origin = (anchor_uv[0] * w as f32, anchor_uv[1] * h as f32);
        let (sr, sg, sb) = (srgb_to_linear(color[0]), srgb_to_linear(color[1]), srgb_to_linear(color[2]));

        for (glyph_id, pen_x_units) in font::layout_line(font, text) {
            let outline = font.glyph_outline(glyph_id);
            let (glyph_mask, gw, gh, ox, oy) = font::rasterize_glyph(&outline, scale);
            if gw == 0 || gh == 0 {
                continue;
            }
            let base_x = pen_origin.0 + pen_x_units * scale + ox;
            let base_y = pen_origin.1 + oy;
            for gy in 0..gh {
                let py = (base_y + gy as f32).round() as i64;
                if py < 0 || py >= h as i64 {
                    continue;
                }
                for gx in 0..gw {
                    let px = (base_x + gx as f32).round() as i64;
                    if px < 0 || px >= w as i64 {
                        continue;
                    }
                    let cov = glyph_mask[gy * gw + gx] as f32 / 255.0;
                    if cov <= 0.0 {
                        continue;
                    }
                    let (px, py) = (px as usize, py as usize);
                    let sel = match &sel_mask {
                        Some(m) => m[py * w + px] as f32 / 255.0,
                        None => 1.0,
                    };
                    let sa = color[3].clamp(0.0, 1.0) * cov * sel;
                    composite_over(&mut pixels, (py * w + px) * 4, sr, sg, sb, sa);
                }
            }
        }
        self.write_active_layer_undoable(&pixels);
    }

    /// **M4: Crop.** Crop the whole document to the UV-space rect `[x0, y0, x1, y1]` (every
    /// layer + the comp/display buffers together, so they stay aligned — same reasoning as
    /// `rotate_canvas_90`). Not undoable (a structural resize). No-op if the rect is
    /// degenerate (zero area).
    pub fn crop_to_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (old_w, old_h) = (self.raster.width() as usize, self.raster.height() as usize);
        let px0 = ((x0 * old_w as f32).floor() as usize).min(old_w.saturating_sub(1));
        let py0 = ((y0 * old_h as f32).floor() as usize).min(old_h.saturating_sub(1));
        let crop_w = (((x1 * old_w as f32).ceil() as usize).saturating_sub(px0)).max(1);
        let crop_h = (((y1 * old_h as f32).ceil() as usize).saturating_sub(py0)).max(1);
        let (new_w, new_h) = (crop_w as u32, crop_h as u32);

        // Drain layers 0-first (see `rotate_canvas_90` for why: reading `layer_pixels(i)`
        // against the shrinking vec would desync pixels from the wrong layer's metadata).
        let n = self.layers.len();
        let mut cropped_layers: Vec<(Vec<u8>, PaintLayer)> = Vec::with_capacity(n);
        for _ in 0..n {
            let src = self.layer_pixels(0);
            let cropped = crop_pixels(&src, old_w, old_h, px0, py0, crop_w, crop_h);
            cropped_layers.push((cropped, self.layers.remove(0)));
        }

        self.raster = RasterCanvas::new(&self.device, new_w, new_h, [0.97, 0.96, 0.94, 1.0]);
        let (comp_a, comp_a_view) = Self::make_scratch_texture(&self.device, new_w, new_h, "layer comp a");
        let (comp_b, comp_b_view) = Self::make_scratch_texture(&self.device, new_w, new_h, "layer comp b");
        self.comp_a = comp_a;
        self.comp_a_view = comp_a_view;
        self.comp_b = comp_b;
        self.comp_b_view = comp_b_view;
        self.paint_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paint bind group"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(self.raster.texture_view()) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.paint_sampler) },
            ],
        });

        self.layers = cropped_layers
            .into_iter()
            .map(|(pixels, old)| {
                let mut canvas = RasterCanvas::new(&self.device, new_w, new_h, [0.0, 0.0, 0.0, 0.0]);
                canvas.upload_rgba(&self.queue, &pixels);
                PaintLayer { canvas, name: old.name, visible: old.visible, opacity: old.opacity, blend: old.blend }
            })
            .collect();
        self.layers_dirty = true;
    }

    /// **Magic wand → selection** (Tier 1 Quick Selection): flood-fill from `seed_uv` over
    /// the flattened composite (so it can select across what's visible, not just the active
    /// layer — same sampling source as `paint_readback_rgba`, used by Export/save). Returns
    /// the region as a `SelectionShape::Mask` rather than painting it directly: the caller
    /// assigns it to `selection_extra`, so any subsequent Paint/Gradient/Move (Tier 1's mask
    /// machinery) respects the exact flood-filled shape, same as an Ellipse/Lasso selection —
    /// Magic Wand used to flood-fill pixels immediately and irreversibly outside undo's
    /// per-stroke granularity; as a selection it composes with every other tool instead.
    /// Thin GPU-readback wrapper around the pure `flood_fill_mask` (see there for the BFS).
    pub fn magic_wand_select(&mut self, seed_uv: [f32; 2], tolerance: f32) -> Option<SelectionShape> {
        let (width, height) = (self.raster.width() as usize, self.raster.height() as usize);
        let pixels = self.paint_readback_rgba();
        let (mask, count) = flood_fill_mask(&pixels, width, height, seed_uv, tolerance);
        if count == 0 {
            return None;
        }
        Some(SelectionShape::Mask { width: width as u32, height: height as u32, data: mask })
    }

    // ----- M4: core 2D ops (gradient fill, layer transforms) ---------------------------

    /// GPU→CPU readback of just the **active layer's** canvas (not the flattened
    /// composite) as row-major RGBA8 (top-left origin). Blocking. The stored texture is
    /// `Rgba8UnormSrgb`, so the returned bytes are sRGB-encoded.
    fn read_active_layer_rgba(&self) -> Vec<u8> {
        let canvas = &self.layers[self.active_layer].canvas;
        let (w, h) = (canvas.width(), canvas.height());
        let bpr = w * 4;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("layer readback enc"),
        });
        enc.copy_texture_to_buffer(
            canvas.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range();
        data.to_vec()
    }

    /// Upload `pixels` into the active layer as one undoable edit (so ⌘Z reverts it).
    fn write_active_layer_undoable(&mut self, pixels: &[u8]) {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("layer write enc"),
        });
        self.layers[self.active_layer]
            .canvas
            .upload_rgba_undoable(&self.device, &self.queue, &mut enc, pixels);
        self.queue.submit(Some(enc.finish()));
        self.layers_dirty = true;
    }

    /// **Gradient fill** on the active layer. Interpolates between `color_a` (at `from_uv`)
    /// and `color_b` (at `to_uv`), both **linear** RGBA. `radial=false` is a linear gradient
    /// along the from→to vector; `radial=true` is a radial gradient centred at `from_uv` with
    /// radius `|to_uv − from_uv|`. The gradient colour is alpha-composited over existing
    /// pixels. Respects the active selection rect (only pixels inside are touched). Undoable.
    pub fn paint_gradient_fill(
        &mut self,
        from_uv: [f32; 2],
        to_uv: [f32; 2],
        color_a: [f32; 4],
        color_b: [f32; 4],
        radial: bool,
    ) {
        let (w, h) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width() as usize, c.height() as usize)
        };
        let mut pixels = self.read_active_layer_rgba();
        // Tier 1: an exact-shape selection (Ellipse/Lasso) rasterizes into a mask on demand
        // — cheap enough here since it happens once per gradient drag, not once per frame.
        let mask = self.current_selection_mask(w, h);
        apply_gradient_fill(
            &mut pixels,
            w,
            h,
            from_uv,
            to_uv,
            color_a,
            color_b,
            radial,
            self.selection_rect,
            mask.as_deref(),
        );
        self.write_active_layer_undoable(&pixels);
    }

    /// **Layer transform** on the active layer's pixels: flip/180°-rotate as a single
    /// undoable edit. Operates on a CPU readback then re-uploads. Always dimension-preserving
    /// (see `LayerTransform`'s doc comment) — a 90° rotation is `rotate_canvas_90` instead.
    pub fn transform_active_layer(&mut self, op: LayerTransform) {
        let (w, h) = {
            let c = &self.layers[self.active_layer].canvas;
            (c.width() as usize, c.height() as usize)
        };
        let src = self.read_active_layer_rgba();
        let dst = apply_layer_transform(&src, w, h, op);
        self.write_active_layer_undoable(&dst);
    }

    /// Per-frame render. `gizmo` optionally draws a translate-gizmo overlay at the
    /// supplied world-space origin. `egui_paint` optionally draws an egui frame on
    /// top after the scene + overlays.
    pub fn render(
        &mut self,
        doc: &Document,
        gizmo: Option<GizmoOverlay>,
        egui_paint: Option<
            &mut dyn FnMut(
                &mut wgpu::CommandEncoder,
                &wgpu::TextureView,
                &wgpu::Device,
                &wgpu::Queue,
                (u32, u32),
            ),
        >,
    ) -> RenderResult {
        // Flatten the 2D layer stack into the display cache before drawing, if it changed.
        if self.layers_dirty {
            self.composite_layers();
        }
        let aspect = self.aspect();
        let view = self.camera.view();
        let proj = self.camera.proj(aspect);
        let view_proj = proj * view;
        let inv_view_proj = view_proj.inverse();
        let eye = self.camera.eye();
        let camera_uniform = CameraUniform {
            view_proj: view_proj.to_cols_array_2d(),
            inv_view_proj: inv_view_proj.to_cols_array_2d(),
            eye: [eye.x, eye.y, eye.z, 1.0],
            proj_kind_and_pad: [
                match self.camera.projection {
                    Projection::Perspective => 0.0,
                    Projection::Orthographic => 1.0,
                },
                0.0,
                0.0,
                0.0,
            ],
        };
        self.queue.write_buffer(
            &self.camera_uniform_buffer,
            0,
            bytemuck::bytes_of(&camera_uniform),
        );

        // Slot 0 is reserved for "world-space identity" — used by the gizmo and any
        // future overlay that wants to pass vertices already in world coords.
        let identity_uniform = ObjectUniform {
            model: Mat4::IDENTITY.to_cols_array_2d(),
            color: [1.0, 1.0, 1.0, 1.0],
            selected_pad: [0.0, 0.0, 0.0, 0.0],
        };
        self.queue.write_buffer(
            &self.object_uniform_buffer,
            0,
            bytemuck::bytes_of(&identity_uniform),
        );

        // Pack visible objects into slots 1..; bail if we'd overflow the buffer.
        let selection = doc.selection();
        let mut draws: Vec<(ObjId, ObjectKind, u32)> = Vec::new();
        for (i, object) in doc.objects().enumerate() {
            if !object.visibility {
                continue;
            }
            let slot_idx = (i as u32).saturating_add(1);
            if slot_idx >= self.object_slots {
                break;
            }
            let uniform = ObjectUniform {
                model: object.world_matrix().to_cols_array_2d(),
                color: object.color,
                selected_pad: [
                    if Some(object.id) == selection {
                        1.0
                    } else {
                        0.0
                    },
                    0.0,
                    0.0,
                    0.0,
                ],
            };
            let offset = slot_idx as u64 * OBJECT_SLOT_BYTES;
            self.queue.write_buffer(
                &self.object_uniform_buffer,
                offset,
                bytemuck::bytes_of(&uniform),
            );
            draws.push((object.id, object.kind, slot_idx));
        }

        // Tessellate every editable Mesh object into a transient vertex buffer up-front so
        // the buffers outlive the render pass. Small meshes → cheap to rebuild each frame;
        // a dirty-flag cache is the optimization once meshes get heavy.
        // Meshes with a paint texture use MeshVertex (with UV) and the mesh_paint_pipeline.
        let mut mesh_draws: Vec<(u32, wgpu::Buffer, u32)> = Vec::new();
        let mut mesh_paint_draws: Vec<(suite_doc::ObjId, u32, wgpu::Buffer, u32)> = Vec::new();
        for &(id, kind, slot_idx) in &draws {
            if kind != ObjectKind::Mesh {
                continue;
            }
            let Some(obj) = doc.get(id) else { continue };
            // The display mesh = base with the modifier stack applied (clone when empty).
            let Some(display) = obj.display_mesh() else {
                continue;
            };
            // Face highlight only maps to display faces when there are no modifiers (a
            // generated mesh's face indices don't correspond to the editable base).
            let hi_face = if Some(id) == selection && obj.modifiers.is_empty() {
                doc.selected_face()
            } else {
                None
            };
            if self.mesh_textures.contains_key(&id) {
                let verts = tessellate_mesh_painted(&display, hi_face);
                if verts.is_empty() { continue; }
                let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mesh paint transient vbuf"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                mesh_paint_draws.push((id, slot_idx, buf, verts.len() as u32));
            } else {
                let verts = tessellate_mesh(&display, hi_face);
                if verts.is_empty() { continue; }
                let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mesh transient vbuf"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                mesh_draws.push((slot_idx, buf, verts.len() as u32));
            }
        }

        let frame = match self.surface_state.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return RenderResult::Skipped,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return RenderResult::SurfaceLostOrOutdated;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let frame_start = Instant::now();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(linear_to_clear(CHROME_BG0_LINEAR)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.surface_state.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rpass.set_bind_group(0, &self.camera_bind_group, &[]);

            // Opaque primitives: cubes, spheres (fixed meshes).
            for &(_, kind, slot_idx) in &draws {
                let mesh = match kind {
                    ObjectKind::Cube => &self.cube_mesh,
                    ObjectKind::Sphere => &self.sphere_mesh,
                    ObjectKind::ImagePlane
                    | ObjectKind::PaintCanvas
                    | ObjectKind::Mesh
                    | ObjectKind::Adjustment => continue,
                };
                let offset = slot_idx * OBJECT_SLOT_BYTES as u32;
                rpass.set_pipeline(&self.scene_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[offset]);
                rpass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                rpass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }

            // Editable meshes (tessellated this frame), non-indexed.
            for (slot_idx, buf, vcount) in &mesh_draws {
                let offset = slot_idx * OBJECT_SLOT_BYTES as u32;
                rpass.set_pipeline(&self.scene_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[offset]);
                rpass.set_vertex_buffer(0, buf.slice(..));
                rpass.draw(0..*vcount, 0..1);
            }

            // Editable meshes with per-mesh paint textures (MeshVertex + mesh_paint_pipeline).
            for (id, slot_idx, buf, vcount) in &mesh_paint_draws {
                let Some((_, paint_bg)) = self.mesh_textures.get(id) else { continue };
                let offset = slot_idx * OBJECT_SLOT_BYTES as u32;
                rpass.set_pipeline(&self.mesh_paint_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[offset]);
                rpass.set_bind_group(2, paint_bg, &[]);
                rpass.set_vertex_buffer(0, buf.slice(..));
                rpass.draw(0..*vcount, 0..1);
            }

            // Textured planes: image planes sample the checker, paint canvases sample the
            // live raster substrate. Both use the same quad pipeline + plane mesh.
            for &(_, kind, slot_idx) in &draws {
                let bind = match kind {
                    ObjectKind::ImagePlane => &self.quad_bind_group,
                    ObjectKind::PaintCanvas => &self.paint_bind_group,
                    _ => continue,
                };
                let offset = slot_idx * OBJECT_SLOT_BYTES as u32;
                rpass.set_pipeline(&self.quad_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[offset]);
                rpass.set_bind_group(2, bind, &[]);
                rpass.set_vertex_buffer(0, self.plane_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(
                    self.plane_mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint16,
                );
                rpass.draw_indexed(0..self.plane_mesh.index_count, 0, 0..1);
            }

            // Selection outline (wireframe AABB), drawn on top with depth read-only.
            if let Some(sel_id) = selection {
                if let Some(slot_idx) = draws.iter().position(|d| d.0 == sel_id) {
                    let offset = slot_idx as u32 * OBJECT_SLOT_BYTES as u32;
                    let aabb = doc
                        .get(sel_id)
                        .map(|o| o.local_aabb)
                        .unwrap_or_else(suite_doc::Aabb::unit);
                    let lines = aabb_line_vertices(aabb, ACCENT_BASE_LINEAR);
                    // One-shot vertex buffer for the selection lines. The buffer is
                    // dropped at end of frame; with ~24 verts it's cheap.
                    let outline_buf =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("outline lines vbuf"),
                                contents: bytemuck::cast_slice(&lines),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                    rpass.set_pipeline(&self.outline_pipeline);
                    rpass.set_bind_group(1, &self.object_bind_group, &[offset]);
                    rpass.set_vertex_buffer(0, outline_buf.slice(..));
                    rpass.draw(0..lines.len() as u32, 0..1);
                    // Keep the buffer alive long enough by leaking into a stack-frame
                    // slot. (wgpu refcounts the backing buffer; the wrapper dropping
                    // here after the draw is fine because the encoder retains the bind.)
                    drop(outline_buf);
                }
            }

            // Translate gizmo on top of the scene, depth-tested so it can hide behind
            // distant geometry but always wins ties against the same surface.
            if let Some(g) = gizmo {
                let lines = gizmo_axis_lines(g);
                let buf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("translate gizmo vbuf"),
                        contents: bytemuck::cast_slice(&lines),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                rpass.set_pipeline(&self.outline_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[0]); // identity slot
                rpass.set_vertex_buffer(0, buf.slice(..));
                rpass.draw(0..lines.len() as u32, 0..1);
                drop(buf);
            }

            // Snap indicator — a small bright sphere at the magnetic snap point.
            if let Some(snap_pos) = self.snap_indicator {
                let scale = 0.08_f32;
                let model = glam::Mat4::from_scale_rotation_translation(
                    glam::Vec3::splat(scale),
                    glam::Quat::IDENTITY,
                    glam::Vec3::from(snap_pos),
                );
                let snap_uniform = ObjectUniform {
                    model: model.to_cols_array_2d(),
                    color: [1.0, 0.85, 0.0, 1.0], // bright yellow
                    selected_pad: [0.0; 4],
                };
                // Write into the identity slot (slot 0 is safe — grid and gizmo already drew).
                self.queue.write_buffer(
                    &self.object_uniform_buffer,
                    0,
                    bytemuck::bytes_of(&snap_uniform),
                );
                rpass.set_pipeline(&self.scene_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[0]);
                rpass.set_vertex_buffer(0, self.sphere_mesh.vertex_buffer.slice(..));
                rpass.set_index_buffer(self.sphere_mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                rpass.draw_indexed(0..self.sphere_mesh.index_count, 0, 0..1);
            }

            // Skeleton overlay — bone segments + a small joint cross at each head. Drawn
            // with the line pipeline in the identity slot, depth-tested so bones inside the
            // mesh are occluded by it (reads as "the rig lives in the surface").
            if !self.skeleton_segments.is_empty() {
                let lines = skeleton_lines(&self.skeleton_segments);
                let buf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("skeleton vbuf"),
                        contents: bytemuck::cast_slice(&lines),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                rpass.set_pipeline(&self.outline_pipeline);
                rpass.set_bind_group(1, &self.object_bind_group, &[0]); // identity slot
                rpass.set_vertex_buffer(0, buf.slice(..));
                rpass.draw(0..lines.len() as u32, 0..1);
                drop(buf);
            }

            // Procedural infinite grid — full-screen triangle, drawn last so transparency works.
            rpass.set_pipeline(&self.grid_pipeline);
            rpass.set_bind_group(1, &self.object_bind_group, &[0]); // unused but satisfies layout
            rpass.draw(0..3, 0..1);
        }

        if let Some(egui_paint) = egui_paint {
            (egui_paint)(&mut encoder, &view, &self.device, &self.queue, self.size());
        }

        let cpu_submit_done = Instant::now();
        self.queue.submit(Some(encoder.finish()));
        let cpu_submit_ms = cpu_submit_done.duration_since(frame_start).as_secs_f32() * 1000.0;
        self.window.pre_present_notify();
        frame.present();

        self.budget.record(cpu_submit_ms);
        RenderResult::Presented
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderResult {
    Presented,
    Skipped,
    SurfaceLostOrOutdated,
}

// ---------- Mesh helpers ----------

fn build_mesh(device: &wgpu::Device, mesh: &(Vec<Vertex>, Vec<u16>)) -> MeshAsset {
    let (vertices, indices) = mesh;
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh vbuf"),
        contents: bytemuck::cast_slice(vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh ibuf"),
        contents: bytemuck::cast_slice(indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    MeshAsset {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
    }
}

/// Fan-triangulate an editable `suite_doc::Mesh` into flat-shaded vertices. Each face's
/// flat normal drives a simple lambert against a fixed sun so the geometry reads as 3D
/// without a real lighting pass (matching the sphere's shading).
fn tessellate_mesh(mesh: &suite_doc::Mesh, highlight_face: Option<usize>) -> Vec<Vertex> {
    let sun = glam::Vec3::new(0.4, 0.8, 0.45).normalize();
    let mut out = Vec::new();
    for (fi, face) in mesh.faces.iter().enumerate() {
        if face.indices.len() < 3 {
            continue;
        }
        let n = mesh.face_normal(face);
        let nv = glam::Vec3::new(n[0], n[1], n[2]);
        let ndotl = nv.dot(sun).max(0.0);
        let shade = 0.28 + 0.72 * ndotl;
        let color = if Some(fi) == highlight_face {
            // Accent-tinted so the focused face reads clearly.
            [
                0.18 + 0.30 * shade,
                0.34 + 0.30 * shade,
                0.86 * shade.max(0.5),
                1.0,
            ]
        } else {
            [0.62 * shade, 0.66 * shade, 0.72 * shade, 1.0]
        };
        // Fan: (v0, vk, vk+1).
        let v0 = mesh.vertex(face.indices[0]);
        for k in 1..face.indices.len() - 1 {
            let a = mesh.vertex(face.indices[k]);
            let b = mesh.vertex(face.indices[k + 1]);
            out.push(Vertex {
                position: v0,
                _pad0: 0.0,
                color,
            });
            out.push(Vertex {
                position: a,
                _pad0: 0.0,
                color,
            });
            out.push(Vertex {
                position: b,
                _pad0: 0.0,
                color,
            });
        }
    }
    out
}

/// Tessellate a mesh with UV coordinates for mesh-paint rendering. Uses box (triplanar)
/// UV projection: the dominant axis of the face normal picks which two world-space axes
/// drive UV so the paint texture wraps predictably without a UV unwrap.
fn tessellate_mesh_painted(
    mesh: &suite_doc::Mesh,
    highlight_face: Option<usize>,
) -> Vec<MeshVertex> {
    let sun = glam::Vec3::new(0.4, 0.8, 0.45).normalize();
    let mut out = Vec::new();
    for (fi, face) in mesh.faces.iter().enumerate() {
        if face.indices.len() < 3 {
            continue;
        }
        let n = mesh.face_normal(face);
        let nv = glam::Vec3::new(n[0], n[1], n[2]);
        let ndotl = nv.dot(sun).max(0.0);
        let shade = 0.28 + 0.72 * ndotl;
        let color = if Some(fi) == highlight_face {
            [
                0.18 + 0.30 * shade,
                0.34 + 0.30 * shade,
                0.86 * shade.max(0.5),
                1.0,
            ]
        } else {
            [0.62 * shade, 0.66 * shade, 0.72 * shade, 1.0]
        };
        // Box UV: dominant face-normal axis selects the UV plane.
        // ax, ay are the two vertex component indices for U and V.
        let abs_n = nv.abs();
        let (ax, ay) = if abs_n.x >= abs_n.y && abs_n.x >= abs_n.z {
            (2usize, 1usize) // YZ plane
        } else if abs_n.y >= abs_n.x && abs_n.y >= abs_n.z {
            (0, 2) // XZ plane
        } else {
            (0, 1) // XY plane
        };
        let uv_of = |pos: [f32; 3]| -> [f32; 2] {
            [pos[ax] * 0.5 + 0.5, pos[ay] * 0.5 + 0.5]
        };

        let v0 = mesh.vertex(face.indices[0]);
        for k in 1..face.indices.len() - 1 {
            let a = mesh.vertex(face.indices[k]);
            let b = mesh.vertex(face.indices[k + 1]);
            out.push(MeshVertex { position: v0, _pad0: 0.0, color, uv: uv_of(v0), _pad1: [0.0; 2] });
            out.push(MeshVertex { position: a,  _pad0: 0.0, color, uv: uv_of(a),  _pad1: [0.0; 2] });
            out.push(MeshVertex { position: b,  _pad0: 0.0, color, uv: uv_of(b),  _pad1: [0.0; 2] });
        }
    }
    out
}

fn cube_vertices() -> (Vec<Vertex>, Vec<u16>) {
    let face_colors: [[f32; 4]; 6] = [
        [0.95, 0.34, 0.30, 1.0],
        [0.25, 0.71, 0.49, 1.0],
        [0.89, 0.70, 0.25, 1.0],
        [0.55, 0.51, 0.94, 1.0],
        [0.30, 0.56, 0.86, 1.0],
        [0.86, 0.45, 0.66, 1.0],
    ];
    let raw: [[[f32; 3]; 4]; 6] = [
        [
            [0.5, -0.5, 0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [0.5, 0.5, 0.5],
        ],
        [
            [-0.5, -0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [-0.5, 0.5, 0.5],
            [-0.5, 0.5, -0.5],
        ],
        [
            [-0.5, 0.5, 0.5],
            [0.5, 0.5, 0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
        ],
        [
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, -0.5, 0.5],
            [-0.5, -0.5, 0.5],
        ],
        [
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ],
        [
            [0.5, -0.5, -0.5],
            [-0.5, -0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [0.5, 0.5, -0.5],
        ],
    ];
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (face_idx, corners) in raw.iter().enumerate() {
        let base = (face_idx * 4) as u16;
        let color = face_colors[face_idx];
        for c in corners {
            vertices.push(Vertex {
                position: *c,
                _pad0: 0.0,
                color,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

fn uv_sphere_vertices(longitudes: u32, latitudes: u32) -> (Vec<Vertex>, Vec<u16>) {
    let radius = 0.5;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for lat in 0..=latitudes {
        let theta = lat as f32 / latitudes as f32 * std::f32::consts::PI;
        let st = theta.sin();
        let ct = theta.cos();
        for lon in 0..=longitudes {
            let phi = lon as f32 / longitudes as f32 * std::f32::consts::TAU;
            let sp = phi.sin();
            let cp = phi.cos();
            let x = radius * st * cp;
            let y = radius * ct;
            let z = radius * st * sp;
            // Shade from a fixed sun direction so the sphere doesn't look flat without lighting.
            let n = glam::Vec3::new(x, y, z).normalize_or_zero();
            let l = glam::Vec3::new(0.55, 0.7, 0.45).normalize();
            let ndotl = n.dot(l).max(0.0);
            let t = 0.25 + 0.75 * ndotl;
            vertices.push(Vertex {
                position: [x, y, z],
                _pad0: 0.0,
                color: [t, t, t, 1.0],
            });
        }
    }
    let stride = longitudes + 1;
    for lat in 0..latitudes {
        for lon in 0..longitudes {
            let a = lat * stride + lon;
            let b = a + stride;
            let c = b + 1;
            let d = a + 1;
            indices
                .extend_from_slice(&[a as u16, b as u16, c as u16, a as u16, c as u16, d as u16]);
        }
    }
    (vertices, indices)
}

fn plane_vertices() -> (Vec<Vertex>, Vec<u16>) {
    let v = |x: f32, y: f32, color: [f32; 4]| Vertex {
        position: [x, y, 0.0],
        _pad0: 0.0,
        color,
    };
    let vertices = vec![
        v(-0.5, -0.5, [0.89, 0.70, 0.25, 1.0]),
        v(0.5, -0.5, [0.89, 0.70, 0.25, 1.0]),
        v(0.5, 0.5, [0.89, 0.70, 0.25, 1.0]),
        v(-0.5, 0.5, [0.89, 0.70, 0.25, 1.0]),
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];
    (vertices, indices)
}

fn gizmo_axis_lines(g: GizmoOverlay) -> Vec<Vertex> {
    // Each axis: a shaft from the origin to origin + axis * scale, plus a small
    // 4-line "X" arrowhead at the tip so a LineList is enough (no triangles).
    let highlight_tint = |base: [f32; 4], lit: bool| -> [f32; 4] {
        if lit {
            [
                (base[0] * 0.4 + 0.6).min(1.0),
                (base[1] * 0.4 + 0.6).min(1.0),
                (base[2] * 0.4 + 0.6).min(1.0),
                base[3],
            ]
        } else {
            base
        }
    };
    let mut out: Vec<Vertex> = Vec::with_capacity(3 * 12);
    let scale = g.world_scale.max(0.05);
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let base_color = match axis {
            GizmoAxis::X => [0.95, 0.34, 0.30, 1.0],
            GizmoAxis::Y => [0.25, 0.71, 0.49, 1.0],
            GizmoAxis::Z => [0.30, 0.56, 0.86, 1.0],
        };
        let color = highlight_tint(base_color, g.highlighted == Some(axis));
        let dir = axis.unit();
        let origin = g.origin;
        let tip = origin + dir * scale;
        // Shaft.
        out.push(Vertex {
            position: origin.into(),
            _pad0: 0.0,
            color,
        });
        out.push(Vertex {
            position: tip.into(),
            _pad0: 0.0,
            color,
        });
        // Tiny "X" cross at the tip — two diagonal lines in the plane perpendicular to the axis.
        let head_len = scale * 0.16;
        let perp = orthogonal_basis(dir);
        let head_tip = tip + dir * head_len * 0.6;
        for sign in [1.0_f32, -1.0_f32] {
            let p = tip + (perp.0 + perp.1) * sign * head_len * 0.5;
            let q = tip - (perp.0 + perp.1) * sign * head_len * 0.5;
            out.push(Vertex {
                position: p.into(),
                _pad0: 0.0,
                color,
            });
            out.push(Vertex {
                position: head_tip.into(),
                _pad0: 0.0,
                color,
            });
            out.push(Vertex {
                position: q.into(),
                _pad0: 0.0,
                color,
            });
            out.push(Vertex {
                position: head_tip.into(),
                _pad0: 0.0,
                color,
            });
        }
    }
    out
}

/// Build a LineList for a skeleton: each bone is a shaft (head→tail) plus a small
/// 3-axis cross at the head so joints are visible. Bone color is a warm bone-white;
/// the cross is a brighter accent so the joint reads against the shaft.
fn skeleton_lines(segments: &[([f32; 3], [f32; 3])]) -> Vec<Vertex> {
    const BONE: [f32; 4] = [0.95, 0.80, 0.45, 1.0]; // warm bone color
    const JOINT: [f32; 4] = [1.0, 0.55, 0.20, 1.0]; // accent at joints
    let mut out: Vec<Vertex> = Vec::with_capacity(segments.len() * 8);
    for &(h, t) in segments {
        let head = Vec3::from(h);
        let tail = Vec3::from(t);
        // Shaft.
        out.push(Vertex { position: head.into(), _pad0: 0.0, color: BONE });
        out.push(Vertex { position: tail.into(), _pad0: 0.0, color: BONE });
        // Joint cross at the head — sized relative to the bone length so it scales sanely.
        let r = (tail - head).length().max(0.05) * 0.12;
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            out.push(Vertex { position: (head - axis * r).into(), _pad0: 0.0, color: JOINT });
            out.push(Vertex { position: (head + axis * r).into(), _pad0: 0.0, color: JOINT });
        }
    }
    out
}

/// Two unit vectors orthogonal to `axis`, used for the gizmo's arrowhead crosshair.
fn orthogonal_basis(axis: Vec3) -> (Vec3, Vec3) {
    let helper = if axis.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let a = axis.cross(helper).normalize_or_zero();
    let b = axis.cross(a).normalize_or_zero();
    (a, b)
}

fn aabb_line_vertices(aabb: suite_doc::Aabb, color: [f32; 4]) -> Vec<Vertex> {
    let corners = [
        [aabb.min.x, aabb.min.y, aabb.min.z],
        [aabb.max.x, aabb.min.y, aabb.min.z],
        [aabb.max.x, aabb.max.y, aabb.min.z],
        [aabb.min.x, aabb.max.y, aabb.min.z],
        [aabb.min.x, aabb.min.y, aabb.max.z],
        [aabb.max.x, aabb.min.y, aabb.max.z],
        [aabb.max.x, aabb.max.y, aabb.max.z],
        [aabb.min.x, aabb.max.y, aabb.max.z],
    ];
    let segs = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    let mut out = Vec::with_capacity(segs.len() * 2);
    for (a, b) in segs {
        out.push(Vertex {
            position: corners[a],
            _pad0: 0.0,
            color,
        });
        out.push(Vertex {
            position: corners[b],
            _pad0: 0.0,
            color,
        });
    }
    out
}

fn build_checker_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let size: u32 = 64;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let on = ((x / 8) + (y / 8)) % 2 == 0;
            let (r, g, b) = if on {
                (0x28, 0x2C, 0x31)
            } else {
                (0x16, 0x18, 0x1B)
            };
            data.extend_from_slice(&[r, g, b, 0xFF]);
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("checker texture"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * size),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("checker sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    (texture, view, sampler)
}

fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn linear_to_clear(linear: [f32; 4]) -> wgpu::Color {
    wgpu::Color {
        r: linear[0] as f64,
        g: linear[1] as f64,
        b: linear[2] as f64,
        a: linear[3] as f64,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_render_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs_entry: &str,
    fs_entry: &str,
    vertex_buffers: &[wgpu::VertexBufferLayout],
    color_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    cull: Option<wgpu::Face>,
    topology: wgpu::PrimitiveTopology,
    depth_write: bool,
    depth_compare: wgpu::CompareFunction,
) -> wgpu::RenderPipeline {
    let depth_stencil = depth_format.map(|f| wgpu::DepthStencilState {
        format: f,
        depth_write_enabled: Some(depth_write),
        depth_compare: Some(depth_compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("scene pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vs_entry),
            compilation_options: Default::default(),
            buffers: vertex_buffers,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology,
            cull_mode: cull,
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn make_render_pipeline_no_vbo(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    color_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
) -> wgpu::RenderPipeline {
    let depth_stencil = depth_format.map(|f| wgpu::DepthStencilState {
        format: f,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("grid pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod m4_tests {
    use super::{apply_free_transform, apply_gradient_fill, apply_layer_transform, crop_pixels, feather_mask, flood_fill_mask, move_selection_pixels, rasterize_selection_mask, rotate_canvas_pixels, selection_shape_bounds, shift_pixels, LayerTransform, SelectionShape};

    /// A width×height buffer where each texel encodes its (x,y) into R,G so transforms are
    /// verifiable — deliberately non-square by default (M5) to catch width/height mixups
    /// that a square fixture would hide.
    fn ramp(width: usize, height: usize) -> Vec<u8> {
        let mut v = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 4;
                v[i] = (x * 10) as u8;
                v[i + 1] = (y * 10) as u8;
                v[i + 2] = 0;
                v[i + 3] = 255;
            }
        }
        v
    }

    fn px(buf: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * width + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn flip_h_mirrors_columns_on_a_non_square_buffer() {
        let (w, h) = (6, 3);
        let src = ramp(w, h);
        let out = apply_layer_transform(&src, w, h, LayerTransform::FlipH);
        // Column 0 of the source (R=0) lands in the last column (w-1).
        assert_eq!(px(&out, w, w - 1, 1), px(&src, w, 0, 1));
        assert_eq!(px(&out, w, 0, 1), px(&src, w, w - 1, 1));
    }

    #[test]
    fn flip_v_mirrors_rows_on_a_non_square_buffer() {
        let (w, h) = (6, 3);
        let src = ramp(w, h);
        let out = apply_layer_transform(&src, w, h, LayerTransform::FlipV);
        assert_eq!(px(&out, w, 2, h - 1), px(&src, w, 2, 0));
    }

    #[test]
    fn rotate_180_twice_is_identity_on_a_non_square_buffer() {
        let (w, h) = (6, 3);
        let src = ramp(w, h);
        let once = apply_layer_transform(&src, w, h, LayerTransform::Rotate180);
        let twice = apply_layer_transform(&once, w, h, LayerTransform::Rotate180);
        assert_eq!(src, twice);
    }

    /// M5: `rotate_canvas_pixels` is the whole-DOCUMENT 90° rotate — it swaps width↔height
    /// in its OUTPUT, unlike `apply_layer_transform`'s three dimension-preserving ops.
    #[test]
    fn rotate_canvas_cw_then_ccw_is_identity() {
        let (w, h) = (6, 3);
        let src = ramp(w, h);
        let cw = rotate_canvas_pixels(&src, w, h, true);
        assert_eq!(cw.len(), h * w * 4, "CW output is the same total pixel count, swapped dims");
        // Rotating the (h×w) result back CCW, using the now-swapped dims, restores the original.
        let back = rotate_canvas_pixels(&cw, h, w, false);
        assert_eq!(src, back, "CW then CCW (with swapped dims) must restore the original");
    }

    #[test]
    fn rotate_canvas_cw_maps_top_left_to_top_right() {
        let (w, h) = (6, 3);
        let src = ramp(w, h); // top-left (0,0) has R=0,G=0
        let out = rotate_canvas_pixels(&src, w, h, true);
        // CW into a (h×w) buffer: (0,0) -> (h-1, 0).
        let out_w = h;
        assert_eq!(px(&out, out_w, h - 1, 0), px(&src, w, 0, 0));
    }

    /// M4: crop extracts exactly the requested sub-rect, at the right output dimensions,
    /// with pixels coming from the right source location (not shifted/misaligned).
    #[test]
    fn crop_extracts_the_requested_sub_rect() {
        let (w, h) = (10, 6);
        let src = ramp(w, h);
        // Crop [x0=2, y0=1, w=4, h=3] out of the 10×6 source.
        let out = crop_pixels(&src, w, h, 2, 1, 4, 3);
        assert_eq!(out.len(), 4 * 3 * 4, "output is crop_w*crop_h*4 bytes");
        // Output (0,0) should be source (2,1); output (3,2) should be source (5,3).
        assert_eq!(px(&out, 4, 0, 0), px(&src, w, 2, 1));
        assert_eq!(px(&out, 4, 3, 2), px(&src, w, 5, 3));
    }

    #[test]
    fn crop_clamps_a_rect_that_overruns_the_source_bounds() {
        let (w, h) = (8, 8);
        let src = ramp(w, h);
        // Ask for a crop that runs 4 texels past the right/bottom edges.
        let out = crop_pixels(&src, w, h, 6, 6, 6, 6);
        // Clamped to the 2×2 that actually exists from (6,6) to (8,8).
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!(px(&out, 2, 0, 0), px(&src, w, 6, 6));
    }

    /// M4 Move (no selection): shifting the whole layer relocates every pixel by (dx,dy);
    /// the revealed edge is transparent, and shifted-off pixels are dropped, not wrapped.
    #[test]
    fn shift_pixels_moves_content_and_reveals_transparent_edges() {
        let (w, h) = (8, 8);
        let src = ramp(w, h);
        let out = shift_pixels(&src, w, h, 2, 1);
        // Source (0,0) should now be at (2,1).
        assert_eq!(px(&out, w, 2, 1), px(&src, w, 0, 0));
        // The revealed strip at x=0..2 (nothing shifted into it) is fully transparent.
        assert_eq!(px(&out, w, 0, 4), [0, 0, 0, 0]);
        // A pixel shifted off the right edge (x=7 -> x=9, out of bounds) is simply dropped —
        // it does not wrap around to reappear at x=1.
        assert_eq!(px(&out, w, 1, 1), [0, 0, 0, 0]);
    }

    /// M4 Move (with a selection): only the selected region relocates; it leaves a
    /// transparent hole behind, and the destination shows the moved content, not the old
    /// background alpha-blended incorrectly.
    #[test]
    fn move_selection_pixels_cuts_a_hole_and_pastes_at_the_new_spot() {
        let (w, h) = (10, 10);
        // Opaque black background, with an opaque white 3x3 block at (2,2)-(5,5).
        let mut src = vec![0u8; w * h * 4];
        for p in src.chunks_exact_mut(4) {
            p[3] = 255;
        }
        for y in 2..5 {
            for x in 2..5 {
                let i = (y * w + x) * 4;
                src[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        // Move that 3x3 selection by (+4, 0).
        let out = move_selection_pixels(&src, w, h, 2, 2, 3, 3, 4, 0, None);
        // The original spot is now a transparent hole (not black background — genuinely cut).
        assert_eq!(px(&out, w, 3, 3), [0, 0, 0, 0], "original selection spot is a hole");
        // The new spot (2+4, 2) through (5+4-1, 4) shows the moved white block.
        assert_eq!(px(&out, w, 6, 3)[0], 255, "moved content lands at the new spot");
        // Untouched background elsewhere is still opaque black.
        assert_eq!(px(&out, w, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn free_transform_scale_moves_content_away_from_the_pivot() {
        let (w, h) = (8, 8);
        let mut buf = vec![0u8; w * h * 4];
        let src_idx = (1 * w + 1) * 4;
        buf[src_idx..src_idx + 4].copy_from_slice(&[255, 0, 0, 255]);
        // Scale 2x about the origin (top-left corner) — the whole layer (region=None), so
        // the original spot is NOT carried over untouched (everything is re-sourced).
        let out = apply_free_transform(&buf, w, h, None, (0.0, 0.0), 2.0, 0.0, None);
        assert_eq!(out[src_idx + 3], 0, "the untransformed layer should not show through");
        // Forward-mapping (1,1) about pivot (0,0) at scale 2 lands at (2,2) — nearest-
        // neighbor sampling spreads that one source texel across the 2x2 block it now covers.
        for &(x, y) in &[(2, 2), (2, 3), (3, 2), (3, 3)] {
            let p = (y * w + x) * 4;
            assert_eq!(&out[p..p + 3], &[255, 0, 0], "expected red at ({x},{y}), got {:?}", &out[p..p + 3]);
        }
    }

    #[test]
    fn free_transform_positive_rotation_is_on_screen_clockwise() {
        let (w, h) = (10, 10);
        let mut buf = vec![0u8; w * h * 4];
        // Pivot at (5.5, 5.5) — deliberately a texel *center* (not a grid corner) so "2
        // texels above" is an exact offset, not an approximation across a half-texel seam.
        // Marker 2 texels above the pivot (image y increases downward, so "above" = smaller y).
        let src_idx = (3 * w + 5) * 4;
        buf[src_idx..src_idx + 4].copy_from_slice(&[0, 255, 0, 255]);
        let out = apply_free_transform(&buf, w, h, None, (5.5, 5.5), 1.0, std::f32::consts::FRAC_PI_2, None);
        // A +90 degree rotation in this image-space convention moves "above the pivot" to
        // "right of the pivot" — on-screen clockwise (12 o'clock -> 3 o'clock).
        let dst_idx = (5 * w + 7) * 4;
        assert_eq!(&out[dst_idx..dst_idx + 3], &[0, 255, 0], "up should rotate to the right (clockwise)");
    }

    #[test]
    fn free_transform_with_a_region_leaves_a_transparent_hole_at_the_original_spot() {
        let (w, h) = (12, 12);
        let mut buf = vec![0u8; w * h * 4];
        for y in 4..8 {
            for x in 4..8 {
                let p = (y * w + x) * 4;
                buf[p..p + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        // Shrink the 4x4 blue region to half size about its own center (6,6) — the far
        // corner of the original footprint should end up outside the shrunk content.
        let region = Some((4usize, 4usize, 4usize, 4usize));
        let out = apply_free_transform(&buf, w, h, region, (6.0, 6.0), 0.5, 0.0, None);
        let corner = (4 * w + 4) * 4;
        assert_eq!(out[corner + 3], 0, "original region corner should be cleared, got {:?}", &out[corner..corner + 4]);
    }

    // ----- Tier 1: selection mask (Ellipse / Lasso) -------------------------------------

    #[test]
    fn ellipse_mask_selects_center_and_excludes_corners() {
        let (w, h) = (40, 40);
        // A circle centred at (0.5,0.5) with radius 0.25 (in UV) — well inside the canvas.
        let shape = SelectionShape::Ellipse { cx: 0.5, cy: 0.5, rx: 0.25, ry: 0.25 };
        let mask = rasterize_selection_mask(w, h, &shape);
        // Dead centre is fully selected.
        assert_eq!(mask[h / 2 * w + w / 2], 255);
        // The far corner (0,0) is well outside the circle — unselected.
        assert_eq!(mask[0], 0);
    }

    #[test]
    fn ellipse_mask_respects_independent_x_and_y_radii() {
        // A wide ellipse (rx=0.4, ry=0.1) on a SQUARE canvas: a point far out on the x-axis
        // from centre should be selected, but the same offset on the y-axis should not —
        // proving rx/ry aren't accidentally aspect-corrected against each other (unlike a
        // round brush dab, an ellipse's radii are already independent per axis).
        let (w, h) = (100, 100);
        let shape = SelectionShape::Ellipse { cx: 0.5, cy: 0.5, rx: 0.4, ry: 0.1 };
        let mask = rasterize_selection_mask(w, h, &shape);
        let at = |ux: f32, uy: f32| {
            let x = (ux * w as f32) as usize;
            let y = (uy * h as f32) as usize;
            mask[y * w + x]
        };
        assert!(at(0.85, 0.5) > 200, "far out on the wide x-axis should still be inside");
        assert!(at(0.5, 0.85) < 50, "the same offset on the narrow y-axis should be outside");
    }

    #[test]
    fn polygon_mask_fills_a_triangle_and_excludes_outside() {
        let (w, h) = (20, 20);
        // A triangle covering roughly the left half of the canvas.
        let shape = SelectionShape::Polygon(vec![[0.0, 0.0], [0.6, 0.0], [0.0, 1.0]]);
        let mask = rasterize_selection_mask(w, h, &shape);
        // A point clearly inside the triangle (near its (0,0) corner).
        assert_eq!(mask[2 * w + 2], 255, "inside the triangle");
        // A point clearly outside (top-right corner, opposite the hypotenuse).
        assert_eq!(mask[1 * w + 18], 0, "outside the triangle");
    }

    #[test]
    fn rasterize_selection_mask_mask_variant_passes_through_at_matching_size_and_resamples_otherwise() {
        let (w, h) = (4, 4);
        let mut data = vec![0u8; w * h];
        data[0] = 255; // top-left texel only
        let shape = SelectionShape::Mask { width: w as u32, height: h as u32, data: data.clone() };

        // Same size: identity pass-through.
        let out = rasterize_selection_mask(w, h, &shape);
        assert_eq!(out, data);

        // Double size: nearest-neighbor resample should still mark (roughly) the top-left
        // quadrant, not the whole canvas and not nothing.
        let out2 = rasterize_selection_mask(w * 2, h * 2, &shape);
        assert_eq!(out2[0], 255, "top-left corner should still be selected after upscaling");
        assert_eq!(out2[(h * 2 - 1) * (w * 2) + (w * 2 - 1)], 0, "bottom-right corner should stay unselected");
    }

    #[test]
    fn selection_shape_bounds_computes_the_right_bbox() {
        let ellipse = SelectionShape::Ellipse { cx: 0.5, cy: 0.4, rx: 0.2, ry: 0.1 };
        let b = selection_shape_bounds(&ellipse);
        assert!((b[0] - 0.3).abs() < 1e-5 && (b[2] - 0.7).abs() < 1e-5);
        assert!((b[1] - 0.3).abs() < 1e-5 && (b[3] - 0.5).abs() < 1e-5);

        let poly = SelectionShape::Polygon(vec![[0.1, 0.2], [0.8, 0.3], [0.4, 0.9]]);
        let b = selection_shape_bounds(&poly);
        assert!((b[0] - 0.1).abs() < 1e-5 && (b[2] - 0.8).abs() < 1e-5);
        assert!((b[1] - 0.2).abs() < 1e-5 && (b[3] - 0.9).abs() < 1e-5);

        // Mask: bbox is the tight extent of the nonzero texels, not the whole canvas.
        let (w, h) = (10, 10);
        let mut data = vec![0u8; w * h];
        for y in 3..6 {
            for x in 2..5 {
                data[y * w + x] = 255;
            }
        }
        let mask = SelectionShape::Mask { width: w as u32, height: h as u32, data };
        let b = selection_shape_bounds(&mask);
        assert!((b[0] - 0.2).abs() < 1e-5 && (b[2] - 0.5).abs() < 1e-5, "x bounds: {b:?}");
        assert!((b[1] - 0.3).abs() < 1e-5 && (b[3] - 0.6).abs() < 1e-5, "y bounds: {b:?}");
    }

    #[test]
    fn flood_fill_mask_selects_the_matching_region_and_stops_at_the_boundary() {
        // A 10x10 buffer: left half red, right half blue (hard edge at x=5). Flood fill from
        // the red side should select exactly the left half, nothing on the blue side.
        let (w, h) = (10, 10);
        let mut buf = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 4;
                if x < 5 {
                    buf[idx..idx + 4].copy_from_slice(&[255, 0, 0, 255]);
                } else {
                    buf[idx..idx + 4].copy_from_slice(&[0, 0, 255, 255]);
                }
            }
        }
        let (mask, count) = flood_fill_mask(&buf, w, h, [0.1, 0.5], 0.1);
        assert_eq!(count, 5 * 10, "should select exactly the left half");
        for y in 0..h {
            for x in 0..w {
                let expect = if x < 5 { 255 } else { 0 };
                assert_eq!(mask[y * w + x], expect, "mismatch at ({x},{y})");
            }
        }
    }

    #[test]
    fn flood_fill_mask_does_not_leak_through_a_diagonal_gap() {
        // 4-connectivity: two red regions touching only at a corner (diagonal) must NOT be
        // treated as connected — this is what distinguishes flood fill from a plain
        // color-threshold mask.
        let (w, h) = (4, 4);
        let mut buf = vec![0u8; w * h * 4];
        for p in buf.chunks_mut(4) {
            p.copy_from_slice(&[0, 255, 0, 255]); // all green
        }
        // Carve out everything except two diagonal corners so only (0,0) and (3,3) are red,
        // and they only "touch" diagonally through (1,1)/(2,2) which stay green.
        let set = |buf: &mut [u8], x: usize, y: usize, c: [u8; 4]| {
            let idx = (y * w + x) * 4;
            buf[idx..idx + 4].copy_from_slice(&c);
        };
        set(&mut buf, 0, 0, [255, 0, 0, 255]);
        set(&mut buf, 1, 1, [255, 0, 0, 255]);
        set(&mut buf, 2, 2, [0, 255, 0, 255]);
        set(&mut buf, 3, 3, [255, 0, 0, 255]);
        let (mask, count) = flood_fill_mask(&buf, w, h, [0.0, 0.0], 0.1);
        // Starting at (0,0): 4-connected reach includes (1,1) only via a shared edge with a
        // matching neighbor, but (1,1) only touches (0,0) diagonally — so it must NOT be
        // selected, and neither must the far corner (3,3).
        assert_eq!(count, 1, "only the seed pixel should match: {mask:?}");
        assert_eq!(mask[0], 255);
        assert_eq!(mask[1 * w + 1], 0, "diagonal neighbor must not leak into the selection");
        assert_eq!(mask[3 * w + 3], 0);
    }

    #[test]
    fn feather_radius_zero_is_an_exact_passthrough() {
        // Hard edge (feather off) must be byte-for-byte identical to the input — this is what
        // guarantees every tool's no-feather path is unchanged from before feathering existed.
        let (w, h) = (16, 16);
        let mut mask = vec![0u8; w * h];
        for y in 4..12 {
            for x in 4..12 {
                mask[y * w + x] = 255;
            }
        }
        assert_eq!(feather_mask(&mask, w, h, 0), mask);
    }

    #[test]
    fn feather_softens_a_hard_edge_into_a_monotonic_ramp() {
        // A hard left/right split (left half 255, right half 0). After feathering, scanning
        // left-to-right across the boundary should give a smooth, monotonically *decreasing*
        // ramp instead of a single 255->0 cliff, and the deep interior on each side should be
        // untouched (still ~255 far left, ~0 far right).
        let (w, h) = (32, 8);
        let mut mask = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..(w / 2) {
                mask[y * w + x] = 255;
            }
        }
        let out = feather_mask(&mask, w, h, 3);
        let row = (h / 2) * w;
        // Deep interior unchanged.
        assert_eq!(out[row + 1], 255, "far-left interior should stay fully selected");
        assert_eq!(out[row + w - 2], 0, "far-right interior should stay fully unselected");
        // The boundary is no longer a cliff — some texel straddling x=16 is a partial value.
        let boundary: Vec<u8> = (13..19).map(|x| out[row + x]).collect();
        assert!(boundary.iter().any(|&v| v > 10 && v < 245), "expected a soft transition, got {boundary:?}");
        // Monotonic non-increasing across the whole row (no ringing/overshoot from the blur).
        for x in 1..w {
            assert!(out[row + x] <= out[row + x - 1], "ramp must be monotonic at x={x}: {} then {}", out[row + x - 1], out[row + x]);
        }
    }

    #[test]
    fn feather_of_a_fully_selected_mask_stays_fully_selected() {
        // An all-255 mask has no edge to soften — clamped (not zero-padded) borders mean it
        // must come back all-255, not darkened at the edges. This is the test that would fail
        // if the box blur zero-padded instead of edge-clamping.
        let (w, h) = (12, 9);
        let mask = vec![255u8; w * h];
        let out = feather_mask(&mask, w, h, 4);
        assert!(out.iter().all(|&v| v == 255), "a full mask must stay full after feathering");
    }

    #[test]
    fn linear_gradient_is_opaque_at_start_and_transparent_at_end_on_non_square() {
        // Opaque white → transparent across a 16-wide, 8-tall canvas on a black background —
        // non-square on purpose, so a width/height mixup in the gradient math would show up.
        let (w, h) = (16, 8);
        let mut buf = vec![0u8; w * h * 4];
        for p in buf.chunks_mut(4) {
            p[3] = 255; // black, opaque
        }
        apply_gradient_fill(
            &mut buf,
            w,
            h,
            [0.0, 0.5],
            [1.0, 0.5],
            [1.0, 1.0, 1.0, 1.0], // white, opaque at start
            [1.0, 1.0, 1.0, 0.0], // white, transparent at end
            false,
            None,
            None,
        );
        let mid_row = h / 2;
        // Far-left column: gradient alpha ~1 → fully white.
        let left = px(&buf, w, 0, mid_row);
        assert!(left[0] > 230, "left edge should be near-white, got {left:?}");
        // Mid column: ~50% blend → a clear mid-grey, darker than the left.
        let mid = px(&buf, w, w / 2, mid_row);
        assert!(mid[0] < left[0] && mid[0] > 60, "middle should be mid-grey, got {mid:?}");
        // Far-right column: alpha ~0, so mostly the black background shows through. (Not
        // pure black: the last texel centre sits at t≈0.97, leaving a few % of white that
        // sRGB encoding lifts — so assert "much darker than the middle", not "near zero".)
        let right = px(&buf, w, w - 1, mid_row);
        assert!(right[0] < mid[0] / 2, "right edge should fall off hard, got {right:?} vs mid {mid:?}");
    }

    #[test]
    fn gradient_respects_selection_bounds() {
        // Fill solid red but restrict to the left half via a selection; right half untouched.
        let (w, h) = (16, 16);
        let mut buf = vec![0u8; w * h * 4];
        for p in buf.chunks_mut(4) {
            p[3] = 255; // opaque black
        }
        apply_gradient_fill(
            &mut buf,
            w,
            h,
            [0.0, 0.5],
            [0.0, 0.5], // zero-length → t=0 everywhere → color_a
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            false,
            Some([0.0, 0.0, 0.5, 1.0]), // left half only
            None,
        );
        let inside = px(&buf, w, 2, 8);
        assert!(inside[0] > 200 && inside[1] < 40, "inside selection is red, got {inside:?}");
        let outside = px(&buf, w, 12, 8);
        assert_eq!(outside, [0, 0, 0, 255], "outside selection stays untouched");
    }

    /// Tier 1: a mask constrains the gradient to the exact shape, not just its bounding
    /// rect — a corner of the rect that's outside the ellipse must stay untouched.
    #[test]
    fn gradient_respects_an_exact_shape_mask_not_just_its_bounding_rect() {
        let (w, h) = (40, 40);
        let mut buf = vec![0u8; w * h * 4];
        for p in buf.chunks_mut(4) {
            p[3] = 255; // opaque black
        }
        let shape = SelectionShape::Ellipse { cx: 0.5, cy: 0.5, rx: 0.2, ry: 0.2 };
        let bounds = selection_shape_bounds(&shape);
        let mask = rasterize_selection_mask(w, h, &shape);
        apply_gradient_fill(
            &mut buf, w, h,
            [0.0, 0.5], [0.0, 0.5], // zero-length -> t=0 everywhere -> color_a
            [1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0],
            false,
            Some(bounds),
            Some(&mask),
        );
        // Dead centre (inside both the rect AND the ellipse) is red.
        let center = px(&buf, w, 20, 20);
        assert!(center[0] > 200 && center[1] < 40, "centre is red, got {center:?}");
        // A corner of the ellipse's bounding rect (inside the rect, but outside the circle)
        // must stay untouched — proving the mask, not just the rect, gates the fill.
        let rect_corner = px(&buf, w, (bounds[0] * w as f32) as usize + 1, (bounds[1] * h as f32) as usize + 1);
        assert_eq!(rect_corner, [0, 0, 0, 255], "bounding-rect corner outside the ellipse stays untouched");
    }
}
