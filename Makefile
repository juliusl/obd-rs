# obd-rs. `make help` lists the targets.
#
# Targets divide by where they can run. Building, unit tests, clippy and
# rustfmt work on any host: the crate compiles everywhere and device operations
# return UnsupportedPlatform off Linux. Anything that drives a device needs a
# Linux kernel with TCMU, root, and overlaybd installed, so those targets run in
# place where the host provides that and dispatch into the devcontainer where it
# does not. `make test-e2e` therefore means the same thing on a macOS laptop, in
# the Lima VM and inside the container.
#
# GNU make 3.81 is the floor, because that is what macOS ships.

CARGO ?= cargo
OBDCTL := ./target/debug/obdctl
# The name .devcontainer/devcontainer.json pins with runArgs.
CONTAINER ?= obd-rs-dev

UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Linux)
ifeq ($(shell id -u),0)
DEVICE_RUN :=
DEVICE_WHERE := here, already root
else
# sudo replaces PATH with secure_path, which drops a rustup toolchain in
# ~/.cargo/bin, so PATH is handed back explicitly.
DEVICE_RUN := sudo -E env PATH="$(PATH)"
DEVICE_WHERE := here, through sudo
endif
else
DEVICE_RUN := tools/dev.sh exec
DEVICE_WHERE := in the $(CONTAINER) devcontainer, created on first use
endif

# Device targets mutate global host state - configfs entries, kernel modules,
# /dev/sdX - which is also why both device suites are single threaded. Two of
# them at once would fight over the same nexus, so this makefile never runs in
# parallel, whatever -j is passed.
.NOTPARALLEL:

.DEFAULT_GOAL := help

.PHONY: help build check doc clean fmt lint test test-device test-e2e verify \
        preflight cleanup install-overlaybd dev dev-shell dev-stop dev-rebuild \
        dev-status

##@ General
help: ## List these targets
	@echo "obd-rs"
	@echo
	@echo "  device targets run $(DEVICE_WHERE)"
	@awk 'BEGIN { FS = ":.*##" } \
		/^##@/ { printf "\n%s\n", substr($$0, 5); next } \
		/^[a-zA-Z0-9_-]+:.*##/ { printf "  %-18s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo

##@ Build
build: ## Compile the library and obdctl
	$(CARGO) build

check: ## Type-check, including the library without the cli feature
	$(CARGO) check --all-targets
	$(CARGO) check --no-default-features

doc: ## Build the API docs, failing on a doc warning
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps

clean: ## Remove the build artifacts
	$(CARGO) clean

##@ Quality
fmt: ## Format the Rust sources
	$(CARGO) fmt

lint: ## rustfmt, clippy and shellcheck, all as errors
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	tools/shellcheck.sh

test: ## Run the tests that need no device, on any platform
	$(CARGO) test

test-device: ## Run the library device suite
	$(DEVICE_RUN) env OBD_DEVICE_TESTS=1 $(CARGO) test --test linux_device -- --test-threads=1

test-e2e: ## Run the obdctl device suite
	$(DEVICE_RUN) $(CARGO) build
	$(DEVICE_RUN) ./tests/lima-e2e.sh

# The bar a branch has to clear before it is a pull request. Off Linux it runs
# whole in the container rather than target by target: half of this crate is
# behind cfg(target_os = "linux"), so clippy on a macOS host never sees it.
#
# Only the first branch carries a help comment: `make help` reads this file as
# text, so a target documented in both would be listed twice.
ifeq ($(UNAME_S),Linux)
verify: lint check doc test test-device test-e2e ## Everything a branch must pass
	@echo "verify: passed"
else
verify:
	$(DEVICE_RUN) make lint check doc test test-device test-e2e
	@echo "verify: passed, on Linux"
endif

##@ Device host
preflight: ## Report whether devices can be driven, and why not
	$(DEVICE_RUN) $(CARGO) build
	$(DEVICE_RUN) $(OBDCTL) preflight

cleanup: ## Sweep the devices and mounts an interrupted run left behind
	$(DEVICE_RUN) $(CARGO) build
	$(DEVICE_RUN) $(OBDCTL) cleanup

install-overlaybd: ## Install overlaybd and start its daemon
	$(DEVICE_RUN) ./scripts/install-overlaybd.sh

##@ Devcontainer
dev: ## Create or start the container and provision it
	tools/dev.sh up

dev-shell: ## Open a shell in the container
	tools/dev.sh shell

dev-status: ## Report the container state and its preflight
	tools/dev.sh status

dev-stop: ## Stop the container, keeping the cargo caches
	tools/dev.sh stop

dev-rebuild: ## Recreate the container from .devcontainer
	tools/dev.sh rebuild
