# homebrew-tap

A [Homebrew tap](https://docs.brew.sh/Taps) for the
[MMA Stats Pipeline](https://github.com/jibi21212/mma-stats-pipeline) — a
polyglot UFC stats app (Go scraper + Python ML + Rust TUI) launched with a
single `mma` command.

> This directory is a **scaffold**. To publish it, copy its contents into a new
> GitHub repository that **must be named `homebrew-tap`** under your account
> (`jibi21212/homebrew-tap`). Homebrew maps `brew tap jibi21212/tap` to the repo
> `github.com/jibi21212/homebrew-tap` — the `homebrew-` prefix is mandatory and
> the short name in `brew tap` is whatever follows it (`tap`).

## Install

```sh
brew tap jibi21212/tap
brew install mma-stats
```

Then launch the terminal UI:

```sh
mma
```

`brew install mma-stats` **builds from source**, so the build toolchains are
required and pulled in automatically as Homebrew dependencies:

- `go` (build) — compiles the scraper
- `rust` (build) — compiles the TUI
- `python@3.11` (runtime) — the ML sidecar's interpreter

### First run

The first `mma` launch provisions a **user-writable runtime directory** at
`${XDG_DATA_HOME:-$HOME/.local/share}/mma-stats`:

- a Python virtualenv with the ML dependencies (numpy / pandas / scikit-learn /
  joblib / …) — installed **at first run**, not during `brew install`, because
  Homebrew's build sandbox has no network access, and
- a **writable copy** of the bundled UFC database (the Go scraper is the sole DB
  writer and cannot write into Homebrew's read-only Cellar).

This is a one-time, network-using step; later launches are instant. Delete that
directory and run `mma` again to start completely fresh.

## Repository layout

A published tap is just a Git repo with a `Formula/` directory:

```
homebrew-tap/
├── README.md
└── Formula/
    └── mma-stats.rb
```

`Formula/mma-stats.rb` here is kept in sync with the canonical copy in the main
repo at
[`packaging/homebrew/Formula/mma-stats.rb`](https://github.com/jibi21212/mma-stats-pipeline/blob/main/packaging/homebrew/Formula/mma-stats.rb).
When you cut a new release, update the formula's `url`/`sha256` (see the comments
at the top of the formula) and copy it back here.

## Status / honesty note

`brew install mma-stats` works end-to-end **only once both of these are true**:

1. The main repo (`jibi21212/mma-stats-pipeline`) is **public** on GitHub — the
   formula downloads its source tarball from there.
2. The formula's `url` points at a downloadable tarball. The scaffold formula
   currently uses the `main` branch archive (works as soon as the repo is
   public, but is **not version-pinned** and has no stable `sha256`). For a real
   release, cut a Git tag and point `url`/`sha256` at the tagged source tarball;
   that also makes `brew audit --strict` pass cleanly.

Until then, `brew audit` reports exactly these expected, publish-dependent
warnings (no structural problems remain):

- `homepage URL ... is not reachable (404)` — repo not public yet
- `source URL ... is not reachable (404)` — repo not public yet
- `Stable: Checksum is missing` — needs a tagged release with a stable `sha256`

`brew style` passes with no offenses today.
