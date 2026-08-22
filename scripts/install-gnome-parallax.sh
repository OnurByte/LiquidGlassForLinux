#!/bin/sh
set -eu

case "${XDG_CURRENT_DESKTOP:-}" in
    *GNOME*) ;;
    *KDE*|*Hyprland*)
        printf '%s\n' 'Liquid Glass parallax is currently GNOME Shell-only; KDE and Hyprland keep static installed icons.'
        exit 0
        ;;
    *)
        printf '%s\n' 'Liquid Glass parallax requires a GNOME Shell session.' >&2
        exit 1
        ;;
esac

extension_uuid='liquid-glass-parallax@onurbyte.github.io'
project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
data_dir="${XDG_DATA_HOME:-"$HOME/.local/share"}"
extension_dir="$data_dir/gnome-shell/extensions/$extension_uuid"

mkdir -p "$extension_dir"
install -m 0644 "$project_dir/extensions/liquid-glass-parallax@onurbyte/metadata.json" "$extension_dir/metadata.json"
install -m 0644 "$project_dir/extensions/liquid-glass-parallax@onurbyte/extension.js" "$extension_dir/extension.js"

if gnome-extensions enable "$extension_uuid" 2>/dev/null; then
    gnome-extensions info "$extension_uuid"
    exit 0
fi

enabled_extensions="$(gsettings get org.gnome.shell enabled-extensions)"
case "$enabled_extensions" in
    *"'$extension_uuid'"*) ;;
    '[]') gsettings set org.gnome.shell enabled-extensions "['$extension_uuid']" ;;
    *) gsettings set org.gnome.shell enabled-extensions "${enabled_extensions%]}, '$extension_uuid']" ;;
esac
printf '%s\n' "Installed $extension_uuid. GNOME Shell will load it on your next login."
