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
# Which published package `make validate-azure` puts on a VM, and from which
# release. The version defaults to what the packages would be named after.
# The hash is escaped because make would otherwise take it as a comment and
# swallow the rest of the line - a recipe line, like the `version` target
# below, needs no escaping.
DISTRO ?= azurelinux3
VERSION ?= v$(shell $(CARGO) pkgid | sed 's/.*[\#@]//')

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

# Packaging needs Linux but not root: a .deb and a .rpm carry a Linux binary,
# and neither cargo-deb nor cargo-generate-rpm cross-compiles one.
ifeq ($(UNAME_S),Linux)
LINUX_RUN :=
else
LINUX_RUN := tools/dev.sh exec
endif

# Device targets mutate global host state - configfs entries, kernel modules,
# /dev/sdX - which is also why both device suites are single threaded. Two of
# them at once would fight over the same nexus, so this makefile never runs in
# parallel, whatever -j is passed.
.NOTPARALLEL:

.DEFAULT_GOAL := help

.PHONY: help quickstart build check doc clean fmt lint test test-device \
        test-e2e verify preflight cleanup install-overlaybd baselayer \
        package package-deb package-rpm version validate-azure dev dev-shell \
        dev-stop dev-rebuild dev-status

##@ General
help: ## List these targets
	@echo "obd-rs"
	@echo
	@echo "  device targets run $(DEVICE_WHERE)"
	@awk 'BEGIN { FS = ":.*##" } \
		/^##@/ { printf "\n%s\n", substr($$0, 5); next } \
		/^[a-zA-Z0-9_-]+:.*##/ { printf "  %-18s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo

# .NOTPARALLEL above is what makes these prerequisites ordered: preflight is
# only meaningful once the install has run.
quickstart: install-overlaybd preflight ## Install overlaybd, build, and report whether this host can drive devices

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

baselayer: ## Generate the ext4 baselayer devices stack as their bottom lower
	$(DEVICE_RUN) ./lib/shell/obd-baselayer.sh

##@ Packaging
package: ## Build the deb and the rpm into target/packages
	$(LINUX_RUN) ./tools/package.sh all

package-deb: ## Build only the deb
	$(LINUX_RUN) ./tools/package.sh deb

package-rpm: ## Build only the rpm
	$(LINUX_RUN) ./tools/package.sh rpm

# The one thing this repository cannot check on the machine that builds it:
# obdctl carries the glibc of its build host, the unit drop-in only means
# something where systemd runs, and the postinst needs a kernel with TCMU.
# DISTRO picks which published asset is under test.
validate-azure: ## Validate a published package on a fresh Azure VM (DISTRO=azurelinux3|ubuntu24)
	./tools/az-validate.sh --distro $(DISTRO) --version $(VERSION)

# The release workflow checks the tag against this before it publishes: a
# vX.Y.Z tag that does not match produces packages named after the manifest,
# not after the tag, and the mismatch is only visible once someone downloads
# one.
version: ## Print the crate version, as the packages are named
	@$(CARGO) pkgid | sed 's/.*[#@]//'

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
