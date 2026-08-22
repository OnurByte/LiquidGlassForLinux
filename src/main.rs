use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use liquid_glass_icon::{
    Appearance, DesktopApplication, default_output_dir,
    desktop::{DesktopTaskEvent, application_output_name, discover_desktop_applications},
    icon_install::{IconInstaller, ManagedIconHealth},
    openai::{CodexExecProvider, DEFAULT_MODEL, OpenAiResponsesClient, SvgProvider},
    pipeline::{
        CacheStatus, archive_cached_conversion, cache_status,
        transform_desktop_icons_with_options_and_assets,
    },
    renderer::{GlassRenderer, RenderSettings},
    repository_assets_dir,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

const SCHEMA_VERSION: &str = "liquid-glass.control.v1";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderArg {
    Codex,
    Api,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AppearanceArg {
    Default,
    Dark,
    ClearLight,
    ClearDark,
    TintedLight,
    TintedDark,
}

impl From<AppearanceArg> for Appearance {
    fn from(value: AppearanceArg) -> Self {
        match value {
            AppearanceArg::Default => Self::Default,
            AppearanceArg::Dark => Self::Dark,
            AppearanceArg::ClearLight => Self::ClearLight,
            AppearanceArg::ClearDark => Self::ClearDark,
            AppearanceArg::TintedLight => Self::TintedLight,
            AppearanceArg::TintedDark => Self::TintedDark,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "liquid-glass-icon",
    about = "Discover, convert and manage Liquid Glass application icons"
)]
struct Args {
    #[arg(long, global = true, help = "Emit compact versioned JSON")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Discover,
    Status {
        #[arg(long, value_name = "DESKTOP_ID")]
        desktop_id: Vec<String>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Convert {
        #[arg(long, value_name = "DESKTOP_ID", required = true)]
        desktop_id: Vec<String>,
        #[arg(long, value_enum, default_value_t = ProviderArg::Codex)]
        provider: ProviderArg,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Apply {
        #[arg(long, value_name = "DESKTOP_ID")]
        desktop_id: Vec<String>,
        #[arg(long, conflicts_with = "desktop_id")]
        managed: bool,
        #[arg(long, value_enum, default_value_t = AppearanceArg::Default)]
        appearance: AppearanceArg,
        #[arg(long, value_name = "RRGGBB")]
        accent: Option<String>,
        #[arg(long, value_name = "RRGGBB")]
        background: Option<String>,
        #[arg(long, value_name = "PERCENT")]
        foreground_opacity: Option<f32>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Repair {
        #[arg(long, value_name = "DESKTOP_ID")]
        desktop_id: Vec<String>,
        #[arg(long, conflicts_with = "desktop_id")]
        managed: bool,
        #[arg(long, value_enum, default_value_t = AppearanceArg::Default)]
        appearance: AppearanceArg,
        #[arg(long, value_name = "RRGGBB")]
        accent: Option<String>,
        #[arg(long, value_name = "RRGGBB")]
        background: Option<String>,
        #[arg(long, value_name = "PERCENT")]
        foreground_opacity: Option<f32>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Archive {
        #[arg(long, value_name = "DESKTOP_ID")]
        desktop_id: Vec<String>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        asset_dir: Option<PathBuf>,
    },
    Restore {
        #[arg(long, value_name = "DESKTOP_ID", required = true)]
        desktop_id: Vec<String>,
    },
}

#[derive(Serialize)]
struct Envelope<T> {
    schema_version: &'static str,
    operation: &'static str,
    data: T,
}

#[derive(Serialize)]
struct ApplicationRecord {
    desktop_id: String,
    name: String,
    category: liquid_glass_icon::AppCategory,
    icon_name: String,
    icon_path: Option<String>,
}

#[derive(Serialize)]
struct StatusRecord {
    desktop_id: String,
    name: String,
    cache: &'static str,
    managed: bool,
    health: Option<&'static str>,
}

#[derive(Serialize)]
struct OperationRecord {
    desktop_id: String,
    status: &'static str,
    message: String,
}

#[derive(Clone, Copy)]
struct RenderOptions<'a> {
    appearance: AppearanceArg,
    accent: Option<&'a str>,
    background: Option<&'a str>,
    foreground_opacity: Option<f32>,
    output: Option<&'a Path>,
}

impl RenderOptions<'_> {
    fn settings(self) -> anyhow::Result<RenderSettings> {
        let mut settings = RenderSettings {
            appearance: self.appearance.into(),
            ..Default::default()
        };
        if let Some(accent) = self.accent {
            settings.accent = parse_accent(accent)?;
        }
        if let Some(background) = self.background {
            settings.background = Some(parse_accent(background)?);
        }
        if let Some(foreground_opacity) = self.foreground_opacity {
            settings.foreground_opacity = parse_foreground_opacity(foreground_opacity)?;
        }
        Ok(settings)
    }

    fn output(self) -> PathBuf {
        self.output
            .map(Path::to_path_buf)
            .unwrap_or_else(default_output_dir)
    }
}

#[derive(Serialize)]
struct ConvertData {
    provider_request_made: bool,
    converted: usize,
    events: Vec<EventRecord>,
}

#[derive(Serialize)]
struct EventRecord {
    desktop_id: String,
    name: String,
    state: String,
    message: String,
}

impl From<DesktopTaskEvent> for EventRecord {
    fn from(event: DesktopTaskEvent) -> Self {
        Self {
            desktop_id: event.application_id,
            name: event.application_name,
            state: event.state.as_str().to_owned(),
            message: event.message,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Discover => discover(args.json),
        Command::Status { desktop_id, output } => status(&desktop_id, output.as_deref(), args.json),
        Command::Convert {
            desktop_id,
            provider,
            model,
            output,
        } => convert(&desktop_id, provider, model, output.as_deref(), args.json).await,
        Command::Apply {
            desktop_id,
            managed,
            appearance,
            accent,
            background,
            foreground_opacity,
            output,
        } => {
            let render = RenderOptions {
                appearance,
                accent: accent.as_deref(),
                background: background.as_deref(),
                foreground_opacity,
                output: output.as_deref(),
            };
            apply(desktop_id, managed, render, args.json).await
        }
        Command::Restore { desktop_id } => restore(&desktop_id, args.json),
        Command::Repair {
            desktop_id,
            managed,
            appearance,
            accent,
            background,
            foreground_opacity,
            output,
        } => {
            let render = RenderOptions {
                appearance,
                accent: accent.as_deref(),
                background: background.as_deref(),
                foreground_opacity,
                output: output.as_deref(),
            };
            repair(desktop_id, managed, render, args.json).await
        }
        Command::Archive {
            desktop_id,
            output,
            asset_dir,
        } => archive(&desktop_id, output.as_deref(), asset_dir, args.json),
    }
}

fn discover(compact: bool) -> anyhow::Result<()> {
    let records = discover_desktop_applications()
        .into_iter()
        .map(|application| ApplicationRecord {
            desktop_id: application.id,
            name: application.name,
            category: application.category,
            icon_name: application.icon_name,
            icon_path: application.icon_path.map(|path| path.display().to_string()),
        })
        .collect::<Vec<_>>();
    emit("discover", records, compact)
}

fn status(ids: &[String], output: Option<&Path>, compact: bool) -> anyhow::Result<()> {
    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(default_output_dir);
    let applications = select_applications(discover_desktop_applications(), ids)?;
    let installer = IconInstaller::default();
    let records = applications
        .into_iter()
        .map(|application| StatusRecord {
            cache: application_cache_status(&application, &output),
            managed: installer.is_managed(&application.id),
            health: installer
                .health(&application.id)
                .ok()
                .flatten()
                .map(ManagedIconHealth::as_str),
            desktop_id: application.id,
            name: application.name,
        })
        .collect::<Vec<_>>();
    emit("status", records, compact)
}

async fn repair(
    mut ids: Vec<String>,
    managed: bool,
    render: RenderOptions<'_>,
    compact: bool,
) -> anyhow::Result<()> {
    let installer = IconInstaller::default();
    if managed {
        ids = installer.managed_ids()?;
    }
    if ids.is_empty() {
        anyhow::bail!("provide --desktop-id or --managed");
    }
    let output = render.output();
    let settings = render.settings()?;
    let mut renderer = GlassRenderer::new().await?;
    let mut records = Vec::new();
    for desktop_id in unique_ids(&ids) {
        match installer.health(desktop_id)? {
            None => records.push(OperationRecord {
                desktop_id: desktop_id.to_owned(),
                status: "not-managed",
                message: "no managed state existed".to_owned(),
            }),
            Some(ManagedIconHealth::Healthy) => records.push(OperationRecord {
                desktop_id: desktop_id.to_owned(),
                status: "healthy",
                message: "launcher and generated icon files are intact".to_owned(),
            }),
            Some(ManagedIconHealth::UserModified) => records.push(OperationRecord {
                desktop_id: desktop_id.to_owned(),
                status: "skipped",
                message: "user desktop entry changed; refusing to overwrite it".to_owned(),
            }),
            Some(ManagedIconHealth::Repairable) => {
                let svg_path = output
                    .join("apps")
                    .join(application_output_name(desktop_id))
                    .join("icon.svg");
                let svg = fs::read_to_string(&svg_path)
                    .with_context(|| format!("read cached SVG for {desktop_id}"))?;
                installer.repair_cached_svg(desktop_id, &svg, &mut renderer, settings)?;
                records.push(OperationRecord {
                    desktop_id: desktop_id.to_owned(),
                    status: "repaired",
                    message: "reinstalled from local cache without an AI request".to_owned(),
                });
            }
        }
    }
    emit("repair", records, compact)
}

fn archive(
    ids: &[String],
    output: Option<&Path>,
    asset_dir: Option<PathBuf>,
    compact: bool,
) -> anyhow::Result<()> {
    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(default_output_dir)
        .join("apps");
    let asset_dir = asset_dir
        .or_else(repository_assets_dir)
        .context("no repository archive found; set LIQUID_GLASS_ASSET_DIR or --asset-dir")?;
    let names = if ids.is_empty() {
        let mut names = fs::read_dir(&output)
            .with_context(|| format!("read converted icon cache at {}", output.display()))?
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()?
                    .is_dir()
                    .then_some(entry.file_name())
            })
            .filter_map(|name| name.into_string().ok())
            .collect::<Vec<_>>();
        names.sort();
        names
    } else {
        unique_ids(ids)
            .into_iter()
            .map(application_output_name)
            .collect()
    };
    let mut records = Vec::with_capacity(names.len());
    for name in names {
        match archive_cached_conversion(&asset_dir, &name, &output.join(&name)) {
            Ok(destination) => records.push(OperationRecord {
                desktop_id: name,
                status: "archived",
                message: destination.display().to_string(),
            }),
            Err(error) => records.push(OperationRecord {
                desktop_id: name,
                status: "skipped",
                message: error.to_string(),
            }),
        }
    }
    emit("archive", records, compact)
}

async fn convert(
    ids: &[String],
    provider: ProviderArg,
    model: String,
    output: Option<&Path>,
    compact: bool,
) -> anyhow::Result<()> {
    let applications = select_applications(discover_desktop_applications(), ids)?;
    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(default_output_dir);
    let provider = match provider {
        ProviderArg::Codex => SvgProvider::Codex(CodexExecProvider::default().with_model(model)),
        ProviderArg::Api => {
            SvgProvider::Responses(OpenAiResponsesClient::from_env()?.with_model(model))
        }
    };
    let force_ids = applications
        .iter()
        .map(|application| application.id.clone())
        .collect::<HashSet<_>>();
    let asset_dir = repository_assets_dir();
    let mut events = Vec::new();
    let results = transform_desktop_icons_with_options_and_assets(
        &applications,
        &output,
        &provider,
        Arc::new(AtomicBool::new(false)),
        &force_ids,
        asset_dir.as_deref(),
        |event| events.push(EventRecord::from(event)),
    )
    .await;
    emit(
        "convert",
        ConvertData {
            provider_request_made: events.iter().any(|event| event.state == "processing"),
            converted: results.len(),
            events,
        },
        compact,
    )
}

async fn apply(
    mut ids: Vec<String>,
    managed: bool,
    render: RenderOptions<'_>,
    compact: bool,
) -> anyhow::Result<()> {
    let installer = IconInstaller::default();
    if managed {
        ids = installer.managed_ids()?;
    }
    if ids.is_empty() {
        anyhow::bail!("provide --desktop-id or --managed");
    }
    let applications = select_applications(discover_desktop_applications(), &ids)?;
    let output = render.output();
    let settings = render.settings()?;
    let mut renderer = GlassRenderer::new().await?;
    let mut records = Vec::with_capacity(applications.len());
    for application in applications {
        let application_output = app_output(&output, &application);
        let input = match application.input() {
            Ok(input) => input,
            Err(error) => {
                records.push(OperationRecord {
                    desktop_id: application.id,
                    status: "skipped",
                    message: error.to_string(),
                });
                continue;
            }
        };
        if !matches!(
            cache_status(&application_output, &input.bytes),
            CacheStatus::Current | CacheStatus::Legacy
        ) {
            records.push(OperationRecord {
                desktop_id: application.id,
                status: "skipped",
                message: "current cache unavailable; run convert explicitly".to_owned(),
            });
            continue;
        }
        let svg = fs::read_to_string(application_output.join("icon.svg"))
            .with_context(|| format!("read cached SVG for {}", application.id))?;
        installer.apply_svg(&application, &svg, &mut renderer, settings)?;
        records.push(OperationRecord {
            desktop_id: application.id,
            status: "applied",
            message: "installed from local cache without an AI request".to_owned(),
        });
    }
    emit("apply", records, compact)
}

fn restore(ids: &[String], compact: bool) -> anyhow::Result<()> {
    let installer = IconInstaller::default();
    let mut records = Vec::with_capacity(ids.len());
    for desktop_id in unique_ids(ids) {
        let managed = installer.is_managed(desktop_id);
        installer.restore(desktop_id)?;
        records.push(OperationRecord {
            desktop_id: desktop_id.to_owned(),
            status: if managed { "restored" } else { "not-managed" },
            message: if managed {
                "restored the original user launcher state".to_owned()
            } else {
                "no managed state existed".to_owned()
            },
        });
    }
    emit("restore", records, compact)
}

fn select_applications(
    applications: Vec<DesktopApplication>,
    ids: &[String],
) -> anyhow::Result<Vec<DesktopApplication>> {
    if ids.is_empty() {
        return Ok(applications);
    }
    let mut by_id = applications
        .into_iter()
        .map(|application| (application.id.clone(), application))
        .collect::<HashMap<_, _>>();
    unique_ids(ids)
        .into_iter()
        .map(|id| {
            by_id
                .remove(id)
                .with_context(|| format!("unknown desktop id: {id}"))
        })
        .collect()
}

fn unique_ids(ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect()
}

fn application_cache_status(application: &DesktopApplication, output: &Path) -> &'static str {
    let Ok(input) = application.input() else {
        return "source-unavailable";
    };
    match cache_status(&app_output(output, application), &input.bytes) {
        CacheStatus::Missing => "missing",
        CacheStatus::Current => "current",
        CacheStatus::Legacy => "legacy",
        CacheStatus::Stale => "stale",
    }
}

fn app_output(output: &Path, application: &DesktopApplication) -> PathBuf {
    output
        .join("apps")
        .join(application_output_name(&application.id))
}

fn parse_accent(value: &str) -> anyhow::Result<[u8; 3]> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("accent must be a six-digit RGB hex value");
    }
    Ok([
        u8::from_str_radix(&value[0..2], 16)?,
        u8::from_str_radix(&value[2..4], 16)?,
        u8::from_str_radix(&value[4..6], 16)?,
    ])
}

fn parse_foreground_opacity(value: f32) -> anyhow::Result<f32> {
    if !value.is_finite() || !(20.0..=150.0).contains(&value) {
        anyhow::bail!("foreground opacity must be a percentage from 20 through 150");
    }
    Ok(value / 100.0)
}

fn emit<T: Serialize>(operation: &'static str, data: T, compact: bool) -> anyhow::Result<()> {
    let envelope = Envelope {
        schema_version: SCHEMA_VERSION,
        operation,
        data,
    };
    if compact {
        println!("{}", serde_json::to_string(&envelope)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_contract_parses_and_validates_accent() {
        let args = Args::try_parse_from(["liquid-glass-icon", "discover", "--json"]).unwrap();
        assert!(args.json);
        assert!(matches!(args.command, Command::Discover));
        let args = Args::try_parse_from([
            "liquid-glass-icon",
            "apply",
            "--desktop-id",
            "demo.desktop",
            "--background",
            "263447",
            "--foreground-opacity",
            "75",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Command::Apply {
                background: Some(background),
                foreground_opacity: Some(foreground_opacity),
                ..
            } if background == "263447" && foreground_opacity == 75.0
        ));
        assert_eq!(parse_accent("#89b4fa").unwrap(), [137, 180, 250]);
        assert!(parse_accent("89b4f").is_err());
        assert_eq!(parse_foreground_opacity(75.0).unwrap(), 0.75);
        assert!(parse_foreground_opacity(151.0).is_err());
    }
}
