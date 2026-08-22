# Liquid Glass icon contract

The identity-bearing source is a 1024×1024 SVG document with one opaque canvas
background and one to four material groups:

```text
background -> group-1 -> [group-2] -> [group-3] -> [group-4]
                   |             |
             layer-1-1...  layer-2-1...
```

Every group contains one to four direct child layers. The source IDs are
`background`, `group-G` and `layer-G-L`; `G` and `L` are 1–4. Existing flat
`foreground-1`…`foreground-4` SVGs remain readable and become one Individual
group each, so cached icons do not require another provider call.

Rules:

- preserve the source 1024-grid position, orientation, proportions and brand
  silhouette; clipping at the canvas edge is allowed, re-centering, mirroring,
  safe-zone fitting and auto-enclosure extraction are not;
- keep source vector-only: no image, external reference, script, text,
  filter, mask, blur, glow, bevel, refraction or permanent shadow;
- leave final canvas masking and dynamic material to the renderer; source
  foreground groups must not contain a pre-made rounded-square canvas mask;
- annotate a group with `data-liquid-mode="individual|combined"`, optional
  `data-liquid-specular`, `data-liquid-effects`, blur/refraction/translucency/
  shadow values, and optional Dark/Mono opacity/effect overrides;
- never put accent, theme, Clear or Tinted values in the SVG or AI prompt.

`Individual` gives each child layer its own material surface. `Combined`
composites a group’s child layers once, then gives that result one surface. The
runtime supports effects on/off, `off|automatic|inside|outside` specular,
normalized blur/refraction/translucency/shadow, plus default Dark and Mono
annotations. The GUI exposes the resulting material surfaces in **Inspect**.

Accent and appearance are local WGPU inputs; updating either only regenerates
the Linux PNG outputs. The renderer applies a single centered final mask to
the composite. It never modifies source artwork beforehand, so the same
asymmetry and optical scale reach the installed icon.

This is an implementation of Apple’s *public* Icon Composer/HIG model.
Apple’s private `.icon` asset internals, shader coefficients and exact corner
curve are not public, so this project cannot honestly claim byte- or
pixel-identical macOS output.
