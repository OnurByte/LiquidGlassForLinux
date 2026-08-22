#!/usr/bin/env bash
set -euo pipefail

repo_url='https://github.com/OnurByte/LiquidGlassForLinux.git'
data_home="${XDG_DATA_HOME:-${HOME:?HOME is required when XDG_DATA_HOME is unset}/.local/share}"
install_dir="$data_home/liquid-glass-for-linux"

command -v git >/dev/null 2>&1 || {
    printf '%s\n' 'git is required; install git and run this command again.' >&2
    exit 1
}
command -v cargo >/dev/null 2>&1 || {
    printf '%s\n' 'Rust cargo is required; install Rust and run this command again.' >&2
    exit 1
}

if [[ -e "$install_dir/.git" ]]; then
    [[ -z "$(git -C "$install_dir" status --porcelain)" ]] || {
        printf 'Refusing to update dirty checkout: %s\n' "$install_dir" >&2
        exit 1
    }
    git -C "$install_dir" fetch --quiet origin main
    git -C "$install_dir" merge --ff-only --quiet origin/main
elif [[ -e "$install_dir" ]]; then
    printf 'Refusing to replace non-repository directory: %s\n' "$install_dir" >&2
    exit 1
else
    git clone --depth 1 --branch main "$repo_url" "$install_dir"
fi

exec "$install_dir/scripts/install-desktop-app.sh"
