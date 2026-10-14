# Embedding

## Rust conversion

```rust
use liquid_glass_icon::{CodexExecProvider, SvgProvider, TransformRequest};
use std::sync::{Arc, atomic::AtomicBool};

let provider = SvgProvider::Codex(
    CodexExecProvider::default().with_model("gpt-5.6-luna"),
);
let result = liquid_glass_icon::transform_icon(
    TransformRequest { input, output_dir },
    &provider,
    Arc::new(AtomicBool::new(false)),
).await?;
```

`TransformResult` identifies the canonical SVG, v4 manifest, source hash and
ordered layer IDs. The v4 manifest additionally stores Icon Composer-style
material groups, their child layers and Dark/Mono annotations. Appearance and
accent are intentionally absent from the conversion request and manifest.

## Local rendering

Use `svg::rasterize_document` when a host needs the background, material groups
and independent 1024×1024 RGBA child layers. `svg::rasterize_layers` remains
for flat legacy callers. The bundled GUI passes the document to the WGPU
renderer. `renderer::RenderSettings` contains runtime appearance, global tint,
optional global background override, foreground opacity and enabled-by-default
automatic contrast; changing them does not invoke a provider. Background-
coloured legacy negative space is treated as a cutout, so it follows a
background override. Flat backgrounds use the exact override; gradients retain
their relative shading. Manual surface colours take precedence over automatic
contrast. `pointer` and `tilt` are preview-only: `RenderTarget::Icon` forces
them to rest so installed assets are deterministic. Installation renders each
Hicolor size directly so its final corner alpha matches the preview at that
size.

## Optional shell motion

`extensions/liquid-glass-parallax@onurbyte` is a GNOME Shell 50 extension
with UUID `liquid-glass-parallax@onurbyte.github.io`,
separate from icon rendering. It reads the user-managed state file, replaces
matching icon content with a fixed base plus transparent foreground layers,
slightly enlarges managed icons, and translates only those foreground layers.
It does not alter exported PNGs or target every shell actor. The source installer is
`scripts/install-gnome-parallax.sh`. KDE Plasma remains static but refreshes
its native application cache when `kbuildsycoca6` is available; Hyprland uses
the normal XDG application/icon paths and needs no shell adapter.

## Other languages

Invoke the CLI and consume `icon.svg` plus `icon-manifest.json`. There is no
localhost service. This avoids exposing an unauthenticated endpoint that can
start Codex or spend API credit.
