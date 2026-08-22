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
                       [--output PATH] [--json]
liquid-glass-icon repair (--desktop-id ID... | --managed)
                        [--appearance APPEARANCE] [--accent RRGGBB] [--background RRGGBB]
                        [--output PATH] [--json]
liquid-glass-icon archive [--desktop-id ID]... [--output PATH]
                          [--asset-dir PATH] [--json]
liquid-glass-icon restore --desktop-id ID... [--json]
```

Desktop IDs must exactly match `discover` output. Unknown IDs fail instead of
being normalized or matched by prefix.

`convert` is the only command that may contact an AI provider. `codex` uses the
current Codex CLI login. `api` reads `OPENAI_API_KEY`; keys are never accepted
as arguments. Every requested conversion is explicit and forced. Provider,
authentication or billing failures stop the remaining batch.

`apply` only renders an existing current SVG cache and installs standard
16–1024 px user-scoped Hicolor PNGs plus a user `.desktop` override. It creates
the Hicolor metadata when absent, refreshes icon/desktop caches and reinserts
an existing override so the GNOME application grid observes the new
fingerprinted icon without logout. Appearance and accent changes do not call
an AI provider. `restore` refuses to overwrite a managed desktop file that the
user changed after installation.

`--background RRGGBB` replaces the source background colour at render time for
the selected batch. It does not modify the canonical SVG or send an AI request.

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
