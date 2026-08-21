use crate::{error::IconError, model::IconInput, prompt::SVG_PROMPT};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::{
    env, fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";
pub const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const CODEX_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub enum SvgProvider {
    Codex(CodexExecProvider),
    Responses(OpenAiResponsesClient),
}

impl SvgProvider {
    pub fn preflight(&self) -> Result<(), IconError> {
        match self {
            Self::Codex(provider) => provider.preflight(),
            Self::Responses(_) => Ok(()),
        }
    }

    pub async fn generate_svg(
        &self,
        input: &IconInput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, IconError> {
        match self {
            Self::Codex(provider) => provider.generate_svg(input, cancelled).await,
            Self::Responses(provider) => provider.generate_svg(input, cancelled).await,
        }
    }

    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Codex(_) => "codex-exec",
            Self::Responses(_) => "responses-api",
        }
    }

    pub fn model(&self) -> Option<String> {
        match self {
            Self::Codex(provider) => Some(provider.model.clone()),
            Self::Responses(provider) => Some(provider.model.clone()),
        }
    }
}

#[derive(Clone)]
pub struct CodexExecProvider {
    executable: PathBuf,
    model: String,
}

impl Default for CodexExecProvider {
    fn default() -> Self {
        Self {
            executable: discover_executable(),
            model: DEFAULT_MODEL.to_owned(),
        }
    }
}

impl CodexExecProvider {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            model: DEFAULT_MODEL.to_owned(),
        }
    }

    pub fn command_args(input_path: &str, schema_path: &str, output_path: &str) -> Vec<String> {
        Self::command_args_for_model(DEFAULT_MODEL, input_path, schema_path, output_path)
    }

    pub fn command_args_for_model(
        model: &str,
        input_path: &str,
        schema_path: &str,
        output_path: &str,
    ) -> Vec<String> {
        vec![
            "exec".into(),
            "--model".into(),
            model.into(),
            "--ephemeral".into(),
            "--ignore-user-config".into(),
            "--sandbox".into(),
            "read-only".into(),
            "--skip-git-repo-check".into(),
            "--image".into(),
            input_path.into(),
            "--output-schema".into(),
            schema_path.into(),
            "--output-last-message".into(),
            output_path.into(),
            SVG_PROMPT.into(),
        ]
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = normalize_model(model);
        self
    }

    pub fn preflight(&self) -> Result<(), IconError> {
        let status = Command::new(&self.executable)
            .args(["exec", "--help"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| IconError::CodexUnavailable(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(IconError::CodexUnavailable(
                "codex exec --help failed".to_owned(),
            ))
        }
    }

    async fn generate_svg(
        &self,
        input: &IconInput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, IconError> {
        let executable = self.executable.clone();
        let model = self.model.clone();
        let bytes = input.bytes.clone();
        tokio::task::spawn_blocking(move || {
            if cancelled.load(Ordering::Relaxed) {
                return Err(IconError::Cancelled);
            }
            let directory = tempfile::tempdir()?;
            let input_path = directory.path().join("input.png");
            let schema_path = directory.path().join("svg-response.schema.json");
            let output_path = directory.path().join("response.json");
            let stderr_path = directory.path().join("stderr.log");
            fs::write(&input_path, bytes)?;
            fs::write(
                &schema_path,
                serde_json::to_vec_pretty(&svg_response_schema())?,
            )?;

            let input_arg = input_path.to_string_lossy().into_owned();
            let schema_arg = schema_path.to_string_lossy().into_owned();
            let output_arg = output_path.to_string_lossy().into_owned();
            let stderr_file = fs::File::create(&stderr_path)?;
            let mut command = Command::new(&executable);
            command
                .args(Self::command_args_for_model(
                    &model,
                    &input_arg,
                    &schema_arg,
                    &output_arg,
                ))
                .current_dir(directory.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(stderr_file));
            #[cfg(unix)]
            command.process_group(0);
            let mut child = command
                .spawn()
                .map_err(|error| IconError::CodexUnavailable(error.to_string()))?;
            let started = Instant::now();

            let status = loop {
                if cancelled.load(Ordering::Relaxed) {
                    terminate_child(&mut child);
                    return Err(IconError::Cancelled);
                }
                if started.elapsed() >= CODEX_TIMEOUT {
                    terminate_child(&mut child);
                    return Err(IconError::CodexUnavailable(format!(
                        "codex exec timed out after {} seconds",
                        CODEX_TIMEOUT.as_secs()
                    )));
                }
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            if !status.success() {
                let stderr = fs::read_to_string(stderr_path).unwrap_or_default();
                return Err(IconError::CodexUnavailable(stderr.trim().to_owned()));
            }
            parse_svg_response(&fs::read(&output_path)?)
        })
        .await
        .map_err(|error| IconError::CodexUnavailable(error.to_string()))?
    }
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        if pid > 1 {
            // The child is placed in its own process group before spawn, so this
            // also terminates the Node wrapper and the vendor Codex process.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn normalize_model(model: impl Into<String>) -> String {
    let model = model.into();
    let model = model.trim();
    if model.is_empty() {
        DEFAULT_MODEL.to_owned()
    } else {
        model.to_owned()
    }
}

fn discover_executable() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path).map(|directory| directory.join("codex")));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.extend(
            [
                ".bun/bin/codex",
                ".local/bin/codex",
                ".npm-global/bin/codex",
            ]
            .into_iter()
            .map(|relative| home.join(relative)),
        );
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("codex"))
}

#[derive(Clone)]
pub struct OpenAiResponsesClient {
    client: Client,
    api_key: String,
    endpoint: Url,
    model: String,
}

impl OpenAiResponsesClient {
    pub fn from_env() -> Result<Self, IconError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| IconError::MissingApiKey)?;
        Self::from_api_key(api_key)
    }

    pub fn from_api_key(api_key: impl Into<String>) -> Result<Self, IconError> {
        Self::new(api_key, RESPONSES_ENDPOINT)
    }

    pub fn new(api_key: impl Into<String>, endpoint: &str) -> Result<Self, IconError> {
        Self::new_with_model(api_key, endpoint, DEFAULT_MODEL)
    }

    pub fn new_with_model(
        api_key: impl Into<String>,
        endpoint: &str,
        model: impl Into<String>,
    ) -> Result<Self, IconError> {
        let api_key = api_key.into().trim().to_owned();
        if api_key.is_empty() {
            return Err(IconError::MissingApiKey);
        }
        Ok(Self {
            client: Client::new(),
            api_key,
            endpoint: Url::parse(endpoint).map_err(|error| IconError::Api {
                status: 0,
                message: error.to_string(),
            })?,
            model: normalize_model(model),
        })
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = normalize_model(model);
        self
    }

    async fn generate_svg(
        &self,
        input: &IconInput,
        cancelled: Arc<AtomicBool>,
    ) -> Result<String, IconError> {
        if cancelled.load(Ordering::Relaxed) {
            return Err(IconError::Cancelled);
        }
        let image_url = format!(
            "data:{};base64,{}",
            input.mime_type,
            STANDARD.encode(&input.bytes)
        );
        let request = async {
            let response = self
                .client
                .post(self.endpoint.clone())
                .bearer_auth(&self.api_key)
                .json(&json!({
                "model": self.model,
                "store": false,
                "max_output_tokens": 32768,
                "reasoning": { "effort": "medium" },
                "input": [{
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": SVG_PROMPT },
                        { "type": "input_image", "image_url": image_url, "detail": "high" }
                    ]
                }],
                "text": { "format": {
                    "type": "json_schema",
                    "name": "layered_icon_svg",
                    "strict": true,
                    "schema": svg_response_schema()
                }}
                }))
                .send()
                .await
                .map_err(|error| IconError::Api {
                    status: 0,
                    message: error.to_string(),
                })?;
            let status = response.status();
            let body = response.bytes().await.map_err(|error| IconError::Api {
                status: status.as_u16(),
                message: error.to_string(),
            })?;
            Ok::<_, IconError>((status, body))
        };
        let (status, body) = tokio::select! {
            response = request => response?,
            _ = wait_for_cancel(Arc::clone(&cancelled)) => return Err(IconError::Cancelled),
        };
        if !status.is_success() {
            let message = serde_json::from_slice::<ApiError>(&body)
                .ok()
                .and_then(|error| error.error.and_then(|body| body.message))
                .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
            return Err(IconError::Api {
                status: status.as_u16(),
                message,
            });
        }
        let response: ResponsesResponse = serde_json::from_slice(&body)?;
        let text = response
            .output
            .into_iter()
            .flat_map(|item| item.content)
            .find(|content| content.kind == "output_text")
            .and_then(|content| content.text)
            .ok_or(IconError::EmptyApiResponse)?;
        parse_svg_response(text.as_bytes())
    }
}

async fn wait_for_cancel(cancelled: Arc<AtomicBool>) {
    while !cancelled.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn svg_response_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["svg"],
        "properties": { "svg": { "type": "string", "minLength": 100 } }
    })
}

fn parse_svg_response(bytes: &[u8]) -> Result<String, IconError> {
    let response: SvgResponse = serde_json::from_slice(bytes)?;
    if response.svg.trim().is_empty() {
        return Err(IconError::EmptyApiResponse);
    }
    Ok(response.svg)
}

#[derive(Deserialize)]
struct SvgResponse {
    svg: String,
}

#[derive(Deserialize)]
struct ResponsesResponse {
    output: Vec<ResponseOutput>,
}

#[derive(Deserialize)]
struct ResponseOutput {
    #[serde(default)]
    content: Vec<ResponseContent>,
}

#[derive(Deserialize)]
struct ResponseContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
    error: Option<ApiErrorBody>,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    message: Option<String>,
}
