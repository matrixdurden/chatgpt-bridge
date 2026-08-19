# ChatGPT Bridge

A small self-hosted Rust service that lets a ChatGPT Custom GPT work on a remote Linux development workspace through HTTPS.

The goal is simple: use web ChatGPT in a Codex-like loop — inspect repositories, run shell commands, read and edit files, build, test, use Git, and continue iterating.

> [!WARNING]
> Shell commands run with the permissions of the Linux user running the service. Do not run ChatGPT Bridge as `root`, do not expose the raw HTTP port directly to the internet, and keep the Bearer token private.

## Architecture

```text
ChatGPT / Custom GPT
        |
        | HTTPS + Bearer token
        v
 reverse proxy / TLS
        |
        v
 chatgpt-bridge
        |
        +-- shell
        +-- files
        +-- Git/build/test tools
        |
        v
 Linux workspace
```

## Install

```bash
git clone https://github.com/matrixdurden/chatgpt-bridge.git
cd chatgpt-bridge
./install.sh
```

The installer builds and installs:

```text
/usr/local/bin/chatgpt-bridge
/etc/chatgpt-bridge/config.env
/etc/systemd/system/chatgpt-bridge.service
```

It also generates the Bearer token. It does **not** choose or modify a workspace.

Requirements:

- Linux with `systemd`
- Rust stable toolchain (`cargo`)
- `sudo` for system installation

## First start

Choose the directory ChatGPT may work inside:

```bash
chatgpt-bridge start --workspace "/projects"
```

The path must already exist.

The workspace is saved in `/etc/chatgpt-bridge/config.env`, so later starts are simply:

```bash
chatgpt-bridge start
```

Changing workspace is also simple:

```bash
chatgpt-bridge start --workspace "/srv/code"
```

If the service is already running, supplying `--workspace` updates the configuration and restarts it so the new workspace takes effect immediately.

## Commands

```text
chatgpt-bridge start --workspace PATH   Set workspace and start
chatgpt-bridge start                    Start with saved workspace
chatgpt-bridge stop                     Stop
chatgpt-bridge restart                  Restart
chatgpt-bridge status                   Show service status
chatgpt-bridge logs                     Show latest logs
chatgpt-bridge logs -f                  Follow logs
chatgpt-bridge version                  Show version
chatgpt-bridge help                     Show help
chatgpt-bridge uninstall                Completely remove ChatGPT Bridge
```

Administrative operations request `sudo` automatically when needed.

## Quick test

After starting:

```bash
curl http://127.0.0.1:8787/health
```

View the token/configuration:

```bash
sudo cat /etc/chatgpt-bridge/config.env
```

Authenticated API test:

```bash
export BRIDGE_TOKEN='your-token-here'

curl http://127.0.0.1:8787/v1/info \
  -H "Authorization: Bearer $BRIDGE_TOKEN"
```

Run a command inside the configured workspace:

```bash
curl -X POST http://127.0.0.1:8787/v1/exec \
  -H "Authorization: Bearer $BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "command": "pwd && ls -la",
    "cwd": ""
  }'
```

`cwd` is always relative to the configured workspace root.

## API

Current endpoints:

- `GET /health`
- `GET /v1/info`
- `POST /v1/exec`
- `POST /v1/files/read`
- `POST /v1/files/write`
- `POST /v1/files/list`

Everything under `/v1/*` requires Bearer authentication.

The repository includes `openapi.yaml` for a Custom GPT Action.

## Expose it to ChatGPT

By default the service listens only on:

```text
127.0.0.1:8787
```

Do not expose that port directly. Put HTTPS in front of it.

Minimal Caddy example:

```caddyfile
bridge.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

Then change the server URL in `openapi.yaml`:

```yaml
servers:
  - url: https://bridge.example.com
```

In the GPT Action configuration:

1. Import/paste `openapi.yaml`.
2. Configure API-key authentication using Bearer auth.
3. Enter the token generated during installation.
4. Test `healthCheck`, `getBridgeInfo`, and a harmless shell command.

## Configuration

Configuration lives at:

```text
/etc/chatgpt-bridge/config.env
```

Main values:

| Variable | Description |
| --- | --- |
| `CHATGPT_BRIDGE_TOKEN` | Bearer token. |
| `CHATGPT_BRIDGE_ROOT` | Saved workspace. Added by `start --workspace`. |
| `CHATGPT_BRIDGE_BIND` | HTTP bind address. Default `127.0.0.1:8787`. |
| `CHATGPT_BRIDGE_SERVICE_USER` | Linux user running the systemd service. |
| `CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS` | Default command timeout. |
| `CHATGPT_BRIDGE_MAX_TIMEOUT_MS` | Maximum command timeout. |
| `CHATGPT_BRIDGE_MAX_OUTPUT_BYTES` | Maximum stdout/stderr returned. |
| `CHATGPT_BRIDGE_MAX_FILE_BYTES` | Maximum file read/write size. |

## Upgrade

```bash
cd chatgpt-bridge
git pull
./install.sh
```

The existing token and saved workspace are preserved. If the service was running, the installer restarts it with the updated binary.

## Uninstall

```bash
chatgpt-bridge uninstall
```

This removes the binary, configuration, systemd service, enablement, and legacy installation files.

It deliberately does **not** remove the configured workspace, repositories, Linux user, Git keys, or source checkout.

## Security model

- Bearer authentication is mandatory for `/v1/*`.
- Listener defaults to localhost only.
- File APIs reject absolute paths, `..` traversal, and symlink escapes.
- Command working directories must resolve inside the workspace.
- Command execution and output sizes are limited.
- The systemd service uses `NoNewPrivileges` and additional hardening.
- The installer refuses to run the service as `root`.

The shell itself is not a perfect sandbox. It still has the operating-system permissions of the configured service user.

## Development

```bash
bash -n install.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Disclaimer

This is an independent project and is not an official OpenAI or ChatGPT product.
