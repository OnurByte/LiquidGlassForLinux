use crate::{
    desktop::{DesktopApplication, DesktopTaskEvent, DesktopTaskState, application_output_name},
    error::IconError,
    manifest::{self, CanvasManifest, GeneratorManifest, Manifest, SCHEMA_VERSION, SourceManifest},
    model::{CANVAS_SIZE, IconInput, TransformRequest, TransformResult},
    normalize::normalize_to_png,
    openai::SvgProvider,
    prompt::PROMPT_VERSION,
    svg::{validate_svg, validate_svg_structure},
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    Missing,
    Current,
    Stale,
}

pub async fn transform_icon(
    request: TransformRequest,
    provider: &SvgProvider,
    cancelled: Arc<AtomicBool>,
) -> Result<TransformResult, IconError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(IconError::Cancelled);
    }
    let source_sha256 = manifest::sha256(&request.input.bytes);
    let normalized = normalize_to_png(&request.input.bytes, &request.input.mime_type)?;
    let normalized_input = IconInput {
        filename: "input.png".to_owned(),
        bytes: normalized,
        mime_type: "image/png".to_owned(),
    };
    let svg = provider
        .generate_svg(&normalized_input, Arc::clone(&cancelled))
        .await?;
    let layers = validate_svg(&svg)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(IconError::Cancelled);
    }

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        source: SourceManifest {
            filename: request.input.filename,
            sha256: source_sha256.clone(),
        },
        canvas: CanvasManifest {
            width: CANVAS_SIZE,
            height: CANVAS_SIZE,
            color_space: "sRGB".to_owned(),
            has_alpha: true,
        },
        svg: "icon.svg".to_owned(),
        layers: layers.clone(),
        generator: GeneratorManifest {
            provider: provider.provider_name().to_owned(),
            model: provider.model(),
            prompt_version: PROMPT_VERSION,
        },
    };
    write_conversion_atomically(&request.output_dir, svg.as_bytes(), &manifest)?;
    Ok(TransformResult {
        source_sha256,
        layers,
        svg_path: request.output_dir.join("icon.svg"),
        manifest_path: request.output_dir.join("icon-manifest.json"),
    })
}

pub fn cache_status(output_dir: &Path, source_bytes: &[u8]) -> CacheStatus {
    let Ok(manifest) = manifest::read_manifest(&output_dir.join("icon-manifest.json")) else {
        return CacheStatus::Missing;
    };
    if !output_dir.join(&manifest.svg).is_file() {
        return CacheStatus::Missing;
    }
    let Ok(svg) = fs::read_to_string(output_dir.join(&manifest.svg)) else {
        return CacheStatus::Missing;
    };
    if validate_svg_structure(&svg).is_err() {
        return CacheStatus::Missing;
    }
    if manifest.source.sha256 == manifest::sha256(source_bytes) {
        CacheStatus::Current
    } else {
        CacheStatus::Stale
    }
}

pub async fn transform_desktop_icons<F>(
    applications: &[DesktopApplication],
    output_dir: &Path,
    provider: &SvgProvider,
    report: F,
) -> Vec<TransformResult>
where
    F: FnMut(DesktopTaskEvent),
{
    transform_desktop_icons_with_options(
        applications,
        output_dir,
        provider,
        Arc::new(AtomicBool::new(false)),
        &HashSet::new(),
        report,
    )
    .await
}

pub async fn transform_desktop_icons_with_options<F>(
    applications: &[DesktopApplication],
    output_dir: &Path,
    provider: &SvgProvider,
    cancelled: Arc<AtomicBool>,
    force_ids: &HashSet<String>,
    mut report: F,
) -> Vec<TransformResult>
where
    F: FnMut(DesktopTaskEvent),
{
    let mut results = Vec::new();
    for (index, application) in applications.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            for application in &applications[index..] {
                report(event(
                    application,
                    DesktopTaskState::Stopped,
                    "stopped by user",
                    None,
                ));
            }
            break;
        }
        let Some(icon_path) = application.icon_path.as_ref() else {
            report(event(
                application,
                DesktopTaskState::Skipped,
                format!("icon not found: {}", application.icon_name),
                None,
            ));
            continue;
        };
        let input = match application.input() {
            Ok(input) => input,
            Err(error) => {
                report(event(
                    application,
                    DesktopTaskState::Failed,
                    error.to_string(),
                    None,
                ));
                continue;
            }
        };
        let application_output = output_dir
            .join("apps")
            .join(application_output_name(&application.id));
        if !force_ids.contains(&application.id) {
            match cache_status(&application_output, &input.bytes) {
                CacheStatus::Current => {
                    report(event(
                        application,
                        DesktopTaskState::Converted,
                        "already converted",
                        None,
                    ));
                    continue;
                }
                CacheStatus::Stale => {
                    report(event(
                        application,
                        DesktopTaskState::Stale,
                        "source changed; reconvert manually",
                        None,
                    ));
                    continue;
                }
                CacheStatus::Missing => {}
            }
        }
        report(event(application, DesktopTaskState::Queued, "queued", None));
        report(event(
            application,
            DesktopTaskState::Processing,
            format!("processing {}", icon_path.display()),
            None,
        ));
        let result = transform_icon(
            TransformRequest {
                input,
                output_dir: application_output,
            },
            provider,
            Arc::clone(&cancelled),
        )
        .await;
        match result {
            Ok(result) => {
                report(event(
                    application,
                    DesktopTaskState::Completed,
                    result.manifest_path.display().to_string(),
                    Some(result.clone()),
                ));
                results.push(result);
            }
            Err(IconError::Cancelled) => {
                report(event(
                    application,
                    DesktopTaskState::Stopped,
                    "stopped by user",
                    None,
                ));
                for application in &applications[index + 1..] {
                    report(event(
                        application,
                        DesktopTaskState::Stopped,
                        "stopped by user",
                        None,
                    ));
                }
                break;
            }
            Err(error) => {
                let provider_failed = matches!(
                    error,
                    IconError::Api { .. }
                        | IconError::CodexUnavailable(_)
                        | IconError::MissingApiKey
                );
                report(event(
                    application,
                    DesktopTaskState::Failed,
                    error.to_string(),
                    None,
                ));
                if provider_failed {
                    for application in &applications[index + 1..] {
                        report(event(
                            application,
                            DesktopTaskState::Stopped,
                            "provider unavailable",
                            None,
                        ));
                    }
                    break;
                }
            }
        }
    }
    results
}

fn event(
    application: &DesktopApplication,
    state: DesktopTaskState,
    message: impl Into<String>,
    result: Option<TransformResult>,
) -> DesktopTaskEvent {
    DesktopTaskEvent {
        application_id: application.id.clone(),
        application_name: application.name.clone(),
        state,
        message: message.into(),
        result,
    }
}

fn write_conversion_atomically(
    output_dir: &Path,
    svg: &[u8],
    manifest: &Manifest,
) -> Result<(), IconError> {
    let parent = output_dir
        .parent()
        .ok_or_else(|| IconError::Manifest("output directory has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".liquid-glass-")
        .tempdir_in(parent)?;
    fs::write(temporary.path().join("icon.svg"), svg)?;
    manifest::write_manifest(&temporary.path().join("icon-manifest.json"), manifest)?;

    let backup = backup_path(output_dir);
    let had_previous = output_dir.exists();
    if had_previous {
        fs::rename(output_dir, &backup)?;
    }
    if let Err(error) = fs::rename(temporary.path(), output_dir) {
        if had_previous {
            let _ = fs::rename(&backup, output_dir);
        }
        return Err(error.into());
    }
    if had_previous {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}

fn backup_path(output_dir: &Path) -> PathBuf {
    let name = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("icon");
    let parent = output_dir.parent().unwrap_or_else(|| Path::new("."));
    let mut index = 0u32;
    loop {
        let path = parent.join(format!(".{name}.backup-{index}"));
        if !path.exists() {
            return path;
        }
        index += 1;
    }
}

pub fn ensure_output_dir(path: &Path) -> Result<(), IconError> {
    fs::create_dir_all(path).map_err(IconError::from)
}
