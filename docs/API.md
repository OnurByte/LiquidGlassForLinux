# CLI contract

The old localhost HTTP server and PNG artifact contract were removed. The
supported process boundary is the CLI; Rust hosts can use the library directly.

```text
liquid-glass-icon [--output PATH] [--provider codex|api] [--model MODEL]
                  [--reconvert DESKTOP_ID]...
```

- `--provider codex` is the default and uses the current Codex CLI login.
- `--model` defaults to `gpt-5.6-luna` and is passed to the selected provider.
- `--provider api` reads `OPENAI_API_KEY`; a key is never accepted as a CLI
  argument because process arguments are observable by other local processes.
- `--reconvert` is the only path that regenerates a valid converted/stale icon.
- One failed icon does not stop the queue, but provider/auth/billing failures do
  stop it to prevent repeated useless or billable calls.

Each successful application produces `icon.svg` and `icon-manifest.json` under
`<output>/apps/<sanitized-desktop-id>/`. Existing valid conversions are reported
without provider calls.

The manifest schema is [`schema/icon-manifest.schema.json`](../schema/icon-manifest.schema.json).
