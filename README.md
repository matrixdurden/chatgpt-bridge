# ChatGPT Bridge

A small self-hosted Rust service that gives a ChatGPT Custom GPT controlled access to a Linux development machine through HTTPS.

The goal is simple: make the web ChatGPT experience capable of doing Codex-like work on a remote workspace — run shell commands, inspect files, edit files, run tests, use Git, and iterate on code — without embedding an SSH client inside ChatGPT.

> [!WARNING]
> This project intentionally exposes powerful development operations. The `exec` endpoint has the same effective permissions as the operating-system user running `chatgpt-bridge`. Never run it as `root`, never expose it without authentication, and never publish the HTTP port directly to the internet.

## How it works

```text
ChatGPT / Custom GPT
        |
        | HTTPS + Bearer token
        v
 reverse proxy / TLS
        |
        v
 chatgpt-bridge (Rust)
        |
        +-- shell commands
        +-- file read/write/list
        +-- build/test/git through shell
        |
        v
 Linux development workspace
```

ChatGPT Custom Actions use an OpenAPI schema to describe external APIs. This repository includes `openapi.yaml`, which can be imported into the GPT Action editor after changing its server URL to your HTTPS endpoint.

## Current API

- `GET /health` — unauthenticated liveness check.
- `GET /v1/info` — bridge capabilities and configured limits.
- `POST /v1/exec` — run a shell command inside the workspace.
- `POST /v1/files/read` — read a UTF-8 text file.
- `POST /v1/files/write` — write a UTF-8 text file.
- `POST /v1/files/list` — list a directory.

Everything under `/v1/*` requires a Bearer token.

## Security model

The bridge is designed to be small and understandable, not to pretend raw shell access is harmless.

- A strong Bearer token is mandatory at startup.
- The default listener is `127.0.0.1:8787`.
- File APIs only accept paths relative to the configured workspace root.
- File path traversal (`..`) and absolute paths are rejected.
- Symlink escapes are rejected for file operations.
- Command working directories must stay inside the workspace.
- Command execution has a configurable timeout.
- Command and file output sizes are capped.

Important: the shell itself is **not a sandbox**. A command such as `cat /etc/passwd` can still access anything the bridge's Linux user is allowed to access. For real isolation, run the bridge under a dedicated unprivileged user and, if needed, inside a container/VM with only the intended workspace mounted.

## Build

Requirements:

- Linux
- Rust stable toolchain

```bash
cargo build --release
```

The binary will be at:

```text
target/release/chatgpt-bridge
```

## Configuration

`CHATGPT_BRIDGE_TOKEN` and `CHATGPT_BRIDGE_ROOT` are required.

```bash
export CHATGPT_BRIDGE_TOKEN="$(openssl rand -hex 32)"
export CHATGPT_BRIDGE_ROOT="/home/bridge/workspaces"
export CHATGPT_BRIDGE_BIND="127.0.0.1:8787"

./target/release/chatgpt-bridge
```

Supported environment variables:

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `CHATGPT_BRIDGE_TOKEN` | yes | — | Bearer token. Minimum 32 characters. |
| `CHATGPT_BRIDGE_ROOT` | yes | — | Existing workspace directory. |
| `CHATGPT_BRIDGE_BIND` | no | `127.0.0.1:8787` | HTTP bind address. |
| `CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS` | no | `30000` | Default command timeout. |
| `CHATGPT_BRIDGE_MAX_TIMEOUT_MS` | no | `300000` | Maximum command timeout. |
| `CHATGPT_BRIDGE_MAX_OUTPUT_BYTES` | no | `1048576` | Maximum bytes returned per stdout/stderr stream. |
| `CHATGPT_BRIDGE_MAX_FILE_BYTES` | no | `1048576` | Maximum file read/write size. |
| `RUST_LOG` | no | `chatgpt_bridge=info` | Rust tracing filter. |

See `.env.example` for a minimal example.

## API examples

Health check:

```bash
curl http://127.0.0.1:8787/health
```

Run a command:

```bash
curl -X POST http://127.0.0.1:8787/v1/exec \
  -H "Authorization: Bearer $CHATGPT_BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "command": "git status --short && cargo test",
    "cwd": "chatgpt-bridge",
    "timeout_ms": 120000
  }'
```

Read a file:

```bash
curl -X POST http://127.0.0.1:8787/v1/files/read \
  -H "Authorization: Bearer $CHATGPT_BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"path":"chatgpt-bridge/Cargo.toml"}'
```

Write a file:

```bash
curl -X POST http://127.0.0.1:8787/v1/files/write \
  -H "Authorization: Bearer $CHATGPT_BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "path": "chatgpt-bridge/example.txt",
    "content": "hello\n",
    "overwrite": true
  }'
```

## Connecting a Custom GPT

1. Put the bridge behind a real HTTPS domain. A reverse proxy such as Caddy or nginx is appropriate.
2. Keep the Rust service bound to localhost unless you deliberately isolate the network another way.
3. Change the `servers` URL in `openapi.yaml` to your public HTTPS bridge URL.
4. In the GPT editor, create an Action and import/paste `openapi.yaml`.
5. Configure authentication as an API key using Bearer authentication and enter the same value as `CHATGPT_BRIDGE_TOKEN`.
6. Test the endpoints from the GPT editor before allowing broader use.

## Planned direction

The first version deliberately stays small. Useful next steps include:

- long-running process/session support
- structured Git endpoints
- patch/diff operations
- audit log with request IDs
- configurable command policy
- optional container isolation
- systemd installer/service hardening

## Project status

Early development. The API may change while the security and session model are being finalized.

## Disclaimer

This is an independent project and is not an official OpenAI or ChatGPT product.
