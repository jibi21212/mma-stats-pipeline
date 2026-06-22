#!/usr/bin/env bash
# setup.sh — one-shot, idempotent setup for the MMA Stats Pipeline.
#
# Builds the whole polyglot app FROM SOURCE in place:
#   1. verify toolchains  (python3, cargo, go)
#   2. Python venv + ML deps   (.venv + ml/requirements.txt)
#   3. native binaries          (make build: Rust TUI release + Go scraper)
#
# Run it directly (`scripts/setup.sh`) or via the self-bootstrapping `./mma`
# launcher. It resolves the repo root from its OWN location, so it works no
# matter the current directory. Re-running is safe and fast: an existing venv is
# reused and the build tools rebuild only what changed.
#
# Flags:
#   --install-deps   On macOS, `brew install` any missing toolchains, then
#                    continue. Without it, missing toolchains are a hard error
#                    with copy-paste install instructions.
#   -h, --help       Show usage and exit.
set -euo pipefail

# --- locate the repo root from this script's own path ------------------------
# scripts/setup.sh lives one level below the repo root.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

VENV="$ROOT/.venv"
VENV_PY="$VENV/bin/python"

INSTALL_DEPS=0

# --- pretty progress helpers -------------------------------------------------
if [[ -t 1 ]]; then
  BOLD="$(printf '\033[1m')"; GREEN="$(printf '\033[32m')"
  YELLOW="$(printf '\033[33m')"; RED="$(printf '\033[31m')"; RESET="$(printf '\033[0m')"
else
  BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi
step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%s  ok%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%swarn%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()  { printf '%serror%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/setup.sh [--install-deps] [-h|--help]

Idempotent build-from-source setup for the MMA Stats Pipeline.

  --install-deps   macOS only: brew install any missing toolchains, then build.
  -h, --help       Show this help.

Steps: verify python3/cargo/go -> create .venv + install ml/requirements.txt
       -> make build (Rust TUI release + Go scraper). Re-running is safe/fast.
After it finishes, run ./mma to launch the TUI.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --install-deps) INSTALL_DEPS=1 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
  shift
done

is_macos() { [[ "$(uname -s)" == "Darwin" ]]; }

# --- step 1: prerequisites ---------------------------------------------------
# Map each required command to the brew formula that provides it.
brew_formula_for() {
  case "$1" in
    python3) echo "python" ;;
    cargo)   echo "rust" ;;
    go)      echo "go" ;;
    *)       echo "$1" ;;
  esac
}

step "Checking prerequisites (python3, cargo, go)"
missing=()
for tool in python3 cargo go; do
  if command -v "$tool" >/dev/null 2>&1; then
    ok "found $tool ($(command -v "$tool"))"
  else
    missing+=("$tool")
  fi
done

if [[ ${#missing[@]} -gt 0 ]]; then
  if [[ "$INSTALL_DEPS" -eq 1 ]] && is_macos; then
    if ! command -v brew >/dev/null 2>&1; then
      die "Homebrew not found. Install it from https://brew.sh then re-run with --install-deps."
    fi
    formulae=()
    for tool in "${missing[@]}"; do formulae+=("$(brew_formula_for "$tool")"); done
    step "Installing missing toolchains via Homebrew: ${formulae[*]}"
    brew install "${formulae[@]}"
    # Re-verify after install.
    for tool in "${missing[@]}"; do
      command -v "$tool" >/dev/null 2>&1 || die "still missing after brew install: $tool"
      ok "installed $tool ($(command -v "$tool"))"
    done
  else
    formulae=()
    for tool in "${missing[@]}"; do formulae+=("$(brew_formula_for "$tool")"); done
    # De-duplicate while preserving order.
    uniq_formulae=()
    for f in "${formulae[@]}"; do
      [[ " ${uniq_formulae[*]-} " == *" $f "* ]] || uniq_formulae+=("$f")
    done
    printf '\n'
    warn "missing required toolchain(s): ${missing[*]}"
    if is_macos; then
      cat >&2 <<EOF

Install them with Homebrew (https://brew.sh):

    brew install ${uniq_formulae[*]}

or re-run this script with --install-deps to install them automatically:

    scripts/setup.sh --install-deps
EOF
    else
      cat >&2 <<EOF

Install the missing toolchains for your platform:
    python3 : https://www.python.org/downloads/  (or your package manager)
    cargo   : https://rustup.rs/                  (the Rust toolchain)
    go      : https://go.dev/dl/
EOF
    fi
    die "prerequisites not satisfied"
  fi
fi

# --- step 2: Python venv + ML deps -------------------------------------------
if [[ -x "$VENV_PY" ]]; then
  step "Reusing existing Python venv ($VENV)"
  ok "venv python: $("$VENV_PY" --version 2>&1)"
else
  step "Creating Python venv ($VENV)"
  python3 -m venv "$VENV"
  ok "created venv with $("$VENV_PY" --version 2>&1)"
fi

step "Upgrading pip"
"$VENV_PY" -m pip install --quiet --upgrade pip
ok "pip $("$VENV_PY" -m pip --version | awk '{print $2}')"

step "Installing ML dependencies (ml/requirements.txt)"
"$VENV_PY" -m pip install --quiet -r "$ROOT/ml/requirements.txt"
ok "Python dependencies installed"

# --- step 3: build the native binaries ---------------------------------------
# `make build` = cargo release (TUI) + `go build -o scraper .` (scraper).
# Both build tools are incremental, so re-runs only rebuild what changed.
step "Building native binaries (Rust TUI release + Go scraper)"
make build
ok "binaries built (tui-rs/target/release/mma-tui + scraper-go/scraper)"

# --- done --------------------------------------------------------------------
printf '\n%sSetup complete.%s Run %s./mma%s to launch the TUI.\n' \
  "$GREEN$BOLD" "$RESET" "$BOLD" "$RESET"
