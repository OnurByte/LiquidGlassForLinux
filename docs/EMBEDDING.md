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

`TransformResult` identifies the canonical SVG, v3 manifest, source hash and
ordered layer IDs. Appearance and accent are intentionally absent from the
conversion request and manifest.

## Local rendering

Use `svg::rasterize_layers` when a host needs the SVG groups as independent
1024×1024 RGBA buffers. The bundled GUI passes those buffers to the WGPU
renderer. `renderer::RenderSettings` contains runtime appearance, global tint
and preview background; changing it does not invoke a provider.

## Other languages

Invoke the CLI and consume `icon.svg` plus `icon-manifest.json`. There is no
localhost service. This avoids exposing an unauthenticated endpoint that can
start Codex or spend API credit.
