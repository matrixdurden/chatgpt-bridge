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
SERVICE_USER="${SUDO_USER:-${USER:-}}"
TOKEN="${CHATGPT_BRIDGE_TOKEN:-}"
TOKEN_WAS_GENERATED=0
TOKEN_WAS_REUSED=0
EXISTING_ROOT_LINE=""
EXISTING_BIND_LINE=""
EXISTING_TLS_CERT_LINE=""
EXISTING_TLS_KEY_LINE=""
EXISTING_NGROK_ENABLED_LINE=""
EXISTING_NGROK_TOKEN_LINE=""
WAS_ACTIVE=0

usage() {
    cat <<'EOF'
ChatGPT Bridge Linux installer

Usage:
  ./install.sh [options]

Options:
  --service-user USER    Linux user that will run the bridge.
                         Default: the invoking non-root user.
  -h, --help             Show this help.

The installer only installs ChatGPT Bridge. Runtime settings belong to the CLI:

  chatgpt-bridge start --workspace "/projects"
  chatgpt-bridge start --workspace "/projects" --port 8787 --public

Automatic public mode uses the embedded ngrok SDK. No ngrok binary, Nginx,
router forwarding, or TLS certificate setup is required. The first public start
asks for an ngrok authtoken once.
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

while (($#)); do
    case "$1" in
        --service-user)
            [[ $# -ge 2 ]] || fail "--service-user requires a value"
            SERVICE_USER="$2"
            shift 2
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

[[ -n "$SERVICE_USER" ]] || fail "could not determine service user; pass --service-user USER"
[[ "$SERVICE_USER" != "root" ]] || fail "refusing to run the bridge service as root"

need awk
need getent
need id
need install
need systemctl
if [[ ${EUID} -ne 0 ]]; then
    need sudo
fi
[[ -d /run/systemd/system ]] || fail "systemd is not running on this machine"

id "$SERVICE_USER" >/dev/null 2>&1 || fail "Linux user does not exist: $SERVICE_USER"

SERVICE_HOME="$(getent passwd "$SERVICE_USER" | awk -F: '{print $6}')"
SERVICE_GROUP="$(id -gn "$SERVICE_USER")"
[[ -n "$SERVICE_HOME" ]] || fail "could not determine home directory for $SERVICE_USER"
[[ -d "$SERVICE_HOME" ]] || fail "service user home directory does not exist: $SERVICE_HOME"

if as_root systemctl is-active --quiet chatgpt-bridge.service 2>/dev/null; then
    WAS_ACTIVE=1
fi

if as_root test -f "$CONFIG_FILE"; then
    EXISTING_ROOT_LINE="$(as_root awk '/^CHATGPT_BRIDGE_ROOT=/ { print; exit }' "$CONFIG_FILE" || true)"
    EXISTING_BIND_LINE="$(as_root awk '/^CHATGPT_BRIDGE_BIND=/ { print; exit }' "$CONFIG_FILE" || true)"
    EXISTING_TLS_CERT_LINE="$(as_root awk '/^CHATGPT_BRIDGE_TLS_CERT=/ { print; exit }' "$CONFIG_FILE" || true)"
    EXISTING_TLS_KEY_LINE="$(as_root awk '/^CHATGPT_BRIDGE_TLS_KEY=/ { print; exit }' "$CONFIG_FILE" || true)"
    EXISTING_NGROK_ENABLED_LINE="$(as_root awk '/^CHATGPT_BRIDGE_NGROK_ENABLED=/ { print; exit }' "$CONFIG_FILE" || true)"
    EXISTING_NGROK_TOKEN_LINE="$(as_root awk '/^NGROK_AUTHTOKEN=/ { print; exit }' "$CONFIG_FILE" || true)"
fi

if [[ -z "$TOKEN" ]] && as_root test -f "$CONFIG_FILE"; then
    EXISTING_TOKEN="$(as_root awk -F'\"' '/^CHATGPT_BRIDGE_TOKEN=\"[A-Za-z0-9._~-]+\"$/ { print $2; exit }' "$CONFIG_FILE")"
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

printf 'Building %s...\n' "$APP_NAME"
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
CHATGPT_BRIDGE_SERVICE_USER=$(env_quote "$SERVICE_USER")
CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS=30000
CHATGPT_BRIDGE_MAX_TIMEOUT_MS=300000
CHATGPT_BRIDGE_MAX_OUTPUT_BYTES=1048576
CHATGPT_BRIDGE_MAX_FILE_BYTES=1048576
RUST_LOG=chatgpt_bridge=info
EOF

if [[ -n "$EXISTING_BIND_LINE" ]]; then
    printf '%s\n' "$EXISTING_BIND_LINE" >>"$TMP_CONFIG"
else
    printf 'CHATGPT_BRIDGE_BIND=%s\n' "$(env_quote "$DEFAULT_BIND")" >>"$TMP_CONFIG"
fi

if [[ -n "$EXISTING_ROOT_LINE" ]]; then
    printf '%s\n' "$EXISTING_ROOT_LINE" >>"$TMP_CONFIG"
fi

if [[ -n "$EXISTING_TLS_CERT_LINE" ]]; then
    printf '%s\n' "$EXISTING_TLS_CERT_LINE" >>"$TMP_CONFIG"
else
    printf 'CHATGPT_BRIDGE_TLS_CERT=""\n' >>"$TMP_CONFIG"
fi

if [[ -n "$EXISTING_TLS_KEY_LINE" ]]; then
    printf '%s\n' "$EXISTING_TLS_KEY_LINE" >>"$TMP_CONFIG"
else
    printf 'CHATGPT_BRIDGE_TLS_KEY=""\n' >>"$TMP_CONFIG"
fi

if [[ -n "$EXISTING_NGROK_ENABLED_LINE" ]]; then
    printf '%s\n' "$EXISTING_NGROK_ENABLED_LINE" >>"$TMP_CONFIG"
else
    printf 'CHATGPT_BRIDGE_NGROK_ENABLED="false"\n' >>"$TMP_CONFIG"
fi

if [[ -n "$EXISTING_NGROK_TOKEN_LINE" ]]; then
    printf '%s\n' "$EXISTING_NGROK_TOKEN_LINE" >>"$TMP_CONFIG"
else
    printf 'NGROK_AUTHTOKEN=""\n' >>"$TMP_CONFIG"
fi

SERVICE_PATH_ENV="${SERVICE_HOME}/.local/bin:${SERVICE_HOME}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

cat >"$TMP_SERVICE" <<EOF
[Unit]
Description=ChatGPT Bridge
Documentation=https://github.com/matrixdurden/chatgpt-bridge
After=network.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_GROUP}
EnvironmentFile=${CONFIG_FILE}
Environment=$(env_quote "HOME=${SERVICE_HOME}")
Environment=$(env_quote "PATH=${SERVICE_PATH_ENV}")
ExecStart=${BINARY_PATH} serve
Restart=on-failure
RestartSec=2s
TimeoutStopSec=15s
KillMode=mixed
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
RuntimeDirectory=chatgpt-bridge
RuntimeDirectoryMode=0700

[Install]
WantedBy=multi-user.target
EOF

printf 'Installing system files...\n'
as_root install -m 0755 "$BUILT_BINARY" "$BINARY_PATH"
as_root install -d -m 0755 "$CONFIG_DIR"
as_root install -m 0600 "$TMP_CONFIG" "$CONFIG_FILE"
as_root install -m 0644 "$TMP_SERVICE" "$SERVICE_FILE"
as_root rm -f -- "$LEGACY_UNINSTALL_PATH"

if command -v systemd-analyze >/dev/null 2>&1; then
    if ! as_root systemd-analyze verify "$SERVICE_FILE"; then
        fail "systemd rejected ${SERVICE_FILE}"
    fi
fi

as_root systemctl daemon-reload

if [[ $WAS_ACTIVE -eq 1 && -n "$EXISTING_ROOT_LINE" ]]; then
    as_root systemctl restart chatgpt-bridge.service
fi

cat <<EOF

ChatGPT Bridge installed.

Local start:
  chatgpt-bridge start --workspace "/projects"

Easy public start:
  chatgpt-bridge start --workspace "/projects" --port 8787 --public

Useful commands:
  chatgpt-bridge status
  chatgpt-bridge logs
  chatgpt-bridge key
  chatgpt-bridge key rotate
  chatgpt-bridge uninstall

Bearer key:
  ${TOKEN}
EOF

if [[ $TOKEN_WAS_GENERATED -eq 1 ]]; then
    printf '\nA new authentication key was generated. Save it; ChatGPT will need it.\n'
elif [[ $TOKEN_WAS_REUSED -eq 1 ]]; then
    printf '\nThe existing authentication key and runtime settings were preserved.\n'
fi

printf '\nOn the first `--public` start, the bridge will open/show the ngrok authtoken page and ask for the token once.\n'
