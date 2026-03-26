# Master Makefile for Contra Programs
# Delegates to subdirectory Makefiles

SHELL := /usr/bin/env bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := build

PROGRAM_DIRS := contra-escrow-program contra-withdraw-program
RUST_DIRS := core indexer gateway
FMT_DIRS := $(PROGRAM_DIRS) $(RUST_DIRS) integration
OBS_SERVICES := cadvisor prometheus grafana

.PHONY: all help
.PHONY: install build fmt generate-idl generate-clients
.PHONY: unit-test integration-test all-test
.PHONY: ci-unit-test ci-integration-test ci-integration-test-prebuilt ci-integration-test-build-test-tree ci-integration-test-indexer
.PHONY: unit-test-ci integration-test-ci integration-test-ci-prebuilt integration-test-ci-build-test-tree integration-test-ci-indexer integration-test-ci-no-build
.PHONY: unit-coverage coverage-html all-coverage ci-unit-coverage ci-e2e-coverage
.PHONY: yellowstone-prepare yellowstone-build-plugin yellowstone-clean
.PHONY: download-yellowstone-grpc build-geyser-plugin clean-geyser
.PHONY: generate-operator-keypair build-localnet build-devnet deploy-devnet
.PHONY: profile obs-up obs-down obs-logs obs-devnet-up obs-devnet-down obs-devnet-logs

all: build

install:
	@echo "Installing dependencies for all projects..."
	@for dir in $(PROGRAM_DIRS); do \
		$(MAKE) -C $$dir install; \
	done

build:
	@echo "Building all projects..."
	@for dir in $(PROGRAM_DIRS) $(RUST_DIRS); do \
		$(MAKE) -C $$dir build; \
	done

fmt:
	@echo "Formatting all projects..."
	@for dir in $(FMT_DIRS); do \
		$(MAKE) -C $$dir fmt; \
	done
	@cd scripts/devnet && cargo fmt

generate-idl:
	@echo "Generating IDL for all programs..."
	@for dir in $(PROGRAM_DIRS); do \
		$(MAKE) -C $$dir generate-idl; \
	done

generate-clients:
	@echo "Generating clients for all programs..."
	@for dir in $(PROGRAM_DIRS); do \
		$(MAKE) -C $$dir generate-clients; \
	done

unit-test:
	@echo "Running unit tests for all projects..."
	@for dir in $(PROGRAM_DIRS) $(RUST_DIRS); do \
		$(MAKE) -C $$dir unit-test; \
	done

integration-test:
	@echo "Running integration tests for all projects..."
	@$(MAKE) -C contra-escrow-program integration-test
	@$(MAKE) -C contra-withdraw-program integration-test
	@echo "Running contra integration test (with production build)..."
	@cd integration && cargo nextest run --test contra_integration
	@echo "Running reconciliation integration tests..."
	@cd integration && cargo test --test reconciliation_integration -- --nocapture
	@echo "Running mint idempotency integration tests..."
	@cd integration && cargo test --test mint_idempotency_integration -- --nocapture
	@echo "Running gap detection integration tests..."
	@cd integration && cargo test --test gap_detection_integration -- --nocapture
	@echo "Running truncate integration tests..."
	@cd integration && cargo test --test truncate_integration -- --nocapture
	@echo "Building escrow with test-tree for indexer and operator lifecycle tests..."
	@$(MAKE) -C contra-escrow-program build-test
	@echo "Running indexer integration test (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test indexer_integration -- --nocapture
	@echo "Running operator lifecycle integration tests (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test operator_lifecycle_integration -- --nocapture

ci-unit-test:
	@echo "Running CI unit tests for core + indexer..."
	@$(MAKE) -C core unit-test
	@$(MAKE) -C indexer unit-test

ci-integration-test:
	@echo "Running CI integration tests (non-program suites)..."
	@echo "Building program artifacts once for integration crate tests..."
	@$(MAKE) -C contra-escrow-program build
	@$(MAKE) -C contra-withdraw-program build
	@$(MAKE) ci-integration-test-build-test-tree

ci-integration-test-build-test-tree:
	@$(MAKE) ci-integration-test-prebuilt
	@echo "Building escrow with test-tree for indexer and operator lifecycle tests..."
	@$(MAKE) -C contra-escrow-program build-test
	@echo "Running indexer integration test (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test indexer_integration -- --nocapture
	@echo "Running operator lifecycle integration tests (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test operator_lifecycle_integration -- --nocapture

ci-integration-test-prebuilt:
	@echo "Running contra integration test (with production build)..."
	@cd integration && cargo nextest run --test contra_integration
	@echo "Running reconciliation integration tests..."
	@cd integration && cargo test --test reconciliation_integration -- --nocapture
	@echo "Running mint idempotency integration tests..."
	@cd integration && cargo test --test mint_idempotency_integration -- --nocapture
	@echo "Running gap detection integration tests..."
	@cd integration && cargo test --test gap_detection_integration -- --nocapture
	@echo "Running truncate integration tests..."
	@cd integration && cargo test --test truncate_integration -- --nocapture

# CI-focused integration target that runs indexer integration tests only.
ci-integration-test-indexer:
	@echo "Building escrow with test-tree for indexer and operator lifecycle tests..."
	@$(MAKE) -C contra-escrow-program build-test
	@echo "Running indexer integration test (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test indexer_integration -- --nocapture
	@echo "Running reconciliation integration tests..."
	@cd integration && cargo test --test reconciliation_integration -- --nocapture
	@echo "Running mint idempotency integration tests..."
	@cd integration && cargo test --test mint_idempotency_integration -- --nocapture
	@echo "Running gap detection integration tests..."
	@cd integration && cargo test --test gap_detection_integration -- --nocapture
	@echo "Running truncate integration tests..."
	@cd integration && cargo test --test truncate_integration -- --nocapture
	@echo "Running operator lifecycle integration tests (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test operator_lifecycle_integration -- --nocapture
	@echo "Running reconciliation e2e tests..."
	@cd indexer && cargo test --test reconciliation_e2e_test -- --nocapture

# Backward-compatible aliases.
unit-test-ci: ci-unit-test
integration-test-ci: ci-integration-test
integration-test-ci-build-test-tree: ci-integration-test-build-test-tree
integration-test-ci-prebuilt: ci-integration-test-prebuilt
integration-test-ci-indexer: ci-integration-test-indexer
integration-test-ci-no-build:
	@echo "Deprecated: use integration-test-ci-build-test-tree"
	@$(MAKE) ci-integration-test-build-test-tree

all-test: unit-test integration-test

unit-coverage:
	@echo "Running unit tests with coverage..."
	@for dir in $(PROGRAM_DIRS) $(RUST_DIRS); do \
		$(MAKE) -C $$dir unit-coverage; \
	done

coverage-html:
	@echo "Generating HTML coverage reports..."
	@for dir in $(PROGRAM_DIRS) $(RUST_DIRS); do \
		$(MAKE) -C $$dir coverage-html; \
	done

all-coverage:
	@echo "Running all coverage tasks..."
	@for dir in $(PROGRAM_DIRS) $(RUST_DIRS); do \
		$(MAKE) -C $$dir all-coverage; \
	done

ci-unit-coverage:
	@echo "Running CI unit tests with coverage for core + indexer + gateway..."
	@$(MAKE) -C core unit-coverage
	@$(MAKE) -C indexer unit-coverage
	@$(MAKE) -C gateway unit-coverage

ci-e2e-coverage:
	@echo "Running E2E integration tests with coverage..."
	@$(MAKE) -C integration integration-coverage

#############
# Integration Test Setup
#############
yellowstone-prepare:
	@echo "Building Yellowstone Geyser plugin for Agave 3.0..."
	@mkdir -p integration/.yellowstone-grpc
	@if [ ! -d "integration/.yellowstone-grpc/.git" ]; then \
		echo "Cloning yellowstone-grpc repository..."; \
		git clone https://github.com/rpcpool/yellowstone-grpc.git integration/.yellowstone-grpc; \
	fi
	@echo "Checking out Agave 3.0 compatible commit..."
	@cd integration/.yellowstone-grpc && \
		git fetch origin && \
		git checkout f3d5e041c427f0f383b520c44b231c851d324ddc
	@echo "Applying macOS compatibility fixes..."
	@if [ "$$(uname)" = "Darwin" ]; then \
		echo "Copying macOS-fixed files from test_utils/geyser/mac-files-fix/..."; \
		cp -rf test_utils/geyser/mac-files-fix/yellowstone-grpc-geyser/* \
			integration/.yellowstone-grpc/yellowstone-grpc-geyser/; \
		cp -f test_utils/geyser/mac-files-fix/Cargo.toml \
			integration/.yellowstone-grpc/; \
		echo "macOS fixes applied (affinity -> core_affinity)"; \
	else \
		echo "Skipping macOS fixes (not on macOS)"; \
	fi

yellowstone-build-plugin: yellowstone-prepare
	@echo "Building plugin (this may take a few minutes)..."
	@cd integration/.yellowstone-grpc/yellowstone-grpc-geyser && \
		cargo build --release --no-default-features
	@echo "Copying plugin to test_utils/geyser/..."
	@mkdir -p test_utils/geyser
	@if [ -f integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.dylib ]; then \
		cp integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.dylib \
			test_utils/geyser/libyellowstone_grpc_geyser.dylib; \
		echo "Geyser plugin built: test_utils/geyser/libyellowstone_grpc_geyser.dylib"; \
	elif [ -f integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.so ]; then \
		cp integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.so \
			test_utils/geyser/libyellowstone_grpc_geyser.so; \
		echo "Geyser plugin built: test_utils/geyser/libyellowstone_grpc_geyser.so"; \
	else \
		echo "Error: Plugin binary not found after build"; \
		exit 1; \
	fi

yellowstone-clean:
	@echo "Cleaning Yellowstone Geyser build artifacts..."
	@rm -rf integration/.yellowstone-grpc
	@rm -f test_utils/geyser/libyellowstone_grpc_geyser.dylib
	@rm -f test_utils/geyser/libyellowstone_grpc_geyser.so
	@echo "Geyser artifacts cleaned"

# Backward-compatible aliases.
download-yellowstone-grpc: yellowstone-prepare
build-geyser-plugin: yellowstone-build-plugin
clean-geyser: yellowstone-clean

#############
# Common
#############
generate-operator-keypair:
	@./scripts/ensure-operator-keypair.sh keypairs/operator-keypair.json

#############
# Localnet
#############
build-localnet:
	@echo "Building all programs for localnet..."
	@$(MAKE) -C contra-escrow-program build-localnet
	@$(MAKE) -C contra-withdraw-program build-localnet
	@$(MAKE) generate-operator-keypair
	@./scripts/update-admin-env.sh .env.local keypairs/operator-keypair.json

#############
# Devnet
#############
build-devnet:
	@echo "Building all programs for devnet..."
	@$(MAKE) -C contra-escrow-program build-devnet
	@$(MAKE) -C contra-withdraw-program build-devnet
	@$(MAKE) generate-operator-keypair
	@./scripts/update-admin-env.sh .env.devnet keypairs/operator-keypair.json

deploy-devnet:
	@echo "Deploying all programs to devnet..."
	@$(MAKE) -C contra-escrow-program deploy-devnet DEPLOYER_KEY=$(DEPLOYER_KEY)
	@$(MAKE) -C contra-withdraw-program deploy-devnet DEPLOYER_KEY=$(DEPLOYER_KEY)

profile:
	@echo "Generating CU profiling report..."
	@python3 generate_profiling.py
	@echo "CU profiling report generated: profiling_report.md"

#############
# Observability
#############
obs-up:
	@echo "Starting observability stack (docker-compose.yml)..."
	@docker compose -f docker-compose.yml up -d $(OBS_SERVICES)

obs-down:
	@echo "Stopping observability stack (docker-compose.yml)..."
	@docker compose -f docker-compose.yml stop $(OBS_SERVICES)

obs-logs:
	@docker compose -f docker-compose.yml logs -f --tail=200 $(OBS_SERVICES)

obs-devnet-up:
	@echo "Starting observability stack (docker-compose.devnet.yml)..."
	@docker compose -f docker-compose.devnet.yml up -d $(OBS_SERVICES)

obs-devnet-down:
	@echo "Stopping observability stack (docker-compose.devnet.yml)..."
	@docker compose -f docker-compose.devnet.yml stop $(OBS_SERVICES)

obs-devnet-logs:
	@docker compose -f docker-compose.devnet.yml logs -f --tail=200 $(OBS_SERVICES)

help:
	@echo "Contra Programs - Available targets:"
	@echo ""
	@echo "Dependencies:"
	@echo "  install              - Install dependencies for all projects"
	@echo ""
	@echo "Build:"
	@echo "  build                - Build all projects"
	@echo "  generate-idl         - Generate IDL for all programs"
	@echo "  generate-clients     - Generate clients for all programs"
	@echo ""
	@echo "Code Quality:"
	@echo "  fmt                  - Format all code"
	@echo ""
	@echo "Testing:"
	@echo "  unit-test            - Run unit tests for all projects"
	@echo "  ci-unit-test         - Run CI unit tests for core + indexer"
	@echo "  ci-unit-coverage     - Run CI unit tests with coverage for core + indexer + gateway"
	@echo "  integration-test     - Run integration tests for all projects"
	@echo "  ci-integration-test  - Build prod artifacts, build test-tree, run CI integration suites"
	@echo "  ci-integration-test-build-test-tree - Run prebuilt test, then test-tree indexer integration"
	@echo "  ci-integration-test-prebuilt - Run contra integration using prebuilt production artifacts"
	@echo "  ci-integration-test-indexer - Build test-tree artifact and run indexer integration only"
	@echo "  all-test             - Run all tests for all projects"
	@echo ""
	@echo "Coverage:"
	@echo "  unit-coverage        - Unit test coverage"
	@echo "  ci-e2e-coverage      - E2E integration test coverage"
	@echo "  coverage-html        - Generate HTML coverage reports"
	@echo "  all-coverage         - Run all coverage tasks"
	@echo ""
	@echo "Integration Test Setup:"
	@echo "  yellowstone-prepare      - Download & patch Yellowstone for Agave 3.0"
	@echo "  yellowstone-build-plugin - Build Yellowstone Geyser plugin"
	@echo "  yellowstone-clean        - Clean Geyser build artifacts"
	@echo ""
	@echo "Devnet:"
	@echo "  build-devnet         - Build programs for devnet"
	@echo "  deploy-devnet        - Deploy programs to devnet (requires DEPLOYER_KEY)"
	@echo ""
	@echo "Profiling:"
	@echo "  profile              - Generate CU profiling report"
	@echo ""
	@echo "Observability:"
	@echo "  obs-up               - Start cadvisor/prometheus/grafana (docker-compose.yml)"
	@echo "  obs-down             - Stop cadvisor/prometheus/grafana (docker-compose.yml)"
	@echo "  obs-logs             - Tail observability logs (docker-compose.yml)"
	@echo "  obs-devnet-up        - Start cadvisor/prometheus/grafana (docker-compose.devnet.yml)"
	@echo "  obs-devnet-down      - Stop cadvisor/prometheus/grafana (docker-compose.devnet.yml)"
	@echo "  obs-devnet-logs      - Tail observability logs (docker-compose.devnet.yml)"
