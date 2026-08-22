# Shared canonical icons

Each directory contains one app's portable v4 `icon.svg` and matching
`icon-manifest.json`. Legacy v2/v3 cache entries stay local until **Reconvert**
upgrades them; they are deliberately not published here. These are flat canonical source artwork: accent,
background, appearance, glass material, and pointer parallax are runtime
settings, not baked into these files.

The GUI and `liquid-glass-icon convert` archive new conversions here when run
from this repository. To target another checked-out collection, set
`LIQUID_GLASS_ASSET_DIR=/path/to/icons`; existing local conversions can be
copied with:

```bash
liquid-glass-icon --json archive
```
