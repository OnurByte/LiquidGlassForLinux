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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerArtifact {
    pub id: String,
    pub z_index: u8,
}

#[derive(Debug, Clone)]
pub struct TransformResult {
    pub source_sha256: String,
    pub layers: Vec<LayerArtifact>,
    pub svg_path: PathBuf,
    pub manifest_path: PathBuf,
}
