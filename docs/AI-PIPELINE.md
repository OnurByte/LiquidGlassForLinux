# AI pipeline

## Discovery and cache

The GUI and CLI enumerate visible `Type=Application` desktop entries and
resolve their icons without modifying the desktop database. SVG sources are
preferred over raster theme entries. A source is normalized locally to a
1024×1024 PNG for provider compatibility while the manifest hashes the original
bytes.

A valid v2, v3 or v4 manifest is the conversion marker. New conversions write
v4; v2/v3 remain readable so old caches do not trigger AI requests:

- same v4 source hash: `converted`, never queued;
- same v2/v3 source hash: `legacy`, never queued; use explicit `Reconvert` to
  upgrade the SVG document without silently spending provider credit;
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
`background`, then one to four `group-G` groups with one to four direct
`layer-G-L` children each. It contains no accent, appearance, Dark, Clear or
Tinted value and requests no glass effect. Negative space is a transparent
foreground cutout, never a separately painted copy of the background colour.
Older cached SVGs that used the latter convention are corrected locally during
rendering. There is no automatic repair/retry request after invalid output.

The Codex child runs ephemerally with user config ignored, so unrelated hooks
or MCP servers cannot block icon conversion. It has a 120-second process-group
timeout; cancellation and timeout terminate the wrapper and its descendants.

## Validation and persistence

The SVG must use a 1024×1024 viewBox, an opaque full-canvas background and one
to four ordered material groups. Every material group has one to four named
child layers and can select Individual/Combined material plus documented local
settings. Scripts, foreign objects, text/fonts, embedded raster images, masks,
filters and external references are rejected. Each layer is rasterized into the
full canvas, so a path that barely spills outside the canvas is clipped
automatically instead of making the icon fail or leak past the frame. A layer
that is entirely outside the canvas still fails validation. Only after parsing
and raster validation succeed are `icon.svg` and its v4 manifest atomically
installed. A failed manual conversion leaves the previous valid directory
untouched.

## Local material stage

`resvg` rasterizes each named SVG child layer into a separate texture layer. A
WGPU shader either materializes those surfaces individually or composites a
Combined group once, then calculates the final icon mask, depth/parallax,
refraction, specular response, shadow, Default/Dark transformation,
Mono-derived Clear treatment and user-selected Tinted treatment. This stage
performs no AI or network work.
