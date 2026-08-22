use crate::error::IconError;
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, str::FromStr};

pub const CANVAS_SIZE: u32 = 1024;

#[derive(Debug, Clone)]
pub struct IconInput {
    pub filename: String,
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    Default,
    Dark,
    ClearLight,
    ClearDark,
    TintedLight,
    TintedDark,
}

impl Appearance {
    pub const ALL: [Self; 6] = [
        Self::Default,
        Self::Dark,
        Self::ClearLight,
        Self::ClearDark,
        Self::TintedLight,
        Self::TintedDark,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Dark => "dark",
            Self::ClearLight => "clear-light",
            Self::ClearDark => "clear-dark",
            Self::TintedLight => "tinted-light",
            Self::TintedDark => "tinted-dark",
        }
    }
}

impl fmt::Display for Appearance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Appearance {
    type Err = IconError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "default" => Ok(Self::Default),
            "dark" => Ok(Self::Dark),
            "clear-light" => Ok(Self::ClearLight),
            "clear-dark" => Ok(Self::ClearDark),
            "tinted-light" => Ok(Self::TintedLight),
            "tinted-dark" => Ok(Self::TintedDark),
            other => Err(IconError::InvalidImage(format!(
                "unknown appearance '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransformRequest {
    pub input: IconInput,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerArtifact {
    pub id: String,
    pub z_index: u8,
}

/// Icon Composer applies material to a group either per child layer or to the
/// flattened group as one piece of glass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum GroupMode {
    #[default]
    Individual,
    Combined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SpecularMode {
    Off,
    #[default]
    Automatic,
    Inside,
    Outside,
}

/// Runtime material knobs intentionally live beside the asset metadata, not
/// in source SVG artwork. Values are normalized to the inclusive 0..=1 range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MaterialSettings {
    #[serde(default = "default_true")]
    pub effects_enabled: bool,
    #[serde(default)]
    pub mode: GroupMode,
    #[serde(default)]
    pub specular: SpecularMode,
    #[serde(default)]
    pub blur: f32,
    #[serde(default)]
    pub refraction: [f32; 2],
    #[serde(default = "default_translucency")]
    pub translucency: f32,
    #[serde(default = "default_shadow")]
    pub shadow: f32,
}

impl Default for MaterialSettings {
    fn default() -> Self {
        Self {
            effects_enabled: true,
            mode: GroupMode::Individual,
            specular: SpecularMode::Automatic,
            blur: 0.0,
            refraction: [0.5, 0.5],
            translucency: default_translucency(),
            shadow: default_shadow(),
        }
    }
}

/// Optional per-appearance annotation. Absent fields deliberately inherit the
/// Default artwork, matching Icon Composer's annotation behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct AppearanceAnnotation {
    #[serde(default)]
    pub opacity: Option<f32>,
    #[serde(default)]
    pub effects_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialGroup {
    pub id: String,
    pub z_index: u8,
    pub layers: Vec<LayerArtifact>,
    #[serde(default)]
    pub material: MaterialSettings,
    #[serde(default)]
    pub dark: AppearanceAnnotation,
    #[serde(default)]
    pub mono: AppearanceAnnotation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IconDocument {
    pub background: LayerArtifact,
    pub groups: Vec<MaterialGroup>,
}

impl IconDocument {
    /// Legacy SVGs have one top-level foreground group per material surface.
    /// This keeps existing caches usable without a provider request.
    pub fn from_flat_layers(layers: &[LayerArtifact]) -> Self {
        let background = layers.first().cloned().unwrap_or(LayerArtifact {
            id: "background".to_owned(),
            z_index: 0,
        });
        let groups = layers
            .iter()
            .skip(1)
            .cloned()
            .map(|layer| MaterialGroup {
                id: layer.id.clone(),
                z_index: layer.z_index,
                layers: vec![layer],
                material: MaterialSettings::default(),
                dark: AppearanceAnnotation::default(),
                mono: AppearanceAnnotation::default(),
            })
            .collect();
        Self { background, groups }
    }

    pub fn layers(&self) -> Vec<LayerArtifact> {
        let mut layers = vec![self.background.clone()];
        for group in &self.groups {
            layers.extend(group.layers.clone());
        }
        layers
    }
}

fn default_true() -> bool {
    true
}

fn default_translucency() -> f32 {
    0.5
}

fn default_shadow() -> f32 {
    0.5
}

#[derive(Debug, Clone)]
pub struct TransformResult {
    pub source_sha256: String,
    pub layers: Vec<LayerArtifact>,
    pub svg_path: PathBuf,
    pub manifest_path: PathBuf,
}
