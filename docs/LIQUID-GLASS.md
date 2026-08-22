# Liquid Glass icon contract

The identity-bearing asset is one flat, layered SVG:

```text
background -> foreground-1 -> [foreground-2] -> [foreground-3] -> [foreground-4]
```

Rules:

- 1024×1024 SVG canvas and an opaque full-canvas background;
- one opaque canvas background plus one to four independent foreground groups;
- preserve the original brand, silhouette, proportions and meaningful detail;
- vector paths/shapes only; text must be represented as paths;
- no raster image, external reference, script, filter, blur, glow, bevel,
  refraction or permanent shadow in source artwork;
- no appearance variant or accent value in the canonical asset.

The local material renderer derives:

```text
Default
Dark
Mono -> Clear Light / Clear Dark
Mono + user tint -> Tinted Light / Tinted Dark
```

Tint is a global runtime uniform. It is not an application-specific AI
instruction and does not cause regeneration. Before WGPU compositing, the
combined foreground alpha bounds are contained in a centered 820 px safe-area
inside the 1024 px canvas. This keeps triangular, portrait and wide artwork
recognizable inside the rounded-square mask without changing the canonical SVG.
Each foreground is composited at its normalized z position with a small static
depth gap, z-weighted shadow/specular response and optional pointer parallax;
the icon therefore retains layer separation even when motion is zero.

Apple's public Icon Composer model similarly separates flat layered artwork
from dynamic system material. Apple's final `.icon` format and exact renderer
remain platform-specific; this project's SVG/WGPU contract is an independent
implementation of the observable model.
