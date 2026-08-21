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
pub use model::{Appearance, IconInput, TransformRequest, TransformResult};
pub use openai::{CodexExecProvider, OpenAiResponsesClient, SvgProvider};
pub use pipeline::transform_icon;
