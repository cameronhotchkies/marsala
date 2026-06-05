# Marsala

Marsala currently ships a local-first Rust service skeleton with a non-streaming OpenAI-shaped chat completions proxy. It starts cleanly, loads config from TOML and env, exposes `GET /healthz` plus `POST /v1/chat/completions`, writes JSONL events, and shuts down on `Ctrl-C`.

That explicit gateway remains supported as a compatibility path. It is not proof that Marsala intercepts Codex today. The next baseline work is Codex traffic acquisition/interception viability: verifying routing, proxy env behavior, upstream hosts/endpoints, streaming shape, auth forwarding policy, and safe logging defaults.

## Prerequisites

- Rust 1.96 or newer
- `cargo` from the same toolchain

This repo declares `rust-version = "1.96"` in `Cargo.toml` and pins `1.96.0` in `rust-toolchain.toml` for `rustup` users. If you manage toolchains manually, use `rustup toolchain install 1.96.0` and `rustup override set 1.96.0` or `rustup default 1.96.0`.

## Workspace layout

- `crates/marsala`: service crate
- `docs/planning`: product and implementation planning

## Local usage

```bash
cargo run -p marsala -- serve
cargo run -p marsala -- config print
cargo run -p marsala -- config print --all
cargo run -p marsala -- logs tail --lines 20
```

First-run smoke path:

Terminal 1:

```bash
cargo run -p marsala -- serve
```

Terminal 2:

```bash
cargo run -p marsala -- config print
curl http://127.0.0.1:8787/healthz
curl http://127.0.0.1:8787/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"Say hi"}]}'
cargo run -p marsala -- logs tail --lines 20
```

Local defaults:

- bind: `127.0.0.1:8787`
- health check: `http://127.0.0.1:8787/healthz`
- chat completions: `http://127.0.0.1:8787/v1/chat/completions`
- JSONL event log: `logs/events.jsonl`
- body logging: disabled unless `logging.log_bodies=true`

Config loading order:

1. built-in defaults
2. `marsala.toml` in the current working directory, if present
3. `--config /path/to/file.toml` or `MARSALA_CONFIG`
4. environment overrides with the `MARSALA__...` prefix

`MARSALA_CONFIG` only selects which TOML file to read. Field overrides come from `MARSALA__...`.

Example environment overrides:

```bash
export MARSALA__SERVER__PORT=9797
export MARSALA__LOGGING__LOG_BODIES=true
export OPENAI_API_KEY=sk-...
```

`marsala.example.toml` shows the current compatibility-gateway config surface. Use `cargo run -p marsala -- config print --all` to inspect the full effective config, including roadmap placeholders that are not active yet.

Current explicit chat-completions gateway settings:

- `openai.base_url`: upstream base, default `https://api.openai.com/v1`
- `openai.api_key_env`: env var name that holds the upstream API key, default `OPENAI_API_KEY`
- Marsala does not read inbound `Authorization` as an upstream fallback
- `stream=true` is rejected locally in the current compatibility gateway; omit `stream` or send `false`

## Docker

Build and run:

```bash
docker build -t marsala .
docker run --rm -p 8787:8787 -v "$(pwd)/logs:/app/logs" marsala
```

The container overrides a few runtime settings:

- bind host becomes `0.0.0.0`
- `logging.capture_stream_chunks=false`

## Current CLI surface

- `serve`: start the Axum server
- `config print`: print the merged active gateway config (`server`, `openai`, `logging`)
- `config print --all`: include roadmap sections such as `openai`, `rewrite`, `tool_capture`, `proxy`, and `mitm`
- `logs tail`: print the last JSONL entries, with optional `--follow`
- `logs tail --follow`: prints one waiting message to stderr when `logs/events.jsonl` does not exist yet, then resumes on file creation or recreation
