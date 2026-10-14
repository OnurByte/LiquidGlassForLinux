# CLI contract

The CLI is the supported integration boundary. It writes a JSON envelope with
`schema_version: "liquid-glass.control.v1"`, an `operation` and operation data.
Pass `--json` for compact output. Without it the same JSON is pretty-printed.

```text
liquid-glass-icon discover [--json]
liquid-glass-icon status [--desktop-id ID]... [--output PATH] [--json]
liquid-glass-icon convert --desktop-id ID... [--provider codex|api]
                         [--model MODEL] [--output PATH] [--json]
liquid-glass-icon apply (--desktop-id ID... | --managed)
                       [--appearance APPEARANCE] [--accent RRGGBB] [--background RRGGBB]
                       [--foreground-opacity PERCENT] [--no-auto-contrast]
                       [--output PATH] [--json]
liquid-glass-icon repair (--desktop-id ID... | --managed)
                        [--appearance APPEARANCE] [--accent RRGGBB] [--background RRGGBB]
                        [--foreground-opacity PERCENT] [--no-auto-contrast]
                        [--output PATH] [--json]
liquid-glass-icon archive [--desktop-id ID]... [--output PATH]
                          [--asset-dir PATH] [--json]
liquid-glass-icon restore --desktop-id ID... [--json]
```

Desktop IDs must exactly match `discover` output. Unknown IDs fail instead of
being normalized or matched by prefix.

`convert` is the only command that may contact an AI provider. `codex` uses the
current Codex CLI login. `api` reads `OPENAI_API_KEY`; keys are never accepted
as arguments. Every requested conversion is explicit and forced. At most ten
conversions run concurrently. Provider, authentication or billing failures
stop the remaining batch; ordinary per-icon validation failures are reported
for that icon while the other queued icons continue.

`apply` only renders an existing current SVG cache and installs standard
16–1024 px user-scoped Hicolor PNGs plus a user `.desktop` override. It renders
every installed Hicolor size directly so the dock and preview share the same
edge sampling. It also writes separate 1024 px base/layer assets for the
optional GNOME Shell compositor. It does not create a partial user Hicolor theme that could shadow the system
theme; an old Liquid Glass-generated index/cache is moved to a recoverable
backup. It refreshes desktop caches and
reinserts an existing override so XDG application monitors observe the new
fingerprinted icon without logout. In KDE/Plasma sessions it also runs
`kbuildsycoca6 --noincremental` when that native cache tool is installed;
Hyprland uses the same standard XDG paths and needs no shell adapter.
Appearance, accent and automatic contrast changes do not call an AI provider.
`restore` refuses to overwrite a managed desktop file that the user changed
after installation.

`--background RRGGBB` replaces the source background colour at render time for
the selected batch. A flat source background becomes the exact requested
colour; a source gradient keeps its relative light/dark contrast. It does not
modify the canonical SVG or send an AI request.

Automatic contrast is enabled by default. When a material surface would blend
into its local background, the renderer chooses the best of black, white and
the global accent for that surface only. Manual layer colours always win.
`--no-auto-contrast` disables this local fallback for `apply` or `repair`.

`--foreground-opacity PERCENT` accepts 20 through 150 and changes only the
material/foreground surfaces. The opaque background and canonical SVG are left
unchanged. Negative-space cutouts expose this runtime background, even when an
older SVG painted them with its original background colour.

`repair` uses only managed state and the cached canonical SVG. It recreates a
missing user launcher, replaces wrong-size or fully transparent generated PNGs,
and upgrades stale renderer output without a provider request. A launcher whose
managed content hash no longer matches is reported as `user-modified` and is
left untouched. The GUI performs this same check once at startup.

`archive` copies only current v4 canonical `icon.svg` and `icon-manifest.json`
files to the shareable asset root. Legacy cache remains local until explicitly
reconverted. A checkout defaults to `assets/icons`; packaged installations
require `--asset-dir` or `LIQUID_GLASS_ASSET_DIR`.

The default cache root is
`$XDG_DATA_HOME/liquid-glass-icon/out`, normally
`~/.local/share/liquid-glass-icon/out`. Each successful conversion writes:

```text
apps/<sanitized-desktop-id>/
├── icon.svg
└── icon-manifest.json
```

The conversion manifest schema is
[`schema/icon-manifest.schema.json`](../schema/icon-manifest.schema.json).
