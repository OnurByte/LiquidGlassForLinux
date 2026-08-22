use crate::{
    error::IconError,
    model::{IconDocument, LayerArtifact},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub source: SourceManifest,
    pub canvas: CanvasManifest,
    pub svg: String,
    pub layers: Vec<LayerArtifact>,
    /// v4 stores Icon Composer-style material groups. Older manifests retain
    /// their flat layer list and are upgraded lazily by `document()`.
    #[serde(default)]
    pub document: Option<IconDocument>,
    pub generator: GeneratorManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceManifest {
    pub filename: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasManifest {
    pub width: u32,
    pub height: u32,
    pub color_space: String,
    pub has_alpha: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorManifest {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub prompt_version: u32,
}

pub const SCHEMA_VERSION: u32 = 4;

pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), IconError> {
    let json = serde_json::to_vec_pretty(manifest)?;
    fs::write(path, json).map_err(IconError::from)
}

pub fn read_manifest(path: &Path) -> Result<Manifest, IconError> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(path)?)?;
    if !matches!(manifest.schema_version, 2 | 3 | SCHEMA_VERSION) || manifest.svg != "icon.svg" {
        return Err(IconError::Manifest(
            "unsupported manifest version".to_owned(),
        ));
    }
    if manifest.schema_version == SCHEMA_VERSION && manifest.document.is_none() {
        return Err(IconError::Manifest(
            "v4 manifest is missing its material document".to_owned(),
        ));
    }
    Ok(manifest)
}

impl Manifest {
    pub fn document(&self) -> IconDocument {
        self.document
            .clone()
            .unwrap_or_else(|| IconDocument::from_flat_layers(&self.layers))
    }
}
