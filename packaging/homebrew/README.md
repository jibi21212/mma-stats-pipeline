# Homebrew packaging

Homebrew packaging for the MMA Stats Pipeline. The app installs build-from-source
(toolchains required) and is launched with a single `mma` command.

## Contents

```
packaging/homebrew/
├── README.md                 this file
├── Formula/
│   └── mma-stats.rb          the canonical Homebrew formula
└── tap/                      scaffold for the public homebrew-tap repo
    ├── README.md             tap install docs (brew tap + brew install)
    └── Formula/
        └── mma-stats.rb      a copy of the formula, in tap layout
```

`Formula/mma-stats.rb` is the source of truth. `tap/` is a ready-to-publish copy
of what your **`homebrew-tap`** GitHub repo should contain.

## What the formula does

- Declares `go` + `rust` as `:build` deps and `python@3.11` as a runtime dep, so
  `brew install` compiles the Go scraper and Rust TUI from source.
- Stages the source tree (`ml/`, `data/`, `scraper-go/`, the launcher logic)
  into `libexec`, installs the `mma-tui` binary via `cargo install`, and writes a
  `bin/mma` wrapper.
- The wrapper, on **first run**, provisions a user-writable runtime dir
  (`${XDG_DATA_HOME:-$HOME/.local/share}/mma-stats`) with a Python venv (`pip
  install -r ml/requirements.txt`) and a writable copy of `data/ufc.db`, then
  execs the TUI with `MMA_PYTHON` / `MMA_DB` / `MMA_SCRAPER` pointed at those
  locations. The `pip install` is deliberately deferred to runtime because
  Homebrew's build sandbox has no network access.

## Publishing the tap

1. Create a GitHub repo named **`homebrew-tap`** under your account
   (`jibi21212/homebrew-tap`). The `homebrew-` prefix is required.
2. Copy `tap/README.md` and `tap/Formula/mma-stats.rb` into it and push.
3. Make the main `mma-stats-pipeline` repo **public** and, ideally, cut a tagged
   release. Update the formula `url`/`sha256` to the tagged tarball (see the
   comments at the top of the formula).
4. Users then run:

   ```sh
   brew tap jibi21212/tap
   brew install mma-stats
   mma
   ```

## Validation

Validated locally with the Homebrew tooling:

- `brew style ./Formula/mma-stats.rb` — **no offenses**.
- `brew audit --new <name>` (audited via a throwaway local tap, since
  `brew audit` requires a tapped name, not a path) — only the expected
  **publish-dependent** warnings remain: unreachable homepage/source URLs (repo
  not public yet) and a missing stable checksum (needs a tagged release). These
  cannot be resolved until the repo is public and a release is cut; there are no
  structural or style problems.
