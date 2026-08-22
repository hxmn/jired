# Developer entry points for jired. Everything here is a thin wrapper over
# cargo; the CI workflow in .github/workflows/ci.yml runs the same commands.

CARGO ?= cargo
# Tests must never touch a developer's real keychain, so they run with the
# same environment CI uses.
TEST_ENV := JIRED_NO_KEYRING=1 JIRED_SECRET_KEY=dev-test-passphrase
# Where `make completions` writes generated shell scripts.
COMPLETION_DIR ?= target/completions
SHELLS := bash zsh fish powershell elvish

# Where `make install` puts the completions it refreshes. Both are the XDG user
# directories, so nothing here needs root; override either to point elsewhere.
DATA_HOME := $(if $(XDG_DATA_HOME),$(XDG_DATA_HOME),$(HOME)/.local/share)
BASH_COMPLETION_DIR ?= $(DATA_HOME)/bash-completion/completions
ZSH_COMPLETION_DIR ?= $(DATA_HOME)/zsh/site-functions
# The binary `cargo install` just wrote. Completions are generated from it
# rather than from `cargo run`, so what completes is what got installed.
CARGO_INSTALL_ROOT ?= $(if $(CARGO_HOME),$(CARGO_HOME),$(HOME)/.cargo)
INSTALLED_BIN := $(CARGO_INSTALL_ROOT)/bin/jired

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-z][a-zA-Z0-9_-]*:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

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
install: ## Install jired into ~/.cargo/bin, with bash and zsh completions
	$(CARGO) install --path . --locked --bin jired
	@$(MAKE) --no-print-directory install-completions

# A stale completion script is worse than none: it offers flags the binary no
# longer takes and hides the ones it grew. Rewriting both on every install keeps
# them honest, and writing them unconditionally is also the update path — the
# scripts are generated, so there is nothing of the user's to preserve. Each goes
# through a temporary file, so a failed generation leaves the working script in
# place rather than a truncated one the shell will complain about at every prompt.
.PHONY: install-completions
install-completions: ## Write the bash and zsh completions into the XDG user directories
	@bin='$(INSTALLED_BIN)'; \
	if [ ! -x "$$bin" ]; then bin='$(CARGO) run --quiet --bin jired --'; fi; \
	write() { \
		mkdir -p "$$(dirname "$$2")" || exit 1; \
		$$bin completion "$$1" > "$$2.new" || { rm -f "$$2.new"; exit 1; }; \
		mv "$$2.new" "$$2"; \
		echo "$$2"; \
	}; \
	write bash '$(BASH_COMPLETION_DIR)/jired'; \
	write zsh '$(ZSH_COMPLETION_DIR)/_jired'; \
	if command -v zsh >/dev/null 2>&1 && \
	   ! zsh -c 'print -l -- $$fpath' 2>/dev/null | grep -qxF '$(ZSH_COMPLETION_DIR)'; then \
		echo; \
		echo "that directory is not on the fpath this zsh reports; if completion stays"; \
		echo "quiet, add to ~/.zshrc, before compinit:"; \
		echo "  fpath=($(ZSH_COMPLETION_DIR) \$$fpath)"; \
	fi; \
	echo "start a new shell to pick them up"

.PHONY: uninstall
uninstall: ## Remove an installed jired and its completions
	$(CARGO) uninstall jired
	rm -f '$(BASH_COMPLETION_DIR)/jired' '$(ZSH_COMPLETION_DIR)/_jired'

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf $(COMPLETION_DIR)
