//! # suite-visual — the Visual app: the unified canvas.
//!
//! 3D modeling/sculpt + graphic design + natural-media paint on ONE canvas (docs/01).
//! Phase 1+3 lite: a wgpu canvas with a Document-backed scene, an egui shell
//! (top bar / left tool strip / right inspector / bottom timeline stub), a tool
//! box, click-to-select, and a translate gizmo.

#![allow(clippy::collapsible_if)]

mod persistence;
mod shell;
mod tools;

use std::sync::Arc;

use shell::FileAction;
use suite_doc::{Document, ObjId};
use suite_gpu::Renderer;
use suite_timeline::Timeline;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

/// M5: an imported image's canvas takes its own native aspect ratio (no more forced-square
/// padding) — this just bounds an oversized source image's VRAM footprint.
const MAX_IMPORT_DIM: u32 = 4096;

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    egui_context: egui::Context,
    document: Document,
    shell: shell::ShellState,
    input: tools::InputState,
    modifiers: Modifiers,
    timeline: Timeline,
    /// Frame timestamp for advancing the playhead between frames.
    last_frame_time: std::time::Instant,
    /// Unified undo ordering across the two undoable surfaces (scene = command-delta in
    /// `Document`, paint = GPU snapshots in `Renderer`). Each entry records which surface
    /// the action touched so ⌘Z/⌘⇧Z target the right one in true chronological order.
    undo_order: Vec<UndoKind>,
    redo_order: Vec<UndoKind>,
    /// Pre-edit snapshot of the selected object, captured at the top of each frame, used to
    /// record one coalesced undo transaction when an inspector edit burst settles.
    frame_baseline: Option<suite_gpu::suite_doc::Object>,
    edit_before: Option<suite_gpu::suite_doc::Object>,
    edit_pending: bool,
    /// A scene-mutating canvas drag (gizmo translate, sculpt, or an add) is between
    /// press and release; its undo transaction is open.
    canvas_edit_active: bool,
    /// An image file passed on the command line, imported once the renderer is ready.
    pending_startup_image: Option<std::path::PathBuf>,
}

/// Which undoable surface a history entry belongs to.
#[derive(Clone, Copy, PartialEq)]
enum UndoKind {
    Scene,
    Paint,
}

impl App {
    fn new() -> Self {
        let ctx = egui::Context::default();
        shell::apply_design_tokens(&ctx);
        // `suite-visual <file>`: if the arg is an image, import it onto the canvas once the
        // renderer is up (drained in window_event). Standard "open with…" behaviour.
        let pending_startup_image = std::env::args().nth(1).map(std::path::PathBuf::from).filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                Some("png" | "jpg" | "jpeg" | "bmp" | "tga" | "gif" | "webp")
            )
        });
        Self {
            window: None,
            renderer: None,
            egui_state: None,
            egui_renderer: None,
            egui_context: ctx,
            document: Document::with_starter_scene(),
            shell: shell::ShellState::default(),
            input: tools::InputState::default(),
            modifiers: Modifiers::default(),
            timeline: Timeline::default(),
            last_frame_time: std::time::Instant::now(),
            undo_order: Vec::new(),
            redo_order: Vec::new(),
            frame_baseline: None,
            edit_before: None,
            edit_pending: false,
            canvas_edit_active: false,
            pending_startup_image,
        }
    }

    /// Record a committed scene (command-delta) transaction onto the unified undo order.
    /// A free associated fn taking disjoint field refs (not `&mut self`) so it can be
    /// called while `renderer`/`egui_state` are borrowed in the event handler.
    fn record_scene(
        undo: &mut Vec<UndoKind>,
        redo: &mut Vec<UndoKind>,
        dirty: &mut bool,
        committed: bool,
    ) {
        if committed {
            undo.push(UndoKind::Scene);
            redo.clear();
            *dirty = true;
        }
    }

    /// M5: rescale every `PaintCanvas` object's world footprint to match `width`×`height`'s
    /// aspect ratio, keeping the longer side at 3.0 world units (the starter-scene artboard
    /// convention) and shrinking the shorter side proportionally. Without this, a non-square
    /// canvas would still render squished into the old fixed 1:1 quad — the pixels/UVs would
    /// be correct, but the on-screen presentation wrong. Called after any op that changes the
    /// canvas's overall dimensions (image import, canvas rotate).
    ///
    /// Takes `&mut Document` directly (not `&mut self`) so callers holding a live
    /// `&mut Renderer` borrowed from `self.renderer` can still call this — it borrows only
    /// the disjoint `self.document` field via `&mut self.document`, not all of `self`.
    fn rescale_paint_canvases_to_aspect(doc: &mut Document, width: u32, height: u32) {
        const LONG_SIDE: f32 = 3.0;
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let (sx, sy) = if aspect >= 1.0 {
            (LONG_SIDE, LONG_SIDE / aspect)
        } else {
            (LONG_SIDE * aspect, LONG_SIDE)
        };
        let ids: Vec<ObjId> = doc
            .objects()
            .filter(|o| o.kind == suite_doc::ObjectKind::PaintCanvas)
            .map(|o| o.id)
            .collect();
        for id in ids {
            if let Some(obj) = doc.get_mut(id) {
                obj.transform.scale.x = sx;
                obj.transform.scale.y = sy;
            }
        }
    }

    /// Clear all undo bookkeeping. Called when the document is replaced (New/Open) so the
    /// unified order doesn't point at history that no longer exists.
    fn reset_undo_state(&mut self) {
        self.undo_order.clear();
        self.redo_order.clear();
        self.edit_before = None;
        self.frame_baseline = None;
        self.edit_pending = false;
        self.canvas_edit_active = false;
    }

    /// Read the painted raster back from the GPU for embedding in a save. `None` if the
    /// renderer isn't up yet.
    /// Read back every layer's pixels + metadata for saving the full stack.
    fn current_layers(&self) -> Vec<persistence::LayerSave> {
        let Some(r) = self.renderer.as_ref() else { return Vec::new() };
        let (width, height) = (r.canvas_width(), r.canvas_height());
        let infos = r.layer_infos();
        (0..infos.len())
            .map(|i| persistence::LayerSave {
                rgba: r.layer_pixels(i),
                width,
                height,
                name: infos[i].name.clone(),
                visible: infos[i].visible,
                opacity: infos[i].opacity,
                blend: infos[i].blend,
            })
            .collect()
    }

    /// Run the file action queued by the previous frame's UI (button or shortcut). The
    /// native `rfd` dialogs block, so we run them here — outside the egui paint closure
    /// and outside the renderer borrow.
    fn handle_file_action(&mut self, action: FileAction) {
        match action {
            FileAction::New => {
                self.document = Document::with_starter_scene();
                self.reset_undo_state();
                let first = self.document.objects().next().map(|o| o.id);
                if let Some(id) = first {
                    self.document.set_selection(Some(id));
                }
                self.shell.tool = tools::Tool::Translate;
                self.shell.current_path = None;
                self.shell.dirty = false;
                self.shell.status = "New project".into();
            }
            FileAction::Open => {
                if let Some((loaded, path, status)) = persistence::open_dialog() {
                    if status.starts_with("Opened") {
                        self.document = loaded.document;
                        self.reset_undo_state();
                        let first = self.document.objects().next().map(|o| o.id);
                        if self.document.selection().is_none() {
                            self.document.set_selection(first);
                        }
                        // Restore the layer stack the bundle carried.
                        if !loaded.layers.is_empty() {
                            if let Some(r) = self.renderer.as_mut() {
                                let layers: Vec<suite_gpu::LoadedLayer> = loaded
                                    .layers
                                    .into_iter()
                                    .map(|l| suite_gpu::LoadedLayer {
                                        rgba: l.rgba,
                                        width: l.width,
                                        height: l.height,
                                        name: l.name,
                                        visible: l.visible,
                                        opacity: l.opacity,
                                        blend: l.blend,
                                    })
                                    .collect();
                                r.replace_layers(layers);
                            }
                        }
                        self.shell.current_path = Some(path);
                        self.shell.dirty = false;
                    }
                    if self.shell.status.is_empty() || status.starts_with("Open failed") {
                        self.shell.status = status;
                    }
                }
            }
            FileAction::Save => {
                let layers = self.current_layers();
                if let Some(path) = self.shell.current_path.clone() {
                    match persistence::save_to(&self.document, &layers, &path) {
                        Ok(s) => {
                            self.shell.dirty = false;
                            self.shell.status = s;
                        }
                        Err(e) => self.shell.status = format!("Save failed: {e}"),
                    }
                } else if let Some((path, status)) =
                    persistence::save_dialog(&self.document, &layers, None)
                {
                    self.shell.current_path = Some(path);
                    self.shell.dirty = false;
                    self.shell.status = status;
                }
            }
            FileAction::SaveAs => {
                let layers = self.current_layers();
                if let Some((path, status)) = persistence::save_dialog(
                    &self.document,
                    &layers,
                    self.shell.current_path.as_deref(),
                ) {
                    self.shell.current_path = Some(path);
                    self.shell.dirty = false;
                    self.shell.status = status;
                }
            }
            FileAction::ImportImage => {
                if self.renderer.is_some() {
                    if let Some(((w, h), rgba, status)) = persistence::import_image_dialog(MAX_IMPORT_DIM) {
                        if !rgba.is_empty() {
                            if let Some(r) = self.renderer.as_mut() {
                                // M5: the canvas takes the image's own aspect ratio. This is a
                                // structural resize (like opening a project), not a single
                                // undoable stroke — it replaces the whole paint substrate, so
                                // there's no prior-canvas-of-this-size to revert to. Clear the
                                // undo/redo queues too so a later ⌘Z doesn't hit a stale
                                // now-inert Paint entry from before the resize.
                                r.import_replace_canvas(w, h, &rgba);
                                self.undo_order.clear();
                                self.redo_order.clear();
                            }
                            Self::rescale_paint_canvases_to_aspect(&mut self.document, w, h);
                            self.shell.dirty = true;
                        }
                        self.shell.status = status;
                    }
                }
            }
            FileAction::ExportPng => {
                if let Some(r) = self.renderer.as_ref() {
                    let rgba = r.paint_readback_rgba();
                    let (width, height) = (r.canvas_width(), r.canvas_height());
                    if let Some(status) = persistence::export_png_dialog(&rgba, width, height) {
                        self.shell.status = status;
                    }
                }
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("SWEET · Visual")
            .with_inner_size(winit::dpi::LogicalSize::new(1400, 880));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let renderer = Renderer::new(window.clone());

        // Sensible launch state: pre-select the first object, start on Move so the
        // translate gizmo is visible immediately.
        let first_id = self.document.objects().next().map(|o| o.id);
        if let Some(id) = first_id {
            self.document.set_selection(Some(id));
        }
        self.shell.tool = tools::Tool::Translate;

        let egui_state = egui_winit::State::new(
            self.egui_context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            renderer.device(),
            renderer.surface_format(),
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );

        eprintln!("SWEET · Visual — open. Tools: 1=Select 2=Move 3=AddCube 4=AddSphere 5=AddImage. O=perspective/ortho, arrows orbit, +/- zoom, Esc=quit.");
        eprintln!("Files: ⌘N new · ⌘O open · ⌘S save · ⌘⇧S save-as (or the top-bar buttons). Projects are .sweet bundles.");
        eprintln!("Scene: {} objects.", self.document.object_count());

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Drain a file action queued by the previous frame's UI (button or shortcut)
        // before re-borrowing the renderer. The native dialog blocks; that's fine here.
        if let Some(action) = self.shell.pending_file_action.take() {
            self.handle_file_action(action);
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        // Drain a command-line image import (e.g. `suite-visual photo.jpg`).
        if let Some(path) = self.pending_startup_image.take() {
            if let Some(r) = self.renderer.as_mut() {
                match persistence::import_image_from(&path, MAX_IMPORT_DIM) {
                    Ok((w, h, rgba)) => {
                        r.import_replace_canvas(w, h, &rgba);
                        self.undo_order.clear();
                        self.redo_order.clear();
                        Self::rescale_paint_canvases_to_aspect(&mut self.document, w, h);
                        self.shell.dirty = true;
                        self.shell.status = format!("Imported {}", path.display());
                        // Switch to the paint tool + ortho so the imported image is framed.
                        self.shell.tool = tools::Tool::Paint;
                    }
                    Err(e) => self.shell.status = format!("Import failed: {e}"),
                }
            }
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        // Drain a clear-canvas request (the brush panel button).
        if self.shell.clear_canvas_requested {
            self.shell.clear_canvas_requested = false;
            if let Some(r) = self.renderer.as_mut() {
                r.paint_clear();
            }
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        // Drain a pending CSG boolean.
        if let Some((tool_id, op)) = self.shell.pending_boolean.take() {
            // The boolean mutates the selected mesh and removes the tool object — capture
            // both for undo.
            let touched: Vec<_> = self
                .document
                .selection()
                .into_iter()
                .chain(std::iter::once(tool_id))
                .collect();
            self.document.checkpoint(&touched);
            let ok = self.document.apply_boolean(tool_id, op);
            let committed = self.document.commit();
            Self::record_scene(&mut self.undo_order, &mut self.redo_order, &mut self.shell.dirty, committed);
            if ok {
                self.shell.csg_tool_id = None;
                self.shell.dirty = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        }
        // Drain an add-adjustment-layer request.
        if self.shell.pending_heightmap {
            self.shell.pending_heightmap = false;
            // Readback the paint canvas and convert to a heightmap mesh.
            // This must happen before the renderer borrow below.
            if let Some(renderer) = self.renderer.as_ref() {
                let pixels = renderer.paint_readback_rgba();
                let (width, height) = (renderer.canvas_width(), renderer.canvas_height());
                let res = self.shell.heightmap_resolution;
                let scale = self.shell.heightmap_scale;
                self.document.checkpoint(&[]);
                let id = self.document.add_heightmap_mesh(
                    &pixels, width, height, res, scale, glam::Vec3::new(0.0, 0.0, -1.5),
                );
                self.document.set_selection(Some(id));
                self.shell.dirty = true;
            }
            let committed = self.document.commit();
            Self::record_scene(&mut self.undo_order, &mut self.redo_order, &mut self.shell.dirty, committed);
        }

        if self.shell.pending_key_bone {
            self.shell.pending_key_bone = false;
            // Key the active bone's current Euler pose at the playhead.
            if let Some(sel_id) = self.document.selection() {
                let key = obj_key(sel_id);
                let bone = self.shell.active_bone as u32;
                let euler = self.shell.bone_pose_deg;
                self.timeline.set_bone_keyframe(key, bone, euler);
            }
        }

        if self.shell.pending_add_adjustment {
            self.shell.pending_add_adjustment = false;
            self.document.checkpoint(&[]);
            let id = self.document.add(
                suite_gpu::suite_doc::ObjectKind::Adjustment,
                glam::Vec3::ZERO,
            );
            self.document.set_selection(Some(id));
            let committed = self.document.commit();
            Self::record_scene(&mut self.undo_order, &mut self.redo_order, &mut self.shell.dirty, committed);
            self.shell.dirty = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        let (Some(renderer), Some(window), Some(egui_state)) = (
            self.renderer.as_mut(),
            self.window.as_ref(),
            self.egui_state.as_mut(),
        ) else {
            return;
        };

        // Let egui consume what it wants (panels, buttons). Pass the rest to the canvas.
        let response = egui_state.on_window_event(window.as_ref(), &event);
        let egui_wants_pointer = self.egui_context.egui_wants_pointer_input();
        let egui_wants_keyboard = self.egui_context.egui_wants_keyboard_input();
        if response.repaint {
            window.request_redraw();
        }
        if response.consumed {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m,
            WindowEvent::CursorMoved { position, .. } => {
                self.input.cursor = (position.x as f32, position.y as f32);
                tools::handle_cursor_moved(
                    &mut self.document,
                    renderer,
                    &self.shell,
                    &mut self.input,
                );
                window.request_redraw();
            }
            // Tablet/stylus pressure arrives as a Touch event alongside CursorMoved.
            // winit normalises Force::Normalized to [0, 1]; we store it so stamp calls
            // can scale radius/flow. Mouse strokes fall back to the inspector slider.
            WindowEvent::Touch(touch) => {
                use winit::event::Force;
                if let Some(force) = touch.force {
                    let p = match force {
                        Force::Normalized(v) => v as f32,
                        Force::Calibrated { force, max_possible_force, .. } => {
                            (force / max_possible_force.max(1.0)) as f32
                        }
                    };
                    self.shell.brush_pressure = p.clamp(0.01, 1.0);
                }
                window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } if !egui_wants_pointer => {
                if button == MouseButton::Left {
                    if state == ElementState::Pressed {
                        // Open an undo transaction for scene-mutating tools (gizmo drag,
                        // sculpt, and the Add tools). Paint has its own undo surface.
                        let scene_tool = matches!(
                            self.shell.tool,
                            tools::Tool::Translate
                                | tools::Tool::Sculpt
                                | tools::Tool::AddCube
                                | tools::Tool::AddSphere
                                | tools::Tool::AddImage
                                | tools::Tool::AddMesh
                                | tools::Tool::AddLathe
                                | tools::Tool::AddPipe
                        );
                        if scene_tool {
                            // Inlined begin_canvas_edit — a method call would borrow all of
                            // `self`, conflicting with the live `renderer` borrow; these
                            // touch only disjoint fields.
                            let sel: Vec<suite_doc::ObjId> =
                                self.document.selection().into_iter().collect();
                            self.document.checkpoint(&sel);
                            self.canvas_edit_active = true;
                        }
                        tools::handle_left_press(
                            &mut self.document,
                            renderer,
                            &self.shell,
                            &mut self.input,
                            self.shell.canvas_rect(renderer.size()),
                        );
                        // Eyedropper picked a colour → write it into the brush.
                        if let Some(rgb) = self.input.picked_color.take() {
                            self.shell.brush.color[0] = rgb[0];
                            self.shell.brush.color[1] = rgb[1];
                            self.shell.brush.color[2] = rgb[2];
                        }
                    } else {
                        if self.shell.tool == tools::Tool::Paint {
                            renderer.paint_end_stroke();
                            self.undo_order.push(UndoKind::Paint);
                            self.redo_order.clear();
                            self.shell.dirty = true;
                        }
                        // Gradient: commit the fill from the recorded drag on release.
                        if self.shell.tool == tools::Tool::Gradient {
                            if let Some([u0, v0, u1, v1]) = self.input.gradient_preview {
                                // Skip a zero-length drag (a click without a drag).
                                if (u1 - u0).abs() > 1e-4 || (v1 - v0).abs() > 1e-4 {
                                    let c = self.shell.brush.color; // linear RGBA (foreground)
                                    let color_a = [c[0], c[1], c[2], c[3]];
                                    let color_b = [c[0], c[1], c[2], 0.0]; // → transparent
                                    renderer.paint_gradient_fill(
                                        [u0, v0],
                                        [u1, v1],
                                        color_a,
                                        color_b,
                                        self.shell.gradient_radial,
                                    );
                                    self.undo_order.push(UndoKind::Paint);
                                    self.redo_order.clear();
                                    self.shell.dirty = true;
                                }
                            }
                            self.input.gradient_drag_start = None;
                            self.input.gradient_preview = None;
                            self.shell.gradient_preview = None;
                        }
                        // Inlined end_canvas_edit (disjoint fields; `renderer` is borrowed).
                        if self.canvas_edit_active {
                            let committed = self.document.commit();
                            if committed {
                                self.undo_order.push(UndoKind::Scene);
                                self.redo_order.clear();
                                self.shell.dirty = true;
                            }
                            self.canvas_edit_active = false;
                        }
                        tools::handle_left_release(&mut self.input);
                    }
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key: PhysicalKey::Code(code),
                        repeat: false,
                        ..
                    },
                ..
            } if !egui_wants_keyboard => {
                // Cmd/Ctrl file shortcuts take priority over the single-key bindings
                // (so Cmd+O is Open, not the ortho toggle).
                let cmd =
                    self.modifiers.state().super_key() || self.modifiers.state().control_key();
                let shift = self.modifiers.state().shift_key();
                if cmd {
                    match code {
                        KeyCode::KeyN => {
                            self.shell.pending_file_action = Some(FileAction::New);
                            window.request_redraw();
                            return;
                        }
                        KeyCode::KeyO => {
                            self.shell.pending_file_action = Some(FileAction::Open);
                            window.request_redraw();
                            return;
                        }
                        KeyCode::KeyS => {
                            self.shell.pending_file_action = Some(if shift {
                                FileAction::SaveAs
                            } else {
                                FileAction::Save
                            });
                            window.request_redraw();
                            return;
                        }
                        // Selection: ⌘A = select all, ⌘D = deselect.
                        KeyCode::KeyA => {
                            self.input.select_rect = Some([0.0, 0.0, 1.0, 1.0]);
                            self.shell.selection_rect = self.input.select_rect;
                            if let Some(r) = self.renderer.as_mut() {
                                r.selection_rect = self.input.select_rect;
                            }
                            window.request_redraw();
                            return;
                        }
                        KeyCode::KeyD => {
                            self.input.select_rect = None;
                            self.shell.selection_rect = None;
                            if let Some(r) = self.renderer.as_mut() {
                                r.selection_rect = None;
                            }
                            window.request_redraw();
                            return;
                        }
                        KeyCode::KeyZ => {
                            // Unified undo across scene (command-delta) + paint (GPU
                            // snapshots), in true chronological order via the origin stack.
                            if shift {
                                if let Some(kind) = self.redo_order.pop() {
                                    match kind {
                                        UndoKind::Scene => { self.document.redo(); }
                                        UndoKind::Paint => { renderer.paint_redo(); }
                                    }
                                    self.undo_order.push(kind);
                                }
                            } else if let Some(kind) = self.undo_order.pop() {
                                match kind {
                                    UndoKind::Scene => { self.document.undo(); }
                                    UndoKind::Paint => { renderer.paint_undo(); }
                                }
                                self.redo_order.push(kind);
                            }
                            self.shell.dirty = true;
                            window.request_redraw();
                            return;
                        }
                        _ => {}
                    }
                }
                // Wrap every non-cmd key in one undo transaction. Mutating keys (digit
                // adds, extrude/inset/loop-cut/bevel, delete) record a transaction; camera
                // and tool-switch keys change nothing so `commit` is a no-op.
                let pre_sel: Vec<suite_doc::ObjId> =
                    self.document.selection().into_iter().collect();
                self.document.checkpoint(&pre_sel);
                match code {
                    KeyCode::Digit1 => {
                        self.shell.tool = tools::Tool::Select;
                        window.request_redraw();
                    }
                    KeyCode::Digit2 => {
                        self.shell.tool = tools::Tool::Translate;
                        window.request_redraw();
                    }
                    KeyCode::KeyB => {
                        self.shell.tool = tools::Tool::Paint;
                        window.request_redraw();
                    }
                    KeyCode::KeyM => {
                        self.shell.tool = tools::Tool::RectSelect;
                        window.request_redraw();
                    }
                    KeyCode::KeyS => {
                        self.shell.tool = tools::Tool::Sculpt;
                        window.request_redraw();
                    }
                    KeyCode::KeyW => {
                        self.shell.tool = tools::Tool::MagicWand;
                        window.request_redraw();
                    }
                    KeyCode::Digit3 => {
                        let id = self
                            .document
                            .add(suite_doc::ObjectKind::Cube, spawn_point(renderer));
                        self.document.set_selection(Some(id));
                        self.shell.dirty = true;
                        window.request_redraw();
                    }
                    KeyCode::Digit4 => {
                        let id = self
                            .document
                            .add(suite_doc::ObjectKind::Sphere, spawn_point(renderer));
                        self.document.set_selection(Some(id));
                        self.shell.dirty = true;
                        window.request_redraw();
                    }
                    KeyCode::Digit5 => {
                        let id = self
                            .document
                            .add(suite_doc::ObjectKind::ImagePlane, spawn_point(renderer));
                        self.document.set_selection(Some(id));
                        self.shell.dirty = true;
                        window.request_redraw();
                    }
                    KeyCode::Digit6 => {
                        let id = self
                            .document
                            .add(suite_doc::ObjectKind::Mesh, spawn_point(renderer));
                        self.document.set_selection(Some(id));
                        self.shell.dirty = true;
                        window.request_redraw();
                    }
                    KeyCode::Digit7 => {
                        // Add a lathe (vase) at the current camera focus point.
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
                        let id = self.document.add_lathe(profile, 32, spawn_point(renderer));
                        self.document.set_selection(Some(id));
                        self.shell.dirty = true;
                        window.request_redraw();
                    }
                    KeyCode::Digit8 => {
                        // Add a path-extruded helix.
                        let spawn = spawn_point(renderer);
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
                        let id = self.document.add_pipe(&path, shape, spawn);
                        self.document.set_selection(Some(id));
                        self.shell.dirty = true;
                        window.request_redraw();
                    }
                    KeyCode::KeyE => {
                        // Extrude the selected mesh's focused face.
                        if self.document.extrude_selected_mesh(0.5) {
                            self.shell.dirty = true;
                            window.request_redraw();
                        }
                    }
                    KeyCode::KeyI => {
                        // Inset the selected mesh's focused face.
                        if self.document.inset_selected_mesh(0.3) {
                            self.shell.dirty = true;
                            window.request_redraw();
                        }
                    }
                    KeyCode::KeyC => {
                        // Loop-cut the selected mesh around the focused face's edge.
                        if self.document.loop_cut_selected_mesh() {
                            self.shell.dirty = true;
                            window.request_redraw();
                        }
                    }
                    KeyCode::KeyV => {
                        // Bevel (chamfer) a corner of the selected mesh's focused face.
                        if self.document.bevel_selected_mesh_corner() {
                            self.shell.dirty = true;
                            window.request_redraw();
                        }
                    }
                    KeyCode::KeyG => {
                        // Context-sensitive: with a focused mesh edge, bevel it (modeling).
                        // Otherwise G selects the 2D Gradient tool. The "Grd" tool-strip
                        // button is the unambiguous path when a mesh is also selected.
                        if self.document.bevel_selected_mesh_edge() {
                            self.shell.dirty = true;
                        } else {
                            self.shell.tool = tools::Tool::Gradient;
                        }
                        window.request_redraw();
                    }
                    KeyCode::KeyK => {
                        // Set a keyframe for the selected object at the current playhead time.
                        if let Some(sel_id) = self.document.selection() {
                            if let Some(obj) = self.document.get(sel_id) {
                                let key = obj_key(sel_id);
                                let pos = obj.transform.translation;
                                let rot = obj.transform.rotation;
                                let scale = obj.transform.scale;
                                self.timeline.set_keyframe_trs(
                                    key,
                                    [pos.x, pos.y, pos.z],
                                    [rot.x, rot.y, rot.z],
                                    [scale.x, scale.y, scale.z],
                                );
                                window.request_redraw();
                            }
                        }
                    }
                    KeyCode::KeyO => {
                        renderer.camera.projection = renderer.camera.projection.toggle();
                        window.request_redraw();
                    }
                    KeyCode::ArrowLeft => {
                        renderer.camera.yaw_radians -= 0.1;
                        window.request_redraw();
                    }
                    KeyCode::ArrowRight => {
                        renderer.camera.yaw_radians += 0.1;
                        window.request_redraw();
                    }
                    KeyCode::ArrowUp => {
                        renderer.camera.pitch_radians =
                            (renderer.camera.pitch_radians + 0.1).clamp(-1.4, 1.4);
                        window.request_redraw();
                    }
                    KeyCode::ArrowDown => {
                        renderer.camera.pitch_radians =
                            (renderer.camera.pitch_radians - 0.1).clamp(-1.4, 1.4);
                        window.request_redraw();
                    }
                    KeyCode::Equal | KeyCode::NumpadAdd => {
                        renderer.camera.distance = (renderer.camera.distance * 0.9).max(1.5);
                        renderer.camera.ortho_height =
                            (renderer.camera.ortho_height * 0.9).max(1.0);
                        window.request_redraw();
                    }
                    KeyCode::Minus | KeyCode::NumpadSubtract => {
                        renderer.camera.distance = (renderer.camera.distance * 1.1).min(40.0);
                        renderer.camera.ortho_height =
                            (renderer.camera.ortho_height * 1.1).min(30.0);
                        window.request_redraw();
                    }
                    KeyCode::Delete | KeyCode::Backspace => {
                        if let Some(sel) = self.document.selection() {
                            self.document.remove(sel);
                            self.shell.dirty = true;
                            window.request_redraw();
                        }
                    }
                    KeyCode::Escape => event_loop.exit(),
                    _ => {}
                }
                let committed = self.document.commit();
                Self::record_scene(&mut self.undo_order, &mut self.redo_order, &mut self.shell.dirty, committed);
            }
            WindowEvent::RedrawRequested => {
                // Advance the animation playhead and apply sampled transforms.
                let now = std::time::Instant::now();
                let dt = now.duration_since(self.last_frame_time).as_secs_f32();
                self.last_frame_time = now;
                self.timeline.playhead.advance(dt.min(0.1)); // cap at 100ms to avoid spiral
                if self.timeline.playhead.playing {
                    apply_timeline_samples(&self.timeline, &mut self.document);
                    window.request_redraw();
                }

                // Capture the selected object's pre-edit state so an inspector edit burst
                // can be recorded as one coalesced undo transaction when it settles.
                if !self.edit_pending {
                    self.frame_baseline = self
                        .document
                        .selection()
                        .and_then(|id| self.document.get(id))
                        .cloned();
                }

                // Sync the layer stack into the shell so the Layers panel can show it.
                self.shell.layer_infos = renderer.layer_infos();
                self.shell.active_layer = renderer.active_layer();
                // Sync the active selection rectangle: input → shell (for UI/overlay) →
                // renderer (for GPU scissor on brush stamps).
                self.shell.selection_rect = self.input.select_rect;
                renderer.selection_rect = self.shell.selection_rect;
                // Gradient guide line: input → shell so the overlay can draw the drag.
                self.shell.gradient_preview = self.input.gradient_preview;

                let raw_input = egui_state.take_egui_input(window.as_ref());
                let full_output = self.egui_context.run_ui(raw_input, |ui| {
                    shell::draw_shell(
                        ui,
                        &mut self.shell,
                        &mut self.document,
                        &renderer.budget,
                        &mut self.timeline,
                    );
                });
                egui_state.handle_platform_output(window.as_ref(), full_output.platform_output);

                // Sync selection changes from the shell UI (inspector buttons) back to
                // input state and the renderer. draw_shell may have changed selection_rect
                // (e.g. "Select All" / "Deselect" buttons), so write that back here.
                if self.shell.selection_rect != self.input.select_rect {
                    self.input.select_rect = self.shell.selection_rect;
                    renderer.selection_rect = self.shell.selection_rect;
                }

                // Apply a Layers-panel command issued this frame.
                if let Some(cmd) = self.shell.pending_layer_cmd.take() {
                    match cmd {
                        shell::LayerCmd::Add => renderer.add_layer(),
                        shell::LayerCmd::Delete(i) => renderer.delete_layer(i),
                        shell::LayerCmd::SetActive(i) => renderer.set_active_layer(i),
                        shell::LayerCmd::SetVisible(i, v) => renderer.set_layer_visible(i, v),
                        shell::LayerCmd::SetOpacity(i, o) => renderer.set_layer_opacity(i, o),
                        shell::LayerCmd::SetBlend(i, b) => renderer.set_layer_blend(i, b),
                        shell::LayerCmd::Move(i, up) => renderer.move_layer(i, up),
                    }
                    self.shell.dirty = true;
                    window.request_redraw();
                }

                // Apply a layer transform (M4 flip/180°) issued by an inspector button.
                if let Some(op) = self.shell.pending_layer_transform.take() {
                    renderer.transform_active_layer(op);
                    self.undo_order.push(UndoKind::Paint);
                    self.redo_order.clear();
                    self.shell.dirty = true;
                    window.request_redraw();
                }

                // Apply a whole-canvas 90° rotate (M5) issued by an inspector button. Not
                // undoable — it's a structural resize like opening a project, not a stroke;
                // clear the undo/redo queues so a later ⌘Z doesn't hit a stale no-op entry.
                if let Some(dir) = self.shell.pending_canvas_rotate.take() {
                    renderer.rotate_canvas_90(dir);
                    let (w, h) = (renderer.canvas_width(), renderer.canvas_height());
                    Self::rescale_paint_canvases_to_aspect(&mut self.document, w, h);
                    self.undo_order.clear();
                    self.redo_order.clear();
                    self.shell.dirty = true;
                    window.request_redraw();
                }

                // Apply a "Crop to Selection" (M4) issued by the RectSelect inspector. Not
                // undoable — a structural resize, same posture as rotate/import. The old
                // selection rect no longer means anything at the new dims, so clear it.
                if let Some([x0, y0, x1, y1]) = self.shell.pending_crop.take() {
                    renderer.crop_to_rect(x0, y0, x1, y1);
                    let (w, h) = (renderer.canvas_width(), renderer.canvas_height());
                    Self::rescale_paint_canvases_to_aspect(&mut self.document, w, h);
                    self.undo_order.clear();
                    self.redo_order.clear();
                    self.shell.selection_rect = None;
                    self.input.select_rect = None;
                    renderer.selection_rect = None;
                    self.shell.dirty = true;
                    window.request_redraw();
                }

                // Inspector undo: an edit this frame opens/extends a burst; a frame with no
                // edit closes it, recording one coalesced transaction.
                if self.shell.edited_object {
                    if !self.edit_pending {
                        self.edit_before = self.frame_baseline.take();
                        self.edit_pending = true;
                    }
                } else if self.edit_pending {
                    if let Some(before) = self.edit_before.take() {
                        let committed = self.document.record_object_change(before);
                        // Inlined note_scene_edit (disjoint fields; `renderer`/`egui_state`
                        // are borrowed in this scope).
                        if committed {
                            self.undo_order.push(UndoKind::Scene);
                            self.redo_order.clear();
                            self.shell.dirty = true;
                        }
                    }
                    self.edit_pending = false;
                }

                let pixels_per_point = self.egui_context.pixels_per_point();
                let paint_jobs = self
                    .egui_context
                    .tessellate(full_output.shapes, pixels_per_point);
                let textures_delta = full_output.textures_delta;

                let egui_renderer = self.egui_renderer.as_mut().unwrap();

                // Apply egui texture deltas (the font atlas, etc.) NOW, unconditionally —
                // *before* the scene render, which may early-return `Skipped` while the
                // surface is warming up. egui only emits the full font-atlas allocation
                // once; if that frame's present is skipped and we applied deltas only
                // inside the (skipped) paint callback, the atlas would be lost forever and
                // every panel would silently fail to draw (it samples the atlas). Applying
                // here guarantees the atlas is uploaded regardless of present success.
                for (id, image_delta) in &textures_delta.set {
                    egui_renderer.update_texture(
                        renderer.device(),
                        renderer.queue(),
                        *id,
                        image_delta,
                    );
                }
                let free_after = textures_delta.free;

                let mut egui_paint = move |encoder: &mut wgpu::CommandEncoder,
                                           view: &wgpu::TextureView,
                                           device: &wgpu::Device,
                                           queue: &wgpu::Queue,
                                           size: (u32, u32)| {
                    let screen_descriptor = egui_wgpu::ScreenDescriptor {
                        size_in_pixels: [size.0, size.1],
                        pixels_per_point,
                    };
                    egui_renderer.update_buffers(
                        device,
                        queue,
                        encoder,
                        &paint_jobs,
                        &screen_descriptor,
                    );
                    let mut rpass = encoder
                        .begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("egui pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view,
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
                        })
                        .forget_lifetime();
                    egui_renderer.render(&mut rpass, &paint_jobs, &screen_descriptor);
                    drop(rpass);
                    for id in &free_after {
                        egui_renderer.free_texture(id);
                    }
                };

                // Sync snap indicator to the renderer.
                renderer.snap_indicator = self.input.snap_indicator.map(|v| [v.x, v.y, v.z]);

                // Sync the selected object's skeleton (if any) into world-space bone
                // segments for the overlay. Bones live in object-local space → multiply
                // by the object's world transform.
                renderer.skeleton_segments.clear();
                if let Some(obj) = self.document.selection().and_then(|id| self.document.get(id)) {
                    if let Some(skel) = &obj.skeleton {
                        let world = obj.transform.matrix();
                        // Posed bone positions: each bone's rest head/tail pushed through its
                        // skinning matrix (identity when unposed → shows the rest skeleton).
                        let mats = skel.skinning_matrices();
                        for (i, bone) in skel.bones.iter().enumerate() {
                            let h = world.transform_point3(mats[i].transform_point3(bone.head));
                            let t = world.transform_point3(mats[i].transform_point3(bone.tail));
                            renderer.skeleton_segments.push((h.into(), t.into()));
                        }
                    }
                }

                let gizmo =
                    tools::gizmo_for_render(&self.document, renderer, &self.shell, &self.input);
                match renderer.render(&self.document, gizmo, Some(&mut egui_paint)) {
                    suite_gpu::RenderResult::Presented | suite_gpu::RenderResult::Skipped => {}
                    suite_gpu::RenderResult::SurfaceLostOrOutdated => {
                        let size = window.inner_size();
                        renderer.resize(size.width, size.height);
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Pack ObjId into u64 for the timeline track map.
fn obj_key(id: ObjId) -> u64 {
    (id.slot as u64) << 32 | id.generation as u64
}

/// Apply sampled transforms from the timeline to all animated objects in the document.
fn apply_timeline_samples(timeline: &Timeline, doc: &mut Document) {
    let samples = timeline.sample_all();
    let ids: Vec<_> = doc.objects().map(|o| o.id).collect();
    for (serial, s) in &samples {
        // Find the object whose packed key matches this serial.
        for &id in &ids {
            if obj_key(id) != *serial {
                continue;
            }
            if let Some(obj) = doc.get_mut(id) {
                if let Some(v) = s.tx { obj.transform.translation.x = v; }
                if let Some(v) = s.ty { obj.transform.translation.y = v; }
                if let Some(v) = s.tz { obj.transform.translation.z = v; }
                if let Some(v) = s.rx { obj.transform.rotation.x = v; }
                if let Some(v) = s.ry { obj.transform.rotation.y = v; }
                if let Some(v) = s.rz { obj.transform.rotation.z = v; }
                if let Some(v) = s.sx { obj.transform.scale.x = v; }
                if let Some(v) = s.sy { obj.transform.scale.y = v; }
                if let Some(v) = s.sz { obj.transform.scale.z = v; }
            }
            break;
        }
    }
    // Apply bone pose samples (rig animation).
    for (serial, bone, euler) in timeline.sample_bones() {
        for &id in &ids {
            if obj_key(id) != serial {
                continue;
            }
            let q = glam::Quat::from_euler(
                glam::EulerRot::XYZ,
                euler[0].to_radians(), euler[1].to_radians(), euler[2].to_radians(),
            );
            doc.set_bone_pose(id, bone as usize, q);
            break;
        }
    }
}

fn spawn_point(renderer: &Renderer) -> glam::Vec3 {
    // Spawn at the camera target so new objects appear under the user's gaze.
    renderer.camera.target
}

fn main() {
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop");
}
