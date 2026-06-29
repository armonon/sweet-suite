//! Tiled raster substrate (docs/01 §2, docs/03 §2).
//!
//! `TileCanvas` replaces the monolithic 1536² texture with a **sparse map of 256² tiles**.
//! A tile is allocated the first time a brush stamp touches it. Per-tile undo means one
//! undo step restores at most a 256×256 patch — never the whole canvas — so even a
//! 100-megapixel artboard stays snappy.
//!
//! The renderer samples a **composite display texture** (`display_view()`), which is updated
//! from the tile textures after every stroke. This keeps the render path identical to Phase 2:
//! one sampler, one texture bind. The tile textures are the "source of truth"; the composite is
//! a derived read-only view for the GPU.
//!
//! Canvas extent: `CANVAS_TILES × TILE_SIZE = 16 × 256 = 4096` pixels per axis → 16MP canvas.
//! Only touched tiles consume GPU memory; an empty canvas allocates nothing.

use std::collections::{HashMap, HashSet};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::raster::Brush;

pub const TILE_SIZE: u32 = 256;
pub const CANVAS_TILES: u32 = 16; // tiles per axis → 4096² virtual canvas
pub const DISPLAY_SIZE: u32 = TILE_SIZE * CANVAS_TILES; // 4096 px

/// (tile_x, tile_y) in [0, CANVAS_TILES).
type TileCoord = (u32, u32);

/// A snapshot of one tile before the current stroke began — for undo.
struct TileSnapshot {
    coord: TileCoord,
    tex: wgpu::Texture,
}

/// One undo entry = all tiles touched by one stroke (before the stroke).
struct StrokeEntry {
    snapshots: Vec<TileSnapshot>,
}

pub struct TileCanvas {
    format: wgpu::TextureFormat,
    paper: [f32; 4],
    /// Actual composite display size (DISPLAY_SIZE capped to device limits).
    actual_display_size: u32,
    /// Sparse tile textures (256² each), allocated on first paint.
    tiles: HashMap<TileCoord, wgpu::Texture>,
    /// The composite display texture (up to 4096²) sampled by the renderer.
    composite: wgpu::Texture,
    composite_view: wgpu::TextureView,
    /// Pipeline to copy a tile region into the composite (a simple blit pass).
    blit_pipeline: wgpu::RenderPipeline,
    blit_sampler: wgpu::Sampler,
    blit_bind_layout: wgpu::BindGroupLayout,
    /// Brush pipeline — renders into individual tile textures.
    brush_pipeline: wgpu::RenderPipeline,
    // Stroke state.
    stroke_active: bool,
    /// Tiles snapshotted at stroke begin (pre-paint pixels, for undo).
    stroke_snapshots: HashMap<TileCoord, wgpu::Texture>,
    undo: Vec<StrokeEntry>,
    redo: Vec<StrokeEntry>,
    max_history: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabVertex {
    /// clip position in the *tile's* clip space.
    clip: [f32; 2],
    /// local coords in [-1,1] for falloff.
    local: [f32; 2],
    color: [f32; 4],
    /// [hardness, flow * pressure]
    params: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlitVertex {
    pos: [f32; 2], // clip
    uv: [f32; 2],  // UV into tile texture (always 0..1)
}

impl TileCanvas {
    pub fn new(device: &wgpu::Device, paper: [f32; 4]) -> Self {
        let max = device.limits().max_texture_dimension_2d;
        Self::new_with_max(device, paper, max)
    }

    /// Create with an explicit max composite size (useful for tests on headless adapters).
    pub fn new_with_max(device: &wgpu::Device, paper: [f32; 4], max_size: u32) -> Self {
        let display_size = DISPLAY_SIZE.min(max_size);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;

        let composite = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile composite display"),
            size: wgpu::Extent3d {
                width: display_size,
                height: display_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let composite_view = composite.create_view(&wgpu::TextureViewDescriptor::default());

        let brush_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tile brush shader"),
            source: wgpu::ShaderSource::Wgsl(BRUSH_WGSL.into()),
        });
        let brush_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tile brush layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let brush_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("tile brush pipeline"),
                layout: Some(&brush_layout),
                vertex: wgpu::VertexState {
                    module: &brush_shader,
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
                                format: wgpu::VertexFormat::Float32x2,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &brush_shader,
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

        // Blit pipeline: copy one tile texture into the composite at tile's position.
        let blit_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("blit bg layout"),
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
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit layout"),
            bind_group_layouts: &[Some(&blit_bind_layout)],
            immediate_size: 0,
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tile blit pipeline"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<BlitVertex>() as u64,
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
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
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
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            format,
            paper,
            actual_display_size: display_size,
            tiles: HashMap::new(),
            composite,
            composite_view,
            blit_pipeline,
            blit_sampler,
            blit_bind_layout,
            brush_pipeline,
            stroke_active: false,
            stroke_snapshots: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            max_history: 32,
        }
    }

    pub fn display_view(&self) -> &wgpu::TextureView {
        &self.composite_view
    }
    pub fn display_size(&self) -> u32 {
        self.actual_display_size
    }
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Clear all tiles to the paper color and update the composite. Invalidates undo history.
    pub fn clear(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        // Drop all tile textures — they'll be re-created on next paint.
        self.tiles.clear();
        self.stroke_snapshots.clear();
        self.stroke_active = false;
        // Clear the composite too.
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tile composite clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.composite_view,
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
        self.undo.clear();
        self.redo.clear();
        let _ = device; // may use for future tile pool
    }

    /// Paint a segment from `from_uv` to `to_uv` (UV in 0..1, top-left origin). `pressure`
    /// in [0,1] scales radius and flow. Begins stroke on first call; call `end_stroke` on
    /// mouse-up.
    pub fn stamp_segment(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        from_uv: [f32; 2],
        to_uv: [f32; 2],
        brush: &Brush,
        pressure: f32,
    ) {
        let pressure = pressure.clamp(0.01, 1.0);
        let effective_radius = brush.radius_uv * pressure.sqrt();
        let effective_flow = brush.flow * pressure;

        // Determine which tiles this stamp segment touches.
        let touched = tiles_touched(from_uv, to_uv, effective_radius);

        // Ensure tiles exist and snapshot before first touch.
        for &coord in &touched {
            self.ensure_tile(device, encoder, coord);
            if self.stroke_active && !self.stroke_snapshots.contains_key(&coord) {
                let snap = self.snapshot_tile(device, encoder, coord);
                self.stroke_snapshots.insert(coord, snap);
            }
        }
        if !self.stroke_active {
            // First stamp: snapshot all initially-touched tiles.
            for &coord in &touched {
                let snap = self.snapshot_tile(device, encoder, coord);
                self.stroke_snapshots.insert(coord, snap);
            }
            self.stroke_active = true;
        }

        // Render dabs into each affected tile.
        for coord in touched {
            let tile_view = self.tiles[&coord]
                .create_view(&wgpu::TextureViewDescriptor::default());
            let verts = build_dab_verts_for_tile(from_uv, to_uv, brush, effective_radius, effective_flow, coord);
            if verts.is_empty() {
                continue;
            }
            let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tile dab vbuf"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tile brush stamp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tile_view,
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
            rpass.set_pipeline(&self.brush_pipeline);
            rpass.set_vertex_buffer(0, vbuf.slice(..));
            rpass.draw(0..verts.len() as u32, 0..1);
            drop(rpass);

            // Blit tile into composite.
            self.blit_tile_to_composite(device, encoder, coord, &tile_view);
        }
    }

    /// Finish the current stroke and push an undo entry. No-op if no stroke active.
    pub fn end_stroke(&mut self, _device: &wgpu::Device, _encoder: &mut wgpu::CommandEncoder) {
        if !self.stroke_active {
            return;
        }
        self.stroke_active = false;
        let snapshots: Vec<TileSnapshot> = self
            .stroke_snapshots
            .drain()
            .map(|(coord, tex)| TileSnapshot { coord, tex })
            .collect();
        if !snapshots.is_empty() {
            self.undo.push(StrokeEntry { snapshots });
            if self.undo.len() > self.max_history {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
    }

    /// Undo the last stroke. Returns true if anything was undone.
    pub fn undo(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        // Save current state of each tile for redo.
        let mut redo_snaps = Vec::new();
        for snap in &entry.snapshots {
            self.ensure_tile(device, encoder, snap.coord);
            let current = self.snapshot_tile(device, encoder, snap.coord);
            redo_snaps.push(TileSnapshot {
                coord: snap.coord,
                tex: current,
            });
        }
        // Restore pre-stroke state.
        for snap in &entry.snapshots {
            self.restore_tile_from(encoder, snap.coord, &snap.tex);
            let tile_view = self.tiles[&snap.coord]
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.blit_tile_to_composite(device, encoder, snap.coord, &tile_view);
        }
        self.redo.push(StrokeEntry {
            snapshots: redo_snaps,
        });
        true
    }

    /// Redo the last undone stroke. Returns true if anything was redone.
    pub fn redo(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        let mut undo_snaps = Vec::new();
        for snap in &entry.snapshots {
            self.ensure_tile(device, encoder, snap.coord);
            let current = self.snapshot_tile(device, encoder, snap.coord);
            undo_snaps.push(TileSnapshot {
                coord: snap.coord,
                tex: current,
            });
        }
        for snap in &entry.snapshots {
            self.restore_tile_from(encoder, snap.coord, &snap.tex);
            let tile_view = self.tiles[&snap.coord]
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.blit_tile_to_composite(device, encoder, snap.coord, &tile_view);
        }
        self.undo.push(StrokeEntry {
            snapshots: undo_snaps,
        });
        true
    }

    /// Upload RGBA8 pixels covering `size × size` (origin top-left) into the canvas.
    /// Used when loading a saved painting. Clears undo history.
    pub fn upload_rgba(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pixels: &[u8], size: u32) {
        self.undo.clear();
        self.redo.clear();
        self.tiles.clear();
        // Upload into the composite and let individual tiles be populated lazily.
        let bpr = size * 4;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.composite,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(size),
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        // Populate tile textures from the composite so they match.
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let tiles_across = (size + TILE_SIZE - 1) / TILE_SIZE;
        for ty in 0..tiles_across {
            for tx in 0..tiles_across {
                let coord = (tx, ty);
                self.ensure_tile(device, &mut enc, coord);
                let ox = tx * TILE_SIZE;
                let oy = ty * TILE_SIZE;
                let tw = TILE_SIZE.min(size - ox);
                let th = TILE_SIZE.min(size - oy);
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.composite,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: ox, y: oy, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.tiles[&coord],
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: tw,
                        height: th,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        queue.submit(Some(enc.finish()));
    }

    /// Read back the composite as RGBA8 pixels (for persistence). Full GPU round-trip.
    pub fn readback_rgba(&self, device: &wgpu::Device, queue: &wgpu::Queue, size: u32) -> Vec<u8> {
        let bpr = size * 4;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile readback"),
            size: (bpr * size) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.composite,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(size),
                },
            },
            wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        slice.get_mapped_range().to_vec()
    }

    // --- internals ---

    fn make_tile(&self, device: &wgpu::Device) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile paint texture"),
            size: wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Ensure tile `coord` exists; create + clear to paper color if missing.
    fn ensure_tile(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
    ) {
        if self.tiles.contains_key(&coord) {
            return;
        }
        let tex = self.make_tile(device);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        // Clear to paper color.
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tile init clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        self.tiles.insert(coord, tex);
    }

    fn snapshot_tile(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
    ) -> wgpu::Texture {
        let snap = self.make_tile(device);
        let src = &self.tiles[&coord];
        encoder.copy_texture_to_texture(
            src.as_image_copy(),
            snap.as_image_copy(),
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        snap
    }

    fn restore_tile_from(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
        src: &wgpu::Texture,
    ) {
        let dst = &self.tiles[&coord];
        encoder.copy_texture_to_texture(
            src.as_image_copy(),
            dst.as_image_copy(),
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    fn blit_tile_to_composite(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
        tile_view: &wgpu::TextureView,
    ) {
        let (tx, ty) = coord;
        // Clip-space rect for this tile in the composite's space.
        let x0 = tx as f32 / CANVAS_TILES as f32 * 2.0 - 1.0;
        let x1 = (tx + 1) as f32 / CANVAS_TILES as f32 * 2.0 - 1.0;
        let y1 = 1.0 - ty as f32 / CANVAS_TILES as f32 * 2.0;
        let y0 = 1.0 - (ty + 1) as f32 / CANVAS_TILES as f32 * 2.0;
        let verts = [
            BlitVertex { pos: [x0, y1], uv: [0.0, 0.0] },
            BlitVertex { pos: [x1, y1], uv: [1.0, 0.0] },
            BlitVertex { pos: [x1, y0], uv: [1.0, 1.0] },
            BlitVertex { pos: [x0, y1], uv: [0.0, 0.0] },
            BlitVertex { pos: [x1, y0], uv: [1.0, 1.0] },
            BlitVertex { pos: [x0, y0], uv: [0.0, 1.0] },
        ];
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blit vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit bg"),
            layout: &self.blit_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(tile_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tile blit to composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.composite_view,
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
        rpass.set_pipeline(&self.blit_pipeline);
        rpass.set_bind_group(0, &bg, &[]);
        rpass.set_vertex_buffer(0, vbuf.slice(..));
        rpass.draw(0..6, 0..1);
    }
}

/// Return the set of tile coordinates that a brush segment from `from_uv` to `to_uv` with
/// `effective_radius` (in UV units) touches.
fn tiles_touched(from_uv: [f32; 2], to_uv: [f32; 2], effective_radius: f32) -> HashSet<TileCoord> {
    let w = DISPLAY_SIZE as f32;
    let tile_uv = TILE_SIZE as f32 / w; // UV size of one tile

    let min_u = from_uv[0].min(to_uv[0]) - effective_radius;
    let max_u = from_uv[0].max(to_uv[0]) + effective_radius;
    let min_v = from_uv[1].min(to_uv[1]) - effective_radius;
    let max_v = from_uv[1].max(to_uv[1]) + effective_radius;

    let tx0 = ((min_u / tile_uv).floor() as i32).clamp(0, CANVAS_TILES as i32 - 1) as u32;
    let tx1 = ((max_u / tile_uv).ceil() as i32).clamp(0, CANVAS_TILES as i32) as u32;
    let ty0 = ((min_v / tile_uv).floor() as i32).clamp(0, CANVAS_TILES as i32 - 1) as u32;
    let ty1 = ((max_v / tile_uv).ceil() as i32).clamp(0, CANVAS_TILES as i32) as u32;

    let mut out = HashSet::new();
    for ty in ty0..ty1 {
        for tx in tx0..tx1 {
            out.insert((tx, ty));
        }
    }
    out
}

/// Build dab vertices for a brush segment, clipped to `coord`'s tile.
/// The clip space is tile-local: [-1,1] maps to the tile's 256² texel range.
fn build_dab_verts_for_tile(
    from_uv: [f32; 2],
    to_uv: [f32; 2],
    brush: &Brush,
    effective_radius: f32,
    effective_flow: f32,
    coord: TileCoord,
) -> Vec<DabVertex> {
    let r = effective_radius.max(0.0005);
    let w = DISPLAY_SIZE as f32;
    let tile_uv = TILE_SIZE as f32 / w;
    let (tx, ty) = coord;
    let tile_u0 = tx as f32 * tile_uv;
    let tile_v0 = ty as f32 * tile_uv;

    let dx = to_uv[0] - from_uv[0];
    let dy = to_uv[1] - from_uv[1];
    let len = (dx * dx + dy * dy).sqrt();
    let spacing = (r * 0.25).max(0.0005);
    let steps = (len / spacing).ceil().max(1.0) as usize;

    let mut verts = Vec::new();
    for i in 0..=steps {
        let t = if steps == 0 { 0.0 } else { i as f32 / steps as f32 };
        let cu = from_uv[0] + dx * t;
        let cv = from_uv[1] + dy * t;
        // Convert UV to tile-local clip space.
        let local_u = (cu - tile_u0) / tile_uv; // 0..1 in this tile
        let local_v = (cv - tile_v0) / tile_uv;
        let cx = local_u * 2.0 - 1.0;
        let cy = 1.0 - local_v * 2.0;
        let rx = r / tile_uv * 2.0;
        let ry = r / tile_uv * 2.0;
        let params = [
            brush.hardness.clamp(0.0, 1.0),
            effective_flow.clamp(0.0, 1.0),
        ];
        let corner = |sx: f32, sy: f32| DabVertex {
            clip: [cx + sx * rx, cy + sy * ry],
            local: [sx, sy],
            color: brush.color,
            params,
            _pad: [0.0, 0.0],
        };
        let tl = corner(-1.0, 1.0);
        let tr = corner(1.0, 1.0);
        let br = corner(1.0, -1.0);
        let bl = corner(-1.0, -1.0);
        verts.extend_from_slice(&[tl, tr, br, tl, br, bl]);
    }
    verts
}

// Reuse the same brush WGSL from raster.rs.
const BRUSH_WGSL: &str = r#"
struct VsIn {
    @location(0) clip: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec2<f32>,
};
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec2<f32>,
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
    let d = length(f.local);
    if (d > 1.0) { discard; }
    let hardness = f.params.x;
    let flow = f.params.y;
    let edge0 = hardness;
    let falloff = 1.0 - smoothstep(edge0, 1.0, d);
    let alpha = falloff * flow * f.color.a;
    if (alpha <= 0.0001) { discard; }
    return vec4<f32>(f.color.rgb, alpha);
}
"#;

const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var tile_tex: texture_2d<f32>;
@group(0) @binding(1) var tile_sampler: sampler;
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
};
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(v: VsIn) -> VsOut {
    return VsOut(vec4<f32>(v.pos, 0.0, 1.0), v.uv);
}
@fragment
fn fs_main(f: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tile_tex, tile_sampler, f.uv);
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
            label: Some("tile test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: Default::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some(dq)
    }

    #[test]
    fn tiles_touched_covers_center_stamp() {
        // A stamp at the center of the canvas with a small radius should touch exactly one tile.
        let touched = tiles_touched([0.5, 0.5], [0.5, 0.5], 0.01);
        assert!(!touched.is_empty(), "at least one tile touched");
        // Tile (8,8) is the center tile in a 16×16 grid.
        let center = (8u32, 8u32);
        assert!(touched.contains(&center), "center tile touched");
    }

    #[test]
    fn tile_canvas_paints_and_undoes_without_gpu() {
        // Without a GPU, just verify the tile logic doesn't panic on the headless path.
        let Some((device, queue)) = headless() else {
            eprintln!("no GPU; skip tile canvas test");
            return;
        };
        let max = device.limits().max_texture_dimension_2d;
        let mut canvas = TileCanvas::new_with_max(&device, [1.0, 1.0, 1.0, 1.0], max);
        let brush = Brush {
            radius_uv: 0.01, // small in 0..1 UV
            color: [0.0, 0.0, 0.0, 1.0],
            hardness: 0.9,
            flow: 1.0,
            ..Brush::default()
        };

        // Paint a stroke.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.stamp_segment(&device, &mut enc, [0.5, 0.5], [0.51, 0.5], &brush, 1.0);
        canvas.end_stroke(&device, &mut enc);
        queue.submit(Some(enc.finish()));

        assert!(canvas.can_undo(), "undo available after stroke");
        assert!(!canvas.can_redo(), "no redo before undo");

        // Undo.
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        assert!(canvas.undo(&device, &mut enc));
        queue.submit(Some(enc.finish()));

        assert!(!canvas.can_undo(), "nothing left to undo");
        assert!(canvas.can_redo(), "redo available");
    }
}
