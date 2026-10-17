# Per-project dev shell: the Rust + Python toolchain for this repo.
# Go comes from the machine's global toolchain (scraper-go needs Go 1.26+).
# Enter with `nix develop`, or run one command: `nix develop -c make test`.
{
  description = "mma-stats-pipeline dev shell (Rust TUI + Python ML)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            # Rust (tui-rs/, edition 2024)
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            cargo-nextest
            cargo-deny
            # Python (ml/); project packages live in .venv via scripts/setup.sh
            python3
            uv
            ruff
            # E2E smoke (make e2e)
            tmux
            # Go's VCS stamping shells out to git; inside the shell the macOS
            # /usr/bin/git shim cannot resolve against the Nix SDK, so ship one.
            git
          ];
        };
      });
    };
}
