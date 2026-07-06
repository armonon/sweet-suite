//! # The raster substrate (Phase 2) — a paintable GPU texture, brush engine, and undo.
//!
//! `RasterCanvas` owns a single RGBA paint texture and a brush render pipeline. A
//! brush *stamp* is a soft round dab rendered into the texture with alpha blending;
//! a *stroke segment* lays a row of dabs from one UV to the next so fast cursor moves
//! don't leave gaps. Pixels live on the GPU and stay there — no CPU readback in the
//! paint path (docs/01 §2, docs/03 §2).
//!
//! **Undo is dirty-region based.** Each stroke snapshots only the texel bounding box it
//! touched into a small history texture (a GPU texture-to-texture copy — no CPU stall),
//! so one undo step costs the painted region, not the whole canvas (the spirit of the
//! dirty-tile doctrine in docs/03 §2; per-256²-tile granularity is the later refinement
//! that matters once the canvas is bigger than one texture).
//!
//! **Arbitrary width×height (M5):** the canvas need not be square. `radius_uv` is defined
//! as a fraction of **width**; dab generation corrects the clip-space y-radius by the
//! width/height aspect so a "round" brush stays round in texel space on any aspect ratio
//! (see `push_dab`). Still deferred: true 256² *sparse* tiling of the display (for
//! canvases larger than a single texture) — premature for an artboard, documented in
//! DECISIONS.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// The dab footprint shape (Krita-referenced "brush tip"). `Round` is the classic soft
/// circle; `Square` is a chebyshev-distance box; `Soft` is an airbrush-style gaussian.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BrushTip {
    #[default]
    Round,
    Square,
    Soft,
}

impl BrushTip {
    /// Shader id passed per-dab (matches the BRUSH_WGSL switch).
    fn id(self) -> f32 {
        match self {
            Self::Round => 0.0,
            Self::Square => 1.0,
            Self::Soft => 2.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Round => "Round",
            Self::Square => "Square",
            Self::Soft => "Soft",
        }
    }
    pub fn all() -> [BrushTip; 3] {
        [Self::Round, Self::Square, Self::Soft]
    }
}

/// How a dab is composited onto the canvas. `Normal` is alpha-over; `Add` is linear-dodge
/// (lighten/glow); `Erase` removes paint back toward transparent. Each is a distinct
/// fixed-function blend state, so each gets its own pipeline. docs/03 §2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BrushBlend {
    #[default]
    Normal,
    Add,
    Erase,
}

impl BrushBlend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Add => "Add",
            Self::Erase => "Erase",
        }
    }
    pub fn all() -> [BrushBlend; 3] {
        [Self::Normal, Self::Add, Self::Erase]
    }
    fn blend_state(self) -> wgpu::BlendState {
        use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState};
        match self {
            Self::Normal => BlendState::ALPHA_BLENDING,
            // Linear dodge: dst + src·srcA (color), alpha saturates.
            Self::Add => BlendState {
                color: BlendComponent {
                    src_factor: BlendFactor::SrcAlpha,
                    dst_factor: BlendFactor::One,
                    operation: BlendOperation::Add,
                },
                alpha: BlendComponent {
                    src_factor: BlendFactor::One,
                    dst_factor: BlendFactor::One,
                    operation: BlendOperation::Add,
                },
            },
            // Eraser: scale destination down by the dab's coverage.
            Self::Erase => BlendState {
                color: BlendComponent {
                    src_factor: BlendFactor::Zero,
                    dst_factor: BlendFactor::OneMinusSrcAlpha,
                    operation: BlendOperation::Add,
                },
                alpha: BlendComponent {
                    src_factor: BlendFactor::Zero,
                    dst_factor: BlendFactor::OneMinusSrcAlpha,
                    operation: BlendOperation::Add,
                },
            },
        }
    }
}

/// A brush configuration. `radius_uv` is the dab radius as a **fraction of canvas width**
/// (0..1). `hardness` in [0,1]: 1 = crisp edge, 0 = fully soft falloff. `flow` is the
/// per-dab alpha multiplier — low flow + overlapping dabs build up gradually. `tip` is the
/// footprint shape; `blend` is the deposit mode; `smudge` (0..1) drags existing canvas
/// colour along the stroke (0 = off).
#[derive(Clone, Copy, Debug)]
pub struct Brush {
    pub radius_uv: f32,
    pub color: [f32; 4],
    pub hardness: f32,
    pub flow: f32,
    pub tip: BrushTip,
    pub blend: BrushBlend,
    pub smudge: f32,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            radius_uv: 0.02,
            color: [0.10, 0.11, 0.13, 1.0],
            hardness: 0.5,
            flow: 0.7,
            tip: BrushTip::Round,
            blend: BrushBlend::Normal,
            smudge: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabVertex {
    clip: [f32; 2],
    local: [f32; 2],
    color: [f32; 4],
    /// [hardness, flow, tip_id, _].
    params: [f32; 4],
}

/// A texel bounding box (exclusive max), clamped to the texture.
#[derive(Clone, Copy, Debug)]
struct Bbox {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Bbox {
    fn w(&self) -> u32 {
        self.x1 - self.x0
    }
    fn h(&self) -> u32 {
        self.y1 - self.y0
    }
}

/// A snapshot of a region for undo/redo: the region's origin + a texture holding the
/// `w`×`h` pixels that were there before the change.
struct HistoryRegion {
    x: u32,
    y: u32,
    tex: wgpu::Texture,
}

pub struct RasterCanvas {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// One pipeline per `BrushBlend` (Normal, Add, Erase), indexed by `blend_index`.
    brush_pipelines: [wgpu::RenderPipeline; 3],
    /// Smudge: a per-stroke copy of the canvas the smudge shader samples from, plus its
    /// pipeline. The dab picks up colour from `smudge_source` behind the stroke and lays
    /// it forward, dragging paint (Krita-referenced).
    smudge_source: wgpu::Texture,
    smudge_view: wgpu::TextureView,
    smudge_sampler: wgpu::Sampler,
    smudge_bind_layout: wgpu::BindGroupLayout,
    smudge_pipeline: wgpu::RenderPipeline,
    paper: [f32; 4],

    // Stroke + undo state.
    stroke_active: bool,
    pre_stroke: wgpu::Texture, // full-size scratch holding pre-stroke pixels
    dirty: Option<Bbox>,
    undo: Vec<HistoryRegion>,
    redo: Vec<HistoryRegion>,
    max_history: usize,
}

impl RasterCanvas {
    pub fn new(device: &wgpu::Device, width: u32, height: u32, paper: [f32; 4]) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let make_tex = |label: &str| {
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
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let texture = make_tex("raster paint texture");
        let pre_stroke = make_tex("raster pre-stroke scratch");
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("brush shader"),
            source: wgpu::ShaderSource::Wgsl(BRUSH_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("brush pipeline layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        // One pipeline per BrushBlend — fixed-function blend state is baked into the
        // pipeline, so the deposit mode is chosen by selecting a pipeline at stamp time.
        let make_pipeline = |blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("brush pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<DabVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 16,
                                shader_location: 2,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                            wgpu::VertexAttribute {
                                offset: 32,
                                shader_location: 3,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let brush_pipelines = [
            make_pipeline(BrushBlend::Normal.blend_state()),
            make_pipeline(BrushBlend::Add.blend_state()),
            make_pipeline(BrushBlend::Erase.blend_state()),
        ];

        // --- Smudge pipeline (samples a copy of the canvas behind the stroke) ---------
        let smudge_source = make_tex("raster smudge source");
        let smudge_view = smudge_source.create_view(&wgpu::TextureViewDescriptor::default());
        let smudge_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("smudge sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let smudge_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("smudge shader"),
            source: wgpu::ShaderSource::Wgsl(SMUDGE_WGSL.into()),
        });
        let smudge_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("smudge bind layout"),
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
        let smudge_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("smudge pipeline layout"),
            bind_group_layouts: &[Some(&smudge_bind_layout)],
            immediate_size: 0,
        });
        let smudge_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("smudge pipeline"),
            layout: Some(&smudge_pl),
            vertex: wgpu::VertexState {
                module: &smudge_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<DabVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 8, shader_location: 1, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 16, shader_location: 2, format: wgpu::VertexFormat::Float32x4 },
                        wgpu::VertexAttribute { offset: 32, shader_location: 3, format: wgpu::VertexFormat::Float32x4 },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &smudge_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            width,
            height,
            format,
            texture,
            view,
            brush_pipelines,
            smudge_source,
            smudge_view,
            smudge_sampler,
            smudge_bind_layout,
            smudge_pipeline,
            paper,
            stroke_active: false,
            pre_stroke,
            dirty: None,
            undo: Vec::new(),
            redo: Vec::new(),
            max_history: 32,
        }
    }

    pub fn texture_view(&self) -> &wgpu::TextureView {
        &self.view
    }
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Fill the whole canvas with the paper color. Records into `encoder`. Note: a bare
    /// clear is *not* undoable on its own — `clear_undoable` is the user-facing path.
    pub fn clear(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("raster clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: self.paper[0] as f64,
                        g: self.paper[1] as f64,
                        b: self.paper[2] as f64,
                        a: self.paper[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// Clear the canvas, recording a full-canvas undo entry first so the clear can be
    /// undone.
    pub fn clear_undoable(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let full = Bbox {
            x0: 0,
            y0: 0,
            x1: self.width,
            y1: self.height,
        };
        self.push_undo_region(device, encoder, full);
        self.redo.clear();
        self.clear(encoder);
    }

    /// Replace the whole canvas with `pixels` (`width*height*4` RGBA8), recording a
    /// full-canvas undo entry first so the replace (e.g. an image import) can be undone.
    ///
    /// The overwrite goes through the **encoder** (staging buffer → `copy_buffer_to_texture`),
    /// not `queue.write_texture`: queue writes are flushed *before* the command buffers in a
    /// submit, which would clobber the snapshot copy and make undo capture the new pixels.
    /// Recording both copies in the encoder keeps them correctly ordered (snapshot, then
    /// overwrite).
    pub fn upload_rgba_undoable(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        pixels: &[u8],
    ) {
        let full = Bbox { x0: 0, y0: 0, x1: self.width, y1: self.height };
        self.push_undo_region(device, encoder, full);
        self.redo.clear();

        let staging = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("raster import staging"),
            contents: pixels,
            usage: wgpu::BufferUsages::COPY_SRC,
        });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.width * 4),
                    rows_per_image: Some(self.height),
                },
            },
            self.texture.as_image_copy(),
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
    }

    /// Stamp a row of dabs from `from_uv` to `to_uv` (UV in 0..1, origin top-left). Begins
    /// a stroke lazily (snapshotting the canvas) on the first stamp; expands the dirty
    /// bbox; records the dabs. Call `end_stroke` on mouse-up.
    ///
    /// `scissor` — when `Some([x, y, w, h])` in texel space, clips dabs to that rectangle
    /// using a wgpu scissor rect. Used for selection-constrained painting (M3).
    pub fn stamp_segment(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        from_uv: [f32; 2],
        to_uv: [f32; 2],
        brush: &Brush,
        scissor: Option<[u32; 4]>,
    ) {
        if !self.stroke_active {
            // Snapshot the full canvas into the pre-stroke scratch so end_stroke can
            // carve out just the dirty region for history.
            encoder.copy_texture_to_texture(
                self.texture.as_image_copy(),
                self.pre_stroke.as_image_copy(),
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
            self.stroke_active = true;
            self.dirty = None;
        }
        self.note_dirty(from_uv, to_uv, brush);

        let aspect = self.width as f32 / self.height as f32;
        let verts = build_dab_vertices(from_uv, to_uv, brush, aspect);
        if verts.is_empty() {
            return;
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("brush dab vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        // Smudge: snapshot the canvas into smudge_source and prepare the sampling bind
        // group BEFORE the render pass begins (it samples a *copy*, not the live target).
        let smudging = brush.smudge > 0.0001;
        let smudge_bg = if smudging {
            encoder.copy_texture_to_texture(
                self.texture.as_image_copy(),
                self.smudge_source.as_image_copy(),
                wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
            );
            // Pick up colour from *behind* the stroke direction by ~one radius (in UV space,
            // which is already axis-normalized regardless of canvas aspect).
            let dx = to_uv[0] - from_uv[0];
            let dy = to_uv[1] - from_uv[1];
            let len = (dx * dx + dy * dy).sqrt();
            let (nx, ny) = if len > 1e-6 { (dx / len, dy / len) } else { (0.0, 0.0) };
            let off = brush.radius_uv * 0.9;
            let inv_w = 1.0 / self.width as f32;
            let inv_h = 1.0 / self.height as f32;
            // [inv_size.xy, pickup_offset.xy, amount, pad, pad, pad]
            let uni: [f32; 8] = [inv_w, inv_h, -nx * off, -ny * off, brush.smudge.clamp(0.0, 1.0), 0.0, 0.0, 0.0];
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("smudge uniform"),
                contents: bytemuck::cast_slice(&uni),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("smudge bg"),
                layout: &self.smudge_bind_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.smudge_view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.smudge_sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: ubuf.as_entire_binding() },
                ],
            }))
        } else {
            None
        };

        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("brush stamp"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(bg) = &smudge_bg {
            rpass.set_pipeline(&self.smudge_pipeline);
            rpass.set_bind_group(0, bg, &[]);
        } else {
            let blend_index = match brush.blend {
                BrushBlend::Normal => 0,
                BrushBlend::Add => 1,
                BrushBlend::Erase => 2,
            };
            rpass.set_pipeline(&self.brush_pipelines[blend_index]);
        }
        if let Some([sx, sy, sw, sh]) = scissor {
            rpass.set_scissor_rect(sx, sy, sw.max(1), sh.max(1));
        }
        rpass.set_vertex_buffer(0, vbuf.slice(..));
        rpass.draw(0..verts.len() as u32, 0..1);
    }

    /// Finish the current stroke: carve the dirty region out of the pre-stroke snapshot
    /// into an undo entry. No-op if no stroke is active.
    pub fn end_stroke(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        if !self.stroke_active {
            return;
        }
        self.stroke_active = false;
        let Some(bbox) = self.dirty.take() else {
            return;
        };
        if bbox.w() == 0 || bbox.h() == 0 {
            return;
        }
        // History region pixels come from the PRE-stroke snapshot (what was there before).
        let region = self.make_region_from(device, encoder, &self.pre_stroke_clone_handle(), bbox);
        self.undo.push(region);
        if self.undo.len() > self.max_history {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Undo the last stroke/clear. Returns true if anything was undone.
    pub fn undo(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        // Save current pixels of the region for redo, then restore the history pixels.
        let bbox = Bbox {
            x0: entry.x,
            y0: entry.y,
            x1: entry.x + entry.tex.width(),
            y1: entry.y + entry.tex.height(),
        };
        let redo_region = self.make_region_from_self(device, encoder, bbox);
        self.redo.push(redo_region);
        self.copy_region_into_canvas(encoder, &entry);
        true
    }

    /// Redo the last undone change. Returns true if anything was redone.
    pub fn redo(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        let bbox = Bbox {
            x0: entry.x,
            y0: entry.y,
            x1: entry.x + entry.tex.width(),
            y1: entry.y + entry.tex.height(),
        };
        let undo_region = self.make_region_from_self(device, encoder, bbox);
        self.undo.push(undo_region);
        self.copy_region_into_canvas(encoder, &entry);
        true
    }

    /// Borrow the raw texture (for readback during persistence).
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Upload full-canvas RGBA8 pixels (used when loading a painting). `pixels` must be
    /// `width*height*4` bytes, row-major, top-left origin.
    /// Raw full-canvas texture write. Does NOT touch undo/redo — callers decide.
    fn write_texture_rgba(&self, queue: &wgpu::Queue, pixels: &[u8]) {
        let bpr = self.width * 4;
        queue.write_texture(
            self.texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn upload_rgba(&mut self, queue: &wgpu::Queue, pixels: &[u8]) {
        self.write_texture_rgba(queue, pixels);
        // A load (e.g. opening a project) invalidates undo history.
        self.undo.clear();
        self.redo.clear();
    }

    // --- internals ---

    fn note_dirty(&mut self, from_uv: [f32; 2], to_uv: [f32; 2], brush: &Brush) {
        let tw = self.width as f32;
        let th = self.height as f32;
        // radius_uv is a fraction of width; convert to texels per axis (texel_radius is the
        // same in both axes — see push_dab — so pad both axes by the same texel amount).
        let pad = brush.radius_uv * tw + 2.0;
        let minu = from_uv[0].min(to_uv[0]) * tw - pad;
        let maxu = from_uv[0].max(to_uv[0]) * tw + pad;
        let minv = from_uv[1].min(to_uv[1]) * th - pad;
        let maxv = from_uv[1].max(to_uv[1]) * th + pad;
        let x0 = minu.floor().clamp(0.0, tw) as u32;
        let y0 = minv.floor().clamp(0.0, th) as u32;
        let x1 = maxu.ceil().clamp(0.0, tw) as u32;
        let y1 = maxv.ceil().clamp(0.0, th) as u32;
        let bbox = Bbox { x0, y0, x1, y1 };
        self.dirty = Some(match self.dirty {
            None => bbox,
            Some(d) => Bbox {
                x0: d.x0.min(bbox.x0),
                y0: d.y0.min(bbox.y0),
                x1: d.x1.max(bbox.x1),
                y1: d.y1.max(bbox.y1),
            },
        });
    }

    fn new_region_texture(&self, device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("raster history region"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Copy `bbox` out of `src` into a fresh history-region texture.
    fn make_region_from(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::Texture,
        bbox: Bbox,
    ) -> HistoryRegion {
        let tex = self.new_region_texture(device, bbox.w(), bbox.h());
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: src,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: bbox.x0,
                    y: bbox.y0,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            tex.as_image_copy(),
            wgpu::Extent3d {
                width: bbox.w(),
                height: bbox.h(),
                depth_or_array_layers: 1,
            },
        );
        HistoryRegion {
            x: bbox.x0,
            y: bbox.y0,
            tex,
        }
    }

    fn make_region_from_self(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        bbox: Bbox,
    ) -> HistoryRegion {
        self.make_region_from(device, encoder, &self.texture, bbox)
    }

    /// Helper so `end_stroke` can pass the pre-stroke texture to `make_region_from`
    /// without a borrow conflict on `self`.
    fn pre_stroke_clone_handle(&self) -> wgpu::Texture {
        self.pre_stroke.clone()
    }

    fn push_undo_region(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        bbox: Bbox,
    ) {
        let region = self.make_region_from_self(device, encoder, bbox);
        self.undo.push(region);
        if self.undo.len() > self.max_history {
            self.undo.remove(0);
        }
    }

    fn copy_region_into_canvas(&self, encoder: &mut wgpu::CommandEncoder, region: &HistoryRegion) {
        encoder.copy_texture_to_texture(
            region.tex.as_image_copy(),
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x,
                    y: region.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: region.tex.width(),
                height: region.tex.height(),
                depth_or_array_layers: 1,
            },
        );
    }
}

/// `canvas_aspect` is width/height — see `push_dab` for why the y-radius needs it.
fn build_dab_vertices(from_uv: [f32; 2], to_uv: [f32; 2], brush: &Brush, canvas_aspect: f32) -> Vec<DabVertex> {
    let r = brush.radius_uv.max(0.0005);
    let dx = to_uv[0] - from_uv[0];
    let dy = to_uv[1] - from_uv[1];
    let len = (dx * dx + dy * dy).sqrt();
    let spacing = (r * 0.25).max(0.0005);
    let steps = (len / spacing).ceil().max(1.0) as usize;

    let mut verts = Vec::with_capacity(steps * 6);
    for i in 0..=steps {
        let t = if steps == 0 {
            0.0
        } else {
            i as f32 / steps as f32
        };
        let cu = from_uv[0] + dx * t;
        let cv = from_uv[1] + dy * t;
        push_dab(&mut verts, cu, cv, r, brush, canvas_aspect);
    }
    verts
}

fn push_dab(out: &mut Vec<DabVertex>, cu: f32, cv: f32, r: f32, brush: &Brush, canvas_aspect: f32) {
    let cx = cu * 2.0 - 1.0;
    let cy = 1.0 - cv * 2.0;
    // radius_uv is a fraction of canvas WIDTH. Clip space is [-1,1] on both axes regardless
    // of the target's pixel aspect ratio, so an equal clip-space rx/ry only looks circular
    // in texels when width==height. Scale ry by (width/height) so the same texel radius
    // applies on both axes on any aspect ratio — a "round" brush stays round.
    let rx = r * 2.0;
    let ry = r * 2.0 * canvas_aspect;
    let params = [
        brush.hardness.clamp(0.0, 1.0),
        brush.flow.clamp(0.0, 1.0),
        brush.tip.id(),
        0.0,
    ];
    let corner = |sx: f32, sy: f32| DabVertex {
        clip: [cx + sx * rx, cy + sy * ry],
        local: [sx, sy],
        color: brush.color,
        params,
    };
    let tl = corner(-1.0, 1.0);
    let tr = corner(1.0, 1.0);
    let br = corner(1.0, -1.0);
    let bl = corner(-1.0, -1.0);
    out.extend_from_slice(&[tl, tr, br, tl, br, bl]);
}

const BRUSH_WGSL: &str = r#"
struct VsIn {
    @location(0) clip: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec4<f32>,
};
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
};
@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(v.clip, 0.0, 1.0);
    out.local = v.local;
    out.color = v.color;
    out.params = v.params;
    return out;
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    let hardness = f.params.x;
    let flow = f.params.y;
    let tip = f.params.z;

    // Distance metric per tip: round = euclidean, square = chebyshev, soft = euclidean
    // with a gaussian falloff.
    var d: f32;
    if (tip == 1.0) {
        d = max(abs(f.local.x), abs(f.local.y));
    } else {
        d = length(f.local);
    }
    if (d > 1.0) { discard; }

    var falloff: f32;
    if (tip == 2.0) {
        // Soft / airbrush: gaussian bell, hardness shifts the sigma.
        let sigma = mix(0.25, 0.7, hardness);
        falloff = exp(-(d * d) / (2.0 * sigma * sigma));
    } else {
        falloff = 1.0 - smoothstep(hardness, 1.0, d);
    }

    let alpha = falloff * flow * f.color.a;
    if (alpha <= 0.0001) { discard; }
    return vec4<f32>(f.color.rgb, alpha);
}
"#;

/// Smudge: each dab samples a copy of the canvas at the fragment's own position offset
/// *backward* along the stroke, then lays that colour down with the dab falloff — so paint
/// is dragged forward along the stroke. Krita-referenced.
const SMUDGE_WGSL: &str = r#"
struct VsIn {
    @location(0) clip: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec4<f32>,
};
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) params: vec4<f32>,
};
struct SmudgeU { inv_size: vec2<f32>, offset: vec2<f32>, amount: vec4<f32> };
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
@group(0) @binding(2) var<uniform> u: SmudgeU;

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(v.clip, 0.0, 1.0);
    out.local = v.local;
    out.params = v.params;
    return out;
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    let hardness = f.params.x;
    let flow = f.params.y;
    let tip = f.params.z;

    var d: f32;
    if (tip == 1.0) { d = max(abs(f.local.x), abs(f.local.y)); }
    else { d = length(f.local); }
    if (d > 1.0) { discard; }

    var falloff: f32;
    if (tip == 2.0) {
        let sigma = mix(0.25, 0.7, hardness);
        falloff = exp(-(d * d) / (2.0 * sigma * sigma));
    } else {
        falloff = 1.0 - smoothstep(hardness, 1.0, d);
    }

    // This fragment's canvas UV, then the pick-up point behind the stroke.
    let frag_uv = f.pos.xy * u.inv_size;
    let pick = clamp(frag_uv + u.offset, vec2<f32>(0.0), vec2<f32>(1.0));
    let src = textureSample(src_tex, src_samp, pick);

    let alpha = falloff * flow * u.amount.x;
    if (alpha <= 0.0001) { discard; }
    return vec4<f32>(src.rgb, alpha);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        let dq = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("raster test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: Default::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some(dq)
    }

    /// Read the center + a corner pixel of the canvas (full readback).
    fn center_corner(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &RasterCanvas,
    ) -> ([u8; 4], [u8; 4]) {
        let w = canvas.width();
        let h = canvas.height();
        let bpr = w * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = slice.get_mapped_range();
        let px = |x: u32, y: u32| {
            let i = (y * bpr + x * 4) as usize;
            [data[i], data[i + 1], data[i + 2], data[i + 3]]
        };
        (px(w / 2, h / 2), px(4, 4))
    }

    #[test]
    fn brush_stroke_changes_center_pixels() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip");
            return;
        };
        let mut canvas = RasterCanvas::new(&device, 256, 256, [1.0, 1.0, 1.0, 1.0]);
        let brush = Brush {
            radius_uv: 0.06,
            color: [0.0, 0.0, 0.0, 1.0],
            hardness: 0.9,
            flow: 1.0,
            ..Brush::default()
        };
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear(&mut enc);
        canvas.stamp_segment(&device, &mut enc, [0.15, 0.5], [0.85, 0.5], &brush, None);
        canvas.end_stroke(&device, &mut enc);
        queue.submit(Some(enc.finish()));
        let (center, corner) = center_corner(&device, &queue, &canvas);
        assert!(center[0] < 128, "center darkened, got {}", center[0]);
        assert!(corner[0] > 220, "corner stays paper, got {}", corner[0]);
    }

    #[test]
    fn undo_restores_pre_stroke_then_redo_reapplies() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip");
            return;
        };
        let mut canvas = RasterCanvas::new(&device, 256, 256, [1.0, 1.0, 1.0, 1.0]);
        let brush = Brush {
            radius_uv: 0.08,
            color: [0.0, 0.0, 0.0, 1.0],
            hardness: 0.9,
            flow: 1.0,
            ..Brush::default()
        };

        // Paint over the center.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear(&mut enc);
        canvas.stamp_segment(&device, &mut enc, [0.3, 0.5], [0.7, 0.5], &brush, None);
        canvas.end_stroke(&device, &mut enc);
        queue.submit(Some(enc.finish()));
        assert!(
            center_corner(&device, &queue, &canvas).0[0] < 128,
            "painted"
        );
        assert!(canvas.can_undo());

        // Undo → center back to paper.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        assert!(canvas.undo(&device, &mut enc));
        queue.submit(Some(enc.finish()));
        assert!(
            center_corner(&device, &queue, &canvas).0[0] > 220,
            "undo restored paper"
        );
        assert!(canvas.can_redo());

        // Redo → center dark again.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        assert!(canvas.redo(&device, &mut enc));
        queue.submit(Some(enc.finish()));
        assert!(
            center_corner(&device, &queue, &canvas).0[0] < 128,
            "redo reapplied"
        );
    }

    #[test]
    fn upload_undoable_replaces_then_undo_restores() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip");
            return;
        };
        let mut canvas = RasterCanvas::new(&device, 256, 256, [1.0, 1.0, 1.0, 1.0]);
        // Initialise the texture to paper (new() doesn't clear until an encoder runs).
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear(&mut enc);
        queue.submit(Some(enc.finish()));
        assert!(center_corner(&device, &queue, &canvas).0[0] > 220, "starts light");

        // Import a fully-red image as one undoable edit.
        let red = {
            let mut v = vec![0u8; 256 * 256 * 4];
            for px in v.chunks_exact_mut(4) {
                px.copy_from_slice(&[220, 20, 20, 255]);
            }
            v
        };
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.upload_rgba_undoable(&device, &queue, &mut enc, &red);
        queue.submit(Some(enc.finish()));
        let after = center_corner(&device, &queue, &canvas).0;
        assert!(after[0] > 150 && after[1] < 90, "canvas now red, got {after:?}");
        assert!(canvas.can_undo(), "import is undoable");

        // Undo → back to the prior (light) canvas.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        assert!(canvas.undo(&device, &mut enc));
        queue.submit(Some(enc.finish()));
        assert!(
            center_corner(&device, &queue, &canvas).0[0] > 220,
            "undo restored the pre-import canvas"
        );
    }

    #[test]
    fn erase_and_smudge_brushes_run_without_panic() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip");
            return;
        };
        let mut canvas = RasterCanvas::new(&device, 256, 256, [1.0, 1.0, 1.0, 1.0]);
        // Paint a black stroke (Normal), then erase part of it, then smudge across it —
        // exercises all three blend pipelines + the smudge pipeline + tip shapes.
        let strokes = [
            Brush { color: [0.0, 0.0, 0.0, 1.0], blend: BrushBlend::Normal, tip: BrushTip::Round, ..Brush::default() },
            Brush { color: [1.0, 1.0, 1.0, 1.0], blend: BrushBlend::Erase, tip: BrushTip::Square, ..Brush::default() },
            Brush { color: [1.0, 0.0, 0.0, 1.0], blend: BrushBlend::Add, tip: BrushTip::Soft, ..Brush::default() },
            Brush { smudge: 0.8, ..Brush::default() },
        ];
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear(&mut enc);
        for b in &strokes {
            canvas.stamp_segment(&device, &mut enc, [0.2, 0.5], [0.8, 0.5], b, None);
        }
        canvas.end_stroke(&device, &mut enc);
        queue.submit(Some(enc.finish()));
        // If nothing panicked and the device didn't error, all the new pipelines are valid.
        let _ = center_corner(&device, &queue, &canvas);
    }

    /// A scissor rect that covers only the right half of a 256² canvas (u∈[0.5,1.0])
    /// should block a stroke at u=0.25 (center-left) from changing those pixels.
    #[test]
    fn scissor_clips_stroke_outside_rect() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip");
            return;
        };
        let size = 256u32;
        let mut canvas = RasterCanvas::new(&device, size, size, [1.0, 1.0, 1.0, 1.0]);
        let brush = Brush {
            radius_uv: 0.06,
            color: [0.0, 0.0, 0.0, 1.0],
            hardness: 0.9,
            flow: 1.0,
            ..Brush::default()
        };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear(&mut enc);
        // Scissor: right half only [128, 0, 128, 256].
        let scissor = Some([128u32, 0, 128, 256]);
        // Stroke aimed at left-center (u=0.25), which is outside the scissor.
        canvas.stamp_segment(&device, &mut enc, [0.2, 0.5], [0.3, 0.5], &brush, scissor);
        canvas.end_stroke(&device, &mut enc);
        queue.submit(Some(enc.finish()));

        // Read the pixel at x=64, y=128 (u=0.25, v=0.5) — should still be paper (light).
        let bpr = size * 4;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scissor test readback"),
            size: (bpr * size) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc2 = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc2.copy_texture_to_buffer(
            canvas.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(size),
                },
            },
            wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        );
        queue.submit(Some(enc2.finish()));
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let data = slice.get_mapped_range();
        let x = 64usize;
        let y = 128usize;
        let idx = (y * size as usize + x) * 4;
        let px = [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]];
        assert!(px[0] > 200, "pixel at u=0.25 should be paper (scissored out), got {px:?}");
    }

    /// M5: a non-square (2:1) canvas paints, reads back at the right dimensions, and
    /// round-trips through undo — the width/height split doesn't silently degrade to square.
    #[test]
    fn non_square_canvas_paints_and_reports_correct_dimensions() {
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip");
            return;
        };
        let (w, h) = (320u32, 160u32);
        let mut canvas = RasterCanvas::new(&device, w, h, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(canvas.width(), w);
        assert_eq!(canvas.height(), h);

        let brush = Brush {
            radius_uv: 0.08,
            color: [0.0, 0.0, 0.0, 1.0],
            hardness: 0.9,
            flow: 1.0,
            ..Brush::default()
        };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear(&mut enc);
        // Dab dead-center; on a non-square canvas this exercises the aspect-corrected
        // vertex generation (an uncorrected dab would stretch into an ellipse but should
        // still darken the center pixel either way — the geometry fix is about *shape*,
        // this test is about *dimensions and basic paint* surviving the refactor).
        canvas.stamp_segment(&device, &mut enc, [0.5, 0.5], [0.5, 0.5], &brush, None);
        canvas.end_stroke(&device, &mut enc);
        queue.submit(Some(enc.finish()));

        let bpr = w * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("non-square readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc2 = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc2.copy_texture_to_buffer(
            canvas.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(Some(enc2.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let data = slice.get_mapped_range();
        assert_eq!(data.len(), (bpr * h) as usize, "readback buffer matches w*h*4, not a square guess");
        let idx = ((h / 2) * bpr + (w / 2) * 4) as usize;
        assert!(data[idx] < 128, "center darkened on a non-square canvas, got {}", data[idx]);

        assert!(canvas.can_undo());
    }
}
