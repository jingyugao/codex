#!/usr/bin/env bash
# Clone (or update) the Codex fork and build its binaries on a fresh Linux or macOS machine.
#
#   curl -fsSL https://raw.githubusercontent.com/jingyugao/codex/feat/fork-v1/scripts/fork/build.sh \
#     | bash -s -- --install-deps
#
# Parallelism is derived from available memory: rustc on codex-core needs several GB per job,
# and using every core on a small-memory machine exhausts RAM.
set -euo pipefail

REPO_URL="https://github.com/jingyugao/codex.git"
REF="feat/fork-v1"
DIR="$PWD/codex-fork"
PROFILE="release"
JOBS=""
BINS="codex codex-exec-server"
PREFIX=""
INSTALL_DEPS=0
NO_BUILD=0
GB_PER_JOB=3

usage() {
  cat <<EOF
Usage: build.sh [options]

  --repo URL          Git repository (default: $REPO_URL)
  --ref REF           Branch, tag or commit to build (default: $REF)
  --dir DIR           Checkout directory (default: ./codex-fork)
  --profile NAME      release or debug (default: $PROFILE)
  --jobs N            Parallel build jobs (default: available GB / $GB_PER_JOB, at least 1)
  --with-code-mode    Also build codex-code-mode-host (pulls a prebuilt V8 library)
  --prefix DIR        Copy built binaries to DIR/bin
  --install-deps      Install system packages with apt, dnf or brew (uses sudo on Linux)
  --no-build          Prepare dependencies and the checkout only
  -h, --help          Show this help
EOF
}

log() { printf '\033[1;34m==>\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO_URL="$2"; shift 2 ;;
    --ref) REF="$2"; shift 2 ;;
    --dir) DIR="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    --jobs) JOBS="$2"; shift 2 ;;
    --with-code-mode) BINS="$BINS codex-code-mode-host"; shift ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    --install-deps) INSTALL_DEPS=1; shift ;;
    --no-build) NO_BUILD=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done
[[ "$PROFILE" == release || "$PROFILE" == debug ]] || die "--profile must be release or debug"

OS="$(uname -s)"
case "$OS" in
  Linux|Darwin) ;;
  *) die "unsupported OS: $OS (use Linux or macOS; on Windows use WSL)" ;;
esac

install_deps() {
  if [[ "$OS" == Darwin ]]; then
    xcode-select -p >/dev/null 2>&1 || die "install Xcode command line tools: xcode-select --install"
    if command -v brew >/dev/null; then brew install git pkg-config; fi
    return
  fi
  local sudo=""
  [[ "$(id -u)" -eq 0 ]] || sudo="sudo"
  if command -v apt-get >/dev/null; then
    $sudo apt-get update
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential pkg-config libcap-dev git curl ca-certificates
  elif command -v dnf >/dev/null; then
    $sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config libcap-devel git curl
  else
    die "no apt-get or dnf; install a C toolchain, pkg-config, libcap headers, git and curl"
  fi
}

check_deps() {
  local missing=()
  for tool in git curl cc pkg-config; do
    command -v "$tool" >/dev/null || missing+=("$tool")
  done
  if [[ "$OS" == Linux ]] && ! pkg-config --exists libcap 2>/dev/null; then
    missing+=("libcap headers (libcap-dev / libcap-devel)")
  fi
  [[ ${#missing[@]} -eq 0 ]] || die "missing: ${missing[*]}. Re-run with --install-deps."
}

ensure_rustup() {
  if ! command -v rustup >/dev/null; then
    [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
  fi
  if ! command -v rustup >/dev/null; then
    log "installing rustup"
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain none
    . "$HOME/.cargo/env"
  fi
}

available_gb() {
  if [[ "$OS" == Linux ]]; then
    awk '/^MemAvailable:/ { printf "%d", $2 / 1048576 }' /proc/meminfo
  else
    # macOS has no MemAvailable; assume 60% of physical memory is usable.
    echo $(( $(sysctl -n hw.memsize) * 6 / 10 / 1073741824 ))
  fi
}

default_jobs() {
  local cpus mem jobs
  cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)"
  mem="$(available_gb)"
  jobs=$(( mem / GB_PER_JOB ))
  (( jobs < 1 )) && jobs=1
  (( jobs > cpus )) && jobs=$cpus
  echo "$jobs"
}

checkout() {
  if [[ -d "$DIR/.git" ]]; then
    log "updating $DIR"
    git -C "$DIR" fetch --filter=blob:none origin "$REF"
  else
    log "cloning $REPO_URL into $DIR"
    git clone --filter=blob:none --no-checkout "$REPO_URL" "$DIR"
    git -C "$DIR" fetch --filter=blob:none origin "$REF"
  fi
  git -C "$DIR" checkout --detach FETCH_HEAD
  log "source: $(git -C "$DIR" log -1 --format='%h %s')"
}

build() {
  local jobs="${JOBS:-$(default_jobs)}"
  local args=(--locked -j "$jobs")
  local out_dir="target/debug"
  if [[ "$PROFILE" == release ]]; then
    args+=(--release)
    out_dir="target/release"
    # Same trade-offs as the fork CI: no LTO or debug info keeps memory and link time down.
    export CARGO_PROFILE_RELEASE_LTO=false
    export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
    export CARGO_PROFILE_RELEASE_DEBUG=0
  else
    export CARGO_PROFILE_DEV_DEBUG=0
  fi
  for bin in $BINS; do args+=(--bin "$bin"); done

  cd "$DIR/codex-rs"
  # rustup installs the toolchain pinned in rust-toolchain.toml on first use.
  log "toolchain: $(rustup show active-toolchain 2>/dev/null || cargo --version)"
  log "building [$BINS] ($PROFILE, $jobs jobs, $(available_gb) GB available)"
  STABLE_GIT_COMMIT="$(git rev-parse HEAD)" cargo build "${args[@]}"

  local built=()
  for bin in $BINS; do built+=("$DIR/codex-rs/$out_dir/$bin"); done
  if [[ -n "$PREFIX" ]]; then
    mkdir -p "$PREFIX/bin"
    cp "${built[@]}" "$PREFIX/bin/"
    log "installed to $PREFIX/bin (add it to PATH)"
  fi
  log "done:"
  printf '  %s\n' "${built[@]}" >&2
  "${built[0]}" --version
}

[[ $INSTALL_DEPS -eq 1 ]] && install_deps
check_deps
ensure_rustup
checkout
if [[ $NO_BUILD -eq 1 ]]; then
  log "--no-build: checkout ready at $DIR"
  exit 0
fi
build
