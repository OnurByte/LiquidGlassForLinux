#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"

cargo build --release --manifest-path "$repo_dir/Cargo.toml" --bins

data_home="${XDG_DATA_HOME:-}"
if [[ -z "$data_home" ]]; then
    data_home="${HOME:?HOME is required when XDG_DATA_HOME is unset}/.local/share"
fi

applications_dir="$data_home/applications"
icons_dir="$data_home/icons/hicolor/scalable/apps"
binary="$repo_dir/target/release/liquid-glass-icon-gui"
desktop_file="$applications_dir/io.github.yargc.LiquidGlassIcons.desktop"

install -d "$applications_dir" "$icons_dir"
sed "s|@EXEC@|$binary|g" \
    "$repo_dir/packaging/liquid-glass-icon.desktop.in" > "$desktop_file"
install -m 0644 "$repo_dir/assets/liquid-glass-icon.svg" \
    "$icons_dir/io.github.yargc.LiquidGlassIcons.svg"

# Migrate only the launcher created by the previous installer; leave unrelated
# user desktop entries untouched.
old_desktop_file="$applications_dir/liquid-glass-icon.desktop"
if [[ -f "$old_desktop_file" ]] && grep -Fq "Liquid Glass Icons" "$old_desktop_file" && grep -Fq "liquid-glass-icon-gui" "$old_desktop_file"; then
    rm -f -- "$old_desktop_file"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications_dir"
fi

printf 'Installed: %s\n' "$desktop_file"
printf 'Open your application menu and launch: Liquid Glass Icons\n'
