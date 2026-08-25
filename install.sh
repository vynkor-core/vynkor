#!/usr/bin/env bash
#
# install.sh — one-command installer/updater for the vynkor kernel (`vyn`)
# and, optionally, the vynkor plugin manager (`vynm`).
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/vynkor-core/vynkor/develop/install.sh | bash
#   git clone https://github.com/vynkor-core/vynkor && ./install.sh
#   ./install.sh --with-vynm --port 9000 --dry-run
#
# Everything installs user-locally: no sudo, nothing touched outside $HOME.
# Re-running the installer pulls the latest sources (git pull --ff-only) and
# rebuilds, so it doubles as an updater.
#
# Dependencies: Rust toolchain (>= 1.85) + git. Nothing else is required —
# no protoc, no openssl, no sqlite, no pkg-config.

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
KERNEL_REPO="https://github.com/vynkor-core/vynkor.git"
MANAGER_REPO="https://github.com/vynkor-core/vynkor-manager.git"
MSRV="1.85"
MSRV_MAJOR="1"
MSRV_MINOR="85"
MIN_JWT_SECRET_BYTES="32"

BIN_DIR="${HOME}/.local/bin"
SRC_DIR="${HOME}/.local/src/vynkor-core"
PORT="8080"
CONFIG_DIR="${HOME}/.config/vyn"
CONFIG_FILE="${CONFIG_DIR}/config.yaml"
RUN_DIR="${HOME}/.vyn/run"

WITH_VYNM=0
NO_CONFIG=0
FORCE_CONFIG=0
DRY_RUN=0

# ---------------------------------------------------------------------------
# Terminal color helpers (disabled when not a TTY or NO_COLOR is set)
# ---------------------------------------------------------------------------
if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    C_BOLD=$'\033[1m'
    C_DIM=$'\033[2m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_RED=$'\033[31m'
    C_RESET=$'\033[0m'
else
    C_BOLD=""
    C_DIM=""
    C_GREEN=""
    C_YELLOW=""
    C_RED=""
    C_RESET=""
fi

step() { printf '%s==>%s %s%s%s\n' "${C_GREEN}" "${C_RESET}" "${C_BOLD}" "$*" "${C_RESET}"; }
info() { printf '    %s\n' "$*"; }
warn() { printf '%s==>%s %s%s%s\n' "${C_YELLOW}" "${C_RESET}" "$*" "${C_RESET}" >&2; }
err() { printf '%s==> error:%s %s\n' "${C_RED}" "${C_RESET}" "$*" >&2; }
die() {
    err "$*"
    exit 1
}

# ---------------------------------------------------------------------------
# Error trap: report the failing line number, then exit non-zero
# ---------------------------------------------------------------------------
on_err() {
    local lineno="${1:-?}"
    err "failed at line ${lineno}: ${BASH_COMMAND:-}"
    exit 1
}
trap 'on_err "${LINENO}"' ERR

# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------
usage() {
    cat <<EOF
Veyron kernel installer

Installs \`vyn\` (and, with --with-vynm, \`vynm\`) from source, user-locally.
Re-run it later to update in place — existing checkouts are fast-forwarded
and rebuilt, so this script is also the updater.

Usage:
  curl -sSL https://core.veyron.online/install.sh | bash
  ./install.sh [options]

Options:
  --with-vynm          Also build & install vynkor-manager (\`vynm\`).
                       Note: \`vyn plugin ...\` commands delegate to \`vynm\`.
  --bin-dir DIR        Install binaries into DIR        (default: ~/.local/bin)
  --src-dir DIR        Check out sources under DIR      (default: ~/.local/src/vynkor-core)
  --port N             HTTP/WebSocket port in generated config (default: 8080)
  --no-config          Skip config generation entirely
  --force-config       Overwrite an existing config.yaml (default: never overwrite)
  --dry-run            Print planned actions, execute nothing
  -h, --help           Show this help and exit

Notes:
  - Only Rust (>= 1.85) and git are required. Linux or macOS; Linux is
    required for full sandbox isolation features.
  - When run through \`curl ... | bash\` (stdin not a terminal, no args) the
    installer proceeds silently with defaults — it never reads stdin and
    never prompts.
EOF
}

# ---------------------------------------------------------------------------
# Action runner: in --dry-run mode, print instead of execute
# ---------------------------------------------------------------------------
run() {
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        info "${C_DIM}would run:${C_RESET} $*"
        return 0
    fi
    "$@"
}

# ---------------------------------------------------------------------------
# Check the Rust toolchain meets MSRV
# ---------------------------------------------------------------------------
check_rust() {
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        step "check: rustc/cargo >= ${MSRV} (skipped in dry-run)"
        return 0
    fi
    if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
        die "Rust toolchain not found. Install it with:
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
      then run: rustup update stable"
    fi
    local ver major minor
    ver="$(rustc --version | grep -oE '[0-9]+\.[0-9]+' | head -n 1)"
    major="${ver%%.*}"
    minor="${ver#*.}"
    if [[ "${major}" -lt "${MSRV_MAJOR}" ]] ||
        { [[ "${major}" -eq "${MSRV_MAJOR}" ]] && [[ "${minor}" -lt "${MSRV_MINOR}" ]]; }; then
        die "Rust ${ver} detected, but vyn requires Rust >= ${MSRV}.
        Fix: rustup update stable"
    fi
    step "rustc ${ver} (>= ${MSRV}) OK"
}

# ---------------------------------------------------------------------------
# Ensure a source checkout exists and is up to date
#   $1 = display name, $2 = repo URL, $3 = checkout path
# ---------------------------------------------------------------------------
ensure_src() {
    local name="$1" repo="$2" dir="$3"
    if [[ -d "${dir}/.git" ]]; then
        step "${name}: updating existing checkout (git pull --ff-only)"
        run git -C "${dir}" pull --ff-only
    elif [[ -e "${dir}" ]]; then
        die "${dir} exists but is not a git checkout — remove it or choose another --src-dir"
    else
        step "${name}: cloning (git clone --depth 1)"
        run git clone --depth 1 "${repo}" "${dir}"
    fi
}

# ---------------------------------------------------------------------------
# Build a Rust binary from a checkout
#   $1 = display name, $2 = checkout path, $3 = binary name
# ---------------------------------------------------------------------------
build() {
    local name="$1" dir="$2" bin="$3"
    step "${name}: building (cargo build --release)"
    run cargo build --release --manifest-path "${dir}/Cargo.toml"
    step "${name}: installing ${bin} -> ${BIN_DIR}/"
    run install -m 0755 "${dir}/target/release/${bin}" "${BIN_DIR}/"
}

# ---------------------------------------------------------------------------
# Generate a minimal config (unless --no-config)
# ---------------------------------------------------------------------------
gen_config() {
    if [[ -e "${CONFIG_FILE}" ]] && [[ "${FORCE_CONFIG}" -ne 1 ]]; then
        step "config: ${CONFIG_FILE} exists — keeping it (--force-config to overwrite)"
        return 0
    fi

    local secret
    if command -v openssl >/dev/null 2>&1; then
        secret="$(openssl rand -base64 48)"
    else
        secret="$(head -c 48 /dev/urandom | base64)"
    fi

    # 48 raw bytes -> base64 -> 64 chars; decodes to >= 32 bytes (MIN_JWT_SECRET_BYTES).

    if [[ "${DRY_RUN}" -eq 1 ]]; then
        step "config: would write ${CONFIG_FILE}"
        info "    port: ${PORT}"
        info "    jwt_secret: \"<generated ${MIN_JWT_SECRET_BYTES}+ bytes>\""
        return 0
    fi

    run mkdir -p "${CONFIG_DIR}"
    if [[ "${FORCE_CONFIG}" -eq 1 ]]; then
        step "config: writing ${CONFIG_FILE} (--force-config)"
    else
        step "config: writing ${CONFIG_FILE}"
    fi
    printf 'bind: 127.0.0.1\nport: %s\njwt_secret: "%s"\n' "${PORT}" "${secret}" >"${CONFIG_FILE}"
}

# ---------------------------------------------------------------------------
# PATH hint
# ---------------------------------------------------------------------------
path_hint() {
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        info "would check: is ${BIN_DIR} on PATH?"
        return 0
    fi
    case ":${PATH}:" in
    *":${BIN_DIR}:"*) return 0 ;;
    esac
    warn "${BIN_DIR} is not on your PATH."
    info "Add it with one of:"
    info "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc"
    info "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
main() {
    step "vynkor kernel installer (MSRV ${MSRV})"

    check_rust

    run mkdir -p "${BIN_DIR}"
    run mkdir -p "${SRC_DIR}"

    step "source dir: ${SRC_DIR}"

    ensure_src "vynkor" "${KERNEL_REPO}" "${SRC_DIR}/vynkor"
    build "vynkor" "${SRC_DIR}/vynkor" "vyn"

    if [[ "${WITH_VYNM}" -eq 1 ]]; then
        ensure_src "vynkor-manager" "${MANAGER_REPO}" "${SRC_DIR}/vynkor-manager"
        build "vynkor-manager" "${SRC_DIR}/vynkor-manager" "vynm"
    fi

    path_hint

    if [[ "${NO_CONFIG}" -eq 1 ]]; then
        step "config: skipped (--no-config)"
    else
        gen_config
    fi

    # Kernel does not create socket parent dirs when custom paths are used.
    run mkdir -p "${RUN_DIR}"

    step "done"
    echo
    info "Next steps:"
    info "  vyn start --config ${CONFIG_FILE}"
    info "  vyn status"
    info "  vyn logs"
    echo
    info "Sources live in ${SRC_DIR} — re-run this installer to update in place."
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
    --with-vynm)
        WITH_VYNM=1
        shift
        ;;
    --bin-dir)
        BIN_DIR="${2:?--bin-dir requires a value}"
        shift 2
        ;;
    --src-dir)
        SRC_DIR="${2:?--src-dir requires a value}"
        shift 2
        ;;
    --port)
        PORT="${2:?--port requires a value}"
        shift 2
        ;;
    --no-config)
        NO_CONFIG=1
        shift
        ;;
    --force-config)
        FORCE_CONFIG=1
        shift
        ;;
    --dry-run)
        DRY_RUN=1
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *) die "unknown option: $1 (run with --help)" ;;
    esac
done

if [[ ! "${PORT}" =~ ^[0-9]+$ ]]; then
    die "invalid --port value: ${PORT} (must be a number)"
fi

main
