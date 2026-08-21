use thiserror::Error;

#[derive(Debug, Error)]
pub enum IconError {
    #[error("input file is empty")]
    EmptyInput,
    #[error("unsupported image format: {0}")]
    UnsupportedFormat(String),
    #[error("invalid image: {0}")]
    InvalidImage(String),
    #[error("invalid SVG: {0}")]
    InvalidSvg(String),
    #[error("OpenAI API key is not set; enter it in the GUI or export OPENAI_API_KEY")]
    MissingApiKey,
    #[error("OpenAI API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("AI provider returned no SVG data")]
    EmptyApiResponse,
    #[error("Codex CLI is unavailable: {0}")]
    CodexUnavailable(String),
    #[error("conversion stopped by user")]
    Cancelled,
    #[error("application {application} has no resolvable icon: {icon}")]
    MissingDesktopIcon { application: String, icon: String },
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
