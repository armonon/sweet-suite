//! Phase 1+3 lite WGSL — four shaders kept inline so the crate is self-contained.
//!
//! Convention: `@group(0)` = camera (scene-wide); `@group(1)` = per-object;
//! `@group(2)+` = per-material (textured quad's checker, etc.). The same convention
//! will scale to the Forward+ graph at Phase 4.

const CAMERA_GLOBAL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    proj_kind_and_pad: vec4<f32>,
};
struct Object {
    model: mat4x4<f32>,
    color: vec4<f32>,
    selected_pad: vec4<f32>,
};
"#;

pub const SCENE_WGSL: &str = concat!(
    r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    proj_kind_and_pad: vec4<f32>,
};
struct Object {
    model: mat4x4<f32>,
    color: vec4<f32>,
    selected_pad: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> obj: Object;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};
struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) selected: f32,
};
@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * obj.model * vec4<f32>(v.position, 1.0);
    out.color = v.color * obj.color;
    out.selected = obj.selected_pad.x;
    return out;
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    var color = f.color;
    if (f.selected > 0.5) {
        // Subtle accent tint on selected objects so the outline isn't the only signal.
        color = vec4<f32>(mix(color.rgb, vec3<f32>(0.05, 0.21, 0.76), 0.18), color.a);
    }
    return color;
}
"#
);

pub const TEXTURED_QUAD_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    proj_kind_and_pad: vec4<f32>,
};
struct Object {
    model: mat4x4<f32>,
    color: vec4<f32>,
    selected_pad: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> obj: Object;
@group(2) @binding(0) var checker_tex: texture_2d<f32>;
@group(2) @binding(1) var checker_sampler: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};
struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) selected: f32,
};
@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * obj.model * vec4<f32>(v.position, 1.0);
    // The plane mesh's local positions are in [-0.5, 0.5]; map to UV [0, 1].
    out.uv = vec2<f32>(v.position.x + 0.5, 0.5 - v.position.y);
    out.selected = obj.selected_pad.x;
    return out;
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    var sample = textureSample(checker_tex, checker_sampler, f.uv);
    if (f.selected > 0.5) {
        sample = vec4<f32>(mix(sample.rgb, vec3<f32>(0.05, 0.21, 0.76), 0.18), sample.a);
    }
    return sample;
}
"#;

pub const OUTLINE_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    proj_kind_and_pad: vec4<f32>,
};
struct Object {
    model: mat4x4<f32>,
    color: vec4<f32>,
    selected_pad: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> obj: Object;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};
struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * obj.model * vec4<f32>(v.position, 1.0);
    out.color = v.color;
    return out;
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    return f.color;
}
"#;

pub const GRID_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    proj_kind_and_pad: vec4<f32>,
};
struct Object {
    model: mat4x4<f32>,
    color: vec4<f32>,
    selected_pad: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> obj: Object;  // unused; satisfies pipeline layout

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var out: VsOut;
    let p = pos[vid];
    out.clip_pos = vec4<f32>(p, 0.0, 1.0);
    out.ndc = p;
    return out;
}
fn unproject(ndc: vec2<f32>, depth01: f32) -> vec3<f32> {
    let ndc_z = depth01 * 2.0 - 1.0;
    let p = camera.inv_view_proj * vec4<f32>(ndc.x, ndc.y, ndc_z, 1.0);
    return p.xyz / p.w;
}
fn grid_alpha(p: vec2<f32>, scale: f32) -> f32 {
    let coord = p * scale;
    let dcoord = fwidth(coord);
    let grid = abs(fract(coord - 0.5) - 0.5) / dcoord;
    let line = min(grid.x, grid.y);
    return 1.0 - min(line, 1.0);
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    let near = unproject(f.ndc, 0.0);
    let far  = unproject(f.ndc, 1.0);
    let dir = far - near;
    if (abs(dir.y) < 1e-5) { discard; }
    let t = -near.y / dir.y;
    if (t < 0.0) { discard; }
    let world = near + t * dir;
    let horizon = clamp(1.0 - length(world.xz) / 60.0, 0.0, 1.0);
    let minor = grid_alpha(world.xz, 1.0);
    let major = grid_alpha(world.xz, 0.1);
    let alpha = max(minor * 0.45, major * 0.85) * horizon;
    if (alpha < 0.001) { discard; }
    let color = vec3<f32>(0.0387, 0.0445, 0.0524);
    return vec4<f32>(color, alpha);
}
"#;

/// Mesh-paint shader — same vertex layout as SCENE_WGSL but with an extra UV slot
/// (`@location(2)`). Multiplies the flat-shaded vertex color by the per-mesh paint
/// texture so brush strokes show on 3D geometry.
pub const MESH_PAINT_WGSL: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    proj_kind_and_pad: vec4<f32>,
};
struct Object {
    model: mat4x4<f32>,
    color: vec4<f32>,
    selected_pad: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var<uniform> obj: Object;
@group(2) @binding(0) var mesh_paint_tex: texture_2d<f32>;
@group(2) @binding(1) var mesh_paint_sampler: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
};
struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) selected: f32,
    @location(2) uv: vec2<f32>,
};
@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_pos = camera.view_proj * obj.model * vec4<f32>(v.position, 1.0);
    out.color = v.color * obj.color;
    out.selected = obj.selected_pad.x;
    out.uv = v.uv;
    return out;
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    var paint = textureSample(mesh_paint_tex, mesh_paint_sampler, f.uv);
    // Composite paint over flat-shaded surface (paint alpha 0 = untouched surface).
    var color = mix(f.color, paint, paint.a);
    if (f.selected > 0.5) {
        color = vec4<f32>(mix(color.rgb, vec3<f32>(0.05, 0.21, 0.76), 0.18), color.a);
    }
    return color;
}
"#;
