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
};

const ICON_SIZES: [u32; 3] = [128, 256, 512];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedIcon {
    desktop_id: String,
    override_path: PathBuf,
    icon_files: Vec<PathBuf>,
    managed_sha256: String,
    backup_path: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ManagedIcons {
    entries: HashMap<String, ManagedIcon>,
}

#[derive(Debug, Clone)]
pub struct IconInstaller {
    data_home: PathBuf,
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
        renderer.load_svg(svg)?;
        let icon_name = format!("liquid-glass-{}", application_output_name(&application.id));
        let mut icon_files = Vec::new();
        for size in ICON_SIZES {
            let image = renderer.render(size, size, settings, RenderTarget::Icon)?;
            let path = self.icon_path(&icon_name, size);
            write_png_atomically(&path, &image)?;
            icon_files.push(path);
        }
        self.install_desktop_override(application, &icon_name, icon_files)
    }

    fn install_desktop_override(
        &self,
        application: &DesktopApplication,
        icon_name: &str,
        icon_files: Vec<PathBuf>,
    ) -> Result<(), IconError> {
        let applications_dir = self.data_home.join("applications");
        let override_path = applications_dir.join(&application.id);
        let state_path = self.state_path();
        let mut state = read_state(&state_path)?;
        let previous = state.entries.get(&application.id).cloned();

        let (source, backup_path) = if override_path.is_file() {
            let contents = fs::read_to_string(&override_path)?;
            let backup_path = if previous.is_none() {
                let path = self
                    .data_home
                    .join("liquid-glass-icon/backups")
                    .join(&application.id);
                write_bytes_atomically(&path, contents.as_bytes())?;
                Some(path)
            } else {
                previous.and_then(|entry| entry.backup_path)
            };
            (contents, backup_path)
        } else {
            (fs::read_to_string(&application.desktop_file)?, None)
        };
        let rewritten = replace_desktop_icon(&source, icon_name, &application.icon_name)?;
        write_bytes_atomically(&override_path, rewritten.as_bytes())?;
        let managed_sha256 = crate::manifest::sha256(rewritten.as_bytes());
        state.entries.insert(
            application.id.clone(),
            ManagedIcon {
                desktop_id: application.id.clone(),
                override_path,
                icon_files,
                managed_sha256,
                backup_path,
            },
        );
        write_state(&state_path, &state)?;
        refresh_desktop_caches(&self.data_home);
        Ok(())
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
                write_bytes_atomically(&entry.override_path, &fs::read(backup)?)?;
            } else {
                fs::remove_file(&entry.override_path)?;
            }
        }
        for path in entry.icon_files {
            let _ = fs::remove_file(path);
        }
        write_state(&state_path, &state)?;
        refresh_desktop_caches(&self.data_home);
        Ok(())
    }

    pub fn is_managed(&self, desktop_id: &str) -> bool {
        read_state(&self.state_path())
            .map(|state| state.entries.contains_key(desktop_id))
            .unwrap_or(false)
    }

    fn icon_path(&self, icon_name: &str, size: u32) -> PathBuf {
        self.data_home
            .join("icons/hicolor")
            .join(format!("{size}x{size}/apps"))
            .join(format!("{icon_name}.png"))
    }

    fn state_path(&self) -> PathBuf {
        self.data_home.join("liquid-glass-icon/managed-icons.json")
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

fn refresh_desktop_caches(data_home: &Path) {
    let icon_root = data_home.join("icons/hicolor");
    let applications = data_home.join("applications");
    if icon_root.join("index.theme").is_file() {
        let _ = Command::new("gtk4-update-icon-cache")
            .args(["-f", "-t"])
            .arg(icon_root)
            .status();
    }
    let _ = Command::new("update-desktop-database")
        .arg(applications)
        .status();
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
        installer
            .install_desktop_override(&application, "liquid-glass-demo", Vec::new())
            .unwrap();
        let override_path = root.path().join("data/applications/demo.desktop");
        let rewritten = fs::read_to_string(&override_path).unwrap();
        assert!(rewritten.contains("Icon=liquid-glass-demo"));
        assert!(rewritten.contains("X-Liquid-Glass-Original-Icon=demo"));
        assert!(rewritten.contains("Categories=Development;"));
        assert!(installer.is_managed("demo.desktop"));
        installer.restore("demo.desktop").unwrap();
        assert!(!override_path.exists());
        assert!(!installer.is_managed("demo.desktop"));
    }
}
