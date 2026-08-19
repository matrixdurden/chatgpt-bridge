#!/usr/bin/env bash
set -Eeuo pipefail

BINARY_PATH="/usr/local/bin/chatgpt-bridge"
UNINSTALL_PATH="/usr/local/bin/chatgpt-bridge-uninstall"
CONFIG_DIR="/etc/chatgpt-bridge"
SERVICE_FILE="/etc/systemd/system/chatgpt-bridge.service"

usage() {
    cat <<'EOF'
ChatGPT Bridge Linux uninstaller

Usage:
  ./uninstall.sh
  sudo chatgpt-bridge-uninstall

Removes every system-level file created by install.sh:
  - /usr/local/bin/chatgpt-bridge
  - /usr/local/bin/chatgpt-bridge-uninstall
  - /etc/chatgpt-bridge/
  - /etc/systemd/system/chatgpt-bridge.service
  - systemd enablement for chatgpt-bridge.service

The configured workspace and its project files are never deleted.
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

as_root() {
    if [[ ${EUID} -eq 0 ]]; then
        "$@"
    else
        command -v sudo >/dev/null 2>&1 || fail "sudo is required"
        sudo "$@"
    fi
}

if (($#)); then
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
fi

if command -v systemctl >/dev/null 2>&1; then
    as_root systemctl stop chatgpt-bridge.service >/dev/null 2>&1 || true
    as_root systemctl disable chatgpt-bridge.service >/dev/null 2>&1 || true
fi

as_root rm -f -- "$SERVICE_FILE"
as_root rm -f -- "$BINARY_PATH"
as_root rm -rf -- "$CONFIG_DIR"

if command -v systemctl >/dev/null 2>&1; then
    as_root systemctl daemon-reload
    as_root systemctl reset-failed chatgpt-bridge.service >/dev/null 2>&1 || true
fi

# Remove the installed copy last. If this script itself is the installed copy,
# Unix keeps the already-open script readable until the process exits.
as_root rm -f -- "$UNINSTALL_PATH"

cat <<'EOF'
ChatGPT Bridge was removed.

System service, binary, configuration, token, and installed uninstaller were
deleted. Your workspace and source repositories were intentionally left intact.
EOF
