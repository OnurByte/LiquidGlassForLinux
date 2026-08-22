use httpmock::Mock;
use httpmock::prelude::*;
use image::{GenericImageView, ImageEncoder, Rgba, RgbaImage};
use liquid_glass_icon::{
    desktop::{
        AppCategory, DesktopApplication, DesktopTaskState, discover_desktop_applications_from_dirs,
    },
    manifest::{SCHEMA_VERSION, sha256},
    model::{Appearance, IconInput, TransformRequest},
    normalize::{has_transparency, normalize_to_png},
    openai::{CodexExecProvider, DEFAULT_MODEL, OpenAiResponsesClient, SvgProvider},
    pipeline::{
        CacheStatus, archive_cached_conversion, cache_status, transform_desktop_icons_with_options,
        transform_icon,
    },
    prompt::SVG_PROMPT,
    renderer::appearance_index,
    svg::{rasterize_layers, validate_svg},
};
use std::{
    collections::HashSet,
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::tempdir;

fn png_bytes(color: [u8; 4]) -> Vec<u8> {
    let mut image = RgbaImage::from_pixel(16, 8, Rgba(color));
    image.put_pixel(0, 0, Rgba([color[0], color[1], color[2], 0]));
    let mut output = Vec::new();
    image::codecs::png::PngEncoder::new(&mut output)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ColorType::Rgba8.into(),
        )
        .unwrap();
    output
}

fn canonical_svg() -> String {
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#254060"/></g>
<g id="foreground-1"><circle cx="512" cy="512" r="260" fill="#ffffff"/></g>
</svg>"##
        .to_owned()
}

fn four_group_svg() -> String {
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#254060"/></g>
<g id="foreground-1"><circle cx="512" cy="512" r="280" fill="#ffffff"/></g>
<g id="foreground-2"><circle cx="512" cy="512" r="180" fill="#ffcc00"/></g>
<g id="foreground-3"><path d="M300 512h424" stroke="#ffffff" stroke-width="40"/></g>
<g id="foreground-4"><circle cx="512" cy="512" r="28" fill="#254060"/></g>
</svg>"##
        .to_owned()
}

async fn svg_mock<'a>(server: &'a MockServer, svg: &str) -> Mock<'a> {
    let text = serde_json::to_string(&serde_json::json!({"svg": svg})).unwrap();
    server
        .mock_async(move |when, then| {
            when.method(POST)
                .path("/v1/responses")
                .body_contains("\"store\":false")
                .body_contains(format!("\"model\":\"{DEFAULT_MODEL}\""))
                .body_contains("\"type\":\"input_image\"")
                .body_contains("\"type\":\"json_schema\"");
            then.status(200).header("content-type", "application/json").json_body(
            serde_json::json!({"output": [{"content": [{"type": "output_text", "text": text}]}]})
        );
        })
        .await
}

fn application(root: &std::path::Path, bytes: &[u8]) -> DesktopApplication {
    let icon_path = root.join("demo.png");
    fs::write(&icon_path, bytes).unwrap();
    DesktopApplication {
        id: "demo.desktop".to_owned(),
        name: "Demo".to_owned(),
        desktop_file: root.join("demo.desktop"),
        icon_name: "demo".to_owned(),
        icon_path: Some(icon_path),
        categories: vec!["Development".to_owned()],
        category: AppCategory::Development,
    }
}

#[test]
fn normalizes_raster_and_svg_inputs() {
    let output = normalize_to_png(&png_bytes([12, 34, 56, 255]), "image/png").unwrap();
    assert_eq!(
        image::load_from_memory(&output).unwrap().dimensions(),
        (1024, 1024)
    );
    assert!(has_transparency(&output).unwrap());
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#;
    let output = normalize_to_png(svg, "image/svg+xml").unwrap();
    assert_eq!(
        image::load_from_memory(&output).unwrap().dimensions(),
        (1024, 1024)
    );
}

#[test]
fn svg_prompt_and_appearance_mapping_are_local_only() {
    let prompt = SVG_PROMPT.to_ascii_lowercase();
    assert!(!prompt.contains("accent"));
    assert!(!prompt.contains("tinted"));
    assert!(!prompt.contains("clear light"));
    assert_eq!(appearance_index(Appearance::Default), 0.0);
    assert_eq!(appearance_index(Appearance::TintedDark), 5.0);
    let args = CodexExecProvider::command_args("input.png", "schema.json", "result.json");
    assert_eq!(args.first().map(String::as_str), Some("exec"));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--model", DEFAULT_MODEL])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"])
    );
    assert!(args.contains(&"--ephemeral".to_owned()));
    assert!(args.contains(&"--ignore-user-config".to_owned()));
    assert!(!args.iter().any(|arg| arg == "--ask-for-approval"));
    assert!(!args.iter().any(|arg| arg.contains("#89B4FA")));

    let custom_args = CodexExecProvider::command_args_for_model(
        "gpt-test",
        "input.png",
        "schema.json",
        "result.json",
    );
    assert!(
        custom_args
            .windows(2)
            .any(|pair| pair == ["--model", "gpt-test"])
    );
}

#[test]
fn validates_and_rasterizes_canonical_layers() {
    let layers = validate_svg(&canonical_svg()).unwrap();
    assert_eq!(
        layers
            .iter()
            .map(|layer| layer.id.as_str())
            .collect::<Vec<_>>(),
        ["background", "foreground-1"]
    );
    let raster = rasterize_layers(&canonical_svg()).unwrap();
    assert_eq!(raster.len(), 2);
    assert_eq!(raster[0].image.dimensions(), (1024, 1024));

    let external = canonical_svg().replace(
        "</svg>",
        "<image href=\"https://example.com/a.png\"/></svg>",
    );
    assert!(validate_svg(&external).is_err());
    let filtered = canonical_svg().replace("<circle", "<filter id=\"x\"/><circle");
    assert!(validate_svg(&filtered).is_err());
    let transparent = canonical_svg().replace("fill=\"#254060\"", "fill=\"none\"");
    assert!(validate_svg(&transparent).is_err());
}

#[test]
fn rasterized_layers_keep_their_svg_coordinates() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#254060"/></g>
<g id="foreground-1"><rect x="240" y="320" width="180" height="140" fill="#ffffff"/></g>
</svg>"##;
    let layer = rasterize_layers(svg).unwrap().remove(1);
    assert!(layer.image.get_pixel(240, 320)[3] > 8);
    assert_eq!(layer.image.get_pixel(0, 0)[3], 0);
}

#[test]
fn clips_foreground_geometry_that_spills_past_the_canvas() {
    let svg = canonical_svg().replace(
        "<circle cx=\"512\" cy=\"512\" r=\"260\" fill=\"#ffffff\"/>",
        "<rect x=\"-160\" y=\"-160\" width=\"1344\" height=\"1344\" fill=\"#ffffff\"/>",
    );
    let layers = rasterize_layers(&svg).unwrap();
    assert_eq!(layers[1].image.dimensions(), (1024, 1024));
    assert!(layers[1].image.get_pixel(0, 0)[3] > 0);
}

#[test]
fn accepts_four_foreground_groups_for_depth_rendering() {
    let layers = validate_svg(&four_group_svg()).unwrap();
    assert_eq!(layers.len(), 5);
    assert_eq!(layers[4].id, "foreground-4");
    assert_eq!(rasterize_layers(&four_group_svg()).unwrap().len(), 5);
}

#[test]
fn source_hash_is_sha256_hex() {
    assert_eq!(
        sha256(b"liquid-glass"),
        "5f26ccb03544d839ca250bf3a780b21328e4920b2abdb2c5ad69ae774b0d8bc4"
    );
}

#[test]
fn discovers_apps_and_prefers_svg_icons() {
    let root = tempdir().unwrap();
    let data_dir = root.path().join("share");
    let applications_dir = data_dir.join("applications");
    let icon_dir = data_dir.join("icons/hicolor/scalable/apps");
    fs::create_dir_all(&applications_dir).unwrap();
    fs::create_dir_all(&icon_dir).unwrap();
    fs::write(icon_dir.join("demo.png"), png_bytes([1, 2, 3, 255])).unwrap();
    fs::write(icon_dir.join("demo.svg"), canonical_svg()).unwrap();
    fs::write(
        applications_dir.join("demo.desktop"),
        "[Desktop Entry]\nType=Application\nName=Demo\\sApp\nIcon=demo\n",
    )
    .unwrap();
    fs::write(
        applications_dir.join("hidden.desktop"),
        "[Desktop Entry]\nType=Application\nName=Hidden\nIcon=demo\nNoDisplay=true\n",
    )
    .unwrap();
    for (id, name, categories, terminal) in [
        (
            "bssh.desktop",
            "Avahi SSH Server Browser",
            "Network;",
            "false",
        ),
        ("browser.desktop", "Browser", "Network;WebBrowser;", "false"),
        ("terminal.desktop", "Terminal helper", "Utility;", "true"),
    ] {
        fs::write(
            applications_dir.join(id),
            format!(
                "[Desktop Entry]\nType=Application\nName={name}\nIcon=demo\nCategories={categories}\nTerminal={terminal}\n"
            ),
        )
        .unwrap();
    }
    let applications = discover_desktop_applications_from_dirs(&[applications_dir], &[data_dir]);
    assert_eq!(applications.len(), 4);
    let demo = applications
        .iter()
        .find(|application| application.id == "demo.desktop")
        .unwrap();
    assert_eq!(demo.name, "Demo App");
    assert_eq!(demo.icon_path.as_ref().unwrap().extension().unwrap(), "svg");
    assert_eq!(
        applications
            .iter()
            .find(|application| application.id == "bssh.desktop")
            .unwrap()
            .category,
        AppCategory::NetworkTools
    );
    assert_eq!(
        applications
            .iter()
            .find(|application| application.id == "browser.desktop")
            .unwrap()
            .category,
        AppCategory::Internet
    );
    assert_eq!(
        applications
            .iter()
            .find(|application| application.id == "terminal.desktop")
            .unwrap()
            .category,
        AppCategory::Terminal
    );
    assert!(!AppCategory::NetworkTools.enabled_by_default());
    assert!(!AppCategory::Terminal.enabled_by_default());
}

#[test]
fn managed_desktop_override_keeps_original_icon_as_the_cache_source() {
    let root = tempdir().unwrap();
    let data_dir = root.path().join("share");
    let applications_dir = data_dir.join("applications");
    let icons_dir = data_dir.join("icons/hicolor/128x128/apps");
    fs::create_dir_all(&applications_dir).unwrap();
    fs::create_dir_all(&icons_dir).unwrap();
    fs::write(icons_dir.join("demo.png"), png_bytes([1, 2, 3, 255])).unwrap();
    fs::write(
        applications_dir.join("demo.desktop"),
        "[Desktop Entry]\nType=Application\nName=Demo\nIcon=liquid-glass-demo\nX-Liquid-Glass-Original-Icon=demo\nCategories=Development;\n",
    )
    .unwrap();
    let applications = discover_desktop_applications_from_dirs(&[applications_dir], &[data_dir]);
    assert_eq!(applications[0].icon_name, "demo");
    assert_eq!(
        applications[0]
            .icon_path
            .as_ref()
            .unwrap()
            .file_name()
            .unwrap(),
        "demo.png"
    );
}

#[cfg(unix)]
#[test]
fn discovers_flatpak_style_symlinked_icons() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let data_dir = root.path().join("share");
    let applications_dir = data_dir.join("applications");
    let export_dir = data_dir.join("icons/hicolor/scalable/apps");
    let target = root.path().join("flatpak-app-export/demo.svg");
    fs::create_dir_all(&applications_dir).unwrap();
    fs::create_dir_all(&export_dir).unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, canonical_svg()).unwrap();
    symlink(&target, export_dir.join("demo.svg")).unwrap();
    fs::write(
        applications_dir.join("demo.desktop"),
        "[Desktop Entry]\nType=Application\nName=Demo\nIcon=demo\n",
    )
    .unwrap();

    let applications = discover_desktop_applications_from_dirs(&[applications_dir], &[data_dir]);
    assert_eq!(applications.len(), 1);
    assert_eq!(
        applications[0].icon_path.as_deref(),
        Some(export_dir.join("demo.svg").as_path())
    );
}

#[tokio::test]
async fn responses_provider_uses_one_structured_request() {
    let server = MockServer::start_async().await;
    let mock = svg_mock(&server, &canonical_svg()).await;
    let provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &server.url("/v1/responses")).unwrap(),
    );
    let svg = provider
        .generate_svg(
            &IconInput {
                filename: "icon.png".to_owned(),
                bytes: png_bytes([1, 2, 3, 255]),
                mime_type: "image/png".to_owned(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    mock.assert_async().await;
    assert_eq!(svg, canonical_svg());
}

#[tokio::test]
async fn conversion_writes_one_svg_and_v4_manifest() {
    let server = MockServer::start_async().await;
    let mock = svg_mock(&server, &canonical_svg()).await;
    let provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &server.url("/v1/responses")).unwrap(),
    );
    let output = tempdir().unwrap();
    let result = transform_icon(
        TransformRequest {
            input: IconInput {
                filename: "icon.png".to_owned(),
                bytes: png_bytes([1, 2, 3, 255]),
                mime_type: "image/png".to_owned(),
            },
            output_dir: output.path().join("demo"),
        },
        &provider,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    assert_eq!(mock.hits(), 1);
    assert!(result.svg_path.is_file());
    assert!(result.manifest_path.is_file());
    assert!(
        !result
            .manifest_path
            .parent()
            .unwrap()
            .join("appearances")
            .exists()
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&result.manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], SCHEMA_VERSION);
    assert_eq!(manifest["svg"], "icon.svg");
    assert_eq!(
        manifest["document"]["groups"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(manifest.get("appearances").is_none());
    assert!(manifest["generator"].get("accent_color").is_none());

    let archive = tempdir().unwrap();
    let archived =
        archive_cached_conversion(archive.path(), "demo", output.path().join("demo").as_path())
            .unwrap();
    assert_eq!(
        fs::read(archived.join("icon.svg")).unwrap(),
        fs::read(&result.svg_path).unwrap()
    );
    assert!(archived.join("icon-manifest.json").is_file());

    let mut legacy = manifest;
    legacy["schema_version"] = serde_json::json!(3);
    legacy.as_object_mut().unwrap().remove("document");
    fs::write(
        &result.manifest_path,
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();
    assert_eq!(
        cache_status(
            output.path().join("demo").as_path(),
            &png_bytes([1, 2, 3, 255])
        ),
        CacheStatus::Legacy
    );
    assert!(
        archive_cached_conversion(
            archive.path(),
            "legacy-demo",
            output.path().join("demo").as_path(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn local_renderer_application_writes_real_linux_icon_sizes() {
    let root = tempdir().unwrap();
    let data_home = root.path().join("data");
    let installer =
        liquid_glass_icon::icon_install::IconInstaller::with_data_home_for_test(data_home.clone());
    let application = application(root.path(), &png_bytes([12, 34, 56, 255]));
    fs::write(
        &application.desktop_file,
        "[Desktop Entry]\nType=Application\nName=Demo\nIcon=demo\nCategories=Development;\n",
    )
    .unwrap();
    let mut renderer = liquid_glass_icon::renderer::GlassRenderer::new()
        .await
        .unwrap();
    installer
        .apply_svg(
            &application,
            &canonical_svg(),
            &mut renderer,
            liquid_glass_icon::renderer::RenderSettings {
                appearance: Appearance::TintedLight,
                ..Default::default()
            },
        )
        .unwrap();
    let desktop_path = data_home.join("applications/demo.desktop");
    let desktop = fs::read_to_string(&desktop_path).unwrap();
    let first_icon = desktop
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .unwrap()
        .to_owned();
    assert!(first_icon.starts_with("liquid-glass-demo-"));
    assert!(data_home.join("icons/hicolor/index.theme").is_file());
    for size in [16, 24, 32, 48, 64, 96, 128, 192, 256, 512, 1024] {
        let path = data_home
            .join("icons/hicolor")
            .join(format!("{size}x{size}/apps/{first_icon}.png"));
        assert_eq!(image::open(path).unwrap().dimensions(), (size, size));
    }

    installer
        .apply_svg(
            &application,
            &canonical_svg(),
            &mut renderer,
            liquid_glass_icon::renderer::RenderSettings {
                appearance: Appearance::TintedLight,
                accent: [255, 88, 120],
                ..Default::default()
            },
        )
        .unwrap();
    let desktop = fs::read_to_string(desktop_path).unwrap();
    let second_icon = desktop
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .unwrap();
    assert_ne!(first_icon, second_icon);
    for size in [16, 24, 32, 48, 64, 96, 128, 192, 256, 512, 1024] {
        let stale_path = data_home
            .join("icons/hicolor")
            .join(format!("{size}x{size}/apps/{first_icon}.png"));
        assert!(!stale_path.exists());
    }
}

#[tokio::test]
async fn cached_repair_recovers_a_missing_launcher_and_empty_icon_file() {
    let root = tempdir().unwrap();
    let data_home = root.path().join("data");
    let installer =
        liquid_glass_icon::icon_install::IconInstaller::with_data_home_for_test(data_home.clone());
    let application = application(root.path(), &png_bytes([12, 34, 56, 255]));
    fs::write(
        &application.desktop_file,
        "[Desktop Entry]\nType=Application\nName=Demo\nIcon=demo\nCategories=Development;\n",
    )
    .unwrap();
    let mut renderer = liquid_glass_icon::renderer::GlassRenderer::new()
        .await
        .unwrap();
    let settings = liquid_glass_icon::renderer::RenderSettings {
        appearance: Appearance::TintedLight,
        ..Default::default()
    };
    installer
        .apply_svg(&application, &canonical_svg(), &mut renderer, settings)
        .unwrap();

    let desktop_path = data_home.join("applications/demo.desktop");
    let icon_name = fs::read_to_string(&desktop_path)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .unwrap()
        .to_owned();
    let empty_path = data_home
        .join("icons/hicolor/16x16/apps")
        .join(format!("{icon_name}.png"));
    RgbaImage::new(16, 16).save(&empty_path).unwrap();
    fs::remove_file(&desktop_path).unwrap();
    assert_eq!(
        installer.health("demo.desktop").unwrap(),
        Some(liquid_glass_icon::icon_install::ManagedIconHealth::Repairable)
    );

    installer
        .repair_cached_svg("demo.desktop", &canonical_svg(), &mut renderer, settings)
        .unwrap();
    assert!(desktop_path.is_file());
    assert_eq!(
        installer.health("demo.desktop").unwrap(),
        Some(liquid_glass_icon::icon_install::ManagedIconHealth::Healthy)
    );
}

#[tokio::test]
async fn cache_never_auto_reconverts_current_or_stale_icons() {
    let server = MockServer::start_async().await;
    let mock = svg_mock(&server, &canonical_svg()).await;
    let provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &server.url("/v1/responses")).unwrap(),
    );
    let root = tempdir().unwrap();
    let original = png_bytes([10, 20, 30, 255]);
    let app = application(root.path(), &original);
    let output = root.path().join("out");
    let mut states = Vec::new();
    transform_desktop_icons_with_options(
        std::slice::from_ref(&app),
        &output,
        &provider,
        Arc::new(AtomicBool::new(false)),
        &HashSet::new(),
        |event| states.push(event.state),
    )
    .await;
    assert_eq!(mock.hits(), 1);
    assert_eq!(
        cache_status(&output.join("apps/demo"), &original),
        CacheStatus::Current
    );

    states.clear();
    transform_desktop_icons_with_options(
        std::slice::from_ref(&app),
        &output,
        &provider,
        Arc::new(AtomicBool::new(false)),
        &HashSet::new(),
        |event| states.push(event.state),
    )
    .await;
    assert_eq!(mock.hits(), 1);
    assert_eq!(states, [DesktopTaskState::Converted]);

    let changed = png_bytes([50, 60, 70, 255]);
    fs::write(app.icon_path.as_ref().unwrap(), &changed).unwrap();
    states.clear();
    transform_desktop_icons_with_options(
        std::slice::from_ref(&app),
        &output,
        &provider,
        Arc::new(AtomicBool::new(false)),
        &HashSet::new(),
        |event| states.push(event.state),
    )
    .await;
    assert_eq!(mock.hits(), 1);
    assert_eq!(states, [DesktopTaskState::Stale]);

    transform_desktop_icons_with_options(
        std::slice::from_ref(&app),
        &output,
        &provider,
        Arc::new(AtomicBool::new(false)),
        &HashSet::from([app.id.clone()]),
        |_| {},
    )
    .await;
    assert_eq!(mock.hits(), 2);
}

#[tokio::test]
async fn cancelled_batch_makes_no_provider_request() {
    let server = MockServer::start_async().await;
    let mock = svg_mock(&server, &canonical_svg()).await;
    let provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &server.url("/v1/responses")).unwrap(),
    );
    let root = tempdir().unwrap();
    let app = application(root.path(), &png_bytes([1, 2, 3, 255]));
    let cancelled = Arc::new(AtomicBool::new(true));
    let mut states = Vec::new();
    let results = transform_desktop_icons_with_options(
        &[app],
        &root.path().join("out"),
        &provider,
        cancelled,
        &HashSet::new(),
        |event| states.push(event.state),
    )
    .await;
    assert!(results.is_empty());
    assert_eq!(states, [DesktopTaskState::Stopped]);
    assert_eq!(mock.hits(), 0);
}

#[tokio::test]
async fn failed_reconversion_preserves_the_previous_svg() {
    let valid_server = MockServer::start_async().await;
    let valid_mock = svg_mock(&valid_server, &canonical_svg()).await;
    let valid_provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &valid_server.url("/v1/responses")).unwrap(),
    );
    let root = tempdir().unwrap();
    let output_dir = root.path().join("converted");
    let request = || TransformRequest {
        input: IconInput {
            filename: "icon.png".to_owned(),
            bytes: png_bytes([1, 2, 3, 255]),
            mime_type: "image/png".to_owned(),
        },
        output_dir: output_dir.clone(),
    };
    transform_icon(request(), &valid_provider, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    valid_mock.assert_async().await;
    let previous = fs::read(output_dir.join("icon.svg")).unwrap();

    let invalid_server = MockServer::start_async().await;
    let invalid_mock = svg_mock(&invalid_server, "<svg viewBox=\"0 0 1 1\"></svg>").await;
    let invalid_provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &invalid_server.url("/v1/responses")).unwrap(),
    );
    assert!(
        transform_icon(
            request(),
            &invalid_provider,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .is_err()
    );
    invalid_mock.assert_async().await;
    assert_eq!(fs::read(output_dir.join("icon.svg")).unwrap(), previous);
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_codex_terminates_the_active_child() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir().unwrap();
    let executable = root.path().join("fake-codex");
    fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).unwrap();

    let provider = SvgProvider::Codex(CodexExecProvider::new(executable));
    let cancelled = Arc::new(AtomicBool::new(false));
    let task_cancelled = Arc::clone(&cancelled);
    let task = tokio::spawn(async move {
        provider
            .generate_svg(
                &IconInput {
                    filename: "icon.png".to_owned(),
                    bytes: png_bytes([1, 2, 3, 255]),
                    mime_type: "image/png".to_owned(),
                },
                task_cancelled,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancelled.store(true, Ordering::Relaxed);
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("Codex child did not stop")
        .unwrap();
    assert!(matches!(
        result,
        Err(liquid_glass_icon::IconError::Cancelled)
    ));
}

#[tokio::test]
async fn stopping_api_cancels_the_active_http_request() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/responses");
            then.status(200)
                .delay(Duration::from_secs(30))
                .json_body(serde_json::json!({"output": []}));
        })
        .await;
    let provider = SvgProvider::Responses(
        OpenAiResponsesClient::new("test-key", &server.url("/v1/responses")).unwrap(),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let task_cancelled = Arc::clone(&cancelled);
    let task = tokio::spawn(async move {
        provider
            .generate_svg(
                &IconInput {
                    filename: "icon.png".to_owned(),
                    bytes: png_bytes([1, 2, 3, 255]),
                    mime_type: "image/png".to_owned(),
                },
                task_cancelled,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    cancelled.store(true, Ordering::Relaxed);
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("API request did not stop")
        .unwrap();
    assert!(matches!(
        result,
        Err(liquid_glass_icon::IconError::Cancelled)
    ));
}

fn combined_bounds(layers: &[liquid_glass_icon::svg::RasterLayer]) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for layer in layers.iter().skip(1) {
        let mut current: Option<(u32, u32, u32, u32)> = None;
        for (x, y, pixel) in layer.image.enumerate_pixels() {
            if pixel[3] <= 8 {
                continue;
            }
            current = Some(match current {
                None => (x, y, x, y),
                Some((min_x, min_y, max_x, max_y)) => {
                    (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
                }
            });
        }
        if let Some((min_x, min_y, max_x, max_y)) = current {
            bounds = Some(match bounds {
                None => (min_x, min_y, max_x, max_y),
                Some((b0, b1, b2, b3)) => {
                    (b0.min(min_x), b1.min(min_y), b2.max(max_x), b3.max(max_y))
                }
            });
        }
    }
    bounds
}

#[test]
fn bridge_space_panels_keep_their_canonical_grid_geometry() {
    let source = rasterize_layers(&four_group_svg()).unwrap();
    let prepared =
        liquid_glass_icon::renderer::prepare_canonical_layers(&four_group_svg()).unwrap();
    assert_eq!(prepared.len(), 5);
    assert_eq!(prepared[1].image, source[1].image);
    assert_eq!(prepared[2].image, source[2].image);
    assert_eq!(prepared[3].image, source[3].image);
    assert_eq!(combined_bounds(&prepared), combined_bounds(&source));
}

#[test]
fn athas_like_asymmetric_source_keeps_orientation_without_mirroring() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<g id="background"><rect width="1024" height="1024" fill="#25202a"/></g>
<g id="foreground-1"><rect x="150" y="380" width="300" height="280" rx="24" fill="#e8eaf0"/></g>
<g id="foreground-2"><circle cx="700" cy="520" r="110" fill="#f2b23c"/></g>
</svg>"##;
    let source = rasterize_layers(svg).unwrap();
    let fitted = liquid_glass_icon::renderer::prepare_canonical_layers(svg).unwrap();

    let side_mass = |layers: &[liquid_glass_icon::svg::RasterLayer]| -> (u64, u64, f64) {
        let mut left = 0u64;
        let mut right = 0u64;
        let mut weighted_x = 0u64;
        let mut total = 0u64;
        for layer in layers.iter().skip(1) {
            for (x, _y, pixel) in layer.image.enumerate_pixels() {
                let alpha = u64::from(pixel[3]);
                if alpha == 0 {
                    continue;
                }
                if x < 512 {
                    left += alpha;
                } else {
                    right += alpha;
                }
                weighted_x += u64::from(x) * alpha;
                total += alpha;
            }
        }
        assert!(total > 0);
        (left, right, weighted_x as f64 / total as f64)
    };

    let (source_left, source_right, source_centroid_x) = side_mass(&source);
    let (fitted_left, fitted_right, fitted_centroid_x) = side_mass(&fitted);
    let source_ratio = source_left as f64 / source_right.max(1) as f64;
    let fitted_ratio = fitted_left as f64 / fitted_right.max(1) as f64;
    // Source-space positioning survives: no mirroring, centering, or scale.
    assert!(source_ratio > 1.5, "fixture should be asymmetric");
    assert_eq!(source_ratio, fitted_ratio);
    assert_eq!(source_centroid_x, fitted_centroid_x);
    assert_eq!(combined_bounds(&source), combined_bounds(&fitted));
}

#[tokio::test]
async fn renderer_revision_change_rebuilds_managed_icons_without_ai() {
    let root = tempdir().unwrap();
    let data_home = root.path().join("data");
    let installer =
        liquid_glass_icon::icon_install::IconInstaller::with_data_home_for_test(data_home.clone());
    let application = application(root.path(), &png_bytes([12, 34, 56, 255]));
    fs::write(
        &application.desktop_file,
        "[Desktop Entry]\nType=Application\nName=Demo\nIcon=demo\nCategories=Development;\n",
    )
    .unwrap();
    let mut renderer = liquid_glass_icon::renderer::GlassRenderer::new()
        .await
        .unwrap();
    installer
        .apply_svg(
            &application,
            &canonical_svg(),
            &mut renderer,
            liquid_glass_icon::renderer::RenderSettings {
                appearance: Appearance::TintedLight,
                ..Default::default()
            },
        )
        .unwrap();
    let desktop_path = data_home.join("applications/demo.desktop");
    let desktop = fs::read_to_string(&desktop_path).unwrap();
    let first_icon = desktop
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .unwrap()
        .to_owned();

    // Simulate state written by an older renderer: the revision field is
    // missing entirely, so the managed PNGs must be rebuilt from the cached
    // canonical SVG without any AI request.
    let state_path = data_home.join("liquid-glass-icon/managed-icons.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["entries"]["demo.desktop"]
        .as_object_mut()
        .unwrap()
        .remove("renderer_revision");
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

    installer
        .apply_svg(
            &application,
            &canonical_svg(),
            &mut renderer,
            liquid_glass_icon::renderer::RenderSettings {
                appearance: Appearance::TintedLight,
                accent: [255, 88, 120],
                ..Default::default()
            },
        )
        .unwrap();

    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(
        state["entries"]["demo.desktop"]["renderer_revision"],
        u64::from(liquid_glass_icon::renderer::RENDERER_REVISION)
    );
    // The desktop entry points at the freshly rendered family and every old
    // family file is gone (the stale tilted-PNG reference problem).
    let desktop = fs::read_to_string(&desktop_path).unwrap();
    let second_icon = desktop
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .unwrap();
    assert_ne!(first_icon, second_icon);
    for size in [128u32, 256, 512] {
        let active = data_home
            .join("icons/hicolor")
            .join(format!("{size}x{size}/apps/{second_icon}.png"));
        assert!(active.is_file());
        let stale = data_home
            .join("icons/hicolor")
            .join(format!("{size}x{size}/apps/{first_icon}.png"));
        assert!(!stale.exists());
    }
}
