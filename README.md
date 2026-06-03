# Marsala

Phase 0 is a local-first Rust service skeleton for an eventual OpenAI-shaped proxy. It starts cleanly, loads config from TOML and env, exposes `GET /healthz`, writes JSONL events, and shuts down on `Ctrl-C`.

## Prerequisites

- Rust 1.82 or newer
- `cargo` from the same toolchain

This repo declares `rust-version = "1.82"` in [Cargo.toml](/home/cameron/code/marsala/Cargo.toml:9) and pins `1.82.0` in [rust-toolchain.toml](/home/cameron/code/marsala/rust-toolchain.toml:1) for `rustup` users.

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
cargo run -p marsala -- logs tail --lines 20
```

Local defaults:

- bind: `127.0.0.1:8787`
- health check: `http://127.0.0.1:8787/healthz`
- JSONL event log: `logs/events.jsonl`

Config loading order:

1. built-in defaults
2. `marsala.toml` in the current working directory, if present
3. `--config /path/to/file.toml` or `MARSALA_CONFIG`
4. environment overrides with the `MARSALA__...` prefix

`MARSALA_CONFIG` only selects which TOML file to read. Field overrides come from `MARSALA__...`.

Example environment overrides:

```bash
export MARSALA__SERVER__PORT=9797
export MARSALA__LOGGING__LOG_BODIES=false
```

`marsala.example.toml` shows the active Phase 0 config surface. Use `cargo run -p marsala -- config print --all` to inspect the full effective config, including roadmap placeholders that are not active yet.

## Docker

Build and run:

```bash
docker build -t marsala .
docker run --rm -p 8787:8787 -v "$(pwd)/logs:/app/logs" marsala
```

The container overrides a few settings for safer defaults:

- bind host becomes `0.0.0.0`
- `logging.log_bodies=false`
- `logging.capture_stream_chunks=false`

## Phase 0 CLI

- `serve`: start the Axum server
- `config print`: print the merged active Phase 0 config (`server`, `logging`)
- `config print --all`: include roadmap sections such as `openai`, `rewrite`, `tool_capture`, `proxy`, and `mitm`
- `logs tail`: print the last JSONL entries, with optional `--follow`
- `logs tail --follow`: prints one waiting message to stderr when `logs/events.jsonl` does not exist yet, then resumes on file creation or recreation
