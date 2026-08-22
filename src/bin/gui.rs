use adw::gtk::{self, gdk, glib};
use adw::prelude::*;
use adw::{Application, ApplicationWindow, Clamp, HeaderBar, ToolbarView};
use libadwaita as adw;
use liquid_glass_icon::{
    AppCategory, Appearance, default_output_dir,
    desktop::{
        DesktopApplication, DesktopTaskEvent, DesktopTaskState, application_output_name,
        discover_desktop_applications,
    },
    icon_install::IconInstaller,
    openai::{CodexExecProvider, DEFAULT_MODEL, OpenAiResponsesClient, SvgProvider},
    pipeline::{CacheStatus, cache_status, transform_desktop_icons_with_options_and_assets},
    renderer::{GlassRenderer, RenderSettings, RenderTarget, apply_canonical_mask},
    repository_assets_dir,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Duration,
};

const APPLICATION_ID: &str = "io.github.yargc.LiquidGlassIcons";
const MODEL_OPTIONS: [&str; 4] = ["gpt-5.6-luna", "gpt-5.6-terra", "gpt-5.6-sol", "gpt-5.4"];

fn preview_layer_from_selection(selected: u32) -> Option<usize> {
    selected.checked_sub(1).map(|layer| layer as usize)
}

fn preview_pointer(x: f64, y: f64, width: i32, height: i32) -> [f32; 2] {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    [
        ((x / width - 0.5) * 2.0).clamp(-1.0, 1.0) as f32,
        ((y / height - 0.5) * 2.0).clamp(-1.0, 1.0) as f32,
    ]
}

fn main() {
    let application = Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    application.connect_activate(|application| {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("runtime unavailable: {error}");
                return;
            }
        };
        let renderer = match runtime.block_on(GlassRenderer::new()) {
            Ok(renderer) => renderer,
            Err(error) => {
                eprintln!("Liquid Glass GPU unavailable: {error}");
                return;
            }
        };
        let state = match IconApp::new(renderer) {
            Ok(state) => Rc::new(RefCell::new(state)),
            Err(error) => {
                eprintln!("Liquid Glass initialization failed: {error}");
                return;
            }
        };
        install_css();
        build_window(application, state);
    });
    application.run();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum ProviderChoice {
    #[default]
    Codex,
    Api,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedSettings {
    #[serde(default)]
    provider: ProviderChoice,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    theme: ThemeChoice,
    #[serde(default = "default_appearance")]
    appearance: Appearance,
    #[serde(default = "default_accent")]
    accent: [u8; 3],
    #[serde(default)]
    background: Option<[u8; 3]>,
    #[serde(default = "default_foreground_opacity")]
    foreground_opacity: f32,
    #[serde(default = "default_tilt")]
    tilt: bool,
    #[serde(default)]
    blocked_categories: HashSet<AppCategory>,
}

impl Default for SavedSettings {
    fn default() -> Self {
        Self {
            provider: ProviderChoice::Codex,
            model: DEFAULT_MODEL.to_owned(),
            theme: ThemeChoice::System,
            appearance: Appearance::TintedLight,
            accent: default_accent(),
            background: None,
            foreground_opacity: default_foreground_opacity(),
            tilt: true,
            blocked_categories: AppCategory::ALL
                .into_iter()
                .filter(|category| !category.enabled_by_default())
                .collect(),
        }
    }
}

struct IconApp {
    applications: Vec<DesktopApplication>,
    tasks: Vec<TaskRow>,
    provider_choice: ProviderChoice,
    model: String,
    theme: ThemeChoice,
    appearance: Appearance,
    accent: [u8; 3],
    background: Option<[u8; 3]>,
    foreground_opacity: f32,
    tilt: bool,
    api_key: String,
    blocked_categories: HashSet<AppCategory>,
    output: PathBuf,
    status: gtk::Label,
    count: gtk::Label,
    list: gtk::ListBox,
    preview: gtk::Picture,
    preview_title: gtk::Label,
    preview_selector: gtk::DropDown,
    preview_layer: Option<usize>,
    preview_pointer: [f32; 2],
    preview_selector_updating: Rc<Cell<bool>>,
    receiver: Option<Receiver<DesktopTaskEvent>>,
    cancelled: Arc<AtomicBool>,
    selected: Option<usize>,
    list_dirty: bool,
    glass: GlassRenderer,
    installer: IconInstaller,
    style_manager: adw::StyleManager,
}

struct TaskRow {
    state: DesktopTaskState,
    message: String,
}

impl IconApp {
    fn new(renderer: GlassRenderer) -> Result<Self, String> {
        let saved = load_settings();
        let applications = discover_desktop_applications();
        let output = default_output_dir();
        let tasks = applications
            .iter()
            .map(|application| initial_task(application, &output))
            .collect();
        let style_manager = adw::StyleManager::default();
        style_manager.set_color_scheme(color_scheme(saved.theme));
        let mut app = Self {
            applications,
            tasks,
            provider_choice: saved.provider,
            model: normalize_model(saved.model),
            theme: saved.theme,
            appearance: saved.appearance,
            accent: saved.accent,
            background: saved.background,
            foreground_opacity: saved.foreground_opacity.clamp(0.20, 1.50),
            tilt: saved.tilt,
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            blocked_categories: saved.blocked_categories,
            output,
            status: gtk::Label::new(Some("Ready")),
            count: gtk::Label::new(None),
            list: gtk::ListBox::new(),
            preview: gtk::Picture::new(),
            preview_title: gtk::Label::new(Some("Preview")),
            preview_selector: gtk::DropDown::from_strings(&["Composite"]),
            preview_layer: None,
            preview_pointer: [0.0, 0.0],
            preview_selector_updating: Rc::new(Cell::new(false)),
            receiver: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            selected: None,
            list_dirty: true,
            glass: renderer,
            installer: IconInstaller::default(),
            style_manager,
        };
        app.repair_cached();
        Ok(app)
    }

    fn visible(&self, index: usize) -> bool {
        !self
            .blocked_categories
            .contains(&self.applications[index].category)
    }

    fn render_settings(&self) -> RenderSettings {
        RenderSettings {
            appearance: self.appearance,
            accent: self.accent,
            foreground_opacity: self.foreground_opacity,
            background: self.background,
            dark_background: self.style_manager.is_dark(),
            pointer: [0.0, 0.0],
            tilt: false,
            layer: None,
        }
    }

    fn rebuild_list(state: &Rc<RefCell<Self>>) {
        let mut app = state.borrow_mut();
        app.list_dirty = false;
        while let Some(child) = app.list.first_child() {
            app.list.remove(&child);
        }
        let mut visible_count = 0;
        for index in 0..app.applications.len() {
            if !app.visible(index) {
                continue;
            }
            visible_count += 1;
            let application = &app.applications[index];
            let task = &app.tasks[index];
            let title = glib::markup_escape_text(&application.name);
            let subtitle = glib::markup_escape_text(&format!(
                "{} · {}",
                application.category.label(),
                task.message
            ));
            let row = adw::ActionRow::builder()
                .title(title.as_str())
                .subtitle(subtitle.as_str())
                .activatable(true)
                .build();
            row.add_css_class("app-row");
            if let Some(path) = &application.icon_path {
                let image = gtk::Image::from_file(path);
                image.set_pixel_size(32);
                row.add_prefix(&image);
            }
            let state_label = gtk::Label::new(Some(task.state.as_str()));
            state_label.add_css_class("dim-label");
            row.add_suffix(&state_label);
            let retry = gtk::Button::with_label("Reconvert");
            retry.set_sensitive(app.receiver.is_none());
            let state_for_retry = Rc::clone(state);
            retry.connect_clicked(move |_| state_for_retry.borrow_mut().reconvert(index));
            row.add_suffix(&retry);
            let restore = gtk::Button::with_label("Restore");
            restore.set_sensitive(app.installer.is_managed(&application.id));
            let state_for_restore = Rc::clone(state);
            restore.connect_clicked(move |_| state_for_restore.borrow_mut().restore_icon(index));
            row.add_suffix(&restore);
            let state_for_row = Rc::clone(state);
            row.connect_activated(move |_| state_for_row.borrow_mut().select(index));
            app.list.append(&row);
        }
        app.count
            .set_text(&format!("{visible_count}/{} apps", app.applications.len()));
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.preview_title.set_text(&self.applications[index].name);
        if self.tasks[index].state == DesktopTaskState::Converted
            && let Err(error) = self.apply_icon(index)
        {
            self.status.set_text(&format!("Apply failed: {error}"));
        }
        self.load_preview(index, true);
    }

    fn load_preview(&mut self, index: usize, refresh_layers: bool) {
        let path = app_output(&self.output, &self.applications[index]).join("icon.svg");
        let result = fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|svg| {
                self.glass
                    .load_svg(&svg)
                    .map_err(|error| error.to_string())?;
                if refresh_layers {
                    let labels = self.glass.inspect_labels();
                    self.set_preview_layers(&labels);
                }
                Ok(())
            });
        match result {
            Ok(()) => self.render_preview(),
            Err(error) => {
                self.preview.set_paintable(Option::<&gdk::Paintable>::None);
                self.status.set_text(&error);
            }
        }
    }

    fn render_preview(&mut self) {
        match self
            .glass
            .render(520, 520, self.preview_settings(), RenderTarget::Preview)
            .map_err(|error| error.to_string())
        {
            Ok(mut image) => {
                // The shader no longer masks output alpha; every preview view
                // (composite and single layers) gets the one canonical mask.
                apply_canonical_mask(&mut image);
                self.set_preview(image);
            }
            Err(error) => {
                self.preview.set_paintable(Option::<&gdk::Paintable>::None);
                self.status.set_text(&error);
            }
        }
    }

    fn set_preview(&self, image: image::RgbaImage) {
        let bytes = glib::Bytes::from_owned(image.into_raw());
        let texture =
            gdk::MemoryTexture::new(520, 520, gdk::MemoryFormat::R8g8b8a8, &bytes, 520 * 4);
        self.preview.set_paintable(Some(&texture));
    }

    fn set_preview_layers(&mut self, inspect_layers: &[String]) {
        let mut labels = vec!["Composite".to_owned()];
        labels.extend(inspect_layers.iter().cloned());
        let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        let selected = self
            .preview_layer
            .filter(|layer| *layer < inspect_layers.len())
            .map(|layer| layer + 1)
            .unwrap_or(0);
        // Composite (0) is not an icon layer.
        self.preview_layer = preview_layer_from_selection(selected as u32);
        self.preview_selector_updating.set(true);
        self.preview_selector
            .set_model(Some(&gtk::StringList::new(&label_refs)));
        self.preview_selector.set_selected(selected as u32);
        self.preview_selector_updating.set(false);
    }

    fn set_preview_layer(&mut self, selected: u32) {
        self.preview_layer = preview_layer_from_selection(selected);
        if self.selected.is_some() {
            self.render_preview();
        }
    }

    fn preview_settings(&self) -> RenderSettings {
        let mut settings = self.render_settings();
        settings.layer = self.preview_layer;
        settings.pointer = self.preview_pointer;
        settings.tilt = self.tilt;
        settings
    }

    fn provider(&self) -> Result<SvgProvider, String> {
        let provider = match self.provider_choice {
            ProviderChoice::Codex => {
                SvgProvider::Codex(CodexExecProvider::default().with_model(self.model.clone()))
            }
            ProviderChoice::Api => SvgProvider::Responses(
                OpenAiResponsesClient::from_api_key(self.api_key.trim().to_owned())
                    .map_err(|error| error.to_string())?
                    .with_model(self.model.clone()),
            ),
        };
        provider.preflight().map_err(|error| error.to_string())?;
        Ok(provider)
    }

    fn start_missing(&mut self) {
        let applications = self
            .applications
            .iter()
            .enumerate()
            .filter(|(index, _)| self.visible(*index))
            .map(|(_, application)| application.clone())
            .collect::<Vec<_>>();
        self.start_batch(applications, HashSet::new());
    }

    fn reconvert(&mut self, index: usize) {
        if self.receiver.is_some() || !self.visible(index) {
            return;
        }
        let application = self.applications[index].clone();
        self.start_batch(vec![application.clone()], HashSet::from([application.id]));
    }

    fn restore_icon(&mut self, index: usize) {
        match self.installer.restore(&self.applications[index].id) {
            Ok(()) => {
                self.tasks[index].message = "original launcher restored".to_owned();
                self.status.set_text("Original launcher restored");
                self.list_dirty = true;
            }
            Err(error) => self.status.set_text(&format!("Restore blocked: {error}")),
        }
    }

    fn start_batch(&mut self, applications: Vec<DesktopApplication>, force_ids: HashSet<String>) {
        if self.receiver.is_some() || applications.is_empty() {
            return;
        }
        let provider = match self.provider() {
            Ok(provider) => provider,
            Err(error) => {
                self.status.set_text(&error);
                return;
            }
        };
        let output = self.output.clone();
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&self.cancelled);
        let asset_dir = repository_assets_dir();
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.status.set_text(&format!(
            "Processing {} application icons…",
            applications.len()
        ));
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Runtime::new() else {
                return;
            };
            runtime.block_on(transform_desktop_icons_with_options_and_assets(
                &applications,
                &output,
                &provider,
                cancelled,
                &force_ids,
                asset_dir.as_deref(),
                |event| {
                    let _ = sender.send(event);
                },
            ));
        });
    }

    fn stop(&mut self) {
        if self.receiver.is_some() {
            self.cancelled.store(true, Ordering::Relaxed);
            self.status.set_text("Stopping current provider request…");
        }
    }

    fn receive_events(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        let mut disconnected = false;
        let mut event_count = 0;
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    event_count += 1;
                    if let Some(index) = self
                        .applications
                        .iter()
                        .position(|application| application.id == event.application_id)
                    {
                        self.tasks[index].state = event.state;
                        self.tasks[index].message = event.message.clone();
                        if matches!(
                            event.state,
                            DesktopTaskState::Completed | DesktopTaskState::Converted
                        ) {
                            match self.apply_icon(index) {
                                Ok(()) => {
                                    self.tasks[index].message =
                                        if event.state == DesktopTaskState::Completed {
                                            "generated and applied".to_owned()
                                        } else {
                                            "cached and applied".to_owned()
                                        }
                                }
                                Err(error) => {
                                    self.tasks[index].message =
                                        format!("icon ready; apply failed: {error}")
                                }
                            }
                        }
                    }
                    self.status
                        .set_text(&format!("{}: {}", event.application_name, event.message));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if !disconnected {
            self.receiver = Some(receiver);
        }
        if disconnected {
            self.status
                .set_text(if self.cancelled.load(Ordering::Relaxed) {
                    "Stopped"
                } else {
                    "Conversion queue finished"
                });
        }
        if event_count > 0 {
            self.list_dirty = true;
        }
    }

    fn apply_icon(&mut self, index: usize) -> Result<(), String> {
        let svg = fs::read_to_string(
            app_output(&self.output, &self.applications[index]).join("icon.svg"),
        )
        .map_err(|error| error.to_string())?;
        let settings = self.render_settings();
        self.installer
            .apply_svg(&self.applications[index], &svg, &mut self.glass, settings)
            .map_err(|error| error.to_string())?;
        if self.selected == Some(index) {
            self.load_preview(index, true);
        }
        Ok(())
    }

    fn save(&self) {
        let settings = SavedSettings {
            provider: self.provider_choice,
            model: self.model.clone(),
            theme: self.theme,
            appearance: self.appearance,
            accent: self.accent,
            background: self.background,
            foreground_opacity: self.foreground_opacity,
            tilt: self.tilt,
            blocked_categories: self.blocked_categories.clone(),
        };
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&settings) {
            let _ = fs::write(path, bytes);
        }
    }

    fn set_theme(&mut self, theme: ThemeChoice) {
        self.theme = theme;
        self.style_manager.set_color_scheme(color_scheme(theme));
        if matches!(
            self.appearance,
            Appearance::TintedLight | Appearance::TintedDark
        ) {
            self.appearance = tinted_for(self.style_manager.is_dark());
        }
        self.save();
        self.reapply_cached();
    }

    fn reapply_cached(&mut self) {
        self.repair_cached();
        let indexes = self
            .tasks
            .iter()
            .enumerate()
            .filter_map(|(index, task)| {
                (self.visible(index)
                    && matches!(
                        task.state,
                        DesktopTaskState::Converted | DesktopTaskState::Completed
                    ))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        let mut failed = 0;
        for index in indexes {
            if let Err(error) = self.apply_icon(index) {
                failed += 1;
                self.tasks[index].message = format!("cached apply failed: {error}");
            }
        }
        if failed > 0 {
            self.status
                .set_text(&format!("{failed} cached icon(s) could not be applied"));
            self.list_dirty = true;
        }
    }

    fn repair_cached(&mut self) {
        let ids = match self.installer.managed_ids() {
            Ok(ids) => ids,
            Err(error) => {
                self.status
                    .set_text(&format!("Managed icon state unreadable: {error}"));
                return;
            }
        };
        let mut repaired = 0;
        let mut failed = 0;
        for desktop_id in ids {
            let Ok(Some(health)) = self.installer.health(&desktop_id) else {
                failed += 1;
                continue;
            };
            if !matches!(
                health,
                liquid_glass_icon::icon_install::ManagedIconHealth::Repairable
            ) {
                continue;
            }
            let svg_path = self
                .output
                .join(application_output_name(&desktop_id))
                .join("icon.svg");
            let settings = self.render_settings();
            let result = fs::read_to_string(svg_path)
                .map_err(|error| error.to_string())
                .and_then(|svg| {
                    self.installer
                        .repair_cached_svg(&desktop_id, &svg, &mut self.glass, settings)
                        .map_err(|error| error.to_string())
                });
            match result {
                Ok(()) => {
                    repaired += 1;
                    if let Some(index) = self
                        .applications
                        .iter()
                        .position(|application| application.id == desktop_id)
                    {
                        self.tasks[index].message = "auto-repaired from local cache".to_owned();
                    }
                }
                Err(error) => {
                    failed += 1;
                    if let Some(index) = self
                        .applications
                        .iter()
                        .position(|application| application.id == desktop_id)
                    {
                        self.tasks[index].message = format!("auto-repair needs attention: {error}");
                    }
                }
            }
        }
        if repaired > 0 || failed > 0 {
            self.status.set_text(&format!(
                "Auto-repair: {repaired} repaired, {failed} need attention"
            ));
            self.list_dirty = true;
        }
    }
}

const UI_CSS: &str = r#"
.content-root {
  padding: 24px;
}

.hero {
  padding: 2px 4px 0;
}

.hero-title {
  font-size: 26px;
  font-weight: 700;
}

.hero-subtitle,
.section-caption,
.field-label {
  color: alpha(@window_fg_color, 0.64);
}

.card {
  background-color: alpha(@card_bg_color, 0.92);
  border: 1px solid alpha(@window_fg_color, 0.08);
  border-radius: 18px;
  padding: 16px;
}

.section-title {
  font-size: 15px;
  font-weight: 700;
}

.counter,
.status-pill {
  background-color: alpha(@accent_bg_color, 0.16);
  border-radius: 999px;
  color: @accent_color;
  padding: 5px 10px;
}

.preview-surface {
  background-color: @view_bg_color;
  border-radius: 16px;
  padding: 12px;
}

.app-row {
  padding-top: 4px;
  padding-bottom: 4px;
}
"#;

fn install_css() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_data(UI_CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn section_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("section-title");
    label
}

fn field_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("field-label");
    label
}

fn build_window(application: &Application, state: Rc<RefCell<IconApp>>) {
    let (list, preview, preview_title, status) = {
        let app = state.borrow();
        (
            app.list.clone(),
            app.preview.clone(),
            app.preview_title.clone(),
            app.status.clone(),
        )
    };
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    preview.set_content_fit(gtk::ContentFit::Contain);
    preview.set_hexpand(true);
    preview.set_vexpand(true);
    preview.set_size_request(480, 480);
    preview_title.add_css_class("title-2");
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("status-pill");
    let count = state.borrow().count.clone();
    count.add_css_class("counter");

    let provider = gtk::DropDown::from_strings(&["Codex exec", "API key"]);
    let model = gtk::DropDown::from_strings(&MODEL_OPTIONS);
    let api_key = gtk::PasswordEntry::new();
    api_key.set_placeholder_text(Some("OpenAI API key"));
    api_key.set_hexpand(false);
    let generate = gtk::Button::with_label("Generate missing");
    generate.add_css_class("suggested-action");
    let stop = gtk::Button::with_label("Stop");
    stop.add_css_class("destructive-action");
    stop.set_sensitive(false);

    let theme = gtk::DropDown::from_strings(&["System", "Light", "Dark"]);
    let appearance = gtk::DropDown::from_strings(&[
        "Default",
        "Dark",
        "Clear Light",
        "Clear Dark",
        "Tinted Light",
        "Tinted Dark",
    ]);
    let tilt = gtk::Switch::new();
    let color_dialog = gtk::ColorDialog::new();
    color_dialog.set_with_alpha(false);
    let color = gtk::ColorDialogButton::new(Some(color_dialog));
    color.set_tooltip_text(Some(
        "Apple Tinted accent — only local renderer, never sent to AI",
    ));
    let background_dialog = gtk::ColorDialog::new();
    background_dialog.set_with_alpha(false);
    let background_color = gtk::ColorDialogButton::new(Some(background_dialog));
    background_color.set_tooltip_text(Some(
        "Replace every icon background locally — never sent to AI",
    ));
    let reset_background = gtk::Button::with_label("Use source");
    let foreground_opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 20.0, 150.0, 5.0);
    foreground_opacity.set_digits(0);
    foreground_opacity.set_value_pos(gtk::PositionType::Right);
    foreground_opacity.set_tooltip_text(Some(
        "Opacity for every material layer; background and canonical SVG stay unchanged",
    ));

    {
        let app = state.borrow();
        provider.set_selected(if app.provider_choice == ProviderChoice::Api {
            1
        } else {
            0
        });
        model.set_selected(model_index(&app.model));
        theme.set_selected(theme_index(app.theme));
        appearance.set_selected(appearance_index(app.appearance));
        tilt.set_active(app.tilt);
        color.set_rgba(&gdk::RGBA::new(
            app.accent[0] as f32 / 255.0,
            app.accent[1] as f32 / 255.0,
            app.accent[2] as f32 / 255.0,
            1.0,
        ));
        let background = app.background.unwrap_or([78, 94, 128]);
        background_color.set_rgba(&gdk::RGBA::new(
            background[0] as f32 / 255.0,
            background[1] as f32 / 255.0,
            background[2] as f32 / 255.0,
            1.0,
        ));
        foreground_opacity.set_value(f64::from(app.foreground_opacity) * 100.0);
        api_key.set_visible(app.provider_choice == ProviderChoice::Api);
    }

    let category_popover = gtk::Popover::new();
    let category_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    category_box.set_margin_top(12);
    category_box.set_margin_bottom(12);
    category_box.set_margin_start(12);
    category_box.set_margin_end(12);
    for category in AppCategory::ALL {
        let check = gtk::CheckButton::with_label(category.label());
        check.set_active(!state.borrow().blocked_categories.contains(&category));
        let state_for_category = Rc::clone(&state);
        check.connect_toggled(move |check| {
            let mut app = state_for_category.borrow_mut();
            if check.is_active() {
                app.blocked_categories.remove(&category);
            } else {
                app.blocked_categories.insert(category);
            }
            app.list_dirty = true;
            app.save();
            drop(app);
            IconApp::rebuild_list(&state_for_category);
        });
        category_box.append(&check);
    }
    category_popover.set_child(Some(&category_box));
    let categories = gtk::MenuButton::new();
    categories.set_label("Categories");
    categories.set_popover(Some(&category_popover));

    provider.set_hexpand(true);
    model.set_hexpand(true);
    api_key.set_hexpand(true);
    theme.set_hexpand(true);
    appearance.set_hexpand(true);
    foreground_opacity.set_hexpand(true);

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 12);
    controls.add_css_class("card");
    controls.append(&section_label("Conversion"));
    let controls_grid = gtk::Grid::new();
    controls_grid.set_column_spacing(12);
    controls_grid.set_row_spacing(10);
    controls_grid.attach(&field_label("Provider"), 0, 0, 1, 1);
    controls_grid.attach(&provider, 1, 0, 1, 1);
    controls_grid.attach(&field_label("Model"), 2, 0, 1, 1);
    controls_grid.attach(&model, 3, 0, 1, 1);
    controls_grid.attach(&field_label("API key"), 0, 1, 1, 1);
    controls_grid.attach(&api_key, 1, 1, 3, 1);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.append(&generate);
    actions.append(&stop);
    actions.append(&categories);
    controls_grid.attach(&actions, 0, 2, 4, 1);
    controls.append(&controls_grid);

    let settings = gtk::Box::new(gtk::Orientation::Vertical, 12);
    settings.add_css_class("card");
    settings.append(&section_label("Appearance"));
    let settings_grid = gtk::Grid::new();
    settings_grid.set_column_spacing(12);
    settings_grid.set_row_spacing(10);
    settings_grid.attach(&field_label("Theme"), 0, 0, 1, 1);
    settings_grid.attach(&theme, 1, 0, 1, 1);
    settings_grid.attach(&field_label("Material"), 2, 0, 1, 1);
    settings_grid.attach(&appearance, 3, 0, 1, 1);
    settings_grid.attach(&field_label("Global accent"), 4, 0, 1, 1);
    settings_grid.attach(&color, 5, 0, 1, 1);
    settings_grid.attach(&field_label("Global background"), 0, 1, 1, 1);
    settings_grid.attach(&background_color, 1, 1, 1, 1);
    settings_grid.attach(&reset_background, 2, 1, 1, 1);
    settings_grid.attach(&field_label("3D tilt"), 4, 1, 1, 1);
    settings_grid.attach(&tilt, 5, 1, 1, 1);
    settings_grid.attach(&field_label("Foreground opacity"), 0, 2, 1, 1);
    settings_grid.attach(&foreground_opacity, 1, 2, 2, 1);
    settings.append(&settings_grid);

    let list_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .min_content_width(430)
        .child(&list)
        .build();
    let list_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    list_header.append(&section_label("Applications"));
    list_header.append(&count);
    let list_panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
    list_panel.add_css_class("card");
    list_panel.set_vexpand(true);
    list_panel.set_size_request(440, -1);
    list_panel.append(&list_header);
    list_panel.append(&list_scroll);

    let preview_caption = gtk::Label::new(Some(
        "Runtime material preview · move the cursor to inspect layer depth",
    ));
    preview_caption.add_css_class("section-caption");
    preview_caption.set_halign(gtk::Align::Start);
    let preview_heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    preview_heading.append(&preview_title);
    preview_heading.append(&preview_caption);
    let preview_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    preview_controls.append(&field_label("Inspect"));
    let preview_selector = state.borrow().preview_selector.clone();
    preview_selector.set_hexpand(true);
    preview_controls.append(&preview_selector);
    preview_heading.append(&preview_controls);
    let preview_surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
    preview_surface.add_css_class("preview-surface");
    preview_surface.set_vexpand(true);
    preview_surface.append(&preview);
    let motion = gtk::EventControllerMotion::new();
    let state_for_motion = Rc::clone(&state);
    let preview_surface_for_motion = preview_surface.clone();
    motion.connect_motion(move |_, x, y| {
        let Ok(mut app) = state_for_motion.try_borrow_mut() else {
            return;
        };
        if app.selected.is_none() || !app.glass.has_preview() {
            return;
        }
        app.preview_pointer = preview_pointer(
            x,
            y,
            preview_surface_for_motion.width(),
            preview_surface_for_motion.height(),
        );
        app.render_preview();
    });
    let state_for_motion_leave = Rc::clone(&state);
    motion.connect_leave(move |_| {
        let Ok(mut app) = state_for_motion_leave.try_borrow_mut() else {
            return;
        };
        if app.preview_pointer != [0.0, 0.0] && app.selected.is_some() {
            app.preview_pointer = [0.0, 0.0];
            app.render_preview();
        }
    });
    preview_surface.add_controller(motion);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 14);
    right.add_css_class("card");
    right.set_hexpand(true);
    right.set_vexpand(true);
    right.append(&preview_heading);
    right.append(&preview_surface);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    content.set_vexpand(true);
    content.append(&list_panel);
    content.append(&right);

    let hero = gtk::Box::new(gtk::Orientation::Vertical, 4);
    hero.add_css_class("hero");
    let hero_title = gtk::Label::new(Some("Make your app grid feel intentional"));
    hero_title.set_halign(gtk::Align::Start);
    hero_title.add_css_class("hero-title");
    let hero_subtitle = gtk::Label::new(Some(
        "Convert each icon once, then tune the glass locally without another AI request.",
    ));
    hero_subtitle.set_halign(gtk::Align::Start);
    hero_subtitle.add_css_class("hero-subtitle");
    hero.append(&hero_title);
    hero.append(&hero_subtitle);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
    root.add_css_class("content-root");
    root.append(&hero);
    root.append(&controls);
    root.append(&settings);
    root.append(&content);
    root.append(&status);
    let clamp = Clamp::new();
    clamp.set_maximum_size(1480);
    clamp.set_tightening_threshold(1000);
    clamp.set_child(Some(&root));

    let header_title = gtk::Label::new(Some("Liquid Glass Icons"));
    header_title.add_css_class("title-4");
    let header_subtitle = gtk::Label::new(Some("Layered app icon studio"));
    header_subtitle.add_css_class("dim-label");
    let header_stack = gtk::Box::new(gtk::Orientation::Vertical, 0);
    header_stack.append(&header_title);
    header_stack.append(&header_subtitle);
    let header = HeaderBar::new();
    header.set_title_widget(Some(&header_stack));
    let toolbar = ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&clamp));
    let window = ApplicationWindow::builder()
        .application(application)
        .title("Liquid Glass Icons")
        .default_width(1240)
        .default_height(820)
        .content(&toolbar)
        .build();

    {
        let state_for_generate = Rc::clone(&state);
        generate.connect_clicked(move |_| state_for_generate.borrow_mut().start_missing());
        let state_for_stop = Rc::clone(&state);
        stop.connect_clicked(move |_| state_for_stop.borrow_mut().stop());
        let state_for_provider = Rc::clone(&state);
        let api_key_for_visibility = api_key.clone();
        provider.connect_selected_notify(move |dropdown| {
            let mut app = state_for_provider.borrow_mut();
            app.provider_choice = if dropdown.selected() == 1 {
                ProviderChoice::Api
            } else {
                ProviderChoice::Codex
            };
            api_key_for_visibility.set_visible(app.provider_choice == ProviderChoice::Api);
            app.save();
        });
        let state_for_api_key = Rc::clone(&state);
        api_key.connect_changed(move |entry| {
            state_for_api_key.borrow_mut().api_key = entry.text().to_string();
        });
        let state_for_model = Rc::clone(&state);
        model.connect_selected_notify(move |dropdown| {
            let mut app = state_for_model.borrow_mut();
            app.model = MODEL_OPTIONS
                .get(dropdown.selected() as usize)
                .unwrap_or(&DEFAULT_MODEL)
                .to_string();
            app.save();
        });
        let state_for_preview = Rc::clone(&state);
        let preview_selector_updating = state.borrow().preview_selector_updating.clone();
        preview_selector.connect_selected_notify(move |dropdown| {
            if preview_selector_updating.get() {
                return;
            }
            if let Ok(mut app) = state_for_preview.try_borrow_mut() {
                app.set_preview_layer(dropdown.selected());
            }
        });
        let state_for_theme = Rc::clone(&state);
        theme.connect_selected_notify(move |dropdown| {
            state_for_theme
                .borrow_mut()
                .set_theme(theme_from_index(dropdown.selected()))
        });
        let state_for_appearance = Rc::clone(&state);
        appearance.connect_selected_notify(move |dropdown| {
            let mut app = state_for_appearance.borrow_mut();
            app.appearance = Appearance::ALL
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or(Appearance::TintedLight);
            app.save();
            app.reapply_cached();
        });
        let state_for_color = Rc::clone(&state);
        color.connect_rgba_notify(move |button| {
            let rgba = button.rgba();
            let mut app = state_for_color.borrow_mut();
            app.accent = [
                (rgba.red() * 255.0) as u8,
                (rgba.green() * 255.0) as u8,
                (rgba.blue() * 255.0) as u8,
            ];
            app.appearance = tinted_for(app.style_manager.is_dark());
            app.save();
            app.reapply_cached();
        });
        let state_for_background = Rc::clone(&state);
        background_color.connect_rgba_notify(move |button| {
            let rgba = button.rgba();
            let mut app = state_for_background.borrow_mut();
            app.background = Some([
                (rgba.red() * 255.0) as u8,
                (rgba.green() * 255.0) as u8,
                (rgba.blue() * 255.0) as u8,
            ]);
            app.save();
            app.reapply_cached();
        });
        let state_for_background_reset = Rc::clone(&state);
        reset_background.connect_clicked(move |_| {
            let mut app = state_for_background_reset.borrow_mut();
            app.background = None;
            app.save();
            app.reapply_cached();
        });
        let state_for_foreground_opacity = Rc::clone(&state);
        foreground_opacity.connect_value_changed(move |scale| {
            let mut app = state_for_foreground_opacity.borrow_mut();
            app.foreground_opacity = (scale.value() as f32 / 100.0).clamp(0.20, 1.50);
            app.save();
            app.reapply_cached();
        });
        let state_for_tilt = Rc::clone(&state);
        tilt.connect_active_notify(move |toggle| {
            let mut app = state_for_tilt.borrow_mut();
            app.tilt = toggle.is_active();
            app.save();
            if app.selected.is_some() {
                app.render_preview();
            }
        });
    }

    let state_for_tick = Rc::clone(&state);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let (running, list_dirty) = {
            let mut app = state_for_tick.borrow_mut();
            let running = app.receiver.is_some();
            app.receive_events();
            (running, app.list_dirty)
        };
        generate.set_sensitive(!running);
        stop.set_sensitive(running);
        if list_dirty {
            IconApp::rebuild_list(&state_for_tick);
        }
        glib::ControlFlow::Continue
    });
    IconApp::rebuild_list(&state);
    if let Some(index) = {
        let app = state.borrow();
        app.applications.iter().enumerate().find_map(|(index, _)| {
            (app.visible(index) && app.tasks[index].state == DesktopTaskState::Converted)
                .then_some(index)
        })
    } {
        state.borrow_mut().select(index);
    }
    window.connect_close_request(move |_| {
        state.borrow().save();
        glib::Propagation::Proceed
    });
    window.present();
}

fn initial_task(application: &DesktopApplication, output: &Path) -> TaskRow {
    let Some(_) = application.icon_path else {
        return TaskRow {
            state: DesktopTaskState::Skipped,
            message: "icon not found".to_owned(),
        };
    };
    let Ok(input) = application.input() else {
        return TaskRow {
            state: DesktopTaskState::Failed,
            message: "icon unreadable".to_owned(),
        };
    };
    match cache_status(&app_output(output, application), &input.bytes) {
        CacheStatus::Missing => TaskRow {
            state: DesktopTaskState::Queued,
            message: "needs conversion".to_owned(),
        },
        CacheStatus::Current => TaskRow {
            state: DesktopTaskState::Converted,
            message: "already converted".to_owned(),
        },
        CacheStatus::Legacy => TaskRow {
            state: DesktopTaskState::Converted,
            message: "legacy layout; reconvert to upgrade".to_owned(),
        },
        CacheStatus::Stale => TaskRow {
            state: DesktopTaskState::Stale,
            message: "source changed; reconvert manually".to_owned(),
        },
    }
}

fn app_output(output: &Path, application: &DesktopApplication) -> PathBuf {
    output
        .join("apps")
        .join(application_output_name(&application.id))
}

fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("liquid-glass-icon/settings.json")
}

fn load_settings() -> SavedSettings {
    fs::read(config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}
fn default_appearance() -> Appearance {
    Appearance::TintedLight
}
fn default_accent() -> [u8; 3] {
    [137, 180, 250]
}
fn default_foreground_opacity() -> f32 {
    1.0
}
fn default_tilt() -> bool {
    true
}
fn normalize_model(model: String) -> String {
    let model = model.trim();
    if model.is_empty() {
        DEFAULT_MODEL.to_owned()
    } else {
        model.to_owned()
    }
}
fn color_scheme(theme: ThemeChoice) -> adw::ColorScheme {
    match theme {
        ThemeChoice::System => adw::ColorScheme::Default,
        ThemeChoice::Light => adw::ColorScheme::ForceLight,
        ThemeChoice::Dark => adw::ColorScheme::ForceDark,
    }
}
fn tinted_for(dark: bool) -> Appearance {
    if dark {
        Appearance::TintedDark
    } else {
        Appearance::TintedLight
    }
}
fn theme_index(theme: ThemeChoice) -> u32 {
    match theme {
        ThemeChoice::System => 0,
        ThemeChoice::Light => 1,
        ThemeChoice::Dark => 2,
    }
}
fn theme_from_index(index: u32) -> ThemeChoice {
    match index {
        1 => ThemeChoice::Light,
        2 => ThemeChoice::Dark,
        _ => ThemeChoice::System,
    }
}
fn appearance_index(appearance: Appearance) -> u32 {
    Appearance::ALL
        .iter()
        .position(|candidate| *candidate == appearance)
        .unwrap_or(4) as u32
}
fn model_index(model: &str) -> u32 {
    MODEL_OPTIONS
        .iter()
        .position(|candidate| *candidate == model)
        .unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::{preview_layer_from_selection, preview_pointer};

    #[test]
    fn composite_selection_has_no_layer() {
        assert_eq!(preview_layer_from_selection(0), None);
        assert_eq!(preview_layer_from_selection(1), Some(0));
        assert_eq!(preview_layer_from_selection(5), Some(4));
    }

    #[test]
    fn preview_pointer_is_normalized_and_clamped() {
        assert_eq!(preview_pointer(50.0, 50.0, 100, 100), [0.0, 0.0]);
        assert_eq!(preview_pointer(0.0, 100.0, 100, 100), [-1.0, 1.0]);
        assert_eq!(preview_pointer(300.0, -20.0, 100, 100), [1.0, -1.0]);
    }
}
