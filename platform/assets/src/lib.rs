//! # suite-assets — universal project format, asset library, interchange.
//!
//! **Not one schema for a reverb and a sculpt** — a shared *container* that references
//! domain-specific documents (`visual.scene` / `video.timeline` / `audio.session`), a
//! shared asset library, and strong cross-app interchange (the StudioLink / Dynamic-Link
//! / OTIO equivalent). This is what lets you start in one app and finish in another.
//! docs/03 §3.6, docs/02 §3.8.
//!
//! Phase "persistence" lite: the bundle is a single `.sweet` JSON file holding a manifest
//! + a `documents` map keyed by role. Document payloads are opaque `serde_json::Value`s —
//! the container deliberately does NOT know the visual/video/audio schemas, so it stays
//! domain-agnostic exactly as docs/02 §8 demands. Heavy payloads (tiles, meshes, media
//! proxies) move to a content-addressed `blobs/` side-store when they exist; today a
//! primitives-only scene embeds cleanly.

#![allow(dead_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// File-format tag + the bundle schema version. Bumped only on a breaking container
/// change — NOT when a domain document's own schema changes (those version independently).
pub const BUNDLE_FORMAT: &str = "sweet.bundle";
pub const BUNDLE_VERSION: u32 = 1;

/// The conventional default file extension for a SWEET project.
pub const BUNDLE_EXTENSION: &str = "sweet";

/// The universal bundle: a manifest + a `documents/` map (domain payloads). `assets/`
/// (shared library) and `blobs/` (content-addressed heavy payloads) join this struct
/// when the features that need them land. docs/03 §3.6.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectBundle {
    pub format: String,
    pub version: u32,
    /// Which app authored this bundle ("visual" / "video" / "audio"). Informational —
    /// any app can open any bundle and read the documents it understands.
    pub app: String,
    /// Domain documents keyed by role, e.g. `"main.visual.scene"`. Payloads are opaque
    /// to the container.
    pub documents: std::collections::BTreeMap<String, Value>,
    /// Heavy binary payloads keyed by name (raster tiles, baked images, media proxies),
    /// stored base64-encoded. The content-addressed `blobs/` store of docs/03 §3.6 in its
    /// embedded form — split to a sidecar directory when bundles get large. `default` so
    /// bundles written before blobs existed still parse.
    #[serde(default)]
    pub blobs: std::collections::BTreeMap<String, String>,
}

impl ProjectBundle {
    /// Start an empty bundle authored by `app`.
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            format: BUNDLE_FORMAT.to_string(),
            version: BUNDLE_VERSION,
            app: app.into(),
            documents: std::collections::BTreeMap::new(),
            blobs: std::collections::BTreeMap::new(),
        }
    }

    /// Attach (or replace) a domain document under `role`.
    pub fn put_document(&mut self, role: impl Into<String>, payload: Value) {
        self.documents.insert(role.into(), payload);
    }

    /// Borrow a document by role.
    pub fn document(&self, role: &str) -> Option<&Value> {
        self.documents.get(role)
    }

    /// Attach (or replace) a base64-encoded binary blob under `name`.
    pub fn put_blob(&mut self, name: impl Into<String>, base64_payload: impl Into<String>) {
        self.blobs.insert(name.into(), base64_payload.into());
    }

    /// Borrow a base64 blob by name.
    pub fn blob(&self, name: &str) -> Option<&str> {
        self.blobs.get(name).map(|s| s.as_str())
    }

    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> Result<String, BundleError> {
        serde_json::to_string_pretty(self).map_err(BundleError::Json)
    }

    /// Parse from JSON. Fail-closed on an unknown format tag or a future major version.
    pub fn from_json(json: &str) -> Result<Self, BundleError> {
        let bundle: ProjectBundle = serde_json::from_str(json).map_err(BundleError::Json)?;
        if bundle.format != BUNDLE_FORMAT {
            return Err(BundleError::UnknownFormat(bundle.format));
        }
        if bundle.version > BUNDLE_VERSION {
            return Err(BundleError::FutureVersion {
                found: bundle.version,
                supported: BUNDLE_VERSION,
            });
        }
        Ok(bundle)
    }

    /// Write the bundle to `path` (whole-file; an atomic temp-then-rename pass joins this
    /// when crash-safety matters — the COMPOSITOR Qt prototype's crash-safe save is the
    /// reference for that upgrade).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), BundleError> {
        let json = self.to_json()?;
        std::fs::write(path, json).map_err(BundleError::Io)
    }

    /// Read a bundle from `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, BundleError> {
        let json = std::fs::read_to_string(path).map_err(BundleError::Io)?;
        Self::from_json(&json)
    }
}

#[derive(Debug)]
pub enum BundleError {
    UnknownFormat(String),
    FutureVersion { found: u32, supported: u32 },
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFormat(s) => write!(f, "not a SWEET bundle (format tag: {s})"),
            Self::FutureVersion { found, supported } => {
                write!(
                    f,
                    "bundle version {found} is newer than supported {supported}"
                )
            }
            Self::Json(e) => write!(f, "bundle json error: {e}"),
            Self::Io(e) => write!(f, "bundle io error: {e}"),
        }
    }
}

impl std::error::Error for BundleError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_round_trips_with_an_opaque_document() {
        let mut bundle = ProjectBundle::new("visual");
        bundle.put_document(
            "main.visual.scene",
            serde_json::json!({ "schema": "sweet.visual.scene", "objects": [] }),
        );
        let json = bundle.to_json().unwrap();
        let reopened = ProjectBundle::from_json(&json).unwrap();
        assert_eq!(reopened.app, "visual");
        assert!(reopened.document("main.visual.scene").is_some());
        assert_eq!(json, reopened.to_json().unwrap());
    }

    #[test]
    fn rejects_foreign_files_and_future_versions() {
        assert!(matches!(
            ProjectBundle::from_json(
                r#"{"format":"something.else","version":1,"app":"x","documents":{}}"#
            ),
            Err(BundleError::UnknownFormat(_))
        ));
        let future = format!(
            r#"{{"format":"{}","version":{},"app":"visual","documents":{{}}}}"#,
            BUNDLE_FORMAT,
            BUNDLE_VERSION + 1
        );
        assert!(matches!(
            ProjectBundle::from_json(&future),
            Err(BundleError::FutureVersion { .. })
        ));
    }

    #[test]
    fn saves_and_loads_from_disk() {
        let path = std::env::temp_dir().join("sweet-assets-test.sweet");
        let mut bundle = ProjectBundle::new("visual");
        bundle.put_document("main.visual.scene", serde_json::json!({ "k": 1 }));
        bundle.save(&path).unwrap();
        let loaded = ProjectBundle::load(&path).unwrap();
        assert_eq!(
            loaded.document("main.visual.scene"),
            bundle.document("main.visual.scene")
        );
        let _ = std::fs::remove_file(&path);
    }
}
