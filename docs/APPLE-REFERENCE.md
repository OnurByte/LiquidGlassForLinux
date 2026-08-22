# Apple reference model

Apple’s public app-icon guidance describes a single layered icon structure
whose system effects provide dimensionality, translucency, refraction and
specular response. Icon Composer imports flat SVG/PNG artwork into up to four
groups and stores one `.icon` asset with Default, Dark and Mono annotations;
Clear and Tinted presentations derive from that structure.

The important behavior for this project is:

- preserve the app’s visual identity across appearances;
- keep one canvas background and up to four depth-bearing foreground groups separable;
- let the material system handle dynamic effects where possible;
- avoid hard-coded static blur, shadow and glow in source artwork;
- keep icons recognizable at smaller sizes.

The public contract does **not** disclose Apple’s actual runtime shader,
numeric material coefficients, private `.icon` representation or exact corner
geometry. This repository therefore uses the public parts as hard rules
(background, groups, flat source artwork, annotations and system-applied
effects), and calls its WGPU output a compatible Linux implementation rather
than a pixel-identical clone. Any claim beyond that needs direct Apple source
or a reproducible comparison, neither of which is public documentation.

References:

- [Apple App Icons](https://developer.apple.com/design/human-interface-guidelines/app-icons/)
- [Apple Materials](https://developer.apple.com/design/human-interface-guidelines/materials)
- [Creating an app icon using Icon Composer](https://developer.apple.com/documentation/xcode/creating-your-app-icon-using-icon-composer)
- [Create icons with Icon Composer](https://developer.apple.com/videos/play/wwdc2025/361/)
