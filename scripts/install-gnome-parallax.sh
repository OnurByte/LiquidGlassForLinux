#!/bin/sh
set -eu

if [ "${1:-}" = '--install-system' ]; then
    project_dir="${2:?project directory is required}"
    extension_uuid='liquid-glass-parallax@onurbyte.github.io'
    extension_dir="/usr/share/gnome-shell/extensions/$extension_uuid"
    install -d "$extension_dir"
    install -m 0644 "$project_dir/extensions/liquid-glass-parallax@onurbyte/metadata.json" "$extension_dir/metadata.json"
    install -m 0644 "$project_dir/extensions/liquid-glass-parallax@onurbyte/extension.js" "$extension_dir/extension.js"
    exit 0
fi

system_scope=false
if [ "${1:-}" = '--system' ]; then
    system_scope=true
elif [ "$#" -ne 0 ]; then
    printf '%s\n' 'Usage: install-gnome-parallax.sh [--system]' >&2
    exit 2
fi

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
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
script_path="$script_dir/$(basename -- "$0")"
project_dir="$(CDPATH= cd -- "$script_dir/.." && pwd)"
data_dir="${XDG_DATA_HOME:-"$HOME/.local/share"}"
extension_dir="$data_dir/gnome-shell/extensions/$extension_uuid"

if [ "$system_scope" = true ]; then
    command -v pkexec >/dev/null 2>&1 || {
        printf '%s\n' 'pkexec is required for system-wide GNOME extension installation.' >&2
        exit 1
    }
    # The GUI invokes this mode so Polkit presents the desktop password prompt.
    pkexec "$script_path" --install-system "$project_dir"
    extension_dir="/usr/share/gnome-shell/extensions/$extension_uuid"
else
    mkdir -p "$extension_dir"
    install -m 0644 "$project_dir/extensions/liquid-glass-parallax@onurbyte/metadata.json" "$extension_dir/metadata.json"
    install -m 0644 "$project_dir/extensions/liquid-glass-parallax@onurbyte/extension.js" "$extension_dir/extension.js"
fi

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
printf '%s\n' "Installed $extension_uuid in $extension_dir. GNOME Shell will load it on your next login."
