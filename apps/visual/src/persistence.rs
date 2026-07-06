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

/// A painted raster ready to embed: `width`×`height` (M5: no longer forced square),
/// row-major RGBA8 `rgba`.
pub struct PaintImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One layer's pixels + metadata for the `.sweet` layer stack.
pub struct LayerSave {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
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
            let png = encode_png(&PaintImage { width: l.width, height: l.height, rgba: l.rgba.clone() })
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
    Ok(Some(LayerSave { rgba: img.rgba, width: img.width, height: img.height, name, visible, opacity, blend }))
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

/// Native "Import Image" dialog → decode any common raster format → downscale (aspect-
/// preserving) only if it exceeds `max_dim` per axis. Returns `(width, height, rgba)` + a
/// status string. `None` if the user cancelled.
///
/// M5: the canvas takes the image's own aspect ratio — no more forcing it into a square
/// with white padding. `max_dim` just bounds VRAM for an oversized source image.
pub fn import_image_dialog(max_dim: u32) -> Option<((u32, u32), Vec<u8>, String)> {
    let path = rfd::FileDialog::new()
        .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tga", "gif", "webp"])
        .pick_file()?;
    match import_image_from(&path, max_dim) {
        Ok((w, h, rgba)) => Some(((w, h), rgba, format!("Imported {}", path.display()))),
        Err(e) => Some(((0, 0), Vec::new(), format!("Import failed: {e}"))),
    }
}

/// Decode `path`, downscaling (Lanczos3, aspect-preserved) only if it's larger than
/// `max_dim` on either axis. Returns the resulting `(width, height, rgba)` — the canvas the
/// caller creates should be exactly this size (M5: no padding into a forced square).
pub fn import_image_from(path: &Path, max_dim: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let src = img.to_rgba8();
    let (sw, sh) = (src.width().max(1), src.height().max(1));

    let scale = (max_dim as f32 / sw as f32).min(max_dim as f32 / sh as f32).min(1.0);
    let (dw, dh) = if scale < 1.0 {
        (
            ((sw as f32 * scale).round() as u32).max(1),
            ((sh as f32 * scale).round() as u32).max(1),
        )
    } else {
        (sw, sh)
    };
    let rgba = if (dw, dh) == (sw, sh) {
        src.into_raw()
    } else {
        image::imageops::resize(&src, dw, dh, image::imageops::FilterType::Lanczos3).into_raw()
    };
    Ok((dw, dh, rgba))
}

/// Native "Export PNG" dialog → write `rgba` (`width`×`height` RGBA8) as a PNG. Returns a status.
pub fn export_png_dialog(rgba: &[u8], width: u32, height: u32) -> Option<String> {
    let path = rfd::FileDialog::new()
        .add_filter("PNG image", &["png"])
        .set_file_name("export.png")
        .save_file()?;
    let path = if path.extension().is_some() { path } else { path.with_extension("png") };
    let img = PaintImage { width, height, rgba: rgba.to_vec() };
    match encode_png(&img).and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string())) {
        Ok(()) => Some(format!("Exported {}", path.display())),
        Err(e) => Some(format!("Export failed: {e}")),
    }
}

fn encode_png(img: &PaintImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.width, img.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&img.rgba)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Decode a PNG into RGBA8 at its own native dimensions (M5: any aspect ratio, not just square).
fn decode_png(bytes: &[u8]) -> Result<PaintImage, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let out_size = reader
        .output_buffer_size()
        .ok_or_else(|| "png output buffer size unavailable".to_string())?;
    let mut buf = vec![0u8; out_size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
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
        width: info.width,
        height: info.height,
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
            LayerSave { rgba: make([10, 20, 30, 255]), width: size, height: size, name: "Background".into(), visible: true, opacity: 1.0, blend: suite_doc::BlendMode::Normal },
            LayerSave { rgba: make([200, 100, 50, 128]), width: size, height: size, name: "Layer 2".into(), visible: false, opacity: 0.5, blend: suite_doc::BlendMode::Multiply },
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
        let png = encode_png(&PaintImage { width: size, height: size, rgba }).unwrap();
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

    /// M5: importing a non-square image under `max_dim` keeps its own native aspect ratio —
    /// no forced-square padding, unlike the pre-M5 behaviour.
    #[test]
    fn import_image_keeps_native_aspect_when_under_max_dim() {
        let mut img = image::RgbaImage::new(4, 2);
        for p in img.pixels_mut() {
            *p = image::Rgba([200, 30, 30, 255]);
        }
        let dir = std::env::temp_dir();
        let path = dir.join("sweet_import_test_aspect.png");
        img.save(&path).unwrap();

        let (w, h, rgba) = import_image_from(&path, 16).unwrap();
        assert_eq!((w, h), (4, 2), "canvas takes the image's own dimensions, not a forced square");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // Every pixel is the solid red source colour — no padding anywhere.
        for px in rgba.chunks_exact(4) {
            assert!(px[0] > 150 && px[1] < 90 && px[2] < 90, "no padding pixels, got {px:?}");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// An oversized image is downscaled to fit `max_dim`, preserving aspect ratio.
    #[test]
    fn import_image_downscales_when_over_max_dim() {
        let mut img = image::RgbaImage::new(40, 20); // 2:1 aspect
        for p in img.pixels_mut() {
            *p = image::Rgba([30, 200, 30, 255]);
        }
        let dir = std::env::temp_dir();
        let path = dir.join("sweet_import_test_downscale.png");
        img.save(&path).unwrap();

        let (w, h, rgba) = import_image_from(&path, 10).unwrap();
        assert_eq!((w, h), (10, 5), "downscaled to fit max_dim=10, aspect preserved (2:1)");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        let _ = std::fs::remove_file(&path);
    }
}
