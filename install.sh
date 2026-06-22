#!/bin/sh
# install.sh — one-command installer for the MMA Stats Pipeline (curl|sh entry).
#
#   curl -fsSL https://raw.githubusercontent.com/jibi21212/mma-stats-pipeline/main/install.sh | sh
#
# It clones (or updates) the repo into a USER-WRITABLE location, runs the
# build-from-source setup (scripts/setup.sh: python venv + ML deps + native
# binaries), and symlinks the `mma` launcher onto your PATH. The Go scraper is
# the sole DB writer, so the install MUST live somewhere writable — hence the
# clone into $MMA_HOME rather than a read-only prefix.
#
# Idempotent: re-running updates an existing install (git pull --ff-only) and
# re-runs setup (venv reused, binaries rebuilt incrementally).
#
# Environment overrides:
#   MMA_HOME       Install location (default: ~/.local/share/mma-stats-pipeline)
#   MMA_REPO_URL   Git source URL (default: the public GitHub repo). Handy for
#                  testing against a local file:// checkout.
set -eu

REPO_URL="${MMA_REPO_URL:-https://github.com/jibi21212/mma-stats-pipeline.git}"
MMA_HOME="${MMA_HOME:-$HOME/.local/share/mma-stats-pipeline}"
BIN_DIR="$HOME/.local/bin"

# --- pretty progress helpers (gated on a TTY; plain when piped) ---------------
if [ -t 1 ]; then
  BOLD="$(printf '\033[1m')"; GREEN="$(printf '\033[32m')"
  YELLOW="$(printf '\033[33m')"; RED="$(printf '\033[31m')"; RESET="$(printf '\033[0m')"
else
  BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi
step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%s  ok%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%swarn%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()  { printf '%serror%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

# --- prerequisite: git (toolchains are checked/installed by setup.sh) ----------
if ! command -v git >/dev/null 2>&1; then
  die "git is required but was not found.
Install git, then re-run this installer. Toolchains (python3, cargo, go) are
checked by the setup step — on macOS you can install everything via Homebrew:
  brew install git python rust go"
fi

# --- step 1: clone or update into a writable home -----------------------------
if [ -d "$MMA_HOME/.git" ]; then
  step "Updating existing install in $MMA_HOME"
  git -C "$MMA_HOME" pull --ff-only \
    || die "git pull failed in $MMA_HOME (local changes or diverged history?).
Fix the repo there, or remove it and re-run: rm -rf \"$MMA_HOME\""
  ok "repository updated"
else
  if [ -e "$MMA_HOME" ]; then
    die "$MMA_HOME exists but is not a git checkout.
Remove it and re-run, or set MMA_HOME to a different path:
  rm -rf \"$MMA_HOME\""
  fi
  step "Cloning $REPO_URL into $MMA_HOME"
  mkdir -p "$(dirname "$MMA_HOME")"
  git clone "$REPO_URL" "$MMA_HOME" \
    || die "git clone failed (check the URL / your network): $REPO_URL"
  ok "repository cloned"
fi

# --- step 2: build from source (venv + ML deps + native binaries) -------------
# setup.sh verifies python3/cargo/go and prints copy-paste install guidance if
# any are missing, then exits non-zero (which aborts us via set -e).
step "Running build-from-source setup (this builds the binaries; first run is slow)"
bash "$MMA_HOME/scripts/setup.sh" \
  || die "setup failed. See the messages above for missing toolchains, then
re-run this installer (or run: bash \"$MMA_HOME/scripts/setup.sh\")."
ok "setup complete"

# --- step 3: put the launcher on PATH -----------------------------------------
step "Linking the 'mma' launcher into $BIN_DIR"
mkdir -p "$BIN_DIR"
ln -sf "$MMA_HOME/mma" "$BIN_DIR/mma"
ok "symlinked $BIN_DIR/mma -> $MMA_HOME/mma"

# --- step 4: PATH check + final message ---------------------------------------
on_path=0
case ":$PATH:" in
  *":$BIN_DIR:"*) on_path=1 ;;
esac

printf '\n%sInstalled!%s\n' "$GREEN$BOLD" "$RESET"

if [ "$on_path" -eq 1 ]; then
  printf 'Run: %smma%s\n' "$BOLD" "$RESET"
else
  # Pick the rc file from the user's login shell so the hint is copy-paste ready.
  shell_name="$(basename "${SHELL:-sh}")"
  case "$shell_name" in
    zsh)  rc="$HOME/.zshrc" ;;
    bash) rc="$HOME/.bashrc" ;;
    *)    rc="your shell profile" ;;
  esac
  warn "$BIN_DIR is not on your PATH yet."
  printf '\nAdd it by appending this line to %s%s%s, then open a new terminal:\n\n' \
    "$BOLD" "$rc" "$RESET"
  # The literal $PATH below is intentional — it's text for the user's rc file.
  # shellcheck disable=SC2016
  printf '    export PATH="%s:$PATH"\n\n' "$BIN_DIR"
  printf 'Then run: %smma%s\n' "$BOLD" "$RESET"
  printf '(Or run it now with the full path: %s%s/mma%s)\n' "$BOLD" "$BIN_DIR" "$RESET"
fi
