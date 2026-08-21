use crate::{error::IconError, input::read_input, model::IconInput};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

const IMAGE_EXTENSIONS: [&str; 5] = ["png", "svg", "webp", "jpg", "jpeg"];

#[derive(Debug, Clone)]
pub struct DesktopApplication {
    pub id: String,
    pub name: String,
    pub desktop_file: PathBuf,
    pub icon_name: String,
    pub icon_path: Option<PathBuf>,
    pub categories: Vec<String>,
    pub category: AppCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppCategory {
    Media,
    Development,
    Education,
    Games,
    Graphics,
    Office,
    Science,
    Utilities,
    Internet,
    NetworkTools,
    System,
    Settings,
    Terminal,
    Other,
}

impl AppCategory {
    pub const ALL: [Self; 14] = [
        Self::Media,
        Self::Development,
        Self::Education,
        Self::Games,
        Self::Graphics,
        Self::Office,
        Self::Science,
        Self::Utilities,
        Self::Internet,
        Self::NetworkTools,
        Self::System,
        Self::Settings,
        Self::Terminal,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Media => "Media",
            Self::Development => "Development",
            Self::Education => "Education & Science",
            Self::Games => "Games",
            Self::Graphics => "Graphics",
            Self::Office => "Office",
            Self::Science => "Science",
            Self::Utilities => "Utilities",
            Self::Internet => "Internet",
            Self::NetworkTools => "Network tools",
            Self::System => "System",
            Self::Settings => "Settings",
            Self::Terminal => "Terminal",
            Self::Other => "Other",
        }
    }

    pub fn enabled_by_default(self) -> bool {
        matches!(
            self,
            Self::Media
                | Self::Development
                | Self::Education
                | Self::Games
                | Self::Graphics
                | Self::Office
                | Self::Science
                | Self::Utilities
                | Self::Internet
        )
    }
}

impl DesktopApplication {
    pub fn input(&self) -> Result<IconInput, IconError> {
        let path = self
            .icon_path
            .as_ref()
            .ok_or_else(|| IconError::MissingDesktopIcon {
                application: self.name.clone(),
                icon: self.icon_name.clone(),
            })?;
        read_input(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopTaskState {
    Queued,
    Processing,
    Completed,
    Converted,
    Stale,
    Stopped,
    Skipped,
    Failed,
}

impl DesktopTaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Converted => "converted",
            Self::Stale => "stale",
            Self::Stopped => "stopped",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DesktopTaskEvent {
    pub application_id: String,
    pub application_name: String,
    pub state: DesktopTaskState,
    pub message: String,
    pub result: Option<crate::model::TransformResult>,
}

pub fn discover_desktop_applications() -> Vec<DesktopApplication> {
    let data_dirs = data_dirs();
    let application_dirs = data_dirs
        .iter()
        .map(|directory| directory.join("applications"))
        .collect::<Vec<_>>();
    discover_desktop_applications_from_dirs(&application_dirs, &data_dirs)
}

pub fn discover_desktop_applications_from_dirs(
    application_dirs: &[PathBuf],
    data_dirs: &[PathBuf],
) -> Vec<DesktopApplication> {
    let icon_roots = icon_roots(application_dirs, data_dirs);
    let icon_index = build_icon_index(&icon_roots);
    let mut seen_ids = HashSet::new();
    let mut applications = Vec::new();

    for directory in application_dirs {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !seen_ids.insert(id.to_owned()) {
                continue;
            }
            let Some(entry) = parse_application_entry(&path) else {
                continue;
            };
            let icon_path = resolve_icon(&entry.icon_name, &path, &icon_index);
            applications.push(DesktopApplication {
                id: id.to_owned(),
                name: entry.name,
                desktop_file: path,
                icon_name: entry.icon_name,
                icon_path,
                category: classify_category(&entry.categories, entry.terminal),
                categories: entry.categories,
            });
        }
    }

    applications.sort_by_cached_key(|application| application.name.to_lowercase());
    applications
}

pub fn application_output_name(id: &str) -> String {
    let stem = Path::new(id)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(id);
    let mut output = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() {
        output.push_str("application");
    }
    output
}

fn data_dirs() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let user_data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".local/share")));
    let system_data = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());

    let mut directories = Vec::new();
    if let Some(directory) = user_data {
        directories.push(directory);
    }
    directories.extend(
        system_data
            .split(':')
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    );
    unique_paths(directories)
}

fn icon_roots(application_dirs: &[PathBuf], data_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = data_dirs.to_vec();
    roots.extend(
        application_dirs
            .iter()
            .filter_map(|directory| directory.parent().map(Path::to_path_buf)),
    );
    unique_paths(roots)
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

struct DesktopEntryMetadata {
    name: String,
    icon_name: String,
    categories: Vec<String>,
    terminal: bool,
}

fn parse_application_entry(path: &Path) -> Option<DesktopEntryMetadata> {
    let contents = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut entry_type = None;
    let mut name = None;
    let mut localized_name = None;
    let mut icon = None;
    let mut original_icon = None;
    let mut categories = Vec::new();
    let mut terminal = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut only_show_in = None;
    let mut not_show_in = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let value = decode_desktop_value(raw_value.trim());
        match key {
            "Type" => entry_type = Some(value),
            "Name" => name = Some(value),
            key if key.starts_with("Name[") && localized_name.is_none() => {
                localized_name = Some(value)
            }
            "Icon" => icon = Some(value),
            "X-Liquid-Glass-Original-Icon" => original_icon = Some(value),
            "Categories" => {
                categories = value
                    .split(';')
                    .filter(|category| !category.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "OnlyShowIn" => only_show_in = Some(split_list(&value)),
            "NotShowIn" => not_show_in = Some(split_list(&value)),
            _ => {}
        }
    }

    if entry_type.as_deref() != Some("Application")
        || hidden
        || no_display
        || !show_in_current_desktop(only_show_in.as_deref(), not_show_in.as_deref())
    {
        return None;
    }
    let name = name.or(localized_name)?;
    let icon = original_icon.or(icon).filter(|value| !value.is_empty())?;
    Some(DesktopEntryMetadata {
        name,
        icon_name: icon,
        categories,
        terminal,
    })
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn show_in_current_desktop(
    only_show_in: Option<&[String]>,
    not_show_in: Option<&[String]>,
) -> bool {
    let current_desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktops = current_desktop
        .split(':')
        .filter(|desktop| !desktop.is_empty())
        .collect::<HashSet<_>>();
    if let Some(only_show_in) = only_show_in
        && !only_show_in
            .iter()
            .any(|desktop| desktops.contains(desktop.as_str()))
    {
        return false;
    }
    if let Some(not_show_in) = not_show_in
        && not_show_in
            .iter()
            .any(|desktop| desktops.contains(desktop.as_str()))
    {
        return false;
    }
    true
}

fn classify_category(categories: &[String], terminal: bool) -> AppCategory {
    let has = |category: &str| categories.iter().any(|value| value == category);
    if terminal || has("ConsoleOnly") {
        return AppCategory::Terminal;
    }
    if has("Settings")
        || categories
            .iter()
            .any(|value| value.starts_with("X-GNOME-Settings"))
    {
        return AppCategory::Settings;
    }
    if has("System") {
        return AppCategory::System;
    }
    if has("Network") {
        let interactive_network = [
            "WebBrowser",
            "Email",
            "InstantMessaging",
            "Chat",
            "IRCClient",
            "FileTransfer",
            "News",
            "P2P",
            "RemoteAccess",
            "Telephony",
            "VideoConference",
        ];
        return if interactive_network.iter().any(|category| has(category)) {
            AppCategory::Internet
        } else {
            AppCategory::NetworkTools
        };
    }
    if has("AudioVideo") || has("Audio") || has("Video") {
        return AppCategory::Media;
    }
    if has("Development") {
        return AppCategory::Development;
    }
    if has("Education") {
        return AppCategory::Education;
    }
    if has("Game") {
        return AppCategory::Games;
    }
    if has("Graphics") {
        return AppCategory::Graphics;
    }
    if has("Office") {
        return AppCategory::Office;
    }
    if has("Science") || has("HealthFitness") {
        return AppCategory::Science;
    }
    if has("Utility") {
        return AppCategory::Utilities;
    }
    AppCategory::Other
}

fn decode_desktop_value(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            decoded.push(match character {
                's' => ' ',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                _ => character,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            decoded.push(character);
        }
    }
    if escaped {
        decoded.push('\\');
    }
    decoded
}

fn resolve_icon(
    icon_name: &str,
    desktop_file: &Path,
    icon_index: &HashMap<String, Vec<PathBuf>>,
) -> Option<PathBuf> {
    let icon_path = Path::new(icon_name);
    if icon_path.is_absolute() {
        return existing_icon_path(icon_path);
    }
    if icon_name.contains('/')
        && let Some(path) = existing_icon_path(&desktop_file.parent()?.join(icon_path))
    {
        return Some(path);
    }

    let requested = Path::new(icon_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(icon_name);
    let key = if icon_index.contains_key(requested) {
        requested.to_owned()
    } else {
        Path::new(requested)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(requested)
            .to_owned()
    };
    icon_index
        .get(&key)
        .and_then(|paths| paths.first())
        .cloned()
}

fn existing_icon_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() && supported_image(path) {
        return Some(path.to_owned());
    }
    if path.extension().is_some() {
        return None;
    }
    IMAGE_EXTENSIONS
        .iter()
        .map(|extension| path.with_extension(extension))
        .find(|candidate| candidate.is_file())
}

fn build_icon_index(icon_roots: &[PathBuf]) -> HashMap<String, Vec<PathBuf>> {
    let mut index = HashMap::new();
    for root in icon_roots {
        collect_icon_files(&root.join("pixmaps"), &mut index);
        collect_icon_files(&root.join("icons"), &mut index);
    }
    for paths in index.values_mut() {
        paths.sort_by_key(|path| {
            (
                extension_rank(path),
                path.to_string_lossy().to_ascii_lowercase(),
            )
        });
    }
    index
}

fn collect_icon_files(directory: &Path, index: &mut HashMap<String, Vec<PathBuf>>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_icon_files(&path, index);
        } else if file_type.is_file() && supported_image(&path) {
            if let Some(file_name) = path.file_name().and_then(|value| value.to_str()) {
                index
                    .entry(file_name.to_owned())
                    .or_default()
                    .push(path.clone());
            }
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                index.entry(stem.to_owned()).or_default().push(path);
            }
        }
    }
}

fn supported_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "svg" | "webp" | "jpg" | "jpeg")
    )
}

fn extension_rank(path: &Path) -> u8 {
    match path.extension().and_then(|value| value.to_str()) {
        Some("svg") => 0,
        Some("png") => 1,
        Some("webp") => 2,
        Some("jpg" | "jpeg") => 3,
        _ => 3,
    }
}
