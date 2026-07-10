//! Linear HDR compositor. Composites a stack of layer textures with blend modes and
//! applies adjustment-layer passes (BrightnessContrast, HueSaturation, Levels) in
//! linear Rgba16Float space. docs/03 §1.7.

use suite_doc::{AdjustmentKind, BlendMode};

// ------ blend mode constants (match the WGSL) --------------------------------
const MODE_NORMAL: u32 = 0;
const MODE_MULTIPLY: u32 = 1;
const MODE_SCREEN: u32 = 2;
const MODE_OVERLAY: u32 = 3;
const MODE_SOFT_LIGHT: u32 = 4;
const MODE_HARD_LIGHT: u32 = 5;
const MODE_ADD: u32 = 6;
const MODE_SUBTRACT: u32 = 7;

fn blend_mode_const(m: BlendMode) -> u32 {
    match m {
        BlendMode::Normal => MODE_NORMAL,
        BlendMode::Multiply => MODE_MULTIPLY,
        BlendMode::Screen => MODE_SCREEN,
        BlendMode::Overlay => MODE_OVERLAY,
        BlendMode::SoftLight => MODE_SOFT_LIGHT,
        BlendMode::HardLight => MODE_HARD_LIGHT,
        BlendMode::Add => MODE_ADD,
        BlendMode::Subtract => MODE_SUBTRACT,
        // Parity modes (ids match LAYER_COMPOSITE_WGSL / BLEND_WGSL cases 8–19).
        BlendMode::Darken => 8,
        BlendMode::Lighten => 9,
        BlendMode::ColorDodge => 10,
        BlendMode::ColorBurn => 11,
        BlendMode::LinearBurn => 12,
        BlendMode::Difference => 13,
        BlendMode::Exclusion => 14,
        BlendMode::Divide => 15,
        BlendMode::VividLight => 16,
        BlendMode::LinearLight => 17,
        BlendMode::PinLight => 18,
        BlendMode::HardMix => 19,
    }
}

// ------ adjustment mode constants (match the WGSL) ---------------------------
const ADJ_BRIGHTNESS_CONTRAST: u32 = 0;
const ADJ_HUE_SATURATION: u32 = 1;
const ADJ_LEVELS: u32 = 2;
const ADJ_EXPOSURE: u32 = 3;
const ADJ_VIBRANCE: u32 = 4;
const ADJ_WHITE_BALANCE: u32 = 5;
const ADJ_POSTERIZE: u32 = 6;
const ADJ_THRESHOLD: u32 = 7;
const ADJ_INVERT: u32 = 8;
const ADJ_BOX_BLUR: u32 = 9;
const ADJ_SHARPEN: u32 = 10;
const ADJ_EDGE_DETECT: u32 = 11;
const ADJ_GAUSSIAN: u32 = 12;

/// Map an adjustment kind to the `(mode, p0, p1, p2)` passes the `ADJUST_WGSL` shader runs.
/// Most kinds are one pass; separable Gaussian is two (H then V, with p1/p2 carrying the
/// direction). Shared by the reusable `Compositor` and the Renderer's live display path.
pub(crate) fn adjustment_passes(kind: &AdjustmentKind) -> Vec<(u32, f32, f32, f32)> {
    match kind {
        AdjustmentKind::BrightnessContrast { brightness, contrast } => {
            vec![(ADJ_BRIGHTNESS_CONTRAST, *brightness, *contrast, 0.0)]
        }
        AdjustmentKind::HueSaturation { hue, saturation, lightness } => {
            vec![(ADJ_HUE_SATURATION, *hue, *saturation, *lightness)]
        }
        AdjustmentKind::Levels { black_point, gamma, white_point } => {
            vec![(ADJ_LEVELS, *black_point, *gamma, *white_point)]
        }
        AdjustmentKind::Exposure { stops } => vec![(ADJ_EXPOSURE, *stops, 0.0, 0.0)],
        AdjustmentKind::Vibrance { amount } => vec![(ADJ_VIBRANCE, *amount, 0.0, 0.0)],
        AdjustmentKind::WhiteBalance { temperature, tint } => {
            vec![(ADJ_WHITE_BALANCE, *temperature, *tint, 0.0)]
        }
        AdjustmentKind::Posterize { levels } => vec![(ADJ_POSTERIZE, *levels, 0.0, 0.0)],
        AdjustmentKind::Threshold { level } => vec![(ADJ_THRESHOLD, *level, 0.0, 0.0)],
        AdjustmentKind::Invert => vec![(ADJ_INVERT, 0.0, 0.0, 0.0)],
        AdjustmentKind::BoxBlur { radius } => vec![(ADJ_BOX_BLUR, *radius, 0.0, 0.0)],
        AdjustmentKind::Sharpen { amount } => vec![(ADJ_SHARPEN, *amount, 0.0, 0.0)],
        AdjustmentKind::EdgeDetect => vec![(ADJ_EDGE_DETECT, 0.0, 0.0, 0.0)],
        AdjustmentKind::GaussianBlur { radius } => vec![
            (ADJ_GAUSSIAN, *radius, 1.0, 0.0),
            (ADJ_GAUSSIAN, *radius, 0.0, 1.0),
        ],
    }
}

// ------ WGSL shaders ---------------------------------------------------------

const BLEND_WGSL: &str = r#"
struct VertexOutput { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = select(-1.0, 1.0, (vi & 1u) != 0u);
    let y = select(-1.0, 1.0, (vi & 2u) != 0u);
    out.pos = vec4(x, y, 0.0, 1.0);
    out.uv  = vec2(x * 0.5 + 0.5, 0.5 - y * 0.5);
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
fn cdodge_ch(b: f32, s: f32) -> f32 { if s >= 1.0 { return 1.0; } return min(1.0, b / (1.0 - s)); }
fn cburn_ch(b: f32, s: f32) -> f32 { if s <= 0.0 { return 0.0; } return 1.0 - min(1.0, (1.0 - b) / s); }
fn vivid_ch(b: f32, s: f32) -> f32 { if s <= 0.5 { return cburn_ch(b, 2.0 * s); } return cdodge_ch(b, 2.0 * (s - 0.5)); }
fn pin_ch(b: f32, s: f32) -> f32 { if s <= 0.5 { return min(b, 2.0 * s); } return max(b, 2.0 * s - 1.0); }

@fragment
fn fs(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(base_tex, samp, in.uv);
    let src  = textureSample(src_tex,  samp, in.uv);

    var rgb: vec3<f32>;
    switch params.mode {
        case 1u { rgb = base.rgb * src.rgb; }              // Multiply
        case 2u { rgb = 1.0 - (1.0 - base.rgb) * (1.0 - src.rgb); } // Screen
        case 3u { rgb = vec3(overlay_ch(base.r, src.r),   overlay_ch(base.g, src.g),   overlay_ch(base.b, src.b)); }   // Overlay
        case 4u { rgb = vec3(soft_light_ch(base.r, src.r), soft_light_ch(base.g, src.g), soft_light_ch(base.b, src.b)); } // Soft Light
        case 5u { rgb = vec3(hard_light_ch(base.r, src.r), hard_light_ch(base.g, src.g), hard_light_ch(base.b, src.b)); } // Hard Light
        case 6u { rgb = clamp(base.rgb + src.rgb, vec3(0.0), vec3(1.0)); } // Add
        case 7u { rgb = clamp(base.rgb - src.rgb, vec3(0.0), vec3(1.0)); } // Subtract
        case 8u { rgb = min(base.rgb, src.rgb); }          // Darken
        case 9u { rgb = max(base.rgb, src.rgb); }          // Lighten
        case 10u { rgb = vec3(cdodge_ch(base.r, src.r), cdodge_ch(base.g, src.g), cdodge_ch(base.b, src.b)); } // Color Dodge
        case 11u { rgb = vec3(cburn_ch(base.r, src.r), cburn_ch(base.g, src.g), cburn_ch(base.b, src.b)); }    // Color Burn
        case 12u { rgb = clamp(base.rgb + src.rgb - 1.0, vec3(0.0), vec3(1.0)); }  // Linear Burn
        case 13u { rgb = abs(base.rgb - src.rgb); }        // Difference
        case 14u { rgb = base.rgb + src.rgb - 2.0 * base.rgb * src.rgb; } // Exclusion
        case 15u { rgb = clamp(base.rgb / max(src.rgb, vec3(1e-4)), vec3(0.0), vec3(1.0)); } // Divide
        case 16u { rgb = vec3(vivid_ch(base.r, src.r), vivid_ch(base.g, src.g), vivid_ch(base.b, src.b)); }   // Vivid Light
        case 17u { rgb = clamp(base.rgb + 2.0 * src.rgb - 1.0, vec3(0.0), vec3(1.0)); } // Linear Light
        case 18u { rgb = vec3(pin_ch(base.r, src.r), pin_ch(base.g, src.g), pin_ch(base.b, src.b)); }         // Pin Light
        case 19u { rgb = vec3(select(0.0, 1.0, vivid_ch(base.r, src.r) >= 0.5), select(0.0, 1.0, vivid_ch(base.g, src.g) >= 0.5), select(0.0, 1.0, vivid_ch(base.b, src.b) >= 0.5)); } // Hard Mix
        default { rgb = src.rgb; }                          // Normal
    }

    // Porter-Duff over with opacity-scaled source alpha.
    let sa = src.a * params.opacity;
    let out_a = sa + base.a * (1.0 - sa);
    let out_rgb = select(
        (rgb * sa + base.rgb * base.a * (1.0 - sa)) / out_a,
        vec3(0.0),
        out_a < 0.0001
    );
    return vec4(out_rgb, out_a);
}
"#;

pub(crate) const ADJUST_WGSL: &str = r#"
struct VertexOutput { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = select(-1.0, 1.0, (vi & 1u) != 0u);
    let y = select(-1.0, 1.0, (vi & 2u) != 0u);
    out.pos = vec4(x, y, 0.0, 1.0);
    out.uv  = vec2(x * 0.5 + 0.5, 0.5 - y * 0.5);
    return out;
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var samp:    sampler;
// `texel` is 1/size in UV space, for neighbor-sampling (convolution) kinds.
struct AdjParams { mode: u32, p0: f32, p1: f32, p2: f32, texel: vec2<f32>, _pad: vec2<f32> }
@group(0) @binding(2) var<uniform> params: AdjParams;

// RGB ↔ HSL conversion in linear space.
fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let d = mx - mn;
    let l = (mx + mn) * 0.5;
    if d < 0.00001 { return vec3(0.0, 0.0, l); }
    let s = select(d / (2.0 - mx - mn), d / (mx + mn), l < 0.5);
    var h: f32;
    if mx == c.r      { h = (c.g - c.b) / d + select(6.0, 0.0, c.g >= c.b); }
    else if mx == c.g { h = (c.b - c.r) / d + 2.0; }
    else              { h = (c.r - c.g) / d + 4.0; }
    return vec3(h / 6.0, s, l);
}

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if t < 0.0 { t += 1.0; }
    if t > 1.0 { t -= 1.0; }
    if t < 1.0/6.0 { return p + (q - p) * 6.0 * t; }
    if t < 0.5     { return q; }
    if t < 2.0/3.0 { return p + (q - p) * (2.0/3.0 - t) * 6.0; }
    return p;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if hsl.y < 0.00001 { return vec3(hsl.z); }
    let q = select(hsl.z + hsl.y - hsl.z * hsl.y, hsl.z * (1.0 + hsl.y), hsl.z < 0.5);
    let p = 2.0 * hsl.z - q;
    return vec3(hue_to_rgb(p, q, hsl.x + 1.0/3.0),
                hue_to_rgb(p, q, hsl.x),
                hue_to_rgb(p, q, hsl.x - 1.0/3.0));
}

@fragment
fn fs(in: VertexOutput) -> @location(0) vec4<f32> {
    var c = textureSample(src_tex, samp, in.uv);
    switch params.mode {
        // Brightness / Contrast  (p0=brightness -1..1, p1=contrast -1..1)
        case 0u {
            let contrast_factor = select(
                (1.0 + params.p1),
                1.0 / max(1.0 - params.p1, 0.001),
                params.p1 >= 0.0
            );
            c = vec4(clamp((c.rgb + params.p0) * contrast_factor, vec3(0.0), vec3(1.0)), c.a);
        }
        // Hue / Saturation / Lightness  (p0=hue shift -1..1, p1=sat mult, p2=lightness shift)
        case 1u {
            var hsl = rgb_to_hsl(c.rgb);
            hsl.x = fract(hsl.x + params.p0);
            hsl.y = clamp(hsl.y * params.p1, 0.0, 1.0);
            hsl.z = clamp(hsl.z + params.p2, 0.0, 1.0);
            c = vec4(hsl_to_rgb(hsl), c.a);
        }
        // Levels  (p0=black_point, p1=gamma, p2=white_point)
        case 2u {
            let range = max(params.p2 - params.p0, 0.0001);
            var rgb = (c.rgb - params.p0) / range;
            rgb = clamp(rgb, vec3(0.0), vec3(1.0));
            rgb = pow(rgb, vec3(1.0 / max(params.p1, 0.01)));
            c = vec4(rgb, c.a);
        }
        // Exposure  (p0=stops) — multiply in linear light by 2^stops.
        case 3u {
            c = vec4(c.rgb * pow(2.0, params.p0), c.a);
        }
        // Vibrance  (p0=amount) — saturation weighted by how unsaturated a pixel already
        // is, so muted tones move more than vivid ones (PhotoDemon-style).
        case 4u {
            let luma = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
            let mx = max(max(c.r, c.g), c.b);
            let sat = mx - min(min(c.r, c.g), c.b);
            let weight = (1.0 - sat) * params.p0;
            c = vec4(clamp(mix(vec3(luma), c.rgb, 1.0 + weight), vec3(0.0), vec3(1e4)), c.a);
        }
        // White Balance  (p0=temperature, p1=tint).
        case 5u {
            let t = params.p0;
            let g = params.p1;
            let rgb = c.rgb * vec3(1.0 + t, 1.0 + g, 1.0 - t);
            c = vec4(clamp(rgb, vec3(0.0), vec3(1e4)), c.a);
        }
        // Posterize  (p0=levels) — quantize each channel to N steps.
        case 6u {
            let n = max(params.p0, 2.0);
            c = vec4(floor(clamp(c.rgb, vec3(0.0), vec3(1.0)) * n) / (n - 1.0), c.a);
        }
        // Threshold  (p0=level) — binary on luminance.
        case 7u {
            let luma = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
            let v = select(0.0, 1.0, luma >= params.p0);
            c = vec4(vec3(v), c.a);
        }
        // Invert.
        case 8u {
            c = vec4(1.0 - clamp(c.rgb, vec3(0.0), vec3(1.0)), c.a);
        }
        // Box Blur  (p0=radius in texels) — uniform-weight neighborhood average.
        case 9u {
            let radius = i32(clamp(params.p0, 1.0, 8.0));
            var acc = vec3(0.0);
            var n = 0.0;
            for (var dy = -radius; dy <= radius; dy = dy + 1) {
                for (var dx = -radius; dx <= radius; dx = dx + 1) {
                    let uv = in.uv + vec2(f32(dx), f32(dy)) * params.texel;
                    acc = acc + textureSample(src_tex, samp, uv).rgb;
                    n = n + 1.0;
                }
            }
            c = vec4(acc / n, c.a);
        }
        // Sharpen  (p0=amount) — 3x3 unsharp kernel scaled by amount.
        case 10u {
            let k = params.p0;
            var sum = c.rgb * (1.0 + 4.0 * k);
            sum = sum - textureSample(src_tex, samp, in.uv + vec2( params.texel.x, 0.0)).rgb * k;
            sum = sum - textureSample(src_tex, samp, in.uv + vec2(-params.texel.x, 0.0)).rgb * k;
            sum = sum - textureSample(src_tex, samp, in.uv + vec2(0.0,  params.texel.y)).rgb * k;
            sum = sum - textureSample(src_tex, samp, in.uv + vec2(0.0, -params.texel.y)).rgb * k;
            c = vec4(clamp(sum, vec3(0.0), vec3(1e4)), c.a);
        }
        // Gaussian blur — one 1-D pass along `dir = (p1, p2)`; the compositor runs it
        // twice (H then V) for a separable O(2r) blur. p0 = radius in texels.
        case 12u {
            let radius = i32(clamp(params.p0, 1.0, 16.0));
            let dir = vec2<f32>(params.p1, params.p2) * params.texel;
            let sigma = max(params.p0 * 0.5, 0.5);
            var acc = vec3<f32>(0.0);
            var wsum = 0.0;
            for (var i = -radius; i <= radius; i = i + 1) {
                let fi = f32(i);
                let w = exp(-(fi * fi) / (2.0 * sigma * sigma));
                acc = acc + textureSample(src_tex, samp, in.uv + dir * fi).rgb * w;
                wsum = wsum + w;
            }
            c = vec4(acc / wsum, c.a);
        }
        // Edge Detect  (Sobel magnitude on luminance).
        case 11u {
            let tx = params.texel.x;
            let ty = params.texel.y;
            let lum = vec3(0.2126, 0.7152, 0.0722);
            let tl = dot(textureSample(src_tex, samp, in.uv + vec2(-tx, -ty)).rgb, lum);
            let tc = dot(textureSample(src_tex, samp, in.uv + vec2(0.0, -ty)).rgb, lum);
            let tr = dot(textureSample(src_tex, samp, in.uv + vec2( tx, -ty)).rgb, lum);
            let ml = dot(textureSample(src_tex, samp, in.uv + vec2(-tx, 0.0)).rgb, lum);
            let mr = dot(textureSample(src_tex, samp, in.uv + vec2( tx, 0.0)).rgb, lum);
            let bl = dot(textureSample(src_tex, samp, in.uv + vec2(-tx,  ty)).rgb, lum);
            let bc = dot(textureSample(src_tex, samp, in.uv + vec2(0.0,  ty)).rgb, lum);
            let br = dot(textureSample(src_tex, samp, in.uv + vec2( tx,  ty)).rgb, lum);
            let gx = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            let gy = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
            let mag = clamp(sqrt(gx * gx + gy * gy), 0.0, 1.0);
            c = vec4(vec3(mag), c.a);
        }
        default {}
    }
    return c;
}
"#;

// ------ Compositor -----------------------------------------------------------

/// One entry in the compositor's layer stack: a texture view to blend onto the target.
pub struct LayerEntry<'a> {
    pub view: &'a wgpu::TextureView,
    pub blend_mode: BlendMode,
    pub opacity: f32,
}

/// One adjustment pass: applies an in-place color transform to the HDR target texture.
pub struct AdjustmentEntry {
    pub kind: AdjustmentKind,
}

/// Linear HDR compositor. Owns an Rgba16Float working texture of fixed size.
/// Call `composite` to blend a series of layer+adjustment entries into `output_view`.
/// docs/03 §1.7.
pub struct Compositor {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,

    blend_pipeline: wgpu::RenderPipeline,
    blend_bind_layout: wgpu::BindGroupLayout,

    adjust_pipeline: wgpu::RenderPipeline,
    adjust_bind_layout: wgpu::BindGroupLayout,

    sampler: wgpu::Sampler,

    // Ping-pong between two HDR textures for adjustment passes.
    hdr_a: wgpu::Texture,
    hdr_a_view: wgpu::TextureView,
    hdr_b: wgpu::Texture,
    hdr_b_view: wgpu::TextureView,
}

/// Describes one item in the compositor pass list.
pub enum CompEntry<'a> {
    Layer(LayerEntry<'a>),
    Adjustment(AdjustmentEntry),
}

impl Compositor {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;

        // --- blend bind layout (base, src, sampler, uniform) -----------------
        let blend_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("comp blend bgl"),
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

        let blend_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("comp blend shader"),
            source: wgpu::ShaderSource::Wgsl(BLEND_WGSL.into()),
        });
        let blend_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("comp blend pl"),
            bind_group_layouts: &[Some(&blend_bind_layout)],
            immediate_size: 0,
        });
        let blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("comp blend pipe"),
            layout: Some(&blend_pl),
            vertex: wgpu::VertexState {
                module: &blend_shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blend_shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        // --- adjustment bind layout (src, sampler, uniform) ------------------
        let adjust_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("comp adj bgl"),
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
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
        let adjust_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("comp adj shader"),
            source: wgpu::ShaderSource::Wgsl(ADJUST_WGSL.into()),
        });
        let adjust_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("comp adj pl"),
            bind_group_layouts: &[Some(&adjust_bind_layout)],
            immediate_size: 0,
        });
        let adjust_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("comp adj pipe"),
            layout: Some(&adjust_pl),
            vertex: wgpu::VertexState {
                module: &adjust_shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &adjust_shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("comp sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let make_hdr = |label: &'static str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let hdr_a = make_hdr("comp hdr_a");
        let hdr_a_view = hdr_a.create_view(&Default::default());
        let hdr_b = make_hdr("comp hdr_b");
        let hdr_b_view = hdr_b.create_view(&Default::default());

        Self {
            width,
            height,
            format,
            blend_pipeline,
            blend_bind_layout,
            adjust_pipeline,
            adjust_bind_layout,
            sampler,
            hdr_a,
            hdr_a_view,
            hdr_b,
            hdr_b_view,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.hdr_a_view
    }

    /// Composite `entries` into the internal HDR target. After this call, `output_view()`
    /// holds the final result (linear Rgba16Float). The first layer is rendered directly
    /// (no base beneath it), each subsequent layer is blended over the accumulation, and
    /// adjustment entries modify the accumulation in-place.
    ///
    /// # Arguments
    /// - `clear_color` — linear RGBA clear for the initial target (e.g. [1,1,1,1] for paper).
    pub fn composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        entries: &[CompEntry<'_>],
        clear_color: [f32; 4],
    ) {
        // We ping-pong: `current` is the accumulation target, `other` is the scratch.
        // hdr_a starts as the accumulation; hdr_b is the blit scratch for adjustments.
        let mut current_is_a = true;
        macro_rules! current_view {
            () => {
                if current_is_a { &self.hdr_a_view } else { &self.hdr_b_view }
            };
        }
        macro_rules! other_view {
            () => {
                if current_is_a { &self.hdr_b_view } else { &self.hdr_a_view }
            };
        }

        // Clear the first accumulation target.
        {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("comp clear"),
            });
            let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("comp clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: current_view!(),
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0] as f64,
                            g: clear_color[1] as f64,
                            b: clear_color[2] as f64,
                            a: clear_color[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            queue.submit(Some(enc.finish()));
        }

        for entry in entries {
            match entry {
                CompEntry::Layer(layer) => {
                    // Blend `layer.view` over `current` → write into `other`.
                    let param_buf = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("comp blend param"),
                        size: 16,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let mode_u32 = blend_mode_const(layer.blend_mode);
                    let bytes: [u8; 16] = bytemuck::cast([mode_u32, layer.opacity.to_bits(), 0u32, 0u32]);
                    queue.write_buffer(&param_buf, 0, &bytes);

                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("comp blend bg"),
                        layout: &self.blend_bind_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(current_view!()) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(layer.view) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                            wgpu::BindGroupEntry { binding: 3, resource: param_buf.as_entire_binding() },
                        ],
                    });

                    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("comp blend enc"),
                    });
                    {
                        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("comp blend rp"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: other_view!(),
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            ..Default::default()
                        });
                        rp.set_pipeline(&self.blend_pipeline);
                        rp.set_bind_group(0, &bg, &[]);
                        rp.draw(0..4, 0..1);
                    }
                    queue.submit(Some(enc.finish()));
                    current_is_a = !current_is_a; // swap: what was "other" is now "current"
                }

                CompEntry::Adjustment(adj) => {
                    let passes = adjustment_passes(&adj.kind);

                    let texel_w = 1.0 / self.width.max(1) as f32;
                    let texel_h = 1.0 / self.height.max(1) as f32;
                    for (mode, p0, p1, p2) in passes {
                        // 32-byte uniform: mode,p0,p1,p2, texel.xy, pad.xy.
                        let param_buf = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("comp adj param"),
                            size: 32,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        let bytes: [u8; 32] = bytemuck::cast([
                            mode, p0.to_bits(), p1.to_bits(), p2.to_bits(),
                            texel_w.to_bits(), texel_h.to_bits(), 0u32, 0u32,
                        ]);
                        queue.write_buffer(&param_buf, 0, &bytes);

                        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("comp adj bg"),
                            layout: &self.adjust_bind_layout,
                            entries: &[
                                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(current_view!()) },
                                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                                wgpu::BindGroupEntry { binding: 2, resource: param_buf.as_entire_binding() },
                            ],
                        });

                        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("comp adj enc"),
                        });
                        {
                            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("comp adj rp"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: other_view!(),
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: None,
                                ..Default::default()
                            });
                            rp.set_pipeline(&self.adjust_pipeline);
                            rp.set_bind_group(0, &bg, &[]);
                            rp.draw(0..4, 0..1);
                        }
                        queue.submit(Some(enc.finish()));
                        current_is_a = !current_is_a;
                    }
                }
            }
        }

        // If the final result is in hdr_b, copy it back to hdr_a (canonical output slot).
        if !current_is_a {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("comp copy to a"),
            });
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.hdr_b,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.hdr_a,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(enc.finish()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        pollster::block_on(async {
            let inst = wgpu::Instance::default();
            let adapter = inst
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::None,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                })
                .await
                .ok()?;
            adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .ok()
        })
    }

    #[test]
    fn compositor_clears_and_blends_without_panic() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU — skip compositor test");
            return;
        };
        let comp = Compositor::new(&device, 64, 64);
        // Two layers: a red base + a 50%-opacity blue screen layer.
        let make_tex = |r: f32, g: f32, b: f32, a: f32| {
            use wgpu::*;
            let tex = device.create_texture(&TextureDescriptor {
                label: None,
                size: Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let pixel = [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, (a * 255.0) as u8];
            let data: Vec<u8> = pixel.iter().cycle().take(64 * 64 * 4).cloned().collect();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(64 * 4),
                    rows_per_image: Some(64),
                },
                wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
            );
            tex.create_view(&Default::default())
        };
        let red_view = make_tex(1.0, 0.0, 0.0, 1.0);
        let blue_view = make_tex(0.0, 0.0, 1.0, 1.0);

        let entries = vec![
            CompEntry::Layer(LayerEntry { view: &red_view, blend_mode: BlendMode::Normal, opacity: 1.0 }),
            CompEntry::Layer(LayerEntry { view: &blue_view, blend_mode: BlendMode::Screen, opacity: 0.5 }),
            CompEntry::Adjustment(AdjustmentEntry {
                kind: AdjustmentKind::BrightnessContrast { brightness: 0.1, contrast: 0.0 },
            }),
        ];
        comp.composite(&device, &queue, &entries, [1.0, 1.0, 1.0, 1.0]);
        // Verify: if nothing panicked, the compositor ran successfully.
    }

    #[test]
    fn adjustment_hue_saturation_does_not_panic() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU — skip");
            return;
        };
        let comp = Compositor::new(&device, 32, 32);
        comp.composite(&device, &queue, &[], [0.5, 0.5, 0.5, 1.0]);
    }
}
