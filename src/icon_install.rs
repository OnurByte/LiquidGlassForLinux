use crate::{
    desktop::{DesktopApplication, application_output_name},
    error::IconError,
    renderer::{GlassRenderer, RenderSettings, RenderTarget},
};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const ICON_SIZES: [u32; 11] = [16, 24, 32, 48, 64, 96, 128, 192, 256, 512, 1024];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedIcon {
    desktop_id: String,
    override_path: PathBuf,
    icon_files: Vec<PathBuf>,
    managed_sha256: String,
    /// Renderer/material/composition revision the PNGs were produced with.
    /// Missing (older state files) means stale so icons are rebuilt from
    /// their canonical SVG without another AI request.
    #[serde(default)]
    renderer_revision: u32,
    backup_path: Option<PathBuf>,
    /// Durable source used to recreate a vanished user override without an AI
    /// request. `backup_path` remains reserved for restoring a pre-existing
    /// user launcher verbatim.
    #[serde(default)]
    source_backup_path: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ManagedIcons {
    entries: HashMap<String, ManagedIcon>,
}

#[derive(Debug, Clone)]
pub struct IconInstaller {
    data_home: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedIconHealth {
    Healthy,
    Repairable,
    UserModified,
}

impl ManagedIconHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Repairable => "repairable",
            Self::UserModified => "user-modified",
        }
    }
}

struct PreparedDesktopOverride {
    source: String,
    override_path: PathBuf,
    previous_override: Option<Vec<u8>>,
    state: ManagedIcons,
    previous: Option<ManagedIcon>,
    backup_path: Option<PathBuf>,
    source_backup_path: Option<PathBuf>,
    source_backup: Option<(PathBuf, Vec<u8>)>,
    backup: Option<(PathBuf, Vec<u8>)>,
}

impl IconInstaller {
    pub fn new() -> Self {
        let data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        Self { data_home }
    }

    #[doc(hidden)]
    pub fn with_data_home_for_test(data_home: PathBuf) -> Self {
        Self { data_home }
    }

    pub fn apply_svg(
        &self,
        application: &DesktopApplication,
        svg: &str,
        renderer: &mut GlassRenderer,
        settings: RenderSettings,
    ) -> Result<(), IconError> {
        // Resolve the launcher before creating PNGs. Previously this happened
        // afterwards, which left orphan files when an app bundle disappeared.
        let prepared = self.prepare_desktop_override(application)?;
        renderer.load_svg(svg)?;
        self.ensure_hicolor_theme()?;
        let rendered = ICON_SIZES
            .into_iter()
            .map(|size| {
                renderer
                    .render(size, size, settings, RenderTarget::Icon)
                    .map(|image| (size, image))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let fingerprint = crate::manifest::sha256(rendered[1].1.as_raw());
        let icon_name = format!(
            "liquid-glass-{}-{}",
            application_output_name(&application.id),
            &fingerprint[..12]
        );
        let mut icon_files = Vec::new();
        for (size, image) in rendered {
            let path = self.icon_path(&icon_name, size);
            if let Err(error) = write_png_atomically(&path, &image) {
                for written in &icon_files {
                    if !prepared
                        .previous
                        .as_ref()
                        .is_some_and(|previous| previous.icon_files.contains(written))
                    {
                        let _ = fs::remove_file(written);
                    }
                }
                return Err(error);
            }
            icon_files.push(path);
        }
        self.install_desktop_override(application, &icon_name, icon_files, prepared)
    }

    fn install_desktop_override(
        &self,
        application: &DesktopApplication,
        icon_name: &str,
        icon_files: Vec<PathBuf>,
        mut prepared: PreparedDesktopOverride,
    ) -> Result<(), IconError> {
        let state_path = self.state_path();
        let previous_icon_files = prepared
            .previous
            .as_ref()
            .map(|entry| entry.icon_files.clone())
            .unwrap_or_default();
        let rewritten = replace_desktop_icon(&prepared.source, icon_name, &application.icon_name)?;
        if let Some((path, bytes)) = prepared.backup.take() {
            write_bytes_atomically(&path, &bytes)?;
        }
        if let Some((path, bytes)) = prepared.source_backup.take() {
            write_bytes_atomically(&path, &bytes)?;
        }
        replace_desktop_entry_for_refresh(&prepared.override_path, rewritten.as_bytes())?;
        let managed_sha256 = crate::manifest::sha256(rewritten.as_bytes());
        let current_file_names = icon_files
            .iter()
            .filter_map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        prepared.state.entries.insert(
            application.id.clone(),
            ManagedIcon {
                desktop_id: application.id.clone(),
                override_path: prepared.override_path.clone(),
                icon_files,
                managed_sha256,
                renderer_revision: crate::renderer::RENDERER_REVISION,
                backup_path: prepared.backup_path,
                source_backup_path: prepared.source_backup_path,
            },
        );
        if let Err(error) = write_state(&state_path, &prepared.state) {
            if let Some(previous) = prepared.previous_override {
                let _ = replace_desktop_entry_for_refresh(&prepared.override_path, &previous);
            } else {
                let _ = fs::remove_file(&prepared.override_path);
            }
            return Err(error);
        }
        if let Some(current) = prepared.state.entries.get(&application.id) {
            for path in previous_icon_files {
                if !current.icon_files.contains(&path) {
                    let _ = fs::remove_file(path);
                }
            }
        }
        self.remove_icon_family(
            &application_output_name(&application.id),
            &current_file_names,
        );
        refresh_desktop_caches(&self.data_home)?;
        Ok(())
    }

    fn prepare_desktop_override(
        &self,
        application: &DesktopApplication,
    ) -> Result<PreparedDesktopOverride, IconError> {
        let override_path = self.data_home.join("applications").join(&application.id);
        let state = read_state(&self.state_path())?;
        let previous = state.entries.get(&application.id).cloned();
        let previous_override = override_path
            .is_file()
            .then(|| fs::read(&override_path))
            .transpose()?;
        let current_source = previous_override
            .as_deref()
            .map(|contents| String::from_utf8_lossy(contents).into_owned());

        if let (Some(previous), Some(current)) = (&previous, &previous_override)
            && crate::manifest::sha256(current) != previous.managed_sha256
        {
            return Err(IconError::Manifest(format!(
                "refusing to replace {}: user desktop entry changed",
                application.id
            )));
        }

        let source = if let Some(source) = current_source {
            source
        } else {
            self.read_recovery_source(application, previous.as_ref())?
        };

        let backup_path = previous
            .as_ref()
            .and_then(|entry| entry.backup_path.clone());
        let backup = if previous.is_none() && previous_override.is_some() {
            let path = self.backup_path(&application.id);
            Some((path.clone(), previous_override.clone().unwrap_or_default()))
        } else {
            None
        };
        let backup_path = backup
            .as_ref()
            .map(|(path, _)| path.clone())
            .or(backup_path);

        let existing_source_backup = previous
            .as_ref()
            .and_then(|entry| entry.source_backup_path.clone())
            .filter(|path| path.is_file())
            .or_else(|| backup_path.clone().filter(|path| path.is_file()));
        let (source_backup_path, source_backup) = if let Some(path) = existing_source_backup {
            (Some(path), None)
        } else if let Some((path, bytes)) = &backup {
            (Some(path.clone()), Some((path.clone(), bytes.clone())))
        } else {
            let path = self.source_backup_path(&application.id);
            let snapshot = fs::read(&application.desktop_file)
                .ok()
                .filter(|bytes| !bytes.is_empty())
                .unwrap_or_else(|| source.as_bytes().to_vec());
            (Some(path.clone()), Some((path, snapshot)))
        };

        Ok(PreparedDesktopOverride {
            source,
            override_path,
            previous_override,
            state,
            previous,
            backup_path,
            source_backup_path,
            source_backup,
            backup,
        })
    }

    fn read_recovery_source(
        &self,
        application: &DesktopApplication,
        previous: Option<&ManagedIcon>,
    ) -> Result<String, IconError> {
        let mut candidates = vec![application.desktop_file.clone()];
        if let Some(previous) = previous {
            if let Some(path) = &previous.source_backup_path {
                candidates.push(path.clone());
            }
            if let Some(path) = &previous.backup_path {
                candidates.push(path.clone());
            }
        }
        for path in candidates {
            if let Ok(source) = fs::read_to_string(&path) {
                return Ok(source);
            }
        }
        Err(IconError::Manifest(format!(
            "cannot repair {}: original desktop entry is unavailable",
            application.id
        )))
    }

    pub fn health(&self, desktop_id: &str) -> Result<Option<ManagedIconHealth>, IconError> {
        let state = read_state(&self.state_path())?;
        let Some(entry) = state.entries.get(desktop_id) else {
            return Ok(None);
        };
        if !entry.override_path.is_file() {
            return Ok(Some(ManagedIconHealth::Repairable));
        }
        let override_contents = fs::read(&entry.override_path)?;
        if crate::manifest::sha256(&override_contents) != entry.managed_sha256 {
            return Ok(Some(ManagedIconHealth::UserModified));
        }
        let valid_files = entry.icon_files.len() == ICON_SIZES.len()
            && entry
                .icon_files
                .iter()
                .zip(ICON_SIZES)
                .all(|(path, size)| usable_icon(path, size));
        if entry.renderer_revision == crate::renderer::RENDERER_REVISION && valid_files {
            Ok(Some(ManagedIconHealth::Healthy))
        } else {
            Ok(Some(ManagedIconHealth::Repairable))
        }
    }

    pub fn repair_cached_svg(
        &self,
        desktop_id: &str,
        svg: &str,
        renderer: &mut GlassRenderer,
        settings: RenderSettings,
    ) -> Result<(), IconError> {
        let state = read_state(&self.state_path())?;
        let entry = state
            .entries
            .get(desktop_id)
            .ok_or_else(|| IconError::Manifest(format!("{desktop_id} is not managed")))?;
        let source_path = entry
            .source_backup_path
            .as_ref()
            .filter(|path| path.is_file())
            .or_else(|| entry.backup_path.as_ref().filter(|path| path.is_file()))
            .ok_or_else(|| {
                IconError::Manifest(format!(
                    "cannot repair {desktop_id}: no saved desktop entry"
                ))
            })?
            .clone();
        let source = fs::read_to_string(&source_path)?;
        let icon_name = original_icon_name(&source).ok_or_else(|| {
            IconError::Manifest(format!(
                "cannot repair {desktop_id}: desktop entry has no Icon key"
            ))
        })?;
        let application = DesktopApplication {
            id: desktop_id.to_owned(),
            name: desktop_id.to_owned(),
            desktop_file: source_path,
            icon_name,
            icon_path: None,
            categories: Vec::new(),
            category: crate::desktop::AppCategory::Other,
        };
        self.apply_svg(&application, svg, renderer, settings)
    }

    pub fn restore(&self, desktop_id: &str) -> Result<(), IconError> {
        let state_path = self.state_path();
        let mut state = read_state(&state_path)?;
        let Some(entry) = state.entries.remove(desktop_id) else {
            return Ok(());
        };
        if entry.override_path.is_file() {
            let current = fs::read(&entry.override_path)?;
            if crate::manifest::sha256(&current) != entry.managed_sha256 {
                return Err(IconError::Manifest(format!(
                    "refusing to restore {desktop_id}: user desktop entry changed"
                )));
            }
            if let Some(backup) = entry.backup_path {
                replace_desktop_entry_for_refresh(&entry.override_path, &fs::read(backup)?)?;
            } else {
                fs::remove_file(&entry.override_path)?;
            }
        }
        for path in entry.icon_files {
            let _ = fs::remove_file(path);
        }
        self.remove_icon_family(&application_output_name(&entry.desktop_id), &[]);
        write_state(&state_path, &state)?;
        refresh_desktop_caches(&self.data_home)?;
        Ok(())
    }

    pub fn is_managed(&self, desktop_id: &str) -> bool {
        read_state(&self.state_path())
            .map(|state| state.entries.contains_key(desktop_id))
            .unwrap_or(false)
    }

    pub fn managed_ids(&self) -> Result<Vec<String>, IconError> {
        let mut ids = read_state(&self.state_path())?
            .entries
            .into_keys()
            .collect::<Vec<_>>();
        ids.sort();
        Ok(ids)
    }

    fn icon_path(&self, icon_name: &str, size: u32) -> PathBuf {
        self.data_home
            .join("icons/hicolor")
            .join(format!("{size}x{size}/apps"))
            .join(format!("{icon_name}.png"))
    }

    fn ensure_hicolor_theme(&self) -> Result<(), IconError> {
        let root = self.data_home.join("icons/hicolor");
        let index = root.join("index.theme");
        if !index.exists() {
            write_bytes_atomically(&index, hicolor_index().as_bytes())?;
        }
        Ok(())
    }

    fn remove_icon_family(&self, output_name: &str, keep: &[String]) {
        let prefix = format!("liquid-glass-{output_name}-");
        for size in ICON_SIZES {
            let directory = self
                .data_home
                .join("icons/hicolor")
                .join(format!("{size}x{size}/apps"));
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if path.is_file()
                    && name.starts_with(&prefix)
                    && !keep.iter().any(|keep| keep == name)
                {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }

    fn state_path(&self) -> PathBuf {
        self.data_home.join("liquid-glass-icon/managed-icons.json")
    }

    fn backup_path(&self, desktop_id: &str) -> PathBuf {
        self.data_home
            .join("liquid-glass-icon/backups")
            .join(desktop_id)
    }

    fn source_backup_path(&self, desktop_id: &str) -> PathBuf {
        self.data_home
            .join("liquid-glass-icon/sources")
            .join(desktop_id)
    }
}

impl Default for IconInstaller {
    fn default() -> Self {
        Self::new()
    }
}

fn replace_desktop_icon(
    contents: &str,
    icon_name: &str,
    original_icon_name: &str,
) -> Result<String, IconError> {
    let mut output = String::with_capacity(contents.len() + icon_name.len());
    let mut in_entry = false;
    let mut replaced = false;
    let has_original_marker = contents
        .lines()
        .any(|line| line.trim().starts_with("X-Liquid-Glass-Original-Icon="));
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_entry = trimmed == "[Desktop Entry]";
        }
        if in_entry && trimmed.starts_with("Icon=") {
            output.push_str("Icon=");
            output.push_str(icon_name);
            replaced = true;
            if !has_original_marker {
                output.push('\n');
                output.push_str("X-Liquid-Glass-Original-Icon=");
                output.push_str(original_icon_name);
                output.push('\n');
                continue;
            }
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if !replaced {
        return Err(IconError::Manifest(
            "desktop entry has no Icon key".to_owned(),
        ));
    }
    if !contents.ends_with('\n') {
        output.pop();
    }
    Ok(output)
}

fn original_icon_name(contents: &str) -> Option<String> {
    let mut in_entry = false;
    let mut icon = None;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_entry = trimmed == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("X-Liquid-Glass-Original-Icon=") {
            return (!value.is_empty()).then(|| value.to_owned());
        }
        if let Some(value) = trimmed.strip_prefix("Icon=") {
            icon = (!value.is_empty()).then(|| value.to_owned());
        }
    }
    icon
}

fn usable_icon(path: &Path, expected_size: u32) -> bool {
    let Ok(image) = image::open(path).map(image::DynamicImage::into_rgba8) else {
        return false;
    };
    if image.dimensions() != (expected_size, expected_size) {
        return false;
    }
    // A valid canonical icon has an opaque material/background. This catches
    // the transparent, zero-byte-like images without treating a flat colour
    // icon as broken.
    let visible = image.pixels().filter(|pixel| pixel[3] > 16).count();
    visible * 4 >= (expected_size as usize * expected_size as usize)
}

fn read_state(path: &Path) -> Result<ManagedIcons, IconError> {
    if !path.is_file() {
        return Ok(ManagedIcons::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_state(path: &Path, state: &ManagedIcons) -> Result<(), IconError> {
    let bytes = serde_json::to_vec_pretty(state)?;
    write_bytes_atomically(path, &bytes)
}

fn write_png_atomically(path: &Path, image: &RgbaImage) -> Result<(), IconError> {
    let parent = path
        .parent()
        .ok_or_else(|| IconError::Manifest("icon path has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    image
        .save_with_format(temporary.path(), image::ImageFormat::Png)
        .map_err(|error| IconError::InvalidImage(error.to_string()))?;
    temporary
        .persist(path)
        .map_err(|error| IconError::Io(error.error))?;
    Ok(())
}

fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<(), IconError> {
    let parent = path
        .parent()
        .ok_or_else(|| IconError::Manifest("path has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    fs::write(temporary.path(), bytes)?;
    temporary
        .persist(path)
        .map_err(|error| IconError::Io(error.error))?;
    Ok(())
}

/// Reinsert an existing user override so GAppInfoMonitor sees a real
/// precedence change instead of a same-inode content update. The system copy
/// remains available during the short staging interval.
fn replace_desktop_entry_for_refresh(path: &Path, bytes: &[u8]) -> Result<(), IconError> {
    if !path.is_file() {
        return write_bytes_atomically(path, bytes);
    }
    let parent = path
        .parent()
        .ok_or_else(|| IconError::Manifest("desktop entry has no parent".to_owned()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| IconError::Manifest("desktop entry has no filename".to_owned()))?;
    let staging = parent.join(format!(".{name}.liquid-glass-refresh"));
    if staging.exists() {
        fs::remove_file(&staging)?;
    }
    fs::rename(path, &staging)?;
    // GAppInfoMonitor coalesces ordinary writes. A short absence lets GNOME
    // observe the system desktop entry, then the fingerprinted override.
    std::thread::sleep(Duration::from_millis(120));
    if let Err(error) = write_bytes_atomically(path, bytes) {
        let _ = fs::rename(&staging, path);
        return Err(error);
    }
    fs::remove_file(staging)?;
    Ok(())
}

fn hicolor_index() -> String {
    let directories = ICON_SIZES
        .iter()
        .map(|size| format!("{size}x{size}/apps"))
        .collect::<Vec<_>>();
    let mut index = String::from(
        "[Icon Theme]\nName=Hicolor\nComment=Fallback icon theme\nHidden=true\nDirectories=",
    );
    index.push_str(&directories.join(","));
    index.push('\n');
    for size in ICON_SIZES {
        index.push_str(&format!(
            "\n[{size}x{size}/apps]\nSize={size}\nContext=Applications\nType=Threshold\n"
        ));
    }
    index
}

fn refresh_desktop_caches(data_home: &Path) -> Result<(), IconError> {
    let icon_root = data_home.join("icons/hicolor");
    let applications = data_home.join("applications");
    if icon_root.is_dir() {
        let status = Command::new("gtk4-update-icon-cache")
            .args(["-q", "-f", "-t"])
            .arg(icon_root)
            .status()
            .map_err(IconError::Io)?;
        if !status.success() {
            return Err(IconError::Manifest(
                "gtk4-update-icon-cache failed".to_owned(),
            ));
        }
    }
    let status = Command::new("update-desktop-database")
        .arg("-q")
        .arg(applications)
        .status()
        .map_err(IconError::Io)?;
    if !status.success() {
        return Err(IconError::Manifest(
            "update-desktop-database failed".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{AppCategory, DesktopApplication};
    use tempfile::tempdir;

    fn app(root: &Path) -> DesktopApplication {
        let desktop = root.join("demo.desktop");
        fs::write(
            &desktop,
            "[Desktop Entry]\nType=Application\nName=Demo\nIcon=demo\nCategories=Development;\n",
        )
        .unwrap();
        DesktopApplication {
            id: "demo.desktop".to_owned(),
            name: "Demo".to_owned(),
            desktop_file: desktop,
            icon_name: "demo".to_owned(),
            icon_path: None,
            categories: vec!["Development".to_owned()],
            category: AppCategory::Development,
        }
    }

    #[test]
    fn desktop_override_is_reversible_and_preserves_other_keys() {
        let root = tempdir().unwrap();
        let installer = IconInstaller::with_data_home_for_test(root.path().join("data"));
        let application = app(root.path());
        let prepared = installer.prepare_desktop_override(&application).unwrap();
        installer
            .install_desktop_override(&application, "liquid-glass-demo", Vec::new(), prepared)
            .unwrap();
        let override_path = root.path().join("data/applications/demo.desktop");
        let rewritten = fs::read_to_string(&override_path).unwrap();
        assert!(rewritten.contains("Icon=liquid-glass-demo"));
        assert!(rewritten.contains("X-Liquid-Glass-Original-Icon=demo"));
        assert!(rewritten.contains("Categories=Development;"));
        assert!(installer.is_managed("demo.desktop"));
        assert_eq!(installer.managed_ids().unwrap(), ["demo.desktop"]);
        installer.restore("demo.desktop").unwrap();
        assert!(!override_path.exists());
        assert!(!installer.is_managed("demo.desktop"));
    }
}
