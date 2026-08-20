#!/usr/bin/env bash
set -Eeuo pipefail

APP_NAME="chatgpt-bridge"
REPOSITORY="matrixdurden/chatgpt-bridge"
RELEASES_URL="https://github.com/${REPOSITORY}/releases"
BINARY_PATH="/usr/local/bin/chatgpt-bridge"
LEGACY_UNINSTALL_PATH="/usr/local/bin/chatgpt-bridge-uninstall"
CONFIG_DIR="/etc/chatgpt-bridge"
CONFIG_FILE="${CONFIG_DIR}/config.env"
SERVICE_FILE="/etc/systemd/system/chatgpt-bridge.service"
DEFAULT_BIND="127.0.0.1:8787"

SERVICE_USER="${SUDO_USER:-${USER:-}}"
TOKEN="${CHATGPT_BRIDGE_TOKEN:-}"
REQUESTED_VERSION=""
FROM_SOURCE=0
SOURCE_DIR="${CHATGPT_BRIDGE_SOURCE_DIR:-$(pwd)}"
TOKEN_WAS_GENERATED=0
TOKEN_WAS_REUSED=0
EXISTING_ROOT_LINE=""
EXISTING_BIND_LINE=""
EXISTING_TLS_CERT_LINE=""
EXISTING_TLS_KEY_LINE=""
EXISTING_NGROK_ENABLED_LINE=""
EXISTING_NGROK_TOKEN_LINE=""
WAS_ACTIVE=0
TMP_CONFIG=""
TMP_SERVICE=""
TMP_DOWNLOAD_DIR=""
BUILT_BINARY=""

usage() {
    cat <<'EOF'
ChatGPT Bridge Linux installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/matrixdurden/chatgpt-bridge/main/install.sh | bash
  ./install.sh [options]

Options:
  --service-user USER    Linux user that will run the bridge.
                         Default: the invoking non-root user.
  --version VERSION      Install a specific release, for example 0.2.0.
  --from-source          Build and install the current source checkout.
  --source-dir PATH      Source checkout used with --from-source.
  -h, --help             Show this help.

Default installation downloads a prebuilt GitHub Release binary, verifies its
SHA-256 checksum, and installs it. Rust, Cargo, Git, Nginx, and ngrok are not
required on the target machine.
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

normalize_tag() {
    local version="$1"
    local tag
    version="${version#v}"
    [[ -n "$version" ]] || fail "version cannot be empty"
    [[ "$version" =~ ^[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || fail "invalid version: $1"
    tag="v${version}"
    printf '%s\n' "$tag"
}

release_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    [[ "$os" == "Linux" ]] || fail "prebuilt installation currently supports Linux only"

    case "$arch" in
        x86_64|amd64)
            printf '%s\n' "x86_64-unknown-linux-gnu"
            ;;
        aarch64|arm64)
            printf '%s\n' "aarch64-unknown-linux-gnu"
            ;;
        *)
            fail "no prebuilt release is available for Linux architecture: $arch"
            ;;
    esac
}

resolve_latest_tag() {
    local effective tag
    effective="$(curl -fsSL --retry 3 --connect-timeout 10 -o /dev/null -w '%{url_effective}' "${RELEASES_URL}/latest")" \
        || fail "could not resolve the latest GitHub release"
    tag="${effective##*/}"
    [[ "$tag" == v* ]] || fail "GitHub did not return a valid latest release tag"
    normalize_tag "$tag"
}

download_release() {
    local target tag asset base archive sums expected actual name

    need curl
    need tar
    need sha256sum
    need uname

    target="$(release_target)"
    if [[ -n "$REQUESTED_VERSION" ]]; then
        tag="$(normalize_tag "$REQUESTED_VERSION")"
    else
        tag="$(resolve_latest_tag)"
    fi

    asset="chatgpt-bridge-${target}.tar.gz"
    base="${RELEASES_URL}/download/${tag}"
    TMP_DOWNLOAD_DIR="$(mktemp -d)"
    archive="${TMP_DOWNLOAD_DIR}/${asset}"
    sums="${TMP_DOWNLOAD_DIR}/SHA256SUMS"

    printf 'Downloading %s %s for %s...\n' "$APP_NAME" "${tag#v}" "$target"
    curl -fL --retry 3 --connect-timeout 10 --output "$archive" "${base}/${asset}" \
        || fail "failed to download release asset: ${asset}"
    curl -fL --retry 3 --connect-timeout 10 --output "$sums" "${base}/SHA256SUMS" \
        || fail "failed to download SHA256SUMS"

    expected="$(awk -v asset="$asset" '
        {
            name=$2
            sub(/^\*/, "", name)
            if (name == asset) { print $1; exit }
        }
    ' "$sums")"
    [[ "$expected" =~ ^[0-9A-Fa-f]{64}$ ]] || fail "SHA256SUMS does not contain a valid digest for ${asset}"

    actual="$(sha256sum "$archive" | awk '{print $1}')"
    [[ "$actual" == "$expected" ]] || fail "SHA-256 verification failed for ${asset}"

    tar -xzf "$archive" -C "$TMP_DOWNLOAD_DIR"
    BUILT_BINARY="${TMP_DOWNLOAD_DIR}/chatgpt-bridge"
    [[ -x "$BUILT_BINARY" ]] || fail "release archive did not contain an executable chatgpt-bridge"

    local reported expected_version
    reported="$($BUILT_BINARY version)" || fail "downloaded binary failed its version check"
    expected_version="${tag#v}"
    [[ "$reported" == "chatgpt-bridge ${expected_version}" ]] \
        || fail "downloaded binary version mismatch: ${reported}"
}

build_from_source() {
    local build_user build_home build_path cargo_bin

    [[ -f "${SOURCE_DIR}/Cargo.toml" ]] || fail "Cargo.toml not found in source directory: ${SOURCE_DIR}"

    build_user="$(id -un)"
    if [[ ${EUID} -eq 0 ]]; then
        if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
            build_user="$SUDO_USER"
        else
            build_user="$SERVICE_USER"
        fi
    fi

    build_home="$(getent passwd "$build_user" | awk -F: '{print $6}')"
    build_path="${build_home}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    cargo_bin="$(as_user "$build_user" env HOME="$build_home" PATH="$build_path" sh -c 'command -v cargo' || true)"
    [[ -n "$cargo_bin" ]] || fail "cargo was not found for $build_user; install Rust or use the default prebuilt installer"

    printf 'Building %s from source...\n' "$APP_NAME"
    as_user "$build_user" env HOME="$build_home" PATH="$build_path" \
        "$cargo_bin" build --release --manifest-path "${SOURCE_DIR}/Cargo.toml"

    BUILT_BINARY="${SOURCE_DIR}/target/release/chatgpt-bridge"
    [[ -x "$BUILT_BINARY" ]] || fail "release binary was not produced: $BUILT_BINARY"
}

cleanup() {
    [[ -z "$TMP_CONFIG" ]] || rm -f -- "$TMP_CONFIG"
    [[ -z "$TMP_SERVICE" ]] || rm -f -- "$TMP_SERVICE"
    [[ -z "$TMP_DOWNLOAD_DIR" ]] || rm -rf -- "$TMP_DOWNLOAD_DIR"
}
trap cleanup EXIT

while (($#)); do
    case "$1" in
        --service-user)
            [[ $# -ge 2 ]] || fail "--service-user requires a value"
            SERVICE_USER="$2"
            shift 2
            ;;
        --version)
            [[ $# -ge 2 ]] || fail "--version requires a value"
            REQUESTED_VERSION="$2"
            shift 2
            ;;
        --from-source)
            FROM_SOURCE=1
            shift
            ;;
        --source-dir)
            [[ $# -ge 2 ]] || fail "--source-dir requires a value"
            SOURCE_DIR="$2"
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
[[ $FROM_SOURCE -eq 1 || -z "$REQUESTED_VERSION" || "$REQUESTED_VERSION" != "" ]] || true

need awk
need getent
need id
need install
need systemctl
need mktemp
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

if [[ $FROM_SOURCE -eq 1 ]]; then
    build_from_source
else
    download_release
fi

TMP_CONFIG="$(mktemp)"
TMP_SERVICE="$(mktemp)"
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

INSTALLED_VERSION="$($BINARY_PATH version 2>/dev/null || true)"

cat <<EOF

ChatGPT Bridge installed${INSTALLED_VERSION:+: ${INSTALLED_VERSION#chatgpt-bridge }}.

Easy public start:
  chatgpt-bridge start --workspace "/projects" --port 8787 --public

Updates:
  chatgpt-bridge update
  chatgpt-bridge update --check

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
