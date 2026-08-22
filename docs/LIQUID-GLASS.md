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
instruction and does not cause regeneration. Before WGPU compositing the
combined foreground alpha bounds follow Apple's safe-zone policy: artwork
already inside the keep band (~791 px) keeps its source 1024-grid
coordinates and is only translated so its combined center sits at (512,
512); smaller artwork grows toward the ~84% (860 px) safe-zone target with
one shared transform; overflowing artwork shrinks with that same transform.
Layers are never scaled against their own bounding boxes, so relative layer
geometry — a Discord face, BridgeSpace panels — stays intact without
changing the canonical SVG. A full-bleed circle or rounded square in the
first foreground slot is treated as an enclosure: its color field (gradients
included) moves into the background layer instead of being flattened.
Each foreground is composited at its normalized z position with a small static
depth gap, z-weighted shadow/specular response and optional pointer parallax;
the icon therefore retains layer separation even when motion is zero. The
rounded-square mask has one canonical definition shared verbatim by the CPU
path and the WGPU shader, centered exactly on (0.5, 0.5), and it is applied
exactly once to the final image — never repeatedly to artwork layers.

Apple's public Icon Composer model similarly separates flat layered artwork
from dynamic system material. Apple's final `.icon` format and exact renderer
remain platform-specific; this project's SVG/WGPU contract is an independent
implementation of the observable model.
