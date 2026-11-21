# mtrak development targets.
#
# Run `make` (or `make help`) to list the available targets.

CARGO ?= cargo

.DEFAULT_GOAL := help

.PHONY: help build release test fmt fmt-check clippy verify run clean

help: ## Show this help message
	@echo "mtrak make targets:"
	@echo ""
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'
	@echo ""

build: ## Build the workspace in debug mode
	$(CARGO) build --workspace

release: ## Build the workspace in release mode
	$(CARGO) build --workspace --release

test: ## Run the full workspace test suite
	$(CARGO) test --workspace

fmt: ## Format all sources with rustfmt
	$(CARGO) fmt --all

fmt-check: ## Check formatting without writing changes
	$(CARGO) fmt --all -- --check

clippy: ## Lint the workspace with clippy (warnings are errors)
	$(CARGO) clippy --workspace --all-targets -- -D warnings

verify: fmt-check clippy test ## Run the full local verification gate

run: ## Run the app, e.g. `make run ARGS=project.mid`
	$(CARGO) run -p mtrak -- $(ARGS)

clean: ## Remove build artifacts
	$(CARGO) clean
