# ChatGPT Bridge

A small self-hosted Rust service that lets a ChatGPT Custom GPT work on a Linux development workspace.

It provides a Codex-like loop from web ChatGPT: inspect repositories, run shell commands, read and edit files, build, test, use Git, and iterate.

> [!WARNING]
> Shell commands run with the permissions of the configured Linux user. Never run the service as `root`. Treat the Bearer key like shell access to that user account.

## Install

```bash
git clone https://github.com/matrixdurden/chatgpt-bridge.git
cd chatgpt-bridge
./install.sh
```

No ngrok binary, Nginx, TLS certificate, or router configuration is needed for the normal public setup.

## Easy public start

```bash
chatgpt-bridge start \
  --workspace "/home/user/projects" \
  --port 8787 \
  --public
```

On the first public start only, ChatGPT Bridge opens or prints ngrok's authtoken page and asks for your ngrok authtoken. The token is entered without echo and saved in `/etc/chatgpt-bridge/config.env`, which is installed with mode `0600`.

After that, `--public` uses the official ngrok Rust SDK embedded directly in ChatGPT Bridge. There is no separate ngrok process to install or manage.

Expected output is similar to:

```text
Workspace: /home/user/projects
Mode: public (ngrok)
Local: http://127.0.0.1:8787
Public: https://example.ngrok.app
GPT Action server: https://example.ngrok.app
```

The workspace, port, public-mode setting, ngrok token, and Bearer key are saved. Later you can simply run:

```bash
chatgpt-bridge start
```

## Local-only mode

```bash
chatgpt-bridge start \
  --workspace "/home/user/projects" \
  --port 8787 \
  --local
```

Test it with:

```bash
curl http://127.0.0.1:8787/health
```

## Commands

```text
chatgpt-bridge start --workspace PATH   Set workspace
chatgpt-bridge start --port PORT        Set local listen port
chatgpt-bridge start --public           Publish automatically through ngrok HTTPS
chatgpt-bridge start --local            Disable the tunnel and use localhost only
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

Ports below `1024` are intentionally rejected because the bridge service runs as an unprivileged user. Automatic public mode does not need port `443`; ngrok provides the external HTTPS endpoint and forwards it to the chosen localhost port.

## Custom GPT Action

Set `servers` in `openapi.yaml` to the `Public:` URL printed by ChatGPT Bridge:

```yaml
servers:
  - url: https://example.ngrok.app
```

Then in the GPT Action configuration:

1. Paste/import `openapi.yaml`.
2. Choose API-key authentication.
3. Choose Bearer authentication.
4. Enter the value from `chatgpt-bridge key`.
5. Test `healthCheck` and `getBridgeInfo`.

The schema marks the bridge POST actions as non-consequential so ChatGPT can offer persistent permission instead of asking on every normal tool call. Grant that only if you intend the GPT to have ongoing command/file access to the configured workspace.

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

## Advanced direct HTTPS mode

If you explicitly want to expose the bridge without ngrok, the previous direct HTTPS mode remains available:

```bash
chatgpt-bridge start \
  --workspace "/home/user/projects" \
  --port 8787 \
  --public \
  --tls-cert "/absolute/path/fullchain.pem" \
  --tls-key "/absolute/path/privkey.pem"
```

When certificate paths are supplied with `--public`, the bridge binds directly to the public interface and does not use ngrok. A publicly trusted certificate is required. For most Custom GPT installations, automatic ngrok mode is simpler.

## Configuration

Installed files:

```text
/usr/local/bin/chatgpt-bridge
/etc/chatgpt-bridge/config.env
/etc/chatgpt-bridge/tls/        # only created for advanced direct TLS mode
/etc/systemd/system/chatgpt-bridge.service
/run/chatgpt-bridge/            # runtime state while the service is active
```

Main environment values:

| Variable | Description |
| --- | --- |
| `CHATGPT_BRIDGE_TOKEN` | Bearer key. |
| `CHATGPT_BRIDGE_ROOT` | Workspace root. |
| `CHATGPT_BRIDGE_BIND` | Local/direct bind address and port. |
| `CHATGPT_BRIDGE_NGROK_ENABLED` | Enables embedded ngrok public mode. |
| `NGROK_AUTHTOKEN` | ngrok account token used by embedded public mode. |
| `CHATGPT_BRIDGE_TLS_CERT` | Managed certificate path for advanced direct TLS mode. |
| `CHATGPT_BRIDGE_TLS_KEY` | Managed private-key path for advanced direct TLS mode. |
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

The installer preserves the Bearer key, workspace, bind settings, ngrok configuration, and direct TLS settings.

## Uninstall

```bash
chatgpt-bridge uninstall
```

This removes the installed binary, configuration, managed TLS copies, systemd service, and saved ngrok token. It never deletes the workspace, repositories, Linux user, or Git/SSH credentials.

## Security model

- Automatic public mode keeps the bridge itself on localhost and exposes it through ngrok HTTPS.
- Bearer authentication is mandatory for `/v1/*`.
- Bearer keys are 256-bit random values and compared in constant time.
- ngrok authentication is stored only in the root-only bridge config.
- The service runs as an unprivileged Linux user.
- File APIs reject absolute paths, `..` traversal, and symlink escapes.
- Command working directories must resolve inside the workspace.
- Command execution has timeouts and output limits.
- The systemd service uses `NoNewPrivileges`, `PrivateTmp`, and a private runtime directory.

The shell itself is not a complete sandbox. Commands still have the operating-system permissions of the configured service user.

## Development

```bash
bash -n install.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```
