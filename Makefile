# MMA Stats Pipeline — root task hub. Run everything from the repo root.
# `make` or `make run` launches the TUI; `make help` lists all targets.

ROOT     := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
PY       := $(ROOT)/.venv/bin/python
CARGO    ?= cargo
MANIFEST := $(ROOT)/tui-rs/Cargo.toml

.DEFAULT_GOAL := run
.PHONY: run dev build test e2e train clean help

run: ## Launch the TUI (release binary; builds once if needed)
	@"$(ROOT)/mma"

dev: ## Launch the TUI in debug mode (faster compile, slower runtime)
	$(CARGO) run --manifest-path "$(MANIFEST)"

build: ## Optimised build: TUI release binary + Go scraper binary (auto-detected by the TUI)
	$(CARGO) build --release --manifest-path "$(MANIFEST)"
	cd "$(ROOT)/scraper-go" && go build -o scraper .

test: ## Run ALL test suites: Rust (cargo) + Python (pytest) + Go (go test)
	$(CARGO) test --manifest-path "$(MANIFEST)"
	cd "$(ROOT)/ml" && "$(PY)" -m pytest -q
	cd "$(ROOT)/scraper-go" && go build ./... && go test ./...

e2e: ## End-to-end tests: PTY suite + tmux smoke (hermetic, offline)
	$(CARGO) test --manifest-path "$(MANIFEST)" --test e2e
	bash "$(ROOT)/scripts/tui_smoke.sh"

train: ## Train / retrain the fight-outcome predictor model
	cd "$(ROOT)/ml" && "$(PY)" predict.py --train

clean: ## Remove build artifacts (Rust target + Go scraper binary)
	$(CARGO) clean --manifest-path "$(MANIFEST)"
	rm -f "$(ROOT)/scraper-go/scraper"

help: ## Show available targets
	@echo "MMA Stats Pipeline — run any of these from the repo root:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN{FS=":.*?## "}{printf "  make %-7s %s\n", $$1, $$2}'
