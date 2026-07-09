//! egui shell — top bar, left tool strip, right inspector, bottom timeline stub.
//!
//! Colors come from `design-tokens/tokens.toml` (or rather: their linearized values
//! transcribed here as `egui::Color32` since we don't have a token loader yet — the
//! token loader is a `platform/design` task once we have two consumers).

use egui::{
    Color32, FontFamily, FontId, Frame, Margin, Panel, RichText, Stroke, TextStyle, Theme, Ui,
};
use std::path::PathBuf;

use suite_doc::{Document, ObjId, ObjectKind};

use crate::tools::{Tool, TransformMode};

/// A file-menu intent set by a top-bar button or a shortcut. The shell only *records*
/// the intent; `main.rs` drains it after the egui frame and runs the (blocking) native
/// dialog outside the paint closure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    New,
    Open,
    Save,
    SaveAs,
    ImportImage,
    ExportPng,
    LoadFont,
}

/// A command from the Layers panel; main.rs applies it to the renderer's layer stack.
#[derive(Clone, Copy, Debug)]
pub enum LayerCmd {
    Add,
    Delete(usize),
    SetActive(usize),
    SetVisible(usize, bool),
    SetOpacity(usize, f32),
    SetBlend(usize, suite_doc::BlendMode),
    Move(usize, bool),
    /// S3: add/replace a mask on layer `i` from the current selection.
    MaskFromSelection(usize),
    /// S3: add an empty (white, reveal-all) mask on layer `i` to paint holes into.
    AddEmptyMask(usize),
    /// S3: remove layer `i`'s mask.
    ClearMask(usize),
}

pub struct ShellState {
    pub tool: Tool,
    pub status: String,
    pub left_strip_w: f32,
    pub right_panel_w: f32,
    pub bottom_strip_h: f32,
    pub top_bar_h: f32,
    /// The canvas viewport in *physical* pixels, captured directly from
    /// `ui.available_rect_before_wrap()` (× `pixels_per_point`) at the one point in the frame
    /// that value is authoritative — right before the canvas overlay is drawn, i.e. after
    /// every panel (top/left/right/*both* bottom panels) has already claimed its space. This
    /// is what `canvas_rect()` returns; see its doc comment for why summing individual panel
    /// dimensions (the older approach, kept below only for the ones still measured for other
    /// UI purposes) is fragile — it silently drifts out of sync if a panel is added without
    /// remembering to fold its height in.
    pub canvas_rect_physical: (f32, f32, f32, f32),
    /// egui's `pixels_per_point` (the display's HiDPI scale factor, e.g. 2.0 on a Retina
    /// Mac) — `left_strip_w`/`right_panel_w`/etc. above are egui layout rects, always in
    /// *logical points*, but `canvas_rect()` converts them against `renderer.size()`, which
    /// is the wgpu surface's *physical* pixel size. This field is what makes that conversion
    /// correct; see `canvas_rect()`.
    pub ui_scale: f32,
    pub current_path: Option<PathBuf>,
    pub dirty: bool,
    pub pending_file_action: Option<FileAction>,
    pub brush: suite_gpu::Brush,
    /// Simulated pressure for mouse input (0..1). Real pressure comes from tablet events.
    pub brush_pressure: f32,
    /// Stabilization window for the cursor (1 = no smoothing, higher = smoother but laggier).
    pub brush_stabilize: usize,
    /// Set by the inspector "Clear Canvas" button; main.rs drains it into the renderer.
    pub clear_canvas_requested: bool,
    /// Set by the "+ Adjustment Layer" button; main.rs drains it into the document.
    pub pending_add_adjustment: bool,
    /// CSG boolean to apply: (tool_id, op). Drained by main.rs each frame.
    pub pending_boolean: Option<(suite_doc::ObjId, u8)>,
    /// Currently picked CSG tool mesh (for the combobox state).
    pub csg_tool_id: Option<suite_doc::ObjId>,
    /// Sculpt brush settings.
    pub sculpt_op: u8,       // 0=Draw, 1=Smooth, 2=Flatten, 3=Pinch
    pub sculpt_radius: f32,
    pub sculpt_strength: f32,
    /// Magic Wand tolerance (0–1 per channel).
    pub magic_wand_tolerance: f32,
    /// Magic Wand: contiguous (flood fill) vs global colour range ("Select by Color").
    pub magic_wand_contiguous: bool,
    /// Tier 1: selection feather radius in canvas texels (0 = hard edge). Synced to
    /// `Renderer::selection_feather`; softens the selection's edge for Paint/Gradient/Move/Text.
    pub selection_feather: f32,
    /// S3: when true, the brush paints the active layer's mask (black hides, white reveals)
    /// instead of its colour. Synced to `Renderer::mask_edit`. Only meaningful when the active
    /// layer has a mask (the renderer falls back to colour otherwise).
    pub mask_edit: bool,
    /// Set when the user clicks "→ 3D Heightmap" on a paint canvas. main.rs drains this.
    pub pending_heightmap: bool,
    /// Heightmap settings.
    pub heightmap_resolution: u32,
    pub heightmap_scale: f32,
    /// Rigging: which bone of the selected skeleton is being posed, and its Euler pose
    /// (degrees, XYZ) as edited in the inspector. main.rs applies this to the bone.
    pub active_bone: usize,
    pub bone_pose_deg: [f32; 3],
    /// Set when the user clicks "Key Bone Pose" — main.rs keys the active bone at the playhead.
    pub pending_key_bone: bool,
    /// Transient (per-frame): an inspector control mutated the selected object this frame.
    /// main.rs reads this to drive command-delta undo, then it is reset next frame.
    pub edited_object: bool,
    /// Symmetry painting (Krita-referenced): mirror brush stamps across the canvas centre.
    pub mirror_x: bool,
    pub mirror_y: bool,
    /// Wrap-around / seamless-texture painting: stamps that cross an edge reappear on the
    /// opposite edge (Krita-referenced).
    pub wrap_tiling: bool,
    /// 2D layer stack mirror (synced from the renderer each frame) + the active index, for
    /// the Layers panel. `pending_layer_cmd` is drained by main.rs into renderer calls.
    pub layer_infos: Vec<suite_gpu::LayerInfo>,
    pub active_layer: usize,
    pub pending_layer_cmd: Option<LayerCmd>,
    /// Active selection rectangle in UV space `[x0, y0, x1, y1]` (0..1, top-left origin).
    /// Synced from `InputState::select_rect` before each frame; drives the overlay + the
    /// GPU scissor (via `Renderer::selection_rect`).
    pub selection_rect: Option<[f32; 4]>,
    /// Tier 1: the selection's exact shape when it isn't a plain rectangle (Ellipse/Lasso).
    /// Synced from `InputState::select_extra` before each frame; drives the overlay outline
    /// and (via `Renderer::selection_extra`) exact-shape masking in Gradient/Move.
    pub selection_extra: Option<suite_gpu::SelectionShape>,
    /// Gradient tool: radial when true, linear when false.
    pub gradient_radial: bool,
    /// Live gradient guide endpoints `[u0, v0, u1, v1]` (UV), synced from `InputState`
    /// before each frame so the overlay can draw the drag line.
    pub gradient_preview: Option<[f32; 4]>,
    /// Live Move-tool guide endpoints `[u0, v0, u1, v1]` (UV), synced from `InputState`
    /// before each frame so the overlay can draw the drag line.
    pub move_preview: Option<[f32; 4]>,
    /// Free Transform (M4c): which handle is being dragged, synced from `InputState` before
    /// each frame — the overlay draws the box + handles whenever the tool is active, and the
    /// live transformed-box preview additionally whenever this is `Some`.
    pub transform_mode: Option<TransformMode>,
    /// Live scale factor / rotation delta (radians) for the Free Transform overlay preview,
    /// synced from `InputState` before each frame. Meaningless while `transform_mode` is
    /// `None` (not read in that case).
    pub transform_scale: f32,
    pub transform_rotation: f32,
    /// Text tool (M4a): baseline anchor, synced from `InputState` before each frame (same
    /// one-way sync as the Free Transform fields above — the overlay/inspector only read it).
    pub text_anchor: Option<[f32; 2]>,
    /// The string being composed in the inspector's text field.
    pub text_input: String,
    /// Point size in canvas pixels (not font units — this is what the user actually sees).
    pub text_size: f32,
    /// The currently loaded font, if any. `Rc` (not owned per-clone) since a `Font` holds the
    /// whole file's bytes (hundreds of KB to a few MB) and cloning it on every sync would be
    /// wasteful; shared read-only access is all any of Load/Apply/inspector-display need.
    pub loaded_font: Option<std::rc::Rc<suite_gpu::font::Font>>,
    /// Filename of the loaded font (display only), or a parse-error message — whichever the
    /// last "Load Font" attempt produced.
    pub text_font_status: Option<String>,
    /// Set by the inspector's "Apply Text" button; drained in main.rs (needs `&mut Renderer`,
    /// which `draw_shell` doesn't have) — same pattern as `pending_crop`/`pending_layer_transform`.
    pub pending_apply_text: bool,
    /// Set by the inspector flip/180° buttons; drained in main.rs to call the renderer.
    /// Always dimension-preserving — safe regardless of canvas aspect ratio (M5).
    pub pending_layer_transform: Option<suite_gpu::LayerTransform>,
    /// Set by the inspector's "Rotate Canvas" buttons; drained in main.rs. Rotates the
    /// WHOLE document (every layer + the canvas dimensions swap) — a 90° rotation can't be
    /// done per-layer once the canvas may be non-square (M5).
    pub pending_canvas_rotate: Option<suite_gpu::CanvasRotate>,
    /// Set by the "Crop to Selection" button; drained in main.rs. UV-space rect
    /// `[x0, y0, x1, y1]` to crop the whole document (every layer) down to (M4).
    pub pending_crop: Option<[f32; 4]>,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            tool: Tool::default(),
            status: String::new(),
            left_strip_w: 0.0,
            right_panel_w: 0.0,
            bottom_strip_h: 0.0,
            top_bar_h: 0.0,
            canvas_rect_physical: (0.0, 0.0, 1.0, 1.0),
            ui_scale: 1.0,
            current_path: None,
            dirty: false,
            pending_file_action: None,
            brush: suite_gpu::Brush::default(),
            brush_pressure: 1.0,
            brush_stabilize: 1,
            clear_canvas_requested: false,
            pending_add_adjustment: false,
            pending_boolean: None,
            csg_tool_id: None,
            sculpt_op: 0,
            sculpt_radius: 0.5,
            sculpt_strength: 0.05,
            magic_wand_tolerance: 0.1,
            magic_wand_contiguous: true,
            selection_feather: 0.0,
            mask_edit: false,
            pending_heightmap: false,
            heightmap_resolution: 64,
            heightmap_scale: 0.5,
            active_bone: 0,
            bone_pose_deg: [0.0; 3],
            pending_key_bone: false,
            edited_object: false,
            mirror_x: false,
            mirror_y: false,
            wrap_tiling: false,
            layer_infos: Vec::new(),
            active_layer: 0,
            pending_layer_cmd: None,
            selection_rect: None,
            selection_extra: None,
            gradient_radial: false,
            gradient_preview: None,
            move_preview: None,
            transform_mode: None,
            transform_scale: 1.0,
            transform_rotation: 0.0,
            text_anchor: None,
            text_input: String::new(),
            text_size: 48.0,
            loaded_font: None,
            text_font_status: None,
            pending_apply_text: false,
            pending_layer_transform: None,
            pending_canvas_rotate: None,
            pending_crop: None,
        }
    }
}

impl ShellState {
    /// The canvas rectangle in *physical* pixels (the area between the panels).
    /// The tool layer uses this to project cursor coords into the world ray.
    ///
    /// Returns `canvas_rect_physical`, captured once per frame directly from
    /// `ui.available_rect_before_wrap()` — the authoritative "what's left after every panel
    /// claimed its space" value egui itself computes — converted to physical pixels via
    /// `ui_scale`. `winit::CursorMoved` (what `input.cursor` holds) is physical too, so this
    /// keeps hit-testing in the same coordinate space as the actual click.
    ///
    /// Deliberately does *not* reconstruct the rect by summing `left_strip_w`/`right_panel_w`/
    /// `top_bar_h`/`bottom_strip_h` (an earlier version of this function did): that approach
    /// silently drifted out of sync when a second bottom panel (`timeline_bar`) was added
    /// later without remembering to fold its height into `bottom_strip_h` — precise targets
    /// like Free Transform's corner handles missed by tens of pixels while looser gestures
    /// (Marquee) still looked "close enough" at a glance. `framebuffer` is unused now except
    /// as the pre-first-frame fallback (before `canvas_rect_physical` has ever been set).
    pub fn canvas_rect(&self, framebuffer: (u32, u32)) -> (f32, f32, f32, f32) {
        if self.canvas_rect_physical.2 > self.canvas_rect_physical.0 + 1.0 {
            return self.canvas_rect_physical;
        }
        // Fallback for the very first frame(s), before `draw_shell` has run once.
        let scale = self.ui_scale.max(1.0);
        let l = self.left_strip_w * scale;
        let t = self.top_bar_h * scale;
        let r = (framebuffer.0 as f32) - self.right_panel_w * scale;
        let b = (framebuffer.1 as f32) - self.bottom_strip_h * scale;
        (l, t, r.max(l + 1.0), b.max(t + 1.0))
    }
}

// --- design tokens (in sRGB byte space) ----------------------------------------------
const BG_0: Color32 = Color32::from_rgb(0x0E, 0x0F, 0x11);
const BG_1: Color32 = Color32::from_rgb(0x16, 0x18, 0x1B);
const BG_2: Color32 = Color32::from_rgb(0x1E, 0x21, 0x25);
const BG_3: Color32 = Color32::from_rgb(0x28, 0x2C, 0x31);
const LINE: Color32 = Color32::from_rgb(0x34, 0x39, 0x3F);
const TEXT_0: Color32 = Color32::from_rgb(0xE8, 0xEA, 0xED);
const TEXT_1: Color32 = Color32::from_rgb(0xA8, 0xAE, 0xB6);
const TEXT_2: Color32 = Color32::from_rgb(0x6B, 0x71, 0x78);
const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
const ACCENT_HOVER: Color32 = Color32::from_rgb(0x5B, 0x9C, 0xF8);
const ACCENT_PRESS: Color32 = Color32::from_rgb(0x2D, 0x6F, 0xE0);

pub fn apply_design_tokens(ctx: &egui::Context) {
    ctx.set_theme(Theme::Dark);
    let mut style = (*ctx.style_of(Theme::Dark)).clone();
    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = BG_0;
    v.window_fill = BG_1;
    v.extreme_bg_color = BG_0;
    v.faint_bg_color = BG_1;
    v.code_bg_color = BG_2;
    v.window_stroke = Stroke {
        width: 1.0,
        color: LINE,
    };
    v.menu_corner_radius = 5.0.into();
    v.widgets.noninteractive.bg_fill = BG_1;
    v.widgets.noninteractive.bg_stroke = Stroke {
        width: 1.0,
        color: LINE,
    };
    v.widgets.noninteractive.fg_stroke = Stroke {
        width: 1.0,
        color: TEXT_1,
    };
    v.widgets.inactive.bg_fill = BG_2;
    v.widgets.inactive.weak_bg_fill = BG_2;
    v.widgets.inactive.bg_stroke = Stroke {
        width: 1.0,
        color: LINE,
    };
    v.widgets.inactive.fg_stroke = Stroke {
        width: 1.0,
        color: TEXT_0,
    };
    v.widgets.hovered.bg_fill = BG_3;
    v.widgets.hovered.weak_bg_fill = BG_3;
    v.widgets.hovered.bg_stroke = Stroke {
        width: 1.0,
        color: ACCENT,
    };
    v.widgets.hovered.fg_stroke = Stroke {
        width: 1.0,
        color: TEXT_0,
    };
    v.widgets.active.bg_fill = ACCENT_PRESS;
    v.widgets.active.weak_bg_fill = ACCENT_PRESS;
    v.widgets.active.bg_stroke = Stroke {
        width: 1.0,
        color: ACCENT_HOVER,
    };
    v.widgets.active.fg_stroke = Stroke {
        width: 1.0,
        color: TEXT_0,
    };
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke {
        width: 1.0,
        color: TEXT_0,
    };
    v.hyperlink_color = ACCENT_HOVER;

    let mut text_styles = std::collections::BTreeMap::new();
    text_styles.insert(
        TextStyle::Small,
        FontId::new(11.0, FontFamily::Proportional),
    );
    text_styles.insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
    text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Heading,
        FontId::new(19.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.0, FontFamily::Monospace),
    );
    style.text_styles = text_styles;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style_of(Theme::Dark, style);
}

pub fn draw_shell(
    ui: &mut Ui,
    state: &mut ShellState,
    doc: &mut Document,
    budget: &suite_gpu::FrameBudget,
    timeline: &mut suite_timeline::Timeline,
) {
    // Reset the per-frame "an inspector edit happened" flag; set below when any inspector
    // control mutates the document. main.rs reads it to drive command-delta undo.
    state.edited_object = false;
    // `canvas_rect()` needs this to correctly convert its logical-point panel measurements
    // against `renderer.size()`'s physical pixels — see the field doc comment.
    state.ui_scale = ui.ctx().pixels_per_point();
    // --- Top bar -------------------------------------------------------------
    let top = Panel::top("top_bar")
        .resizable(false)
        .frame(
            Frame::default()
                .fill(BG_0)
                .stroke(Stroke {
                    width: 1.0,
                    color: LINE,
                })
                .inner_margin(Margin::symmetric(12, 6)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SWEET · Visual").color(TEXT_0).strong());
                ui.add_space(12.0);
                // File actions — record the intent; main.rs runs the dialog after the frame.
                if ui.button(RichText::new("New").color(TEXT_0)).clicked() {
                    state.pending_file_action = Some(FileAction::New);
                }
                if ui
                    .button(RichText::new("Open").color(TEXT_0))
                    .on_hover_text("⌘O")
                    .clicked()
                {
                    state.pending_file_action = Some(FileAction::Open);
                }
                if ui
                    .button(RichText::new("Save").color(TEXT_0))
                    .on_hover_text("⌘S")
                    .clicked()
                {
                    state.pending_file_action = Some(FileAction::Save);
                }
                if ui.button(RichText::new("Save As").color(TEXT_0)).clicked() {
                    state.pending_file_action = Some(FileAction::SaveAs);
                }
                ui.add_space(8.0);
                if ui
                    .button(RichText::new("Import Image").color(TEXT_0))
                    .on_hover_text("Open a PNG/JPG/… onto the paint canvas")
                    .clicked()
                {
                    state.pending_file_action = Some(FileAction::ImportImage);
                }
                if ui
                    .button(RichText::new("Export PNG").color(TEXT_0))
                    .on_hover_text("Write the paint canvas to a .png file")
                    .clicked()
                {
                    state.pending_file_action = Some(FileAction::ExportPng);
                }
                ui.add_space(12.0);
                // Document title: file name + dirty marker.
                let title = match &state.current_path {
                    Some(p) => p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("project")
                        .to_string(),
                    None => "untitled".to_string(),
                };
                let dirty_mark = if state.dirty { " •" } else { "" };
                ui.label(RichText::new(format!("{title}{dirty_mark}")).color(TEXT_1));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if budget.last_frame_ms > 0.0 {
                        format!(
                            "{:>5.2} ms CPU+submit · peak {:>5.2} ms · {} frames",
                            budget.last_frame_ms, budget.peak_frame_ms, budget.frames
                        )
                    } else {
                        "warming up…".to_string()
                    };
                    let color = if budget.last_frame_ms > suite_gpu::FRAME_BUDGET_MS {
                        ACCENT_HOVER
                    } else {
                        TEXT_2
                    };
                    ui.label(RichText::new(label).color(color).monospace());
                });
            });
        });
    state.top_bar_h = top.response.rect.height();

    // --- Bottom strip (timeline placeholder) ---------------------------------
    let bottom = Panel::bottom("timeline_strip")
        .resizable(false)
        .frame(Frame::default().fill(BG_1).stroke(Stroke { width: 1.0, color: LINE }).inner_margin(Margin::same(12)))
        .show(ui, |ui| {
            ui.label(RichText::new("Timeline").color(TEXT_1));
            ui.add_space(4.0);
            ui.label(RichText::new("Animation tracks land in Phase 8. The shared timeline engine lives in platform/timeline.").color(TEXT_2));
        });
    state.bottom_strip_h = bottom.response.rect.height();

    // --- Left tool strip -----------------------------------------------------
    let left = Panel::left("tool_strip")
        .resizable(false)
        .frame(
            Frame::default()
                .fill(BG_0)
                .stroke(Stroke {
                    width: 1.0,
                    color: LINE,
                })
                .inner_margin(Margin::symmetric(4, 8)),
        )
        .show(ui, |ui| {
            ui.vertical_centered_justified(|ui| {
                for (tool, label, hotkey) in [
                    (Tool::Select, "Sel", "1"),
                    (Tool::Translate, "Mov", "2"),
                    (Tool::Paint, "Pnt", "B"),
                    (Tool::RectSelect, "Mrq", "M"),
                    (Tool::EllipseSelect, "Elps", "·"),
                    (Tool::Lasso, "Lso", "L"),
                    (Tool::Gradient, "Grd", "G"),
                    (Tool::MoveLayer, "MovL", "V"),
                    (Tool::FreeTransform, "Xfrm", "T"),
                    (Tool::Text, "Txt", "·"),
                    (Tool::AddCube, "Cub", "3"),
                    (Tool::AddSphere, "Sph", "4"),
                    (Tool::AddImage, "Img", "5"),
                    (Tool::AddMesh, "Msh", "6"),
                    (Tool::AddLathe, "Lth", "7"),
                    (Tool::AddPipe, "Pip", "8"),
                    (Tool::Sculpt, "Sct", "S"),
                    (Tool::MagicWand, "Wnd", "W"),
                    (Tool::Eyedropper, "Eye", "·"),
                ] {
                    let selected = state.tool == tool;
                    let bg = if selected { ACCENT_PRESS } else { BG_2 };
                    let stroke = if selected {
                        Stroke {
                            width: 1.0,
                            color: ACCENT_HOVER,
                        }
                    } else {
                        Stroke {
                            width: 1.0,
                            color: LINE,
                        }
                    };
                    let resp = ui
                        .add_sized(
                            [40.0, 40.0],
                            egui::Button::new(RichText::new(label).color(TEXT_0).monospace())
                                .corner_radius(5.0)
                                .fill(bg)
                                .stroke(stroke),
                        )
                        .on_hover_text(format!("{} ({})", tool.label(), hotkey));
                    if resp.clicked() {
                        match tool {
                            Tool::Select | Tool::Translate | Tool::Paint
                            | Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso
                            | Tool::Gradient | Tool::MoveLayer | Tool::FreeTransform
                            | Tool::Text | Tool::Eyedropper => {
                                state.tool = tool
                            }
                            Tool::AddCube => {
                                let id = doc.add(ObjectKind::Cube, glam::Vec3::ZERO);
                                doc.set_selection(Some(id));
                                state.dirty = true;
                            }
                            Tool::AddSphere => {
                                let id = doc.add(ObjectKind::Sphere, glam::Vec3::ZERO);
                                doc.set_selection(Some(id));
                                state.dirty = true;
                            }
                            Tool::AddImage => {
                                let id = doc.add(ObjectKind::ImagePlane, glam::Vec3::ZERO);
                                doc.set_selection(Some(id));
                                state.dirty = true;
                            }
                            Tool::AddMesh => {
                                let id = doc.add(ObjectKind::Mesh, glam::Vec3::ZERO);
                                doc.set_selection(Some(id));
                                state.dirty = true;
                            }
                            Tool::AddLathe => {
                                // Default vase profile: axis-symmetric, revolves around Y.
                                let profile: &[[f32; 2]] = &[
                                    [0.0, -1.0],  // bottom center (axis pole)
                                    [0.6, -0.8],  // base rim
                                    [0.7, -0.5],
                                    [0.5, 0.0],   // waist
                                    [0.7, 0.5],
                                    [0.4, 0.9],   // shoulder
                                    [0.15, 1.0],  // neck
                                    [0.0, 1.0],   // top center (axis pole)
                                ];
                                let id = doc.add_lathe(profile, 32, glam::Vec3::ZERO);
                                doc.set_selection(Some(id));
                                state.dirty = true;
                            }
                            Tool::Sculpt => {
                                state.tool = Tool::Sculpt;
                            }
                            Tool::MagicWand => {
                                state.tool = Tool::MagicWand;
                            }
                            Tool::AddPipe => {
                                // Demo: a 32-step helix path.
                                let path: Vec<glam::Vec3> = (0..=32)
                                    .map(|i| {
                                        let t = i as f32 / 32.0;
                                        let angle = t * std::f32::consts::TAU * 2.0;
                                        glam::Vec3::new(angle.cos() * 1.5, t * 2.0 - 1.0, angle.sin() * 1.5)
                                    })
                                    .collect();
                                // Square cross-section.
                                let shape: &[[f32; 2]] = &[
                                    [-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1],
                                ];
                                let id = doc.add_pipe(&path, shape, glam::Vec3::ZERO);
                                doc.set_selection(Some(id));
                                state.dirty = true;
                            }
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        });
    state.left_strip_w = left.response.rect.width();

    // --- Right inspector -----------------------------------------------------
    let mut inspector_changed = false;
    let right = Panel::right("inspector")
        .resizable(true)
        .frame(Frame::default().fill(BG_1).stroke(Stroke { width: 1.0, color: LINE }).inner_margin(Margin::same(12)))
        .show(ui, |ui| {
          egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.heading(RichText::new(state.tool.label()).color(TEXT_0));
            ui.add_space(4.0);
            ui.label(RichText::new(state.tool.hint()).color(TEXT_2));
            ui.add_space(12.0);

            // --- Brush controls (Paint tool) ---
            if state.tool == Tool::Paint {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Brush").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Color").color(TEXT_2));
                    let mut rgb = [state.brush.color[0], state.brush.color[1], state.brush.color[2]];
                    if ui.color_edit_button_rgb(&mut rgb).changed() {
                        state.brush.color[0] = rgb[0];
                        state.brush.color[1] = rgb[1];
                        state.brush.color[2] = rgb[2];
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Size").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.brush.radius_uv, 0.002..=0.12).logarithmic(true));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Hardness").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.brush.hardness, 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Flow").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.brush.flow, 0.02..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Opacity").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.brush.color[3], 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Pressure").color(TEXT_2));
                    ui.add(
                        egui::Slider::new(&mut state.brush_pressure, 0.05..=1.0)
                            .text("(simulated)"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Tip").color(TEXT_2));
                    egui::ComboBox::from_id_salt("brush_tip")
                        .selected_text(state.brush.tip.label())
                        .show_ui(ui, |ui| {
                            for t in suite_gpu::BrushTip::all() {
                                if ui.selectable_label(state.brush.tip == t, t.label()).clicked() {
                                    state.brush.tip = t;
                                }
                            }
                        });
                    ui.label(RichText::new("Blend").color(TEXT_2));
                    egui::ComboBox::from_id_salt("brush_blend")
                        .selected_text(state.brush.blend.label())
                        .show_ui(ui, |ui| {
                            for b in suite_gpu::BrushBlend::all() {
                                if ui.selectable_label(state.brush.blend == b, b.label()).clicked() {
                                    state.brush.blend = b;
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Smudge").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.brush.smudge, 0.0..=1.0));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Stabilize").color(TEXT_2));
                    ui.add(
                        egui::Slider::new(&mut state.brush_stabilize, 1usize..=16)
                            .text("samples"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Symmetry").color(TEXT_2));
                    ui.checkbox(&mut state.mirror_x, "X");
                    ui.checkbox(&mut state.mirror_y, "Y");
                    ui.checkbox(&mut state.wrap_tiling, "Wrap");
                });
                ui.add_space(6.0);
                if ui.button(RichText::new("Clear Canvas").color(TEXT_0)).clicked() {
                    state.clear_canvas_requested = true;
                }

                // --- Layers panel ----------------------------------------------
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Layers").color(TEXT_0).strong());
                    if ui.small_button(RichText::new("+ Add").color(TEXT_0)).clicked() {
                        state.pending_layer_cmd = Some(LayerCmd::Add);
                    }
                });
                let infos = state.layer_infos.clone();
                let active = state.active_layer;
                let n = infos.len();
                // Show the stack top-first (top layer = highest index).
                for i in (0..n).rev() {
                    let info = &infos[i];
                    let is_active = i == active;
                    ui.horizontal(|ui| {
                        let mut vis = info.visible;
                        if ui.checkbox(&mut vis, "").changed() {
                            state.pending_layer_cmd = Some(LayerCmd::SetVisible(i, vis));
                        }
                        let name = RichText::new(&info.name)
                            .color(if is_active { TEXT_0 } else { TEXT_2 });
                        if ui.selectable_label(is_active, name).clicked() {
                            state.pending_layer_cmd = Some(LayerCmd::SetActive(i));
                        }
                        if ui.small_button(RichText::new("Up").color(TEXT_2)).clicked() {
                            state.pending_layer_cmd = Some(LayerCmd::Move(i, true));
                        }
                        if ui.small_button(RichText::new("Dn").color(TEXT_2)).clicked() {
                            state.pending_layer_cmd = Some(LayerCmd::Move(i, false));
                        }
                        if n > 1 && ui.small_button(RichText::new("Del").color(TEXT_2)).clicked() {
                            state.pending_layer_cmd = Some(LayerCmd::Delete(i));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("   opacity").color(TEXT_2).small());
                        let mut op = info.opacity;
                        if ui.add(egui::Slider::new(&mut op, 0.0..=1.0)).changed() {
                            state.pending_layer_cmd = Some(LayerCmd::SetOpacity(i, op));
                        }
                    });
                    // Per-layer blend mode (disabled-looking on the bottom layer, which has
                    // nothing to blend with, but harmless to set).
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("   blend").color(TEXT_2).small());
                        egui::ComboBox::from_id_salt(("layer_blend", i))
                            .selected_text(info.blend.label())
                            .show_ui(ui, |ui| {
                                for &b in suite_doc::BlendMode::all() {
                                    if ui.selectable_label(info.blend == b, b.label()).clicked() {
                                        state.pending_layer_cmd = Some(LayerCmd::SetBlend(i, b));
                                    }
                                }
                            });
                    });
                    // S3: layer mask — add (from selection or empty), clear, or (active layer)
                    // toggle whether the brush paints the colour or the mask.
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("   mask").color(TEXT_2).small());
                        if info.has_mask {
                            if ui.small_button(RichText::new("Clear").color(TEXT_2)).clicked() {
                                state.pending_layer_cmd = Some(LayerCmd::ClearMask(i));
                            }
                            // The Layer/Mask paint target is a property of the active layer.
                            if is_active {
                                if ui.selectable_label(!state.mask_edit, RichText::new("Paint Layer").color(TEXT_1).small()).clicked() {
                                    state.mask_edit = false;
                                }
                                if ui.selectable_label(state.mask_edit, RichText::new("Paint Mask").color(TEXT_1).small()).clicked() {
                                    state.mask_edit = true;
                                }
                            } else {
                                ui.label(RichText::new("◧ on").color(TEXT_2).small());
                            }
                        } else {
                            let has_sel = state.selection_rect.is_some();
                            if ui.add_enabled(has_sel, egui::Button::new(RichText::new("From Sel").color(TEXT_0)).small()).clicked() {
                                state.pending_layer_cmd = Some(LayerCmd::MaskFromSelection(i));
                            }
                            if ui.small_button(RichText::new("Add").color(TEXT_0)).clicked() {
                                state.pending_layer_cmd = Some(LayerCmd::AddEmptyMask(i));
                            }
                        }
                    });
                }
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Mask (non-destructive): \"From Sel\" keeps only the selection (feather works); \"Add\" makes a blank mask. Then pick \"Paint Mask\" and brush black to hide / white to reveal. Saved with the project.")
                        .color(TEXT_2)
                        .small(),
                );

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(RichText::new("Image → 3D").color(TEXT_0).strong());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Res").color(TEXT_2));
                    ui.add(egui::DragValue::new(&mut state.heightmap_resolution).speed(1.0).range(8..=256));
                    ui.label(RichText::new("Scale").color(TEXT_2));
                    ui.add(egui::DragValue::new(&mut state.heightmap_scale).speed(0.01).range(0.01..=5.0));
                });
                if ui.button(RichText::new("→ 3D Heightmap").color(TEXT_0)).clicked() {
                    state.pending_heightmap = true;
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("⌘Z undo · ⌘⇧Z redo. Painting saves with the project (embedded PNG in the .sweet bundle).")
                        .color(TEXT_2)
                        .small(),
                );
                ui.add_space(12.0);
            }

            if matches!(state.tool, Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::MagicWand) {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Selection").color(TEXT_0).strong());
                ui.add_space(4.0);
                let shape_kind = match &state.selection_extra {
                    Some(suite_gpu::SelectionShape::Ellipse { .. }) => "Ellipse",
                    Some(suite_gpu::SelectionShape::Polygon(_)) => "Lasso",
                    Some(suite_gpu::SelectionShape::Mask { .. }) => "Quick Select",
                    None => "Rect",
                };
                match state.selection_rect {
                    Some([x0, y0, x1, y1]) => {
                        ui.label(RichText::new(format!(
                            "Active ({shape_kind}): ({:.0}%,{:.0}%) → ({:.0}%,{:.0}%)",
                            x0 * 100.0, y0 * 100.0, x1 * 100.0, y1 * 100.0
                        )).color(TEXT_2).small());
                    }
                    None => {
                        ui.label(RichText::new("No selection — drag to draw one.").color(TEXT_2).small());
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Select All (⌘A)").color(TEXT_0)).clicked() {
                        state.selection_rect = Some([0.0, 0.0, 1.0, 1.0]);
                        state.selection_extra = None;
                    }
                    if ui.button(RichText::new("Deselect (⌘D)").color(TEXT_0)).clicked() {
                        state.selection_rect = None;
                        state.selection_extra = None;
                    }
                });
                ui.add_space(4.0);
                if ui.button(RichText::new("Invert (⌘⇧I)").color(TEXT_0)).clicked() {
                    // Invert: selecting the complement. For a single rect, inversion is not
                    // trivially representable as one rect; for now just toggle all↔none.
                    state.selection_rect = match state.selection_rect {
                        Some([0.0, 0.0, x1, y1]) if x1 >= 1.0 && y1 >= 1.0 => None,
                        None => Some([0.0, 0.0, 1.0, 1.0]),
                        _ => None, // partial selection → deselect (full invert needs polygon)
                    };
                    state.selection_extra = None;
                }
                ui.add_space(8.0);
                // M4: crop the whole document down to the selection rect. Disabled with no
                // selection, or with the whole-canvas selection (nothing to crop to).
                let can_crop = matches!(state.selection_rect, Some([x0, y0, x1, y1])
                    if (x1 - x0) < 0.999 || (y1 - y0) < 0.999);
                if ui.add_enabled(can_crop, egui::Button::new(RichText::new("Crop to Selection").color(TEXT_0))).clicked() {
                    state.pending_crop = state.selection_rect;
                }
                ui.add_space(4.0);
                let crop_hint = if state.selection_extra.is_some() {
                    "Crops every layer + the canvas to the selection's bounding rect (crop is always rectangular). Not undoable."
                } else {
                    "Crops every layer + the canvas to the selection. Not undoable."
                };
                ui.label(RichText::new(crop_hint).color(TEXT_2).small());
                ui.add_space(8.0);
                let shape_hint = if matches!(&state.selection_extra, Some(suite_gpu::SelectionShape::Mask { .. })) {
                    "Paint/Gradient/Move respect the exact flood-filled region. The marching-ants outline here shows its bounding box, not the exact shape (a precise outline trace for Quick Select is a tracked follow-up)."
                } else {
                    "Paint/Gradient/Move all respect the exact shape (Ellipse/Lasso), not just the bounding box."
                };
                ui.label(RichText::new(shape_hint).color(TEXT_2).small());
                ui.add_space(8.0);
                // Feather (Tier 1): soften the selection edge. 0 = hard edge (unchanged).
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Feather").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.selection_feather, 0.0..=64.0).suffix(" px"));
                });
                ui.label(
                    RichText::new("Softens the selection edge — Paint/Gradient/Move/Text fade out across it instead of a hard cut. Applied on the interior side of the boundary (the selection's bounds don't grow).")
                        .color(TEXT_2)
                        .small(),
                );
                ui.add_space(12.0);
            }

            if state.tool == Tool::Gradient {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Gradient").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Shape").color(TEXT_2));
                    if ui.selectable_label(!state.gradient_radial,
                        RichText::new("Linear").color(TEXT_1)).clicked() {
                        state.gradient_radial = false;
                    }
                    if ui.selectable_label(state.gradient_radial,
                        RichText::new("Radial").color(TEXT_1)).clicked() {
                        state.gradient_radial = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Drag on the canvas: fills the active layer from the brush colour to transparent. Linear follows the drag direction; radial centres on the start point. Respects the active selection. ⌘Z undoes.")
                        .color(TEXT_2)
                        .small(),
                );
                ui.add_space(12.0);
            }

            if state.tool == Tool::FreeTransform {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Free Transform").color(TEXT_0).strong());
                ui.add_space(6.0);
                let box_label = if state.selection_rect.is_some() { "the selection" } else { "the whole layer" };
                ui.label(
                    RichText::new(format!(
                        "Drag a corner square to scale {box_label} (anchored at the opposite corner); drag the circle above the box to rotate about its center. Each drag commits as its own step (⌘Z undoes it)."
                    ))
                    .color(TEXT_2)
                    .small(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Nearest-neighbor sampling (not smoothed/bilinear yet) — scaled and rotated edges will look a little aliased.")
                        .color(TEXT_2)
                        .small(),
                );
                ui.add_space(12.0);
            }

            if state.tool == Tool::Text {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Text").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Load Font…").color(TEXT_0)).clicked() {
                        state.pending_file_action = Some(FileAction::LoadFont);
                    }
                    if let Some(status) = &state.text_font_status {
                        ui.label(RichText::new(status).color(TEXT_2).small());
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("String").color(TEXT_2));
                ui.text_edit_singleline(&mut state.text_input);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Size").color(TEXT_2));
                    ui.add(egui::DragValue::new(&mut state.text_size).speed(0.5).range(4.0..=400.0));
                });
                ui.add_space(4.0);
                let anchor_label = match state.text_anchor {
                    Some([u, v]) => format!("Baseline at ({:.0}%, {:.0}%) — click elsewhere to move it.", u * 100.0, v * 100.0),
                    None => "Click on the canvas to place the baseline.".to_string(),
                };
                ui.label(RichText::new(anchor_label).color(TEXT_2).small());
                ui.add_space(4.0);
                let can_apply = state.loaded_font.is_some() && !state.text_input.is_empty() && state.text_anchor.is_some();
                if ui.add_enabled(can_apply, egui::Button::new(RichText::new("Apply Text").color(TEXT_0))).clicked() {
                    state.pending_apply_text = true;
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Uses the brush colour. Single line only (no wrapping/kerning/bidi). TrueType (.ttf) glyf outlines only — not OTF/CFF or composite glyphs (accented characters may render blank). ⌘Z undoes.")
                        .color(TEXT_2)
                        .small(),
                );
                ui.add_space(12.0);
            }

            // Layer transforms (M4): flip / 180°-rotate the active layer's pixels. Shown for
            // the 2D paint-ish tools where a raster layer is the working surface. These three
            // never change dimensions, so they're always safe regardless of canvas aspect (M5).
            if matches!(state.tool, Tool::Paint | Tool::Gradient | Tool::RectSelect | Tool::MagicWand) {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Layer Transform").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Flip H").color(TEXT_0)).clicked() {
                        state.pending_layer_transform = Some(suite_gpu::LayerTransform::FlipH);
                    }
                    if ui.button(RichText::new("Flip V").color(TEXT_0)).clicked() {
                        state.pending_layer_transform = Some(suite_gpu::LayerTransform::FlipV);
                    }
                    if ui.button(RichText::new("180°").color(TEXT_0)).clicked() {
                        state.pending_layer_transform = Some(suite_gpu::LayerTransform::Rotate180);
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Flips/rotates the active layer. ⌘Z undoes.").color(TEXT_2).small());
                ui.add_space(8.0);
                // A 90° rotation must rotate every layer + the canvas dimensions together to
                // stay aligned, so it's a document-level op (M5), not per-layer like the above.
                ui.label(RichText::new("Rotate Canvas").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Rotate ⟳").color(TEXT_0)).clicked() {
                        state.pending_canvas_rotate = Some(suite_gpu::CanvasRotate::Cw);
                    }
                    if ui.button(RichText::new("Rotate ⟲").color(TEXT_0)).clicked() {
                        state.pending_canvas_rotate = Some(suite_gpu::CanvasRotate::Ccw);
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Rotates the whole document 90°. Not undoable.").color(TEXT_2).small());
                ui.add_space(12.0);
            }

            if state.tool == Tool::Sculpt {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Sculpt Brush").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mode").color(TEXT_2));
                    for (label, op) in [("Draw", 0u8), ("Smooth", 1), ("Flatten", 2), ("Pinch", 3)] {
                        if ui.selectable_label(state.sculpt_op == op,
                            RichText::new(label).color(TEXT_1)).clicked() {
                            state.sculpt_op = op;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Radius").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.sculpt_radius, 0.05..=3.0).logarithmic(true));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Strength").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.sculpt_strength, 0.001..=0.5).logarithmic(true));
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Drag on the selected mesh to sculpt. Select a mesh first.")
                        .color(TEXT_2)
                        .small(),
                );
                ui.add_space(12.0);
            }

            if state.tool == Tool::MagicWand {
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Magic Wand").color(TEXT_0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Tolerance").color(TEXT_2));
                    ui.add(egui::Slider::new(&mut state.magic_wand_tolerance, 0.0..=1.0));
                });
                ui.checkbox(&mut state.magic_wand_contiguous, RichText::new("Contiguous").color(TEXT_1));
                ui.add_space(4.0);
                let wand_hint = if state.magic_wand_contiguous {
                    "Click to select the connected colour region under the cursor. Produces a selection (like Ellipse/Lasso) — use Paint/Gradient/Move afterward. Uncheck Contiguous to select that colour everywhere on the canvas (Select by Color)."
                } else {
                    "Select by Color: click to select every pixel of that colour across the whole canvas, connected or not. Re-check Contiguous for a single connected region."
                };
                ui.label(RichText::new(wand_hint).color(TEXT_2).small());
                ui.add_space(12.0);
            }

            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new(format!("Scene — {} objects", doc.object_count())).color(TEXT_1));
            if ui.small_button(RichText::new("+ Adjustment Layer").color(TEXT_0)).clicked() {
                state.pending_add_adjustment = true;
            }
            ui.add_space(4.0);
            // Object list
            let sel = doc.selection();
            let object_ids: Vec<_> = doc.objects().map(|o| (o.id, o.name.clone(), o.kind)).collect();
            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                for (id, name, kind) in &object_ids {
                    let selected = sel == Some(*id);
                    let label = format!("{} · {}", name, kind.label());
                    if ui.selectable_label(selected, label).clicked() {
                        doc.set_selection(Some(*id));
                    }
                }
            });
            ui.add_space(12.0);

            // Inspector for the selected object
            if let Some(id) = doc.selection() {
                if let Some(obj) = doc.get_mut(id) {
                    let mut changed = false;
                    ui.label(RichText::new(format!("Editing {}", obj.name)).color(TEXT_0).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Position").color(TEXT_2));
                    let mut t = obj.transform.translation;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("X").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut t.x).speed(0.02)).changed();
                        ui.label(RichText::new("Y").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut t.y).speed(0.02)).changed();
                        ui.label(RichText::new("Z").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut t.z).speed(0.02)).changed();
                    });
                    obj.transform.translation = t;

                    let mut e = obj.transform.rotation_euler();
                    ui.add_space(4.0);
                    ui.label(RichText::new("Rotation (rad)").color(TEXT_2));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("X").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut e.x).speed(0.01)).changed();
                        ui.label(RichText::new("Y").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut e.y).speed(0.01)).changed();
                        ui.label(RichText::new("Z").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut e.z).speed(0.01)).changed();
                    });
                    if changed {
                        obj.transform.set_rotation_euler(e);
                    }

                    let mut s = obj.transform.scale;
                    ui.add_space(4.0);
                    ui.label(RichText::new("Scale").color(TEXT_2));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("X").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut s.x).speed(0.01).range(0.001..=20.0)).changed();
                        ui.label(RichText::new("Y").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut s.y).speed(0.01).range(0.001..=20.0)).changed();
                        ui.label(RichText::new("Z").color(TEXT_2).monospace());
                        changed |= ui.add(egui::DragValue::new(&mut s.z).speed(0.01).range(0.001..=20.0)).changed();
                    });
                    obj.transform.scale = s;

                    ui.add_space(8.0);
                    changed |= ui.checkbox(&mut obj.visibility, RichText::new("visible").color(TEXT_1)).changed();
                    ui.add_space(2.0);
                    ui.label(RichText::new(format!("id: slot {} gen {}", id.slot, id.generation)).color(TEXT_2).monospace());

                    // --- Compositor: blend mode + opacity for every object ----
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(RichText::new("Compositing").color(TEXT_0).strong());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Opacity").color(TEXT_2));
                        changed |= ui
                            .add(egui::Slider::new(&mut obj.opacity, 0.0..=1.0))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Blend Mode").color(TEXT_2));
                        egui::ComboBox::from_id_salt("blend_mode")
                            .selected_text(obj.blend_mode.label())
                            .show_ui(ui, |ui| {
                                for mode in suite_doc::BlendMode::all() {
                                    if ui.selectable_label(obj.blend_mode == *mode, mode.label()).clicked() {
                                        obj.blend_mode = *mode;
                                        changed = true;
                                    }
                                }
                            });
                    });

                    // --- Adjustment layer controls ----------------------------
                    if obj.kind == suite_doc::ObjectKind::Adjustment {
                        ui.add_space(6.0);
                        ui.label(RichText::new("Adjustment").color(TEXT_0).strong());
                        ui.add_space(4.0);
                        let adj = obj.adjustment.get_or_insert(
                            suite_doc::AdjustmentKind::BrightnessContrast {
                                brightness: 0.0,
                                contrast: 0.0,
                            },
                        );
                        // Type picker — driven by AdjustmentKind::all_defaults() so new
                        // kinds appear automatically.
                        egui::ComboBox::from_id_salt("adj_kind")
                            .selected_text(adj.label())
                            .show_ui(ui, |ui| {
                                for def in suite_doc::AdjustmentKind::all_defaults() {
                                    let selected = std::mem::discriminant(adj) == std::mem::discriminant(&def);
                                    if ui.selectable_label(selected, def.label()).clicked() && !selected {
                                        *adj = def;
                                        changed = true;
                                    }
                                }
                            });
                        ui.add_space(4.0);
                        match adj {
                            suite_doc::AdjustmentKind::BrightnessContrast { brightness, contrast } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Brightness").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(brightness, -1.0..=1.0)).changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Contrast").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(contrast, -1.0..=1.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::HueSaturation { hue, saturation, lightness } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Hue shift").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(hue, -0.5..=0.5)).changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Saturation").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(saturation, 0.0..=3.0)).changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Lightness").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(lightness, -0.5..=0.5)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Levels { black_point, gamma, white_point } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Black pt").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(black_point, 0.0..=0.5)).changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Gamma").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(gamma, 0.1..=3.0)).changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("White pt").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(white_point, 0.5..=1.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Exposure { stops } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Stops").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(stops, -4.0..=4.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Vibrance { amount } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Amount").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(amount, -1.0..=1.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::WhiteBalance { temperature, tint } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Temp").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(temperature, -0.5..=0.5)).changed();
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Tint").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(tint, -0.5..=0.5)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Posterize { levels } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Levels").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(levels, 2.0..=32.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Threshold { level } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Level").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(level, 0.0..=1.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Invert => {
                                ui.label(RichText::new("(no parameters)").color(TEXT_2).small());
                            }
                            suite_doc::AdjustmentKind::BoxBlur { radius } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Radius").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(radius, 1.0..=8.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::Sharpen { amount } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Amount").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(amount, 0.0..=3.0)).changed();
                                });
                            }
                            suite_doc::AdjustmentKind::EdgeDetect => {
                                ui.label(RichText::new("(Sobel — no parameters)").color(TEXT_2).small());
                            }
                            suite_doc::AdjustmentKind::GaussianBlur { radius } => {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Radius").color(TEXT_2));
                                    changed |= ui.add(egui::Slider::new(radius, 1.0..=16.0)).changed();
                                });
                            }
                        }
                    }

                    inspector_changed |= changed;
                } else {
                    ui.label(RichText::new("Selection is stale.").color(TEXT_2));
                }

                // Mesh modeling ops (Extrude). Outside the obj borrow so we can call the
                // Document op. Shown only when the selection is an editable mesh.
                let is_mesh = doc.get(id).map(|o| o.kind == ObjectKind::Mesh).unwrap_or(false);
                if is_mesh {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.label(RichText::new("Mesh").color(TEXT_0).strong());
                    ui.add_space(4.0);
                    if let Some(m) = doc.get(id).and_then(|o| o.mesh.as_ref()) {
                        ui.label(RichText::new(format!("{} verts · {} faces", m.vertices.len(), m.faces.len())).color(TEXT_2));
                    }
                    let face_label = match doc.selected_face() {
                        Some(f) => format!("Selected face: {f} (click a face to change)"),
                        None => "No face selected — click a face, or extrude uses the top".to_string(),
                    };
                    ui.label(RichText::new(face_label).color(TEXT_2));
                    ui.add_space(4.0);
                    let extrude_label = if doc.selected_face().is_some() {
                        "Extrude selected face (E)"
                    } else {
                        "Extrude top face (E)"
                    };
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(extrude_label).color(TEXT_0)).clicked()
                            && doc.extrude_selected_mesh(0.5)
                        {
                            inspector_changed = true;
                        }
                        if ui.button(RichText::new("Inset (I)").color(TEXT_0)).clicked()
                            && doc.inset_selected_mesh(0.3)
                        {
                            inspector_changed = true;
                        }
                        if ui.button(RichText::new("Loop Cut (C)").color(TEXT_0)).clicked()
                            && doc.loop_cut_selected_mesh()
                        {
                            inspector_changed = true;
                        }
                        if ui.button(RichText::new("Bevel Corner (V)").color(TEXT_0)).clicked()
                            && doc.bevel_selected_mesh_corner()
                        {
                            inspector_changed = true;
                        }
                        if ui.button(RichText::new("Bevel Edge (G)").color(TEXT_0)).clicked()
                            && doc.bevel_selected_mesh_edge()
                        {
                            inspector_changed = true;
                        }
                    });

                    // --- Auto-Rig ---
                    ui.add_space(8.0);
                    ui.label(RichText::new("Skeleton / Auto-Rig").color(TEXT_0).strong());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Auto-Rig (spine)").color(TEXT_0)).clicked() {
                            if doc.auto_rig_selected_mesh(6) {
                                inspector_changed = true;
                            }
                        }
                        // Show bone count if skeleton exists.
                        if let Some(obj) = doc.selection().and_then(|id| doc.get(id)) {
                            if let Some(sk) = &obj.skeleton {
                                ui.label(RichText::new(format!("{} bones", sk.bones.len())).color(TEXT_2).small());
                            }
                        }
                    });

                    // Bone posing — drives linear-blend skinning live. Only shown for a
                    // rigged mesh.
                    let bone_count = doc
                        .selection()
                        .and_then(|id| doc.get(id))
                        .and_then(|o| o.skeleton.as_ref())
                        .map(|s| s.bones.len())
                        .unwrap_or(0);
                    if bone_count > 0 {
                        ui.add_space(4.0);
                        if state.active_bone >= bone_count {
                            state.active_bone = bone_count - 1;
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Bone").color(TEXT_2).small());
                            if ui.add(egui::Slider::new(&mut state.active_bone, 0..=bone_count - 1)).changed() {
                                // Switching bones resets the editable Euler to neutral.
                                state.bone_pose_deg = [0.0; 3];
                            }
                        });
                        let mut pose_changed = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Rot°").color(TEXT_2).small());
                            for axis in 0..3 {
                                pose_changed |= ui
                                    .add(egui::DragValue::new(&mut state.bone_pose_deg[axis]).speed(1.0).range(-180.0..=180.0))
                                    .changed();
                            }
                        });
                        if pose_changed {
                            let r = state.bone_pose_deg;
                            let q = glam::Quat::from_euler(
                                glam::EulerRot::XYZ,
                                r[0].to_radians(), r[1].to_radians(), r[2].to_radians(),
                            );
                            if doc.set_selected_bone_pose(state.active_bone, q) {
                                inspector_changed = true;
                            }
                        }
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Key Bone Pose (at playhead)").color(TEXT_0)).clicked() {
                                state.pending_key_bone = true;
                            }
                        });
                    }

                    // --- CSG Booleans (manifold3d) ---
                    ui.add_space(10.0);
                    ui.label(RichText::new("Boolean CSG").color(TEXT_0).strong());
                    ui.add_space(4.0);
                    // Tool object picker: any other mesh in the scene.
                    let mesh_ids: Vec<(ObjId, String)> = doc
                        .objects()
                        .filter(|o| o.kind == ObjectKind::Mesh && Some(o.id) != doc.selection())
                        .map(|o| (o.id, o.name.clone()))
                        .collect();
                    if mesh_ids.is_empty() {
                        ui.label(RichText::new("(add a second mesh to enable CSG)").color(TEXT_2).small());
                    } else {
                        let tool_label = state.csg_tool_id
                            .and_then(|tid| mesh_ids.iter().find(|(id, _)| *id == tid).map(|(_, n)| n.as_str()))
                            .unwrap_or("— pick tool —");
                        egui::ComboBox::from_id_salt("csg_tool")
                            .selected_text(tool_label)
                            .show_ui(ui, |ui| {
                                for (mid, name) in &mesh_ids {
                                    if ui.selectable_label(state.csg_tool_id == Some(*mid), name.as_str()).clicked() {
                                        state.csg_tool_id = Some(*mid);
                                    }
                                }
                            });
                        if let Some(tool_id) = state.csg_tool_id {
                            if mesh_ids.iter().any(|(id, _)| *id == tool_id) {
                                ui.horizontal(|ui| {
                                    if ui.button(RichText::new("Union").color(TEXT_0)).clicked() {
                                        state.pending_boolean = Some((tool_id, 0));
                                    }
                                    if ui.button(RichText::new("Subtract").color(TEXT_0)).clicked() {
                                        state.pending_boolean = Some((tool_id, 1));
                                    }
                                    if ui.button(RichText::new("Intersect").color(TEXT_0)).clicked() {
                                        state.pending_boolean = Some((tool_id, 2));
                                    }
                                });
                            }
                        }
                    }

                    // --- Modifier stack (non-destructive) ---
                    ui.add_space(10.0);
                    ui.label(RichText::new("Modifiers").color(TEXT_0).strong());
                    ui.add_space(4.0);
                    let mut remove_at: Option<usize> = None;
                    let mut stack_changed = false;
                    if let Some(obj) = doc.get_mut(id) {
                        for (mi, m) in obj.modifiers.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(m.label()).color(TEXT_1).monospace());
                                match m {
                                    suite_doc::Modifier::Mirror { axis } => {
                                        for (ax, name) in [(0u32, "X"), (1, "Y"), (2, "Z")] {
                                            if ui.selectable_label(*axis == ax, name).clicked() {
                                                *axis = ax;
                                                stack_changed = true;
                                            }
                                        }
                                    }
                                    suite_doc::Modifier::Array { count, offset } => {
                                        stack_changed |= ui
                                            .add(egui::DragValue::new(count).speed(0.1).range(1..=64).prefix("×"))
                                            .changed();
                                        stack_changed |= ui
                                            .add(egui::DragValue::new(&mut offset[0]).speed(0.05).prefix("dx "))
                                            .changed();
                                    }
                                    suite_doc::Modifier::Subdivide { levels } => {
                                        stack_changed |= ui
                                            .add(egui::DragValue::new(levels).speed(0.05).range(0..=4).prefix("lvl "))
                                            .changed();
                                    }
                                    suite_doc::Modifier::Decimate { grid_res } => {
                                        stack_changed |= ui
                                            .add(egui::DragValue::new(grid_res).speed(0.1).range(4..=64).prefix("res "))
                                            .changed();
                                    }
                                }
                                if ui.button(RichText::new("✕").color(TEXT_2)).clicked() {
                                    remove_at = Some(mi);
                                }
                            });
                        }
                        if let Some(i) = remove_at {
                            obj.modifiers.remove(i);
                            stack_changed = true;
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("+ Mirror").color(TEXT_0)).clicked() {
                                obj.modifiers.push(suite_doc::Modifier::Mirror { axis: 0 });
                                stack_changed = true;
                            }
                            if ui.button(RichText::new("+ Array").color(TEXT_0)).clicked() {
                                obj.modifiers.push(suite_doc::Modifier::Array { count: 3, offset: [1.5, 0.0, 0.0] });
                                stack_changed = true;
                            }
                            if ui.button(RichText::new("+ Subdiv").color(TEXT_0)).clicked() {
                                obj.modifiers.push(suite_doc::Modifier::Subdivide { levels: 2 });
                                stack_changed = true;
                            }
                            if ui.button(RichText::new("+ Decimate").color(TEXT_0)).clicked() {
                                obj.modifiers.push(suite_doc::Modifier::Decimate { grid_res: 16 });
                                stack_changed = true;
                            }
                        });
                    }
                    inspector_changed |= stack_changed;
                }
            } else {
                ui.label(RichText::new("Nothing selected. Use the Select tool (1) and click a primitive.").color(TEXT_2));
            }

            if !state.status.is_empty() {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new(&state.status).color(TEXT_2).monospace());
            }
          });
        });
    state.right_panel_w = right.response.rect.width();
    if inspector_changed {
        state.dirty = true;
        state.edited_object = true;
    }

    // --- Bottom timeline transport bar ----------------------------------------
    Panel::bottom("timeline_bar")
        .resizable(false)
        .frame(
            Frame::default()
                .fill(BG_0)
                .stroke(Stroke { width: 1.0, color: LINE })
                .inner_margin(Margin::symmetric(12, 6)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Transport controls.
                let playing = timeline.playhead.playing;
                if ui.button(RichText::new(if playing { "⏸" } else { "▶" }).color(TEXT_0)).clicked() {
                    if playing { timeline.pause(); } else { timeline.play(); }
                }
                if ui.button(RichText::new("⏹").color(TEXT_0)).clicked() {
                    timeline.stop();
                }

                ui.add_space(8.0);

                // Set keyframe for the selected object at the current playhead time.
                if ui.button(RichText::new("K  Set Key").color(TEXT_0)).clicked() {
                    if let Some(sel_id) = doc.selection() {
                        if let Some(obj) = doc.get(sel_id) {
                            let key = (sel_id.slot as u64) << 32 | sel_id.generation as u64;
                            let pos = obj.transform.translation;
                            let rot = obj.transform.rotation;
                            let scale = obj.transform.scale;
                            timeline.set_keyframe_trs(
                                key,
                                [pos.x, pos.y, pos.z],
                                [rot.x, rot.y, rot.z],
                                [scale.x, scale.y, scale.z],
                            );
                        }
                    }
                }

                ui.add_space(8.0);

                // Playhead scrubber.
                let dur = timeline.playhead.clip_duration.max(0.01);
                let mut t = timeline.playhead.time;
                let scrubber = ui.add(
                    egui::Slider::new(&mut t, 0.0..=dur)
                        .text("t")
                        .min_decimals(2)
                        .max_decimals(2),
                );
                if scrubber.changed() {
                    timeline.playhead.time = t;
                }

                ui.add_space(8.0);

                // Duration input.
                let mut dur_val = timeline.playhead.clip_duration;
                if ui.add(
                    egui::DragValue::new(&mut dur_val)
                        .speed(0.1)
                        .range(0.5..=60.0)
                        .suffix(" s"),
                ).changed() {
                    timeline.playhead.clip_duration = dur_val;
                    timeline.clip.duration = dur_val;
                }

                // Loop toggle.
                let mut looping = timeline.playhead.loop_play;
                if ui.checkbox(&mut looping, RichText::new("Loop").color(TEXT_1).small()).changed() {
                    timeline.playhead.loop_play = looping;
                }

                // Keyframe count indicator.
                let n_tracks = timeline.clip.tracks.len();
                if n_tracks > 0 {
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("{n_tracks} animated")).color(TEXT_2).small());
                }
            });
        });

    // Canvas overlays (selection marching-ants + gradient guide line).
    //
    // IMPORTANT: do NOT use an `egui::CentralPanel` here. A panel claims the central rect as
    // egui's own interactive surface, so egui reports `wants_pointer_input()` while the
    // cursor is over the canvas and `egui_winit` marks the pointer events as *consumed* —
    // which swallows the very press/drag events the canvas tools (paint / gradient / marquee)
    // depend on. Instead we draw onto a non-interactive **foreground layer**: it allocates no
    // widgets, senses nothing, and lets the raw winit pointer events flow through to the
    // canvas handlers. `available_rect_before_wrap()` is the leftover central region after
    // the side/top/bottom panels were laid out — i.e. the canvas viewport, matching the
    // `canvas_rect`/UV convention the tool handlers use.
    let canvas_rect = ui.available_rect_before_wrap();
    // Publish the authoritative physical-pixel canvas rect for `canvas_rect()` (see its doc
    // comment) — captured right here since this is the one place in the frame
    // `available_rect_before_wrap()` reflects every panel, not just the ones a hand-summed
    // reconstruction remembered to include.
    let ppp = ui.ctx().pixels_per_point();
    state.canvas_rect_physical = (
        canvas_rect.left() * ppp,
        canvas_rect.top() * ppp,
        canvas_rect.right() * ppp,
        canvas_rect.bottom() * ppp,
    );
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("canvas_overlay"),
    ));
    if state.selection_rect.is_some() {
        let rect = canvas_rect;
        let to_screen = |u: f32, v: f32| egui::pos2(rect.left() + u * rect.width(), rect.top() + v * rect.height());
        // Tier 1: trace the exact shape (Ellipse as an N-gon approximation, Lasso as its own
        // point path) instead of always drawing the bounding rect — falls back to the plain
        // rect when there's no exact shape (RectSelect, the common case, unchanged).
        let corners: Vec<egui::Pos2> = match &state.selection_extra {
            Some(suite_gpu::SelectionShape::Ellipse { cx, cy, rx, ry }) => {
                const SEGMENTS: usize = 48;
                (0..=SEGMENTS)
                    .map(|i| {
                        let a = (i as f32 / SEGMENTS as f32) * std::f32::consts::TAU;
                        to_screen(cx + rx * a.cos(), cy + ry * a.sin())
                    })
                    .collect()
            }
            Some(suite_gpu::SelectionShape::Polygon(points)) if points.len() >= 2 => points
                .iter()
                .chain(points.first())
                .map(|p| to_screen(p[0], p[1]))
                .collect(),
            _ => {
                let [u0, v0, u1, v1] = state.selection_rect.unwrap();
                vec![to_screen(u0, v0), to_screen(u1, v0), to_screen(u1, v1), to_screen(u0, v1), to_screen(u0, v0)]
            }
        };
        // Marching-ants: alternating white/black dashed border.
        let t = ui.ctx().input(|i| i.time) as f32;
        let march_speed = 40.0_f32; // pixels per second
        let dash = 6.0_f32;
        let gap  = 6.0_f32;
        let period = dash + gap;
        let phase = (t * march_speed).rem_euclid(period);
        let white_shapes = egui::Shape::dashed_line(
            &corners, egui::Stroke::new(2.0, egui::Color32::WHITE), dash, gap,
        );
        painter.extend(white_shapes);
        // Black inner dashes offset by half a period to fill the white gaps. Perimeter is
        // the actual path length (sum of segment lengths), not a rect-only shortcut, so this
        // works for the ellipse/polygon traces too.
        let segs: Vec<(egui::Pos2, egui::Pos2)> = corners.windows(2).map(|w| (w[0], w[1])).collect();
        let perim: f32 = segs.iter().map(|(a, b)| (*b - *a).length()).sum();
        if perim > 0.0 {
            let start_frac = phase / perim;
            let start_d = start_frac * perim + period * 0.5;
            let mut shifted: Vec<egui::Pos2> = Vec::with_capacity(corners.len() + 1);
            let mut accum = 0.0_f32;
            let mut started = false;
            let mut extra_start = corners[0];
            for (a, b) in &segs {
                let seg_len = (*b - *a).length();
                if !started && accum + seg_len >= start_d % perim {
                    let local_t = (start_d % perim - accum) / seg_len;
                    extra_start = egui::pos2(
                        a.x + (b.x - a.x) * local_t.clamp(0.0, 1.0),
                        a.y + (b.y - a.y) * local_t.clamp(0.0, 1.0),
                    );
                    started = true;
                }
                accum += seg_len;
            }
            shifted.push(extra_start);
            for p in &corners[1..] { shifted.push(*p); }
            shifted.push(extra_start);
            let black_shapes = egui::Shape::dashed_line(
                &shifted, egui::Stroke::new(2.0, egui::Color32::BLACK), dash, gap,
            );
            painter.extend(black_shapes);
        }
        // Request continuous repaint so the ants march.
        ui.ctx().request_repaint();
    }

    // Gradient guide line while dragging (start → endpoint), with end caps.
    if let Some([gu0, gv0, gu1, gv1]) = state.gradient_preview {
        let rect = canvas_rect;
        let p0 = egui::pos2(rect.left() + gu0 * rect.width(), rect.top() + gv0 * rect.height());
        let p1 = egui::pos2(rect.left() + gu1 * rect.width(), rect.top() + gv1 * rect.height());
        painter.line_segment([p0, p1], egui::Stroke::new(3.0, egui::Color32::BLACK));
        painter.line_segment([p0, p1], egui::Stroke::new(1.5, egui::Color32::WHITE));
        painter.circle_filled(p0, 4.0, egui::Color32::WHITE);
        painter.circle_stroke(p0, 4.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
        painter.circle_filled(p1, 4.0, egui::Color32::WHITE);
        painter.circle_stroke(p1, 4.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
    }

    // Move guide line while dragging (start → endpoint), with end caps. The canvas itself
    // doesn't change until release (commit-on-release, same shape as Gradient) — this line
    // is the only feedback during the drag.
    if let Some([mu0, mv0, mu1, mv1]) = state.move_preview {
        let rect = canvas_rect;
        let p0 = egui::pos2(rect.left() + mu0 * rect.width(), rect.top() + mv0 * rect.height());
        let p1 = egui::pos2(rect.left() + mu1 * rect.width(), rect.top() + mv1 * rect.height());
        painter.line_segment([p0, p1], egui::Stroke::new(3.0, egui::Color32::BLACK));
        painter.line_segment([p0, p1], egui::Stroke::new(1.5, egui::Color32::WHITE));
        painter.circle_filled(p0, 4.0, egui::Color32::WHITE);
        painter.circle_stroke(p0, 4.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
        painter.circle_filled(p1, 4.0, egui::Color32::WHITE);
        painter.circle_stroke(p1, 4.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
    }

    // Free Transform (M4c): the transform box + its handles, always visible while the tool
    // is active (so there's something to grab), plus a live transformed-box preview while a
    // handle is being dragged. No pixel resampling happens here — same commit-on-release
    // shape as Gradient/Move above; this is pure guide-line feedback.
    if state.tool == Tool::FreeTransform {
        let rect = canvas_rect;
        let to_screen = |u: f32, v: f32| egui::pos2(rect.left() + u * rect.width(), rect.top() + v * rect.height());
        let [bx0, by0, bx1, by1] = state.selection_rect.unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let corners_uv = [[bx0, by0], [bx1, by0], [bx1, by1], [bx0, by1]];
        let box_pts: Vec<egui::Pos2> = corners_uv.iter().chain(corners_uv.first()).map(|p| to_screen(p[0], p[1])).collect();
        painter.add(egui::Shape::closed_line(box_pts.clone(), egui::Stroke::new(1.5, egui::Color32::from_gray(180))));
        for p in &box_pts[..4] {
            painter.rect_filled(egui::Rect::from_center_size(*p, egui::vec2(8.0, 8.0)), 1.0, egui::Color32::WHITE);
            painter.rect_stroke(egui::Rect::from_center_size(*p, egui::vec2(8.0, 8.0)), 1.0, egui::Stroke::new(1.5, egui::Color32::BLACK), egui::StrokeKind::Outside);
        }
        let center_uv = [(bx0 + bx1) * 0.5, (by0 + by1) * 0.5];
        let top_center = to_screen(center_uv[0], by0);
        let rotate_handle = egui::pos2(top_center.x, top_center.y - 24.0);
        painter.line_segment([top_center, rotate_handle], egui::Stroke::new(1.5, egui::Color32::from_gray(180)));
        painter.circle_filled(rotate_handle, 5.0, egui::Color32::WHITE);
        painter.circle_stroke(rotate_handle, 5.0, egui::Stroke::new(1.5, egui::Color32::BLACK));

        // Live preview: forward-map the box's 4 corners through the in-progress scale/
        // rotation (same formula as `apply_free_transform`'s corner-bbox step) and draw the
        // result in accent color, so the user sees where release will land before committing.
        if let Some(mode) = state.transform_mode {
            let (pivot_u, pivot_v) = match mode {
                TransformMode::Scale { anchor_uv } => (anchor_uv[0], anchor_uv[1]),
                TransformMode::Rotate { center_uv, .. } => (center_uv[0], center_uv[1]),
            };
            let (cw, ch) = (rect.width().max(1.0), rect.height().max(1.0));
            let pivot_px = (pivot_u * cw, pivot_v * ch);
            let (sin_r, cos_r) = state.transform_rotation.sin_cos();
            let scale = state.transform_scale;
            let preview_pts: Vec<egui::Pos2> = corners_uv
                .iter()
                .chain(corners_uv.first())
                .map(|p| {
                    let (px, py) = (p[0] * cw, p[1] * ch);
                    let (ox, oy) = (px - pivot_px.0, py - pivot_px.1);
                    let (sx, sy) = (ox * scale, oy * scale);
                    let (fx, fy) = (sx * cos_r - sy * sin_r + pivot_px.0, sx * sin_r + sy * cos_r + pivot_px.1);
                    egui::pos2(rect.left() + fx, rect.top() + fy)
                })
                .collect();
            painter.add(egui::Shape::closed_line(preview_pts, egui::Stroke::new(2.0, ACCENT_HOVER)));
        }
    }

    // Text tool (M4a): a crosshair at the baseline anchor — no live glyph preview (that would
    // mean rasterizing on every frame while the inspector's text field is being typed into;
    // "click, type, Apply" keeps this cheap and simple, same commit-on-demand spirit as the
    // other M4 tools' commit-on-release).
    if state.tool == Tool::Text {
        if let Some([u, v]) = state.text_anchor {
            let rect = canvas_rect;
            let p = egui::pos2(rect.left() + u * rect.width(), rect.top() + v * rect.height());
            let r = 7.0;
            painter.line_segment([egui::pos2(p.x - r, p.y), egui::pos2(p.x + r, p.y)], egui::Stroke::new(1.5, egui::Color32::BLACK));
            painter.line_segment([egui::pos2(p.x, p.y - r), egui::pos2(p.x, p.y + r)], egui::Stroke::new(1.5, egui::Color32::BLACK));
            painter.circle_stroke(p, r, egui::Stroke::new(1.5, egui::Color32::WHITE));
        }
    }
}
