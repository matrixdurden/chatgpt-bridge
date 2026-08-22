# ChatGPT Bridge

A small self-hosted Rust service that lets a ChatGPT Custom GPT work on a Linux development workspace.

It provides a Codex-like loop from web ChatGPT: inspect repositories, run shell commands, read and edit files, build, test, use Git, and iterate.

> [!WARNING]
> Shell commands run with the permissions of the configured Linux user. Never run the service as `root`. Treat the Bearer key like shell access to user account.

## Install

Recommended one-line install:

```bash
curl -fsSL https://raw.githubusercontent.com/matrixdurden/chatgpt-bridge/main/install.sh | bash
```

The installer detects `x86_64` or `arm64`, downloads the matching prebuilt GitHub Release binary, verifies it against the published SHA-256 checksum, installs the systemd service, and generates the Bearer key.

The target machine does **not** need Rust, Cargo, Git, ngrok, Nginx, a TLS certificate, or router port forwarding for the normal setup.

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/matrixdurden/chatgpt-bridge/main/install.sh \
  | bash -s -- --version 0.2.0
```

For development from a source checkout:

```bash
git clone https://github.com/matrixdurden/chatgpt-bridge.git
cd chatgpt-bridge
./install.sh --from-source
```

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

## Updates

Check for a new release:

```bash
chatgpt-bridge update --check
```

Install the latest release:

```bash
chatgpt-bridge update
```

Install or roll back to a specific release:

```bash
chatgpt-bridge update --version 0.2.0
```

The updater downloads the architecture-specific GitHub Release archive, verifies `SHA256SUMS`, verifies the downloaded binary reports the expected version, replaces `/usr/local/bin/chatgpt-bridge`, and restarts the service if it was already active. Existing workspace, Bearer key, ngrok token, and runtime configuration are preserved.

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
chatgpt-bridge update                    Install latest GitHub Release
chatgpt-bridge update --check            Check whether an update exists
chatgpt-bridge update --version VERSION  Install/roll back to a release
chatgpt-bridge version                   Show installed version
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

For checkpoint-aware editing, add this behavior to the GPT instructions:

```text
Before the first mutating workspace operation for a user request, call beginChange for the project directory.
Use the returned transaction_id for that request.
After all edits, builds, and tests are complete, call finishChange.
If finishChange returns a change_id, show it at the top of the final response.
Do not show a change ID when changed=false.
When the user asks to go back to a change ID, call restoreCheckpoint.
When the user explicitly asks to undo that change itself, call undoCheckpoint.
```

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
- `GET /v1/changes`
- `POST /v1/changes/begin`
- `POST /v1/changes/finish`
- `POST /v1/changes/restore`
- `POST /v1/changes/undo`

### Checkpoints and undo

Checkpoint data is stored outside the configured workspace, under `~/.local/state/chatgpt-bridge/checkpoints` by default (or under `CHATGPT_BRIDGE_STATE_DIR` when set). It uses a content-addressed object store, so unchanged file contents are reused across checkpoints. The project does not need to be a Git repository.

To keep normal coding checkpoints fast and compact, common generated/cache directories (`node_modules`, `target`, `.venv`, `venv`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.cache`, `dist`, `build`, `.next`, `.nuxt`, and `coverage`) are excluded by default. Set `CHATGPT_BRIDGE_CHECKPOINT_INCLUDE_GENERATED=true` for full project snapshots except VCS metadata.

Checkpoint scopes are project directories relative to the configured workspace root. `.git`, `.hg`, and `.svn` metadata directories are never snapshotted, restored, or deleted by the checkpoint engine, including nested repositories. Symlinks are recorded as links and are never followed while snapshotting.

`beginChange` captures the pre-edit state. `finishChange` creates a visible `chg-XXXXXX` checkpoint only when the filesystem changed. `restoreCheckpoint` means “make the project look exactly as it did after this checkpoint”; later checkpoint objects remain available, so restoring an older ID does not destroy history. `undoCheckpoint` restores the selected checkpoint's parent state.

If files changed outside checkpoint history, restore returns `409 Conflict` by default. With `force=true`, those current files are first preserved in a hidden `safe-XXXXXX` checkpoint and its ID is returned.

Everything under `/v1/*` requires `Authorization: Bearer <key>`.

## Advanced direct HTTPS mode

If you explicitly want to expose the bridge without ngrok, direct HTTPS mode remains available:

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
| `CHATGPT_BRIDGE_STATE_DIR` | Optional checkpoint-state base directory. Defaults to the service user state directory. |
| `CHATGPT_BRIDGE_CHECKPOINT_INCLUDE_GENERATED` | Include generated/cache directories in checkpoints. Defaults to `false`. |
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

## Releases

`Cargo.toml` is the version source of truth. When a commit reaches `main` with a version whose `vVERSION` tag does not already exist, the Release workflow builds:

```text
chatgpt-bridge-x86_64-unknown-linux-gnu.tar.gz
chatgpt-bridge-aarch64-unknown-linux-gnu.tar.gz
SHA256SUMS
```

After both builds succeed, the workflow creates the matching Git tag and GitHub Release. Bumping the package version is therefore enough to publish the next release.

## Uninstall

```bash
chatgpt-bridge uninstall
```

This removes the installed binary, configuration, managed TLS copies, systemd service, and saved ngrok token. It never deletes the workspace, repositories, Linux user, or Git/SSH credentials.

## Security model

- Release binaries are verified against a SHA-256 checksum downloaded from the same GitHub Release.
- Automatic public mode keeps the bridge itself on localhost and exposes it through ngrok HTTPS.
- Bearer authentication is mandatory for `/v1/*`.
- Bearer keys are 256-bit random values and compared in constant time.
- ngrok authentication is stored only in the root-only bridge config.
- The service runs as an unprivileged Linux user.
- File APIs reject absolute paths, `..` traversal, and symlink escapes.
- Command working directories must resolve inside the workspace.
- Command execution has timeouts and output limits.
- Checkpoints are stored outside the workspace and never modify VCS metadata directories.
- The systemd service uses `NoNewPrivileges`, `PrivateTmp`, and a private runtime directory.

The shell itself is not a complete sandbox. Commands still have the operating-system permissions of the configured service user.

## Development

```bash
bash -n install.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```
