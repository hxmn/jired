# Developer entry points for jired. Everything here is a thin wrapper over
# cargo; the CI workflow in .github/workflows/ci.yml runs the same commands.

CARGO ?= cargo
# Tests must never touch a developer's real keychain, so they run with the
# same environment CI uses.
TEST_ENV := JIRED_NO_KEYRING=1 JIRED_SECRET_KEY=dev-test-passphrase
# Where `make completions` writes generated shell scripts.
COMPLETION_DIR ?= target/completions
SHELLS := bash zsh fish powershell elvish

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-z][a-zA-Z0-9_-]*:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: build
build: ## Build the debug binaries
	$(CARGO) build --all-targets

.PHONY: release
release: ## Build the optimised binary
	$(CARGO) build --release

.PHONY: run
run: ## Run jired; pass arguments with ARGS="..."
	$(CARGO) run --bin jired -- $(ARGS)

.PHONY: check
check: ## Type-check without producing binaries
	$(CARGO) check --all-targets

.PHONY: test
test: ## Run the test suite
	$(TEST_ENV) $(CARGO) test --all-targets

.PHONY: test-all
test-all: test ## Run the test suite including the ignored scale suite
	$(TEST_ENV) $(CARGO) test --test scale -- --ignored --nocapture

.PHONY: fmt
fmt: ## Format the source tree
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Fail if the source tree is not formatted
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint: ## Run clippy with warnings denied
	$(CARGO) clippy --all-targets -- -D warnings

.PHONY: audit
audit: ## Check advisories, licences and bans (needs cargo-deny)
	$(CARGO) deny check

.PHONY: doc
doc: ## Build the API documentation
	$(CARGO) doc --no-deps --document-private-items

.PHONY: completions
completions: ## Generate shell completion scripts under target/completions
	@mkdir -p $(COMPLETION_DIR)
	@for shell in $(SHELLS); do \
		$(CARGO) run --quiet --bin jired -- completion $$shell > $(COMPLETION_DIR)/jired.$$shell || exit 1; \
		echo "$(COMPLETION_DIR)/jired.$$shell"; \
	done

.PHONY: ci
ci: fmt-check lint test-all audit ## Run everything CI runs

.PHONY: install
install: ## Install jired into ~/.cargo/bin
	$(CARGO) install --path . --locked --bin jired

.PHONY: uninstall
uninstall: ## Remove an installed jired
	$(CARGO) uninstall jired

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf $(COMPLETION_DIR)
