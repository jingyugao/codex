#!/usr/bin/env bash
# Clone (or update) the Codex fork and build its binaries on a fresh Linux or macOS machine.
#
#   curl -fsSL https://raw.githubusercontent.com/jingyugao/codex/feat/fork-v1/scripts/fork/build.sh \
#     | bash -s -- --install-deps
#
# From an extracted source tarball (no git needed):
#
#   tar -xzf codex-fork-<sha>.tar.gz && codex-fork/scripts/fork/build.sh --source codex-fork
#
# Parallelism is derived from available memory: rustc on codex-core needs several GB per job,
# and using every core on a small-memory machine exhausts RAM.
set -euo pipefail

REPO_URL="https://github.com/jingyugao/codex.git"
REF="feat/fork-v1"
DIR="$PWD/codex-fork"
PROFILE="release"
JOBS=""
BINS="codex codex-exec-server codex-code-mode-host"
PREFIX=""
INSTALL_DEPS=0
NO_BUILD=0
SOURCE=""
GB_PER_JOB=3

usage() {
  cat <<EOF
Usage: build.sh [options]

  --repo URL          Git repository (default: $REPO_URL)
  --ref REF           Branch, tag or commit to build (default: $REF)
  --dir DIR           Checkout directory (default: ./codex-fork)
  --source DIR        Build an existing source tree (e.g. an extracted tarball); skips git
  --profile NAME      release or debug (default: $PROFILE)
  --jobs N            Parallel build jobs (default: available GB / $GB_PER_JOB, at least 1)
  --without-code-mode Skip codex-code-mode-host (Code Mode; needs a prebuilt V8 download)
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
    --source) SOURCE="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    --jobs) JOBS="$2"; shift 2 ;;
    --with-code-mode) shift ;;  # default; kept for compatibility
    --without-code-mode) BINS="${BINS/ codex-code-mode-host/}"; shift ;;
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
  local tools=(curl cc pkg-config)
  [[ -n "$SOURCE" ]] || tools+=(git)
  for tool in "${tools[@]}"; do
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

# codex-code-mode-host links V8. Cargo would otherwise fetch an incompatible upstream build, so
# use the Codex-built archive pinned by Cargo.lock, verified against its published checksums.
fetch_v8() {
  [[ " $BINS " == *" codex-code-mode-host "* ]] || return 0
  if [[ -n "${RUSTY_V8_ARCHIVE:-}" && -n "${RUSTY_V8_SRC_BINDING_PATH:-}" ]]; then
    return 0
  fi
  local version target profile=ptrcomp_sandbox_release
  version="$(awk '/^name = "v8"$/ { getline; gsub(/version = |"/, ""); print; exit }' \
    "$DIR/codex-rs/Cargo.lock")"
  [[ -n "$version" ]] || die "cannot find the v8 version in Cargo.lock"
  case "$OS-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *) die "no prebuilt V8 for $OS $(uname -m); use --without-code-mode" ;;
  esac
  local url="https://github.com/openai/codex/releases/download/rusty-v8-v$version"
  local cache="${XDG_CACHE_HOME:-$HOME/.cache}/codex-v8/rusty-v8-$version-$target"
  local sums="rusty_v8_${profile}_$target.sha256"
  local archive="librusty_v8_${profile}_$target.a.gz"
  local binding="src_binding_${profile}_$target.rs"
  mkdir -p "$cache"
  for file in "$sums" "$archive" "$binding"; do
    [[ -s "$cache/$file" ]] || curl -fsSL -o "$cache/$file" "$url/$file" \
      || die "download $url/$file failed (or use --without-code-mode)"
  done
  if command -v sha256sum >/dev/null; then
    (cd "$cache" && sha256sum -c --quiet "$sums") || die "V8 checksum mismatch in $cache"
  else
    (cd "$cache" && shasum -a 256 -c --quiet "$sums") || die "V8 checksum mismatch in $cache"
  fi
  export RUSTY_V8_ARCHIVE="$cache/$archive" RUSTY_V8_SRC_BINDING_PATH="$cache/$binding"
  log "V8 $version ($target) from $cache"
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

  fetch_v8
  cd "$DIR/codex-rs"
  # rustup installs the toolchain pinned in rust-toolchain.toml on first use.
  log "toolchain: $(rustup show active-toolchain 2>/dev/null || cargo --version)"
  log "building [$BINS] ($PROFILE, $jobs jobs, $(available_gb) GB available)"
  local commit
  commit="$(git rev-parse HEAD 2>/dev/null || cat "$DIR/FORK_COMMIT" 2>/dev/null || echo unknown)"
  STABLE_GIT_COMMIT="$commit" cargo build "${args[@]}"

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
if [[ -n "$SOURCE" ]]; then
  DIR="$(cd "$SOURCE" && pwd)"
  [[ -f "$DIR/codex-rs/Cargo.toml" ]] || die "$DIR is not a codex source tree"
  log "source: $DIR ($(cat "$DIR/FORK_COMMIT" 2>/dev/null || echo 'commit unknown'))"
else
  checkout
fi
if [[ $NO_BUILD -eq 1 ]]; then
  log "--no-build: checkout ready at $DIR"
  exit 0
fi
build
