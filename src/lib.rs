pub mod desktop;
pub mod error;
pub mod icon_install;
pub mod input;
pub mod manifest;
pub mod model;
pub mod normalize;
pub mod openai;
pub mod pipeline;
pub mod prompt;
pub mod renderer;
pub mod svg;

pub use desktop::{AppCategory, DesktopApplication, DesktopTaskEvent, DesktopTaskState};
pub use error::IconError;
pub use model::{
    Appearance, AppearanceAnnotation, GroupMode, IconDocument, IconInput, MaterialGroup,
    MaterialSettings, SpecularMode, TransformRequest, TransformResult,
};
pub use openai::{CodexExecProvider, OpenAiResponsesClient, SvgProvider};
pub use pipeline::transform_icon;

/// The checked-out repository is the default community asset archive while
/// developing this project. Packaged binaries have no repository target unless
/// the caller explicitly sets `LIQUID_GLASS_ASSET_DIR`.
pub fn repository_assets_dir() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("LIQUID_GLASS_ASSET_DIR")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
    {
        return Some(path);
    }
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join(".git")
        .exists()
        .then(|| manifest_dir.join("assets/icons"))
}

pub fn default_output_dir() -> std::path::PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    data_home.join("liquid-glass-icon/out")
}
