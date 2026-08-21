use crate::{error::IconError, model::IconInput};
use std::{fs, path::Path};

pub fn read_input(path: &Path) -> Result<IconInput, IconError> {
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Err(IconError::EmptyInput);
    }

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("icon.png")
        .to_owned();
    let mime_type = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        other => return Err(IconError::UnsupportedFormat(other.to_owned())),
    };

    Ok(IconInput {
        filename,
        bytes,
        mime_type: mime_type.to_owned(),
    })
}

pub fn file_extension(filename: &str) -> &str {
    Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png")
}
