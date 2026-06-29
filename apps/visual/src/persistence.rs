//! Save / Open / New for the Visual app, on top of `suite_assets::ProjectBundle`.
//!
//! The app's `Document` serializes to a `visual.scene` payload (owned by `suite-doc`);
//! this module wraps that payload in a `.sweet` bundle (owned by `suite-assets`) and
//! drives the native file dialogs (`rfd`). The painted raster is read back from the GPU,
//! PNG-encoded, and stored as a base64 blob in the bundle so a painting survives
//! save→reopen.

use std::path::{Path, PathBuf};

use base64::Engine;
use suite_assets::{ProjectBundle, BUNDLE_EXTENSION};
use suite_doc::Document;

const MAIN_SCENE_ROLE: &str = "main.visual.scene";
const PAINT_BLOB: &str = "main.paint.png";
const LAYERS_META_ROLE: &str = "main.layers";

/// A painted raster ready to embed: square `size`, row-major RGBA8 `rgba`.
pub struct PaintImage {
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// One layer's pixels + metadata for the `.sweet` layer stack.
pub struct LayerSave {
    pub rgba: Vec<u8>,
    pub size: u32,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend: suite_doc::BlendMode,
}

/// Write the document + the full 2D layer stack to `path` as a `.sweet` bundle. Each layer
/// is a `main.layer.{i}.png` blob; `main.layers` holds the ordered metadata.
pub fn save_to(doc: &Document, layers: &[LayerSave], path: &Path) -> Result<String, String> {
    let scene_json = doc.to_scene_json().map_err(|e| e.to_string())?;
    let scene_value: serde_json::Value =
        serde_json::from_str(&scene_json).map_err(|e| e.to_string())?;
    let mut bundle = ProjectBundle::new("visual");
    bundle.put_document(MAIN_SCENE_ROLE, scene_value);

    if !layers.is_empty() {
        let meta: Vec<serde_json::Value> = layers
            .iter()
            .map(|l| {
                let blend = serde_json::to_value(l.blend).unwrap_or(serde_json::Value::Null);
                serde_json::json!({ "name": l.name, "visible": l.visible, "opacity": l.opacity, "blend": blend })
            })
            .collect();
        bundle.put_document(LAYERS_META_ROLE, serde_json::Value::Array(meta));
        for (i, l) in layers.iter().enumerate() {
            let png = encode_png(&PaintImage { size: l.size, rgba: l.rgba.clone() })
                .map_err(|e| format!("layer {i} encode failed: {e}"))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
            bundle.put_blob(format!("main.layer.{i}.png"), b64);
        }
    }
    bundle.save(path).map_err(|e| e.to_string())?;
    Ok(format!("Saved {}", path.display()))
}

/// What a load produced: the scene + the layer stack (empty = use the renderer default).
pub struct Loaded {
    pub document: Document,
    pub layers: Vec<LayerSave>,
}

/// Native "Save As" dialog → write. Returns the chosen path + status on success.
pub fn save_dialog(
    doc: &Document,
    layers: &[LayerSave],
    suggested: Option<&Path>,
) -> Option<(PathBuf, String)> {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("SWEET project", &[BUNDLE_EXTENSION])
        .set_file_name("untitled.sweet");
    if let Some(dir) = suggested.and_then(|p| p.parent()) {
        dialog = dialog.set_directory(dir);
    }
    let path = dialog.save_file()?;
    let path = ensure_extension(path);
    match save_to(doc, layers, &path) {
        Ok(status) => Some((path, status)),
        Err(e) => Some((path, format!("Save failed: {e}"))),
    }
}

/// Native "Open" dialog → load a bundle → rebuild a `Document` (+ painting). `None` if
/// the user cancelled.
pub fn open_dialog() -> Option<(Loaded, PathBuf, String)> {
    let path = rfd::FileDialog::new()
        .add_filter("SWEET project", &[BUNDLE_EXTENSION])
        .pick_file()?;
    match load_from(&path) {
        Ok(loaded) => {
            let status = format!("Opened {}", path.display());
            Some((loaded, path, status))
        }
        Err(e) => Some((
            Loaded {
                document: Document::default(),
                layers: Vec::new(),
            },
            path.clone(),
            format!("Open failed: {e}"),
        )),
    }
}

/// Decode one base64 PNG blob into a `LayerSave` with the given metadata.
fn decode_layer_blob(
    bundle: &ProjectBundle,
    blob_name: &str,
    name: String,
    visible: bool,
    opacity: f32,
    blend: suite_doc::BlendMode,
) -> Result<Option<LayerSave>, String> {
    let Some(b64) = bundle.blob(blob_name) else { return Ok(None) };
    let png = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("{blob_name} base64 decode failed: {e}"))?;
    let img = decode_png(&png).map_err(|e| format!("{blob_name} decode failed: {e}"))?;
    Ok(Some(LayerSave { rgba: img.rgba, size: img.size, name, visible, opacity, blend }))
}

/// Load a `.sweet` bundle from `path`. Fail-closed on a foreign file, a future
/// bundle/scene version, or a missing `main.visual.scene` document. Reads the multi-layer
/// stack if present, else falls back to the legacy single `main.paint.png` blob.
pub fn load_from(path: &Path) -> Result<Loaded, String> {
    let bundle = ProjectBundle::load(path).map_err(|e| e.to_string())?;
    let scene = bundle
        .document(MAIN_SCENE_ROLE)
        .ok_or_else(|| format!("bundle has no {MAIN_SCENE_ROLE} document"))?;
    let scene_json = serde_json::to_string(scene).map_err(|e| e.to_string())?;
    let document = Document::from_scene_json(&scene_json).map_err(|e| e.to_string())?;

    let mut layers: Vec<LayerSave> = Vec::new();
    if let Some(meta) = bundle.document(LAYERS_META_ROLE).and_then(|v| v.as_array()) {
        for (i, m) in meta.iter().enumerate() {
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("Layer").to_string();
            let visible = m.get("visible").and_then(|v| v.as_bool()).unwrap_or(true);
            let opacity = m.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            let blend = m
                .get("blend")
                .and_then(|v| serde_json::from_value::<suite_doc::BlendMode>(v.clone()).ok())
                .unwrap_or(suite_doc::BlendMode::Normal);
            if let Some(layer) = decode_layer_blob(
                &bundle,
                &format!("main.layer.{i}.png"),
                name,
                visible,
                opacity,
                blend,
            )? {
                layers.push(layer);
            }
        }
    } else if let Some(layer) =
        // Legacy single-painting projects → one Background layer.
        decode_layer_blob(&bundle, PAINT_BLOB, "Background".to_string(), true, 1.0, suite_doc::BlendMode::Normal)?
    {
        layers.push(layer);
    }

    Ok(Loaded { document, layers })
}

/// Native "Import Image" dialog → decode any common raster format → fit it (aspect-
/// preserving, white-padded) into a `canvas_size`² RGBA8 buffer ready for the paint canvas.
/// Returns the pixels + a status string. `None` if the user cancelled.
pub fn import_image_dialog(canvas_size: u32) -> Option<(Vec<u8>, String)> {
    let path = rfd::FileDialog::new()
        .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tga", "gif", "webp"])
        .pick_file()?;
    match import_image_from(&path, canvas_size) {
        Ok(rgba) => Some((rgba, format!("Imported {}", path.display()))),
        Err(e) => Some((Vec::new(), format!("Import failed: {e}"))),
    }
}

/// Decode `path` and fit it into a `size`² white canvas, aspect-preserved + centered.
pub fn import_image_from(path: &Path, size: u32) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let src = img.to_rgba8();
    let (sw, sh) = (src.width().max(1), src.height().max(1));

    // Scale to fit *within* the square (contain), preserving aspect.
    let scale = (size as f32 / sw as f32).min(size as f32 / sh as f32);
    let dw = ((sw as f32 * scale).round() as u32).clamp(1, size);
    let dh = ((sh as f32 * scale).round() as u32).clamp(1, size);
    let resized = image::imageops::resize(&src, dw, dh, image::imageops::FilterType::Lanczos3);

    // White, opaque background; blit the resized image centered.
    let mut canvas = vec![255u8; (size * size * 4) as usize];
    let ox = (size - dw) / 2;
    let oy = (size - dh) / 2;
    for y in 0..dh {
        for x in 0..dw {
            let p = resized.get_pixel(x, y).0; // [r,g,b,a]
            let cx = ox + x;
            let cy = oy + y;
            let di = ((cy * size + cx) * 4) as usize;
            // Source-over the (possibly transparent) pixel onto white.
            let a = p[3] as f32 / 255.0;
            for c in 0..3 {
                let over = p[c] as f32 * a + 255.0 * (1.0 - a);
                canvas[di + c] = over.round().clamp(0.0, 255.0) as u8;
            }
            canvas[di + 3] = 255;
        }
    }
    Ok(canvas)
}

/// Native "Export PNG" dialog → write `rgba` (`size`² RGBA8) as a PNG. Returns a status.
pub fn export_png_dialog(rgba: &[u8], size: u32) -> Option<String> {
    let path = rfd::FileDialog::new()
        .add_filter("PNG image", &["png"])
        .set_file_name("export.png")
        .save_file()?;
    let path = if path.extension().is_some() { path } else { path.with_extension("png") };
    let img = PaintImage { size, rgba: rgba.to_vec() };
    match encode_png(&img).and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string())) {
        Ok(()) => Some(format!("Exported {}", path.display())),
        Err(e) => Some(format!("Export failed: {e}")),
    }
}

fn encode_png(img: &PaintImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.size, img.size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&img.rgba)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn decode_png(bytes: &[u8]) -> Result<PaintImage, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let out_size = reader
        .output_buffer_size()
        .ok_or_else(|| "png output buffer size unavailable".to_string())?;
    let mut buf = vec![0u8; out_size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.width != info.height {
        return Err(format!(
            "paint image must be square, got {}x{}",
            info.width, info.height
        ));
    }
    // Normalize to RGBA8 if the PNG came in as RGB.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let mut v = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in buf[..info.buffer_size()].chunks_exact(3) {
                v.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            v
        }
        other => return Err(format!("unsupported paint color type {other:?}")),
    };
    Ok(PaintImage {
        size: info.width,
        rgba,
    })
}

fn ensure_extension(path: PathBuf) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()) == Some(BUNDLE_EXTENSION) {
        path
    } else {
        path.with_extension(BUNDLE_EXTENSION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use suite_doc::ObjectKind;

    #[test]
    fn scene_and_layers_round_trip_through_a_bundle() {
        let mut doc = Document::with_starter_scene();
        let id = doc.add(ObjectKind::Sphere, glam::Vec3::new(4.0, 0.0, 0.0));
        doc.set_selection(Some(id));
        let count = doc.object_count();

        // Two 4×4 layers with recognizable corner pixels + distinct metadata.
        let size = 4u32;
        let make = |c: [u8; 4]| {
            let mut v = vec![255u8; (size * size * 4) as usize];
            v[0..4].copy_from_slice(&c);
            v
        };
        let layers = vec![
            LayerSave { rgba: make([10, 20, 30, 255]), size, name: "Background".into(), visible: true, opacity: 1.0, blend: suite_doc::BlendMode::Normal },
            LayerSave { rgba: make([200, 100, 50, 128]), size, name: "Layer 2".into(), visible: false, opacity: 0.5, blend: suite_doc::BlendMode::Multiply },
        ];

        let path = std::env::temp_dir().join("sweet-visual-layers-roundtrip.sweet");
        save_to(&doc, &layers, &path).expect("save");
        let loaded = load_from(&path).expect("load");

        assert_eq!(loaded.document.object_count(), count);
        assert_eq!(loaded.layers.len(), 2, "both layers survive");
        assert_eq!(loaded.layers[0].name, "Background");
        assert_eq!(&loaded.layers[0].rgba[0..4], &[10, 20, 30, 255]);
        // Metadata + pixels of the second layer survive.
        assert_eq!(loaded.layers[1].name, "Layer 2");
        assert!(!loaded.layers[1].visible);
        assert!((loaded.layers[1].opacity - 0.5).abs() < 1e-4);
        assert_eq!(loaded.layers[1].blend, suite_doc::BlendMode::Multiply, "blend mode survives");
        assert_eq!(&loaded.layers[1].rgba[0..4], &[200, 100, 50, 128]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_single_paint_blob_loads_as_one_layer() {
        // A project saved by the pre-layers build (one main.paint.png) → one Background layer.
        let mut bundle = ProjectBundle::new("visual");
        let doc = Document::with_starter_scene();
        let scene: serde_json::Value =
            serde_json::from_str(&doc.to_scene_json().unwrap()).unwrap();
        bundle.put_document(MAIN_SCENE_ROLE, scene);
        let size = 4u32;
        let mut rgba = vec![255u8; (size * size * 4) as usize];
        rgba[0..4].copy_from_slice(&[7, 8, 9, 255]);
        let png = encode_png(&PaintImage { size, rgba }).unwrap();
        bundle.put_blob(PAINT_BLOB, base64::engine::general_purpose::STANDARD.encode(&png));
        let path = std::env::temp_dir().join("sweet-visual-legacy.sweet");
        bundle.save(&path).unwrap();

        let loaded = load_from(&path).expect("load legacy");
        assert_eq!(loaded.layers.len(), 1, "legacy paint → one layer");
        assert_eq!(loaded.layers[0].name, "Background");
        assert_eq!(&loaded.layers[0].rgba[0..4], &[7, 8, 9, 255]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_rejects_a_non_bundle_file() {
        let path = std::env::temp_dir().join("sweet-visual-not-a-bundle2.sweet");
        std::fs::write(&path, "this is not json").unwrap();
        assert!(load_from(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn import_image_fits_centered_on_white() {
        // A 4×2 solid-red image imported into a 16² canvas: scale = min(16/4,16/2)=4 →
        // 16×8, so it fills width and is letterboxed top/bottom with white.
        let mut img = image::RgbaImage::new(4, 2);
        for p in img.pixels_mut() {
            *p = image::Rgba([200, 30, 30, 255]);
        }
        let dir = std::env::temp_dir();
        let path = dir.join("sweet_import_test.png");
        img.save(&path).unwrap();

        let size = 16u32;
        let out = import_image_from(&path, size).unwrap();
        assert_eq!(out.len(), (size * size * 4) as usize);

        let at = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        // Center is the red image; top-row is white padding.
        let c = at(8, 8);
        assert!(c[0] > 150 && c[1] < 90 && c[2] < 90, "center should be red, got {c:?}");
        assert_eq!(at(8, 0), [255, 255, 255, 255], "top row is white padding");
        let _ = std::fs::remove_file(&path);
    }
}
