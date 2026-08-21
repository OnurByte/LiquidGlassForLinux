# AI pipeline

## Discovery and cache

The GUI and CLI enumerate visible `Type=Application` desktop entries and
resolve their icons without modifying the desktop database. SVG sources are
preferred over raster theme entries. A source is normalized locally to a
1024×1024 PNG for provider compatibility while the manifest hashes the original
bytes.

A valid v2 or v3 manifest is the conversion marker. New conversions write v3;
v2 remains readable so old caches do not trigger AI requests:

- same source hash: `converted`, never queued;
- changed source hash: `stale`, never queued;
- no valid v2 SVG/manifest: queued for first conversion;
- manual `Reconvert`: replace the existing conversion only after the new one
  passes validation.

## The only AI stage

One conversion attempt makes one provider request. Codex exec and the OpenAI
Responses API both receive the same normalized image, prompt and strict JSON
Schema and return:

```json
{ "svg": "<svg>...</svg>" }
```

The prompt asks only for identity-preserving flat vector decomposition:
`background`, then `foreground-1` through optional `foreground-4`. It contains
no accent, appearance, dark, clear or tinted value and requests no glass effect.
There is no automatic repair/retry request after invalid output.

The Codex child runs ephemerally with user config ignored, so unrelated hooks
or MCP servers cannot block icon conversion. It has a 120-second process-group
timeout; cancellation and timeout terminate the wrapper and its descendants.

## Validation and persistence

The SVG must use a 1024×1024 viewBox, an opaque full-canvas background and one
to four ordered foreground groups. Scripts, foreign objects, text/fonts,
embedded raster images, masks, filters and external references are rejected.
Each group is rasterized into the full canvas, so a path that barely spills
outside the canvas is clipped automatically instead of making the icon fail or
leak past the frame. A group that is entirely outside the canvas still fails
validation. Only after parsing and raster validation succeed are `icon.svg` and
its v3 manifest atomically installed. A failed manual conversion leaves the
previous valid directory untouched.

## Local material stage

`resvg` rasterizes each named SVG group into a separate texture layer. A WGPU
shader composes the layers and calculates the icon mask, depth/parallax,
refraction, specular response, shadow, Default/Dark transformation, Mono-derived
Clear treatment and user-selected Tinted treatment. This stage performs no AI
or network work.
