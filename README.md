# ChatGPT Bridge

A small self-hosted Rust service that lets a ChatGPT Custom GPT work on a remote Linux development workspace through HTTPS.

The goal is to make web ChatGPT capable of a Codex-like loop: inspect a repository, run shell commands, read and edit files, build, test, use Git, inspect the result, and continue iterating.

> [!WARNING]
> ChatGPT Bridge exposes powerful development operations. Shell commands run with the permissions of the Linux user configured for the service. Do not run the service as `root`, do not expose the raw HTTP port directly to the internet, and keep the Bearer token private.

## Architecture

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
        +-- builds/tests/Git
        |
        v
 Linux development workspace
```

The repository includes `openapi.yaml`, which describes the bridge API for a Custom GPT Action.

## Current API

- `GET /health` — unauthenticated liveness check.
- `GET /v1/info` — bridge capabilities and configured limits.
- `POST /v1/exec` — run a shell command inside the workspace.
- `POST /v1/files/read` — read a UTF-8 text file.
- `POST /v1/files/write` — write a UTF-8 text file.
- `POST /v1/files/list` — list a directory.

Everything under `/v1/*` requires a Bearer token.

## Requirements

- Linux with `systemd`
- Rust stable toolchain (`cargo`)
- `sudo` when installing or managing the service as a normal user
- an existing workspace directory

## Install

Clone the repository and run the installer from the repository root:

```bash
git clone https://github.com/matrixdurden/chatgpt-bridge.git
cd chatgpt-bridge
./install.sh --workspace "$HOME/projects"
```

Replace `$HOME/projects` with the directory ChatGPT should work in.

The installer:

- builds an optimized release binary
- installs `/usr/local/bin/chatgpt-bridge`
- creates `/etc/chatgpt-bridge/config.env`
- creates and enables `chatgpt-bridge.service`
- generates a strong Bearer token on first install
- keeps the existing token during reinstall/upgrade
- starts the service

The service runs as the user who launched the installer, not as root. If you run the installer through `sudo`, it uses `SUDO_USER` when available.

### Installer options

```text
./install.sh --workspace PATH [options]

--workspace PATH       Existing workspace directory. Required.
--service-user USER    Linux user that runs the bridge.
--bind ADDRESS         Listen address. Default: 127.0.0.1:8787
--no-start             Install and enable without starting immediately.
-h, --help             Show help.
```

Example with an explicit service user:

```bash
./install.sh \
  --workspace /home/developer/projects \
  --service-user developer
```

To provide your own token instead of generating one:

```bash
CHATGPT_BRIDGE_TOKEN='replace-with-a-long-random-token' \
  ./install.sh --workspace "$HOME/projects"
```

Custom tokens must contain at least 32 characters and may use letters, numbers, `.`, `_`, `~`, and `-`.

## Installed layout

```text
/usr/local/bin/chatgpt-bridge
/etc/chatgpt-bridge/config.env
/etc/systemd/system/chatgpt-bridge.service
```

There is deliberately no separate uninstaller binary or shell script. After installation, one command manages the application: `chatgpt-bridge`.

The installer does **not** create, move, chown, or own your workspace. This is deliberate: uninstalling the bridge must never delete source code or project data.

## Command line

```text
chatgpt-bridge <command>

start       Start the systemd service
stop        Stop the systemd service
restart     Restart the systemd service
status      Show service status
logs        Show the latest 100 service log lines
logs -f     Follow service logs
uninstall   Remove the installed service, config, and binary
serve       Run the HTTP server; normally used only by systemd
version     Show the installed version
help        Show command help
```

Examples:

```bash
chatgpt-bridge status
chatgpt-bridge restart
chatgpt-bridge logs -f
```

Commands that need administrative access invoke `sudo` themselves when necessary, so normal use does not require writing `sudo systemctl ...` manually.

The systemd service explicitly runs:

```text
/usr/local/bin/chatgpt-bridge serve
```

Running the binary with no arguments also starts the server for compatibility with older installations, but `serve` is the explicit server command.

## Configuration

View the configuration and token:

```bash
sudo cat /etc/chatgpt-bridge/config.env
```

The configuration file is installed with mode `0600`.

Supported values:

| Variable | Default | Description |
| --- | --- | --- |
| `CHATGPT_BRIDGE_TOKEN` | generated | Bearer token, minimum 32 characters. |
| `CHATGPT_BRIDGE_ROOT` | installer value | Workspace root. |
| `CHATGPT_BRIDGE_BIND` | `127.0.0.1:8787` | HTTP bind address. |
| `CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS` | `30000` | Default command timeout. |
| `CHATGPT_BRIDGE_MAX_TIMEOUT_MS` | `300000` | Maximum command timeout. |
| `CHATGPT_BRIDGE_MAX_OUTPUT_BYTES` | `1048576` | Maximum stdout/stderr bytes returned per stream. |
| `CHATGPT_BRIDGE_MAX_FILE_BYTES` | `1048576` | Maximum file read/write size. |
| `RUST_LOG` | `chatgpt_bridge=info` | Rust tracing filter. |

After editing the file:

```bash
chatgpt-bridge restart
```

## Local test

The default listener is `127.0.0.1:8787`.

Health check:

```bash
curl http://127.0.0.1:8787/health
```

For authenticated endpoints, copy the token from `/etc/chatgpt-bridge/config.env`:

```bash
export BRIDGE_TOKEN='your-token-here'
```

Get bridge information:

```bash
curl http://127.0.0.1:8787/v1/info \
  -H "Authorization: Bearer $BRIDGE_TOKEN"
```

Run a command:

```bash
curl -X POST http://127.0.0.1:8787/v1/exec \
  -H "Authorization: Bearer $BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "command": "git status --short && cargo test",
    "cwd": "chatgpt-bridge",
    "timeout_ms": 120000
  }'
```

The `cwd` is always relative to the configured workspace root.

Read a file:

```bash
curl -X POST http://127.0.0.1:8787/v1/files/read \
  -H "Authorization: Bearer $BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"path":"chatgpt-bridge/Cargo.toml"}'
```

Write a file:

```bash
curl -X POST http://127.0.0.1:8787/v1/files/write \
  -H "Authorization: Bearer $BRIDGE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "path": "chatgpt-bridge/example.txt",
    "content": "hello\n",
    "overwrite": true
  }'
```

## Expose it to ChatGPT

Do not change the bridge to `0.0.0.0` and publish port `8787` directly.

Keep it on localhost and put a real HTTPS reverse proxy or secure tunnel in front of it.

A minimal Caddy example:

```caddyfile
bridge.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

After HTTPS works externally, change the server URL near the top of `openapi.yaml`:

```yaml
servers:
  - url: https://bridge.example.com
```

Then configure the Custom GPT Action:

1. Open the GPT editor.
2. Go to **Actions** and create a new action.
3. Use API-key authentication with Bearer authentication.
4. Enter the token generated by `install.sh`.
5. Paste or import `openapi.yaml`.
6. Test `healthCheck`, `getBridgeInfo`, and a harmless command.

Once connected, a request such as this becomes possible:

```text
Inspect the project in matrixcode, run the tests, find the failure, fix it,
run the tests again, show me the diff, then commit the change.
```

The GPT can use the bridge endpoints repeatedly to perform that workflow.

## Workspace and permissions

The selected service user must already be able to read, write, and enter the workspace directory. The installer refuses to change workspace ownership automatically.

For example, if your normal account already owns `/home/me/projects`, the simplest setup is:

```bash
./install.sh --workspace /home/me/projects
```

This also means commands executed by the bridge can use that user's installed tools and Git/SSH configuration.

Important: the workspace root constrains the dedicated file endpoints and command working directory, but the shell itself is not a complete sandbox. A shell command still has the operating-system permissions of the service user. For stronger isolation, use a dedicated Linux user, container, or VM.

## Upgrade

Pull the new code and run the installer again with the same workspace:

```bash
git pull
./install.sh --workspace "$HOME/projects"
```

The binary and service definition are replaced cleanly. Unless you explicitly set `CHATGPT_BRIDGE_TOKEN`, the existing token is retained so the GPT Action does not need to be reconfigured.

Upgrading from the older standalone-uninstaller layout automatically removes `/usr/local/bin/chatgpt-bridge-uninstall`.

## Uninstall

Use the same binary:

```bash
chatgpt-bridge uninstall
```

It stops and disables the service, then removes:

```text
/usr/local/bin/chatgpt-bridge
/etc/chatgpt-bridge/
/etc/systemd/system/chatgpt-bridge.service
```

It also removes the legacy `/usr/local/bin/chatgpt-bridge-uninstall` path if an older installation left it behind, reloads systemd, and clears the failed-service state.

The workspace, repositories, Linux user, Git keys, and source checkout are intentionally untouched. They are user data, not ChatGPT Bridge installation files.

## Manual build

If you do not want the installer:

```bash
cargo build --release
```

The binary is produced at:

```text
target/release/chatgpt-bridge
```

Minimum runtime configuration:

```bash
export CHATGPT_BRIDGE_TOKEN="$(openssl rand -hex 32)"
export CHATGPT_BRIDGE_ROOT="$HOME/projects"
export CHATGPT_BRIDGE_BIND="127.0.0.1:8787"
./target/release/chatgpt-bridge serve
```

## Security model

- Bearer authentication is mandatory for `/v1/*`.
- The default listener is localhost only.
- File APIs reject absolute paths and `..` traversal.
- File APIs reject symlink escapes outside the configured workspace.
- Command working directories must resolve inside the workspace.
- Command execution has configurable timeouts.
- Command and file output sizes are capped.
- The systemd service uses `NoNewPrivileges` and additional hardening directives.
- The service is forbidden from running as root by the installer.

This still does not turn arbitrary shell execution into a perfect sandbox. Treat the service account as part of the security boundary.

## Development

Run the same checks used by CI:

```bash
bash -n install.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Project status

Early development. The API and security model may evolve as process/session management, structured Git operations, patching, audit logging, and stronger isolation are added.

## Disclaimer

This is an independent project and is not an official OpenAI or ChatGPT product.
