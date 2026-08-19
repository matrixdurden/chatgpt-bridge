#!/usr/bin/env bash
set -Eeuo pipefail

APP_NAME="chatgpt-bridge"
BINARY_PATH="/usr/local/bin/chatgpt-bridge"
LEGACY_UNINSTALL_PATH="/usr/local/bin/chatgpt-bridge-uninstall"
CONFIG_DIR="/etc/chatgpt-bridge"
CONFIG_FILE="${CONFIG_DIR}/config.env"
SERVICE_FILE="/etc/systemd/system/chatgpt-bridge.service"
DEFAULT_BIND="127.0.0.1:8787"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE=""
BIND="${DEFAULT_BIND}"
SERVICE_USER="${SUDO_USER:-${USER:-}}"
START_SERVICE=1
TOKEN="${CHATGPT_BRIDGE_TOKEN:-}"
TOKEN_WAS_GENERATED=0
TOKEN_WAS_REUSED=0

usage() {
    cat <<'EOF'
ChatGPT Bridge Linux installer

Usage:
  ./install.sh --workspace PATH [options]

Required:
  --workspace PATH       Existing directory ChatGPT Bridge may work in.

Options:
  --service-user USER    Linux user that will run the bridge.
                         Default: the invoking non-root user.
  --bind ADDRESS         HTTP listen address. Default: 127.0.0.1:8787
  --no-start             Install and enable the service, but do not start it now.
  -h, --help             Show this help.

Authentication:
  A 64-character random token is generated automatically on first install.
  Reinstalling preserves the existing generated token. To provide your own,
  set CHATGPT_BRIDGE_TOKEN in the environment before running the installer.

Examples:
  ./install.sh --workspace "$HOME/projects"
  ./install.sh --workspace /srv/projects --service-user developer
  CHATGPT_BRIDGE_TOKEN='your-long-random-token' ./install.sh --workspace "$HOME/projects"

Do not run the bridge service as root. The installer uses sudo only for system
files and systemd; the service itself runs as --service-user.
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

as_root() {
    if [[ ${EUID} -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

as_user() {
    local user="$1"
    shift

    if [[ "$(id -un)" == "$user" ]]; then
        "$@"
    elif [[ ${EUID} -eq 0 ]]; then
        need runuser
        runuser -u "$user" -- "$@"
    else
        sudo -u "$user" -- "$@"
    fi
}

generate_token() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
        return
    fi

    if command -v od >/dev/null 2>&1; then
        od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
        return
    fi

    fail "cannot generate a token: install openssl or coreutils (od)"
}

env_quote() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    printf '"%s"' "$value"
}

unit_quote() {
    env_quote "$1"
}

while (($#)); do
    case "$1" in
        --workspace)
            [[ $# -ge 2 ]] || fail "--workspace requires a value"
            WORKSPACE="$2"
            shift 2
            ;;
        --service-user)
            [[ $# -ge 2 ]] || fail "--service-user requires a value"
            SERVICE_USER="$2"
            shift 2
            ;;
        --bind)
            [[ $# -ge 2 ]] || fail "--bind requires a value"
            BIND="$2"
            shift 2
            ;;
        --no-start)
            START_SERVICE=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

[[ -n "$WORKSPACE" ]] || fail "--workspace is required"
[[ -n "$SERVICE_USER" ]] || fail "could not determine service user; pass --service-user USER"
[[ "$SERVICE_USER" != "root" ]] || fail "refusing to run the bridge service as root"

need awk
need getent
need id
need install
need realpath
need systemctl
if [[ ${EUID} -ne 0 ]]; then
    need sudo
fi
[[ -d /run/systemd/system ]] || fail "systemd is not running on this machine"

id "$SERVICE_USER" >/dev/null 2>&1 || fail "Linux user does not exist: $SERVICE_USER"

WORKSPACE="$(realpath -e -- "$WORKSPACE")"
[[ -d "$WORKSPACE" ]] || fail "workspace is not a directory: $WORKSPACE"

case "$WORKSPACE" in
    *$'\n'*|*$'\r'*) fail "workspace path cannot contain newlines" ;;
esac

SERVICE_HOME="$(getent passwd "$SERVICE_USER" | awk -F: '{print $6}')"
SERVICE_GROUP="$(id -gn "$SERVICE_USER")"
[[ -n "$SERVICE_HOME" ]] || fail "could not determine home directory for $SERVICE_USER"
[[ -d "$SERVICE_HOME" ]] || fail "service user home directory does not exist: $SERVICE_HOME"

if ! as_user "$SERVICE_USER" test -r "$WORKSPACE"; then
    fail "$SERVICE_USER cannot read workspace: $WORKSPACE"
fi
if ! as_user "$SERVICE_USER" test -w "$WORKSPACE"; then
    fail "$SERVICE_USER cannot write workspace: $WORKSPACE"
fi
if ! as_user "$SERVICE_USER" test -x "$WORKSPACE"; then
    fail "$SERVICE_USER cannot enter workspace: $WORKSPACE"
fi

# Reinstall/upgrade should not silently invalidate the token already configured
# in the GPT. Custom tokens are deliberately restricted to a shell-safe,
# header-safe alphabet so the systemd EnvironmentFile stays simple.
if [[ -z "$TOKEN" ]] && as_root test -f "$CONFIG_FILE"; then
    EXISTING_TOKEN="$(as_root awk -F'"' '/^CHATGPT_BRIDGE_TOKEN="[A-Za-z0-9._~-]+"$/ { print $2; exit }' "$CONFIG_FILE")"
    if [[ ${#EXISTING_TOKEN} -ge 32 ]]; then
        TOKEN="$EXISTING_TOKEN"
        TOKEN_WAS_REUSED=1
    fi
fi

if [[ -z "$TOKEN" ]]; then
    TOKEN="$(generate_token)"
    TOKEN_WAS_GENERATED=1
fi
[[ ${#TOKEN} -ge 32 ]] || fail "CHATGPT_BRIDGE_TOKEN must contain at least 32 characters"
[[ "$TOKEN" =~ ^[A-Za-z0-9._~-]+$ ]] || fail "CHATGPT_BRIDGE_TOKEN may contain only A-Z, a-z, 0-9, '.', '_', '~', and '-'"

[[ -f "${SCRIPT_DIR}/Cargo.toml" ]] || fail "run install.sh from the ChatGPT Bridge source checkout"

BUILD_USER="$(id -un)"
if [[ ${EUID} -eq 0 ]]; then
    if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
        BUILD_USER="$SUDO_USER"
    else
        BUILD_USER="$SERVICE_USER"
    fi
fi
BUILD_HOME="$(getent passwd "$BUILD_USER" | awk -F: '{print $6}')"
BUILD_PATH="${BUILD_HOME}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

CARGO_BIN="$(as_user "$BUILD_USER" env HOME="$BUILD_HOME" PATH="$BUILD_PATH" sh -c 'command -v cargo' || true)"
[[ -n "$CARGO_BIN" ]] || fail "cargo was not found for $BUILD_USER; install the Rust toolchain first"

printf 'Building %s as %s...\n' "$APP_NAME" "$BUILD_USER"
as_user "$BUILD_USER" env HOME="$BUILD_HOME" PATH="$BUILD_PATH" \
    "$CARGO_BIN" build --release --manifest-path "${SCRIPT_DIR}/Cargo.toml"

BUILT_BINARY="${SCRIPT_DIR}/target/release/chatgpt-bridge"
[[ -x "$BUILT_BINARY" ]] || fail "release binary was not produced: $BUILT_BINARY"

TMP_CONFIG="$(mktemp)"
TMP_SERVICE="$(mktemp)"
cleanup() {
    rm -f -- "$TMP_CONFIG" "$TMP_SERVICE"
}
trap cleanup EXIT
chmod 600 "$TMP_CONFIG" "$TMP_SERVICE"

cat >"$TMP_CONFIG" <<EOF
CHATGPT_BRIDGE_TOKEN=$(env_quote "$TOKEN")
CHATGPT_BRIDGE_ROOT=$(env_quote "$WORKSPACE")
CHATGPT_BRIDGE_BIND=$(env_quote "$BIND")
CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS=30000
CHATGPT_BRIDGE_MAX_TIMEOUT_MS=300000
CHATGPT_BRIDGE_MAX_OUTPUT_BYTES=1048576
CHATGPT_BRIDGE_MAX_FILE_BYTES=1048576
RUST_LOG=chatgpt_bridge=info
EOF

SERVICE_PATH_ENV="${SERVICE_HOME}/.local/bin:${SERVICE_HOME}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

cat >"$TMP_SERVICE" <<EOF
[Unit]
Description=ChatGPT Bridge
Documentation=https://github.com/matrixdurden/chatgpt-bridge
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_GROUP}
EnvironmentFile=${CONFIG_FILE}
Environment=$(unit_quote "HOME=${SERVICE_HOME}")
Environment=$(unit_quote "PATH=${SERVICE_PATH_ENV}")
WorkingDirectory=$(unit_quote "$WORKSPACE")
ExecStart=${BINARY_PATH} serve
Restart=on-failure
RestartSec=2s
TimeoutStopSec=15s
KillMode=mixed
UMask=0077

# Conservative hardening that keeps normal compilers, package managers, Git,
# SSH, and other development tooling usable for the selected service user.
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
ProtectClock=true
RestrictSUIDSGID=true
LockPersonality=true
RestrictRealtime=true
ReadWritePaths=$(unit_quote "$WORKSPACE") $(unit_quote "$SERVICE_HOME")

[Install]
WantedBy=multi-user.target
EOF

printf 'Installing system files...\n'
as_root install -m 0755 "$BUILT_BINARY" "$BINARY_PATH"
as_root install -d -m 0755 "$CONFIG_DIR"
as_root install -m 0600 "$TMP_CONFIG" "$CONFIG_FILE"
as_root install -m 0644 "$TMP_SERVICE" "$SERVICE_FILE"
# Remove the standalone uninstaller left by versions before the single CLI.
as_root rm -f -- "$LEGACY_UNINSTALL_PATH"

as_root systemctl daemon-reload
as_root systemctl enable chatgpt-bridge.service >/dev/null

if [[ $START_SERVICE -eq 1 ]]; then
    as_root systemctl restart chatgpt-bridge.service
    if ! as_root systemctl is-active --quiet chatgpt-bridge.service; then
        as_root systemctl --no-pager --full status chatgpt-bridge.service || true
        fail "service failed to start; inspect: chatgpt-bridge logs"
    fi
fi

cat <<EOF

ChatGPT Bridge installed successfully.

  Binary:       ${BINARY_PATH}
  Config:       ${CONFIG_FILE}
  Service:      chatgpt-bridge.service
  Service user: ${SERVICE_USER}
  Workspace:    ${WORKSPACE}
  Bind:         ${BIND}

Bearer token:
  ${TOKEN}

Useful commands:
  chatgpt-bridge status
  chatgpt-bridge start
  chatgpt-bridge stop
  chatgpt-bridge restart
  chatgpt-bridge logs
  chatgpt-bridge logs -f
  chatgpt-bridge uninstall
  curl http://${BIND}/health

Keep the token private. The Rust service is intentionally bound to localhost by
default; put it behind HTTPS before connecting it to a Custom GPT.
EOF

if [[ $TOKEN_WAS_GENERATED -eq 1 ]]; then
    printf '\nA new authentication token was generated. Save it now; ChatGPT will need it.\n'
elif [[ $TOKEN_WAS_REUSED -eq 1 ]]; then
    printf '\nThe existing authentication token was preserved.\n'
fi
