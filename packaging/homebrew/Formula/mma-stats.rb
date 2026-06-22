# MMA Stats Pipeline — Homebrew formula (build-from-source).
#
# This is a POLYGLOT app: a Go scraper (sole DB writer), a Python ML sidecar,
# and a Rust TUI that ties them together via the `mma` launcher. There is no
# precompiled bottle — Homebrew BUILDS the binaries from source, so the build
# toolchains (go, rust) are :build dependencies and a Python is a runtime dep.
#
# Why a runtime venv + writable DB copy (not a plain `bin.install`):
#   * The Go scraper WRITES data/ufc.db, so the database cannot live in
#     Homebrew's read-only Cellar. On first run the wrapper copies the bundled
#     DB into a USER-WRITABLE runtime dir and points the app at it via $MMA_DB.
#   * The Python ML deps (numpy/pandas/scikit-learn/joblib/...) are installed
#     into a venv AT RUNTIME (first launch), NOT during `brew install`. Homebrew
#     sandboxes the build with NO network access, so a build-time `pip install`
#     would fail. Doing it lazily in the wrapper sidesteps that entirely.
#
# HONESTY NOTE: `brew install` of this formula only works once the URL below
# resolves to a real, downloadable source tarball. Two ways to get there:
#   1. (recommended) cut a tagged release on GitHub and point `url`/`sha256`
#      at that tag's source tarball (see the commented block below), or
#   2. use the `main` branch archive (url already set to that) — but note an
#      un-pinned branch tarball has no stable sha256, so `brew audit --strict`
#      will (correctly) complain. A tag is the right answer for distribution.
class MmaStats < Formula
  desc "Polyglot UFC stats pipeline: Go scraper + Python ML + Rust TUI"
  homepage "https://github.com/jibi21212/mma-stats-pipeline"
  # The `main` branch source archive. GitHub serves this as a .tar.gz of the
  # repo tree at HEAD of main. It is the simplest thing that works today, but it
  # is NOT version-pinned.
  url "https://github.com/jibi21212/mma-stats-pipeline/archive/refs/heads/main.tar.gz"
  version "0.1.0"
  license "MIT"
  head "https://github.com/jibi21212/mma-stats-pipeline.git", branch: "main"

  # RECOMMENDED for a real release — replace the `url` above with a tagged
  # tarball and add its sha256, e.g.:
  #
  #   url "https://github.com/jibi21212/mma-stats-pipeline/archive/refs/tags/v0.1.0.tar.gz"
  #   sha256 "PUT_THE_REAL_SHA256_HERE"   # `brew fetch ./mma-stats.rb` prints it
  #
  # With a tag in place, drop the `version "0.1.0"` line (Homebrew infers it
  # from the tag) and `brew audit --strict` is happy.

  depends_on "go" => :build
  depends_on "rust" => :build
  # Runtime Python: the wrapper builds a venv from THIS interpreter on first run.
  # Pinned to a Homebrew-keg Python so the venv is stable across user upgrades.
  depends_on "python@3.11"

  def install
    # --- 1. Build the native binaries from source ----------------------------
    # Rust TUI (release): `cargo install` builds the `mma-tui` binary and places
    # it in `bin`. The binary's LOCATION is irrelevant to path resolution — the
    # TUI walks up from its CWD (which the wrapper sets to libexec) to find the
    # code/data — so installing it to `bin` is clean and audit-friendly.
    system "cargo", "install", *std_cargo_args(path: "tui-rs")
    # Go scraper — the SOLE DB writer. Built to scraper-go/scraper, the exact
    # name + location the TUI auto-detects (and that $MMA_SCRAPER points at).
    cd "scraper-go" do
      system "go", "build", "-o", "scraper", "."
    end

    # --- 2. Stage the source tree into libexec -------------------------------
    # The TUI resolves the repo root by walking up for data/ufc.db and runs the
    # Python sidecar (ml/serve.py) + scraper relative to it, so we ship the tree
    # intact. Skip the Rust crate (its multi-GB target/ is regenerable and the
    # built binary already went to `bin` via cargo install above).
    libexec.install Dir["*"].reject { |p| File.basename(p) == "tui-rs" }

    # --- 3. The wrapper: lazy runtime setup, then exec the TUI ---------------
    # First run creates a writable runtime dir (venv + DB copy); later runs are
    # instant. We point the TUI at all of it via the documented env overrides:
    #   MMA_PYTHON  -> the runtime venv's python (used for the ML sidecar)
    #   MMA_DB      -> the writable DB copy (flows to the Go scraper as --db)
    #   MMA_SCRAPER -> the prebuilt Go scraper binary in libexec
    # We `cd` into libexec first so the TUI's repo-root walk finds the code
    # (ml/serve.py, scraper-go/, etc.); $MMA_DB then redirects the DB elsewhere.
    python = Formula["python@3.11"].opt_bin/"python3.11"
    (bin/"mma").write <<~SH
      #!/bin/bash
      # mma — Homebrew wrapper for the MMA Stats Pipeline.
      # Lazily provisions a USER-WRITABLE runtime dir on first launch, then execs
      # the Rust TUI with env pointed at the writable venv + DB. Re-runs are fast.
      set -euo pipefail

      LIBEXEC="#{libexec}"
      # Writable runtime home (honors $XDG_DATA_HOME; else ~/.local/share).
      RUNTIME="${XDG_DATA_HOME:-$HOME/.local/share}/mma-stats"
      VENV="$RUNTIME/.venv"
      VENV_PY="$VENV/bin/python"
      DB="$RUNTIME/ufc.db"

      mkdir -p "$RUNTIME"

      # First-run: create the Python venv and install the ML deps. This pip
      # install happens HERE, at runtime, NOT in the formula `install` block —
      # Homebrew's build sandbox has no network, so build-time pip would fail.
      if [[ ! -x "$VENV_PY" ]]; then
        echo "mma: first run — creating Python venv and installing ML deps (one time)…" >&2
        "#{python}" -m venv "$VENV"
        "$VENV_PY" -m pip install --quiet --upgrade pip
        "$VENV_PY" -m pip install --quiet -r "$LIBEXEC/ml/requirements.txt"
      fi

      # First-run: seed a WRITABLE copy of the bundled DB (the Cellar copy is
      # read-only; the scraper must be able to write its target).
      if [[ ! -f "$DB" ]]; then
        echo "mma: seeding writable database copy at $DB" >&2
        cp "$LIBEXEC/data/ufc.db" "$DB"
      fi

      # cd into the staged source so the TUI resolves ml/serve.py + scraper-go/;
      # the env overrides redirect python/DB/scraper to the writable locations.
      cd "$LIBEXEC"
      export MMA_PYTHON="$VENV_PY"
      export MMA_DB="$DB"
      export MMA_SCRAPER="$LIBEXEC/scraper-go/scraper"
      exec "#{bin}/mma-tui" "$@"
    SH
    chmod 0755, bin/"mma"
  end

  def caveats
    <<~EOS
      The MMA Stats Pipeline keeps a WRITABLE runtime directory (Python venv +
      a writable copy of the bundled UFC database) at:

        ${XDG_DATA_HOME:-$HOME/.local/share}/mma-stats

      The first `mma` run provisions it (creates the venv, installs the Python ML
      dependencies, and copies the database). This is a one-time, network-using
      step; subsequent launches are instant. To start completely fresh, delete
      that directory and run `mma` again.

      Launch the terminal UI with:
        mma
    EOS
  end

  test do
    # The wrapper must exist and be executable, and must reference the env
    # overrides that wire the app to its writable runtime locations.
    assert_predicate bin/"mma", :executable?
    wrapper = (bin/"mma").read
    assert_match "MMA_PYTHON=", wrapper
    assert_match "MMA_DB=", wrapper
    assert_match "MMA_SCRAPER=", wrapper
    # The built native binaries must be where the wrapper expects them: the TUI
    # in `bin` (via cargo install), the Go scraper staged in libexec.
    assert_path_exists bin/"mma-tui"
    assert_path_exists libexec/"scraper-go/scraper"
    # The bundled, read-only seed DB and the ML requirements must ship too.
    assert_path_exists libexec/"data/ufc.db"
    assert_path_exists libexec/"ml/requirements.txt"
  end
end
