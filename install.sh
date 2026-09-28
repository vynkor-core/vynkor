#!/usr/bin/env bash
#
# install.sh — install or update prebuilt `vyn` (kernel) + `vynm` (plugin
# manager) from GitHub Releases, user-locally.
#
# Usage:
#   curl -fsSL https://github.com/vynkor-core/vynkor/releases/latest/download/install.sh | bash
#   ./install.sh --version v0.1.3 --no-vynm --dry-run
#
# Nothing touches the system: binaries land in ~/.local/bin, config in
# ~/.config/vyn. No sudo, no self-update — re-running the script IS the update.
# Every archive is checked against the release's SHA256SUMS before install.
#
# Needs: bash, curl or wget, tar, sha256sum (or shasum). Linux x86_64/aarch64.
# Other platforms: build from source (see README "Build from source").

# the whole script lives in functions and main runs on the last line, so a
# truncated `curl | bash` download executes nothing
set -euo pipefail

KERNEL_REPO="vynkor-core/vynkor"
MANAGER_REPO="vynkor-core/vynkor-manager"

BIN_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.config/vyn"
CONFIG_FILE="${CONFIG_DIR}/config.yaml"
# kernel default (config.rs default_port) — written explicitly so the
# operator sees where the gateway listens
PORT="8000"
VERSION="latest"
MANAGER_VERSION="latest"
WITH_VYNM=1
NO_CONFIG=0
DRY_RUN=0

if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    C_BOLD=$'\033[1m' C_DIM=$'\033[2m' C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m' C_RED=$'\033[31m' C_RESET=$'\033[0m'
else
    C_BOLD="" C_DIM="" C_GREEN="" C_YELLOW="" C_RED="" C_RESET=""
fi

step() { printf '%s==>%s %s%s%s\n' "${C_GREEN}" "${C_RESET}" "${C_BOLD}" "$*" "${C_RESET}"; }
info() { printf '    %s\n' "$*"; }
warn() { printf '%s==>%s %s\n' "${C_YELLOW}" "${C_RESET}" "$*" >&2; }
die() {
    printf '%s==> error:%s %s\n' "${C_RED}" "${C_RESET}" "$*" >&2
    exit 1
}

usage() {
    cat <<EOF
vynkor installer — prebuilt vyn + vynm from GitHub Releases

Usage:
  curl -fsSL https://github.com/${KERNEL_REPO}/releases/latest/download/install.sh | bash
  ./install.sh [options]

Options:
  --version TAG          Kernel release tag, e.g. v0.1.3   (default: latest)
  --manager-version TAG  vynm release tag, e.g. v0.1.0     (default: latest)
  --no-vynm              Install only the kernel (vyn, vyn-pair)
  --bin-dir DIR          Install binaries into DIR         (default: ~/.local/bin)
  --port N               Gateway port for a NEW config     (default: 8000)
  --no-config            Do not create/extend ~/.config/vyn/config.yaml
  --dry-run              Print planned actions, change nothing
  -h, --help             Show this help

Re-run any time to update. An existing config is never overwritten; missing
kernel keys (port, jwt_secret) are appended once and left alone afterwards.
EOF
}

run() {
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        info "${C_DIM}would run:${C_RESET} $*"
        return 0
    fi
    "$@"
}

need() { command -v "$1" >/dev/null 2>&1; }

# musl builds are fully static, so one archive covers every glibc/musl distro
detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    [[ "${os}" == "Linux" ]] || die "prebuilt binaries are Linux-only (got ${os}); build from source instead:
      cargo install --git https://github.com/${KERNEL_REPO} vynkor"
    case "${arch}" in
    x86_64 | amd64) echo "x86_64-unknown-linux-musl" ;;
    aarch64 | arm64) echo "aarch64-unknown-linux-musl" ;;
    *) die "no prebuilt binary for ${arch}; build from source (see README)" ;;
    esac
}

download() {
    local url="$1" out="$2"
    if need curl; then
        curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "${out}" "${url}"
    elif need wget; then
        wget -q --https-only -O "${out}" "${url}"
    else
        die "need curl or wget"
    fi
}

sha256_of() {
    if need sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif need shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "need sha256sum or shasum to verify downloads"
    fi
}

# /releases/latest/download/<asset> follows GitHub's redirect to the newest
# non-prerelease — no API call, so no unauthenticated rate limit to hit
release_url() {
    local repo="$1" tag="$2" asset="$3"
    if [[ "${tag}" == "latest" ]]; then
        echo "https://github.com/${repo}/releases/latest/download/${asset}"
    else
        echo "https://github.com/${repo}/releases/download/${tag}/${asset}"
    fi
}

# $1 repo, $2 tag, $3 archive prefix (vyn|vynm), $4 target, $5 workdir;
# extracts into $5/$3 and echoes nothing
fetch_verified() {
    local repo="$1" tag="$2" name="$3" target="$4" work="$5"
    local asset="${name}-${target}.tar.gz"

    step "${name}: downloading ${asset} (${tag})"
    if [[ "${DRY_RUN}" -eq 1 ]]; then
        info "${C_DIM}would fetch:${C_RESET} $(release_url "${repo}" "${tag}" "${asset}")"
        info "${C_DIM}would verify against:${C_RESET} $(release_url "${repo}" "${tag}" SHA256SUMS)"
        return 0
    fi
    download "$(release_url "${repo}" "${tag}" "${asset}")" "${work}/${asset}" ||
        die "download failed — does release ${tag} of ${repo} ship ${asset}?"
    download "$(release_url "${repo}" "${tag}" SHA256SUMS)" "${work}/${name}.SHA256SUMS" ||
        die "release ${tag} of ${repo} has no SHA256SUMS — refusing to install unverified"

    local want got
    want="$(awk -v a="${asset}" '$2 == a || $2 == "*"a { print $1 }' "${work}/${name}.SHA256SUMS")"
    [[ -n "${want}" ]] || die "${asset} is not listed in SHA256SUMS"
    got="$(sha256_of "${work}/${asset}")"
    [[ "${want}" == "${got}" ]] || die "checksum mismatch for ${asset}
      expected ${want}
      got      ${got}"
    info "sha256 ok ${got}"

    mkdir -p "${work}/${name}"
    tar -xzf "${work}/${asset}" -C "${work}/${name}"
}

install_bins() {
    local src="$1"
    shift
    local bin
    for bin in "$@"; do
        [[ "${DRY_RUN}" -eq 1 ]] || [[ -f "${src}/${bin}" ]] || die "archive is missing ${bin}"
        # install(1) writes a new inode, so a running `vyn` keeps its old binary
        run install -m 0755 "${src}/${bin}" "${BIN_DIR}/${bin}"
        info "installed ${BIN_DIR}/${bin}"
    done
}

gen_secret() {
    # 48 random bytes → 64 base64 chars, well above the kernel's 32-byte floor
    if need openssl; then
        openssl rand -base64 48 | tr -d '\n'
    else
        head -c 48 /dev/urandom | base64 | tr -d '\n'
    fi
}

setup_config() {
    if [[ "${NO_CONFIG}" -eq 1 ]]; then
        step "config: skipped (--no-config)"
        return 0
    fi
    run mkdir -p "${CONFIG_DIR}"

    # vynm owns the registries template; let it seed the shared file first so
    # the official registry + pinned key are in place before kernel keys land
    if [[ ! -e "${CONFIG_FILE}" ]] && [[ "${WITH_VYNM}" -eq 1 ]]; then
        step "config: seeding ${CONFIG_FILE} (vynm init)"
        run "${BIN_DIR}/vynm" --config "${CONFIG_FILE}" init >/dev/null
    fi

    if [[ "${DRY_RUN}" -eq 1 ]]; then
        step "config: would ensure port/jwt_secret in ${CONFIG_FILE}"
        return 0
    fi
    touch "${CONFIG_FILE}"
    chmod 600 "${CONFIG_FILE}"

    # only top-level keys count; the vynm template mentions them in comments
    local added=0
    if ! grep -qE '^port:' "${CONFIG_FILE}"; then
        printf '\nport: %s\n' "${PORT}" >>"${CONFIG_FILE}"
        added=1
    fi
    if ! grep -qE '^jwt_secret:' "${CONFIG_FILE}"; then
        printf 'jwt_secret: "%s"\n' "$(gen_secret)" >>"${CONFIG_FILE}"
        added=1
    fi
    if [[ "${added}" -eq 1 ]]; then
        step "config: added kernel keys to ${CONFIG_FILE}"
    else
        step "config: ${CONFIG_FILE} already complete — left untouched"
    fi
}

path_hint() {
    case ":${PATH}:" in
    *":${BIN_DIR}:"*) return 0 ;;
    esac
    warn "${BIN_DIR} is not on your PATH. Add it:"
    info "echo 'export PATH=\"${BIN_DIR}:\$PATH\"' >> ~/.bashrc   # or ~/.zshrc"
}

main() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
        --version) VERSION="${2:?--version needs a tag}"; shift 2 ;;
        --manager-version) MANAGER_VERSION="${2:?--manager-version needs a tag}"; shift 2 ;;
        --no-vynm) WITH_VYNM=0; shift ;;
        --bin-dir) BIN_DIR="${2:?--bin-dir needs a path}"; shift 2 ;;
        --port) PORT="${2:?--port needs a number}"; shift 2 ;;
        --no-config) NO_CONFIG=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
        esac
    done
    if [[ ! "${PORT}" =~ ^[0-9]+$ ]] || ((PORT < 1 || PORT > 65535)); then
        die "invalid --port: ${PORT}"
    fi
    for tag in "${VERSION}" "${MANAGER_VERSION}"; do
        [[ "${tag}" == "latest" || "${tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] ||
            die "invalid release tag: ${tag} (expected vX.Y.Z)"
    done
    need tar || die "need tar"

    local target work
    target="$(detect_target)"
    step "vynkor installer — target ${target}"

    work="$(mktemp -d)"
    # shellcheck disable=SC2064 # expand now: work is local to main
    trap "rm -rf '${work}'" EXIT

    run mkdir -p "${BIN_DIR}"

    fetch_verified "${KERNEL_REPO}" "${VERSION}" vyn "${target}" "${work}"
    install_bins "${work}/vyn" vyn vyn-pair

    if [[ "${WITH_VYNM}" -eq 1 ]]; then
        fetch_verified "${MANAGER_REPO}" "${MANAGER_VERSION}" vynm "${target}" "${work}"
        install_bins "${work}/vynm" vynm
    fi

    setup_config
    path_hint

    step "done"
    echo
    info "Next:"
    info "  vyn start --config ${CONFIG_FILE}"
    info "  vyn status"
    [[ "${WITH_VYNM}" -eq 1 ]] && info "  vynm install ai network database"
    info "  vyn device connect --name my-phone   # pair the Android app"
}

main "$@"
