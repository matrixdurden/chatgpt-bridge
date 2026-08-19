# ChatGPT Bridge

A small self-hosted Rust service that lets a ChatGPT Custom GPT work on a remote Linux development workspace.

It provides a Codex-like loop from web ChatGPT: inspect repositories, run shell commands, read and edit files, build, test, use Git, and iterate.

> [!WARNING]
> Shell commands run with the permissions of the configured Linux user. Never run the service as `root`. Public mode requires HTTPS and Bearer authentication.

## Install

```bash
git clone https://github.com/matrixdurden/chatgpt-bridge.git
cd chatgpt-bridge
./install.sh
```

First start:

```bash
chatgpt-bridge start --workspace "/projects"
```

The workspace and runtime settings are saved, so later:

```bash
chatgpt-bridge start
```

## Commands

```text
chatgpt-bridge start --workspace PATH   Set workspace
chatgpt-bridge start --port PORT        Set listen port
chatgpt-bridge start --public           Use public interface (TLS required)
chatgpt-bridge start --local            Return to localhost-only HTTP
chatgpt-bridge start                     Start with saved settings
chatgpt-bridge stop                      Stop
chatgpt-bridge restart                   Restart
chatgpt-bridge status                    Show service status
chatgpt-bridge logs                      Show latest logs
chatgpt-bridge logs -f                   Follow logs
chatgpt-bridge key                       Show Bearer key
chatgpt-bridge key rotate                Generate a new Bearer key
chatgpt-bridge uninstall                 Completely remove ChatGPT Bridge
```

Ports below `1024` are intentionally rejected because the bridge runs as an unprivileged user. If you want external port `443`, forward it at the router/firewall to an internal bridge port such as `8787`.

## Local mode

Local mode is the default:

```bash
chatgpt-bridge start --workspace "/projects" --port 8787
```

Test:

```bash
curl http://127.0.0.1:8787/health
```

## Secure public mode

Public mode is HTTPS-only. The bridge refuses to bind to a non-loopback interface unless a certificate and private key are configured.

Use a publicly trusted certificate whose SAN matches the hostname or public IP address clients will use:

```bash
chatgpt-bridge start \
  --workspace "/projects" \
  --port 8787 \
  --public \
  --tls-cert "/absolute/path/fullchain.pem" \
  --tls-key "/absolute/path/privkey.pem"
```

The certificate and key are copied into managed paths:

```text
/etc/chatgpt-bridge/tls/fullchain.pem
/etc/chatgpt-bridge/tls/privkey.pem
```

The private key is readable only by root and the service user's primary group. The original source files are not required while the bridge is running.

After the first successful public start, settings are saved:

```bash
chatgpt-bridge start
```

When a short-lived certificate is renewed, rerun the public start command with the renewed certificate/key paths to import them and restart the service.

To return to localhost-only HTTP:

```bash
chatgpt-bridge start --local --port 8787
```

## Bearer key

Installation automatically creates a random 256-bit key.

Show it:

```bash
chatgpt-bridge key
```

Rotate it:

```bash
chatgpt-bridge key rotate
```

Rotation saves the new key and restarts an active service. Update the Custom GPT Action authentication after rotating.

## API

Current endpoints:

- `GET /health`
- `GET /v1/info`
- `POST /v1/exec`
- `POST /v1/files/read`
- `POST /v1/files/write`
- `POST /v1/files/list`

Everything under `/v1/*` requires `Authorization: Bearer <key>`.

The repository includes `openapi.yaml` for a Custom GPT Action.

## Custom GPT Action

Once the public HTTPS endpoint works, set `servers` in `openapi.yaml` to the real endpoint, for example:

```yaml
servers:
  - url: https://203.0.113.10:8787
```

Then in the GPT Action configuration:

1. Paste/import `openapi.yaml`.
2. Choose API-key authentication.
3. Choose Bearer authentication.
4. Enter the value from `chatgpt-bridge key`.
5. Test `healthCheck` and `getBridgeInfo` before executing shell commands.

## Configuration

Installed files:

```text
/usr/local/bin/chatgpt-bridge
/etc/chatgpt-bridge/config.env
/etc/chatgpt-bridge/tls/        # created when TLS is imported
/etc/systemd/system/chatgpt-bridge.service
```

Main environment values:

| Variable | Description |
| --- | --- |
| `CHATGPT_BRIDGE_TOKEN` | Bearer key. |
| `CHATGPT_BRIDGE_ROOT` | Workspace root. |
| `CHATGPT_BRIDGE_BIND` | Saved bind address/port. |
| `CHATGPT_BRIDGE_TLS_CERT` | Managed TLS certificate path. |
| `CHATGPT_BRIDGE_TLS_KEY` | Managed TLS private-key path. |
| `CHATGPT_BRIDGE_SERVICE_USER` | Unprivileged Linux service user. |
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

The installer preserves the Bearer key, workspace, bind address, port, and TLS settings.

## Uninstall

```bash
chatgpt-bridge uninstall
```

This removes the installed binary, configuration, managed TLS copies, systemd service, and legacy installation files. It never deletes the workspace, repositories, Linux user, or Git/SSH credentials.

## Security model

- Public mode cannot start without TLS.
- Bearer authentication is mandatory for `/v1/*`.
- Bearer keys are 256-bit random values and compared in constant time.
- Listener defaults to localhost only.
- The service runs as an unprivileged Linux user.
- File APIs reject absolute paths, `..` traversal, and symlink escapes.
- Command working directories must resolve inside the workspace.
- Command execution has timeouts and output limits.
- TLS private keys are copied with restricted filesystem permissions.
- The systemd service uses `NoNewPrivileges` and `PrivateTmp`.

The shell itself is not a complete sandbox. Commands still have the operating-system permissions of the configured service user.

## Development

```bash
bash -n install.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Disclaimer

This is an independent project and is not an official OpenAI or ChatGPT product.
