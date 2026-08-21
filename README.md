# liquid-glass-icon

Rust application that discovers installed desktop application icons, converts
each icon once into one canonical layered SVG, and renders Apple-like Liquid
Glass appearances locally with WGPU.

```text
XDG application icon
  -> one AI conversion request
  -> icon.svg (background + 1-4 foreground groups)
  -> local WGPU Default / Dark / Clear / Tinted renderer
```

Accent, appearance, refraction, highlights, shadows and motion never enter the
AI prompt. They are local renderer inputs. Changing tint therefore creates no
Codex or OpenAI API request.

## Run the GUI

```bash
cargo run --release --bin liquid-glass-icon-gui
```

The default provider is `Codex exec`, which reuses the existing Codex CLI
login and does not require API credit. The default model is `gpt-5.6-luna`; the
GUI model selector passes the selected model to either provider. Select `API
key` in the application to use the Responses API instead. A pasted key is
masked, retained only in memory, and never written to settings, manifests, logs
or command arguments.

Theme is global: `System` is the default and follows the OS light/dark theme;
the accent picker selects Apple-style `Tinted Light`/`Tinted Dark` and is shared
by every preview. It never enters the AI request. Foreground layers are
automatically contained
in a centered safe-area, so narrow, triangular or wide source artwork is not
cropped by the rounded-square Liquid Glass frame.

The GUI never starts AI work on launch. Only visible, daily-use categories are
enabled by default; Avahi/SSH server browsers, settings, system helpers and
terminal entries stay blocked until enabled in **Categories**. Converted icons
are never regenerated automatically. A changed source icon is marked `stale`
and requires the row's **Reconvert** button. **Stop** cancels the
current HTTP request or terminates the current Codex child process.
Codex conversions run ephemerally without the user's hooks/MCP config
and fail after 120 seconds instead of waiting forever.

## Install as a desktop app

```bash
./scripts/install-desktop-app.sh
```

Then open **Liquid Glass Icons** from the application menu. The launcher uses
the release binary, stores GUI output under `$XDG_DATA_HOME/liquid-glass-icon/out`
(or `~/.local/share/liquid-glass-icon/out`), and finds Codex in the current PATH
or common user install locations before running `codex exec`.

## CLI

Codex login:

```bash
cargo run --release -- --provider codex --output out
```

Select another model explicitly with `--model`, for example:

```bash
cargo run --release -- --provider codex --model gpt-5.6-terra --output out
```

OpenAI API:

```bash
OPENAI_API_KEY='...' cargo run --release -- --provider api --output out
```

Manual regeneration is explicit:

```bash
cargo run --release -- --reconvert org.gnome.Calculator.desktop
```

The CLI applies the same daily-use category filter; pass `--all-categories`
when server browsers, settings, system helpers or terminal entries are
intentionally in scope.

Output:

```text
out/apps/org.gnome.Calculator/
├── icon.svg
└── icon-manifest.json
```

Successful conversions can be applied from the GUI. Applying writes only
user-scoped PNGs under `$XDG_DATA_HOME/icons/hicolor` and a user `.desktop`
override, records a hash/backup under `$XDG_DATA_HOME/liquid-glass-icon`, and
can be restored without touching `/usr/share/applications`.

## Apple model

Apple's public guidance uses one layered icon structure and applies dynamic
material effects at runtime. Icon Composer ultimately writes Apple's `.icon`
container; this Linux project mirrors that behavior with named groups in one
SVG and does not claim binary compatibility with Apple's private renderer.

- [Apple App Icons HIG](https://developer.apple.com/design/human-interface-guidelines/app-icons/)
- [Creating your app icon using Icon Composer](https://developer.apple.com/documentation/xcode/creating-your-app-icon-using-icon-composer)
- [WWDC25: Create icons with Icon Composer](https://developer.apple.com/videos/play/wwdc2025/361/)

The Responses implementation follows the official [Create a model response](https://developers.openai.com/api/reference/cli/resources/responses/methods/create)
contract. Codex execution follows [non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode).

## Verify

```bash
cargo fmt --check
cargo test --all-targets
cargo check --all-targets
```

Offline tests mock the Responses API. A live provider smoke test is separate
because it requires a Codex login or billable API credentials.

# LiquidGlassForLinux
