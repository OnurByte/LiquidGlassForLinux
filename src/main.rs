use clap::{Parser, ValueEnum};
use liquid_glass_icon::{
    Appearance,
    desktop::discover_desktop_applications,
    icon_install::IconInstaller,
    openai::{CodexExecProvider, DEFAULT_MODEL, OpenAiResponsesClient, SvgProvider},
    pipeline::{CacheStatus, cache_status, transform_desktop_icons_with_options},
    renderer::{GlassRenderer, RenderSettings},
};
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderArg {
    Codex,
    Api,
}

#[derive(Debug, Parser)]
#[command(
    name = "liquid-glass-icon",
    about = "Convert installed application icons to canonical layered SVGs"
)]
struct Args {
    #[arg(short, long, default_value = "out")]
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = ProviderArg::Codex)]
    provider: ProviderArg,
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    #[arg(long, value_name = "DESKTOP_ID")]
    reconvert: Vec<String>,
    #[arg(
        long,
        help = "Include system, settings, terminal and other utility entries"
    )]
    all_categories: bool,
    #[arg(
        long,
        value_name = "DESKTOP_ID",
        help = "Restore a managed launcher and icon"
    )]
    restore: Vec<String>,
    #[arg(
        long,
        help = "Apply existing current SVG caches without calling an AI provider"
    )]
    apply_cache: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if !args.restore.is_empty() {
        let installer = IconInstaller::default();
        for desktop_id in &args.restore {
            installer.restore(desktop_id)?;
            println!("restored {desktop_id}");
        }
        if args.output.as_path() == std::path::Path::new("out")
            && args.reconvert.is_empty()
            && !args.apply_cache
        {
            return Ok(());
        }
    }
    let discovered = discover_desktop_applications();
    let requested_ids = args.reconvert.iter().cloned().collect::<HashSet<_>>();
    let eligible = discovered
        .iter()
        .filter(|application| args.all_categories || application.category.enabled_by_default())
        .collect::<Vec<_>>();
    let eligible_count = eligible.len();
    let applications = eligible
        .into_iter()
        .filter(|application| requested_ids.is_empty() || requested_ids.contains(&application.id))
        .cloned()
        .collect::<Vec<_>>();
    if applications.is_empty() {
        anyhow::bail!("no Type=Application desktop entries were found");
    }
    if args.apply_cache {
        let mut renderer = GlassRenderer::new().await?;
        let installer = IconInstaller::default();
        let settings = RenderSettings {
            appearance: Appearance::TintedLight,
            ..Default::default()
        };
        let mut applied = 0;
        let mut skipped = 0;
        for application in &applications {
            let Ok(input) = application.input() else {
                skipped += 1;
                println!(
                    "skipped cached icon {}: source icon unavailable",
                    application.id
                );
                continue;
            };
            let output =
                args.output
                    .join("apps")
                    .join(liquid_glass_icon::desktop::application_output_name(
                        &application.id,
                    ));
            if cache_status(&output, &input.bytes) != CacheStatus::Current {
                continue;
            }
            let svg = fs::read_to_string(output.join("icon.svg"))?;
            installer.apply_svg(application, &svg, &mut renderer, settings)?;
            println!("applied cached icon {}", application.id);
            applied += 1;
        }
        println!("applied {applied} cached icons; skipped {skipped}");
        if args.reconvert.is_empty() {
            return Ok(());
        }
    }
    let provider = match args.provider {
        ProviderArg::Codex => {
            SvgProvider::Codex(CodexExecProvider::default().with_model(args.model.clone()))
        }
        ProviderArg::Api => {
            SvgProvider::Responses(OpenAiResponsesClient::from_env()?.with_model(args.model))
        }
    };
    let force_ids = args.reconvert.into_iter().collect::<HashSet<_>>();
    let skipped = discovered.len().saturating_sub(eligible_count);
    let scope = if requested_ids.is_empty() {
        "discovered"
    } else {
        "selected"
    };
    println!(
        "{scope} {} application icons ({} category-blocked; use --all-categories to include)",
        applications.len(),
        skipped
    );
    let results = transform_desktop_icons_with_options(
        &applications,
        &args.output,
        &provider,
        Arc::new(AtomicBool::new(false)),
        &force_ids,
        |event| {
            println!(
                "[{}] {} — {}",
                event.state.as_str(),
                event.application_name,
                event.message
            )
        },
    )
    .await;
    println!(
        "converted {}; output: {}",
        results.len(),
        args.output.display()
    );
    Ok(())
}
