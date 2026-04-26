SHELL := /usr/bin/env bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := build

PROGRAM_SO := ../target/deploy/contra_escrow_program.so
PROGRAM_KEYPAIR := ../target/deploy/contra_escrow_program-keypair.json
PROGRAM_ENV_KEY := ESCROW_PROGRAM_ID

.PHONY: install build build-test build-no-clients fmt generate-idl generate-clients
.PHONY: unit-test integration-test integration-test-no-build all-test
.PHONY: unit-coverage coverage-html all-coverage
.PHONY: build-localnet build-devnet deploy-devnet

# Install dependencies
install:
	pnpm install

# Build the program
build:
	$(MAKE) generate-clients
	cd program && cargo-build-sbf

# Build the program with test-tree feature for integration tests
build-test:
	cd program && cargo-build-sbf --features test-tree

# Build the prod program WITHOUT regenerating IDL / clients. Use after
# `make build` (or after restoring a CI artifact that already contains
# `clients/` and `idl/`) when you only need to rebuild the BPF binary —
# e.g. mid-test-suite when toggling between test-tree and prod features.
# Avoids the pnpm dependency that `generate-clients` requires.
build-no-clients:
	cd program && cargo-build-sbf

# Format / lint code
fmt:
	cargo fmt --all
	@cd program && cargo clippy --all-targets -- -D warnings
	@cd tests/integration-tests && cargo clippy --all-targets -- -D warnings
	pnpm format

# Generate IDL from Codama annotations
generate-idl:
	@echo "Generating IDL..."
	pnpm run generate-idl

# Generate clients from IDL using Codama
generate-clients: generate-idl
	@echo "Generating clients..."
	pnpm run generate-clients

### Tests ###

# Run unit tests in program/
unit-test:
	@echo "Running unit tests for escrow program..."
	pnpm test:unit
	@cd program && cargo test

# Run integration tests without build (for CI composition)
integration-test-no-build:
	@echo "Running integration tests for escrow program..."
	@cd tests/integration-tests && cargo test -- --nocapture

# Run integration tests (includes build)
integration-test: build integration-test-no-build

all-test: unit-test integration-test

# Run unit tests with coverage
unit-coverage:
	@echo "Running unit tests with coverage..."
	@mkdir -p ../coverage
	@cd program && cargo llvm-cov --lib --tests --lcov --output-path ../../coverage/coverage-escrow-unit.lcov

# Generate HTML coverage report
coverage-html:
	@echo "Generating HTML coverage report..."
	@mkdir -p ../coverage
	@cd program && cargo llvm-cov --html --output-dir ../../coverage/coverage-escrow-html

# Run all coverage tasks
all-coverage: unit-coverage coverage-html

#############
# Localnet
#############
build-localnet:
	$(MAKE) generate-clients
	cargo-build-sbf
	@echo "Updating .env.local with program IDs..."
	@../scripts/update-program-env.sh ../.env.local $(PROGRAM_ENV_KEY) $(PROGRAM_KEYPAIR)

#############
# Devnet
#############

# Build the program with devnet feature
build-devnet:
	$(MAKE) generate-clients
	cargo-build-sbf

# Deploy to devnet
deploy-devnet: build-devnet
	@echo "Deploying to devnet..."
	@if [ -z "$(DEPLOYER_KEY)" ]; then \
		echo "Error: DEPLOYER_KEY parameter is required. Usage: make deploy-devnet DEPLOYER_KEY=/path/to/deployer-keypair.json"; \
		exit 1; \
	fi
	solana program deploy $(PROGRAM_SO) \
		--program-id $(PROGRAM_KEYPAIR) \
		--keypair $(DEPLOYER_KEY) \
		--url devnet
