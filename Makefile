# Master Makefile for Contra Programs
# Delegates to subdirectory Makefiles

.PHONY: install build fmt generate-idl generate-clients
.PHONY: unit-test unit-test-ci integration-test integration-test-ci integration-test-ci-build-test-tree integration-test-ci-prebuilt integration-test-ci-indexer integration-test-ci-no-build all-test
.PHONY: unit-coverage integration-coverage coverage-html all-coverage
.PHONY: build-devnet deploy-devnet
.PHONY: download-yellowstone-grpc build-geyser-plugin clean-geyser
.PHONY: profile help

# Default target
all: build

# Common targets that run on all projects (escrow + withdraw + indexer)
install:
	@echo "📦 Installing dependencies for all projects..."
	@$(MAKE) -C contra-escrow-program install
	@$(MAKE) -C contra-withdraw-program install

build:
	@echo "🔨 Building all projects..."
	@$(MAKE) -C contra-escrow-program build
	@$(MAKE) -C contra-withdraw-program build
	@$(MAKE) -C core build
	@$(MAKE) -C indexer build

fmt:
	@echo "✨ Formatting all projects..."
	@$(MAKE) -C contra-escrow-program fmt
	@$(MAKE) -C contra-withdraw-program fmt
	@$(MAKE) -C core fmt
	@$(MAKE) -C indexer fmt
	@$(MAKE) -C integration fmt
	@cd scripts/devnet && cargo fmt

generate-idl:
	@echo "📝 Generating IDL for all programs..."
	@$(MAKE) -C contra-escrow-program generate-idl
	@$(MAKE) -C contra-withdraw-program generate-idl

generate-clients:
	@echo "🔧 Generating clients for all programs..."
	@$(MAKE) -C contra-escrow-program generate-clients
	@$(MAKE) -C contra-withdraw-program generate-clients

unit-test:
	@echo "🧪 Running unit tests for all projects..."
	@$(MAKE) -C contra-escrow-program unit-test
	@$(MAKE) -C contra-withdraw-program unit-test
	@$(MAKE) -C core unit-test
	@$(MAKE) -C indexer unit-test

# CI-focused unit tests for non-program crates.
# Program unit tests run in a dedicated workflow.
unit-test-ci:
	@echo "🧪 Running CI unit tests for core + indexer..."
	@$(MAKE) -C core unit-test
	@$(MAKE) -C indexer unit-test

integration-test:
	@echo "🔗 Running integration tests for all projects..."
	@$(MAKE) -C contra-escrow-program integration-test
	@$(MAKE) -C contra-withdraw-program integration-test
	@echo "🔗 Running contra integration test (with production build)..."
	@cd integration && cargo test --test contra_integration -- --nocapture
	@echo "🔗 Building escrow with test-tree for indexer tests..."
	@$(MAKE) -C contra-escrow-program build-test
	@echo "🔗 Running indexer integration test (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test indexer_integration -- --nocapture

# CI-focused integration target that avoids running program integration
# suites already covered in the dedicated program workflow.
integration-test-ci:
	@echo "🔗 Running CI integration tests (non-program suites)..."
	@echo "🔨 Building program artifacts once for integration crate tests..."
	@$(MAKE) -C contra-escrow-program build
	@$(MAKE) -C contra-withdraw-program build
	@$(MAKE) integration-test-ci-build-test-tree

# CI-focused integration target that assumes production program artifacts
# and generated clients are already available.
integration-test-ci-build-test-tree:
	@$(MAKE) integration-test-ci-prebuilt
	@echo "🔗 Building escrow with test-tree for indexer tests..."
	@$(MAKE) -C contra-escrow-program build-test
	@echo "🔗 Running indexer integration test (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test indexer_integration -- --nocapture

# CI-focused integration target that runs production-artifact integration tests only.
integration-test-ci-prebuilt:
	@echo "🔗 Running contra integration test (with production build)..."
	@cd integration && cargo test --test contra_integration -- --nocapture

# CI-focused integration target that runs indexer integration tests only.
integration-test-ci-indexer:
	@echo "🔗 Building escrow with test-tree for indexer tests..."
	@$(MAKE) -C contra-escrow-program build-test
	@echo "🔗 Running indexer integration test (with test-tree build)..."
	@cd integration && cargo test --features test-tree --test indexer_integration -- --nocapture

# Backward-compatible alias for historical target name.
integration-test-ci-no-build:
	@echo "⚠️  Deprecated: use integration-test-ci-build-test-tree"
	@$(MAKE) integration-test-ci-build-test-tree

all-test: unit-test integration-test

unit-coverage:
	@echo "📊 Running unit tests with coverage..."
	@$(MAKE) -C contra-escrow-program unit-coverage
	@$(MAKE) -C contra-withdraw-program unit-coverage

integration-coverage:
	@echo "📊 Running integration tests with coverage..."
	@$(MAKE) -C contra-escrow-program integration-coverage
	@$(MAKE) -C contra-withdraw-program integration-coverage

coverage-html:
	@echo "📊 Generating HTML coverage reports..."
	@$(MAKE) -C contra-escrow-program coverage-html
	@$(MAKE) -C contra-withdraw-program coverage-html

all-coverage:
	@echo "📊 Running all coverage tasks..."
	@$(MAKE) -C contra-escrow-program all-coverage
	@$(MAKE) -C contra-withdraw-program all-coverage

#############
# Integration Test Setup
#############
download-yellowstone-grpc:
	@echo "🔧 Building Yellowstone Geyser plugin for Agave 3.0..."
	@mkdir -p integration/.yellowstone-grpc
	@if [ ! -d "integration/.yellowstone-grpc/.git" ]; then \
		echo "📦 Cloning yellowstone-grpc repository..."; \
		git clone https://github.com/rpcpool/yellowstone-grpc.git integration/.yellowstone-grpc; \
	fi
	@echo "🔀 Checking out Agave 3.0 compatible commit..."
	@cd integration/.yellowstone-grpc && \
		git fetch origin && \
		git checkout f3d5e041c427f0f383b520c44b231c851d324ddc
	@echo "🔧 Applying macOS compatibility fixes..."
	@if [ "$$(uname)" = "Darwin" ]; then \
		echo "Copying macOS-fixed files from test_utils/geyser/mac-files-fix/..."; \
		cp -rf test_utils/geyser/mac-files-fix/yellowstone-grpc-geyser/* \
			integration/.yellowstone-grpc/yellowstone-grpc-geyser/; \
		cp -f test_utils/geyser/mac-files-fix/Cargo.toml \
			integration/.yellowstone-grpc/; \
		echo "✅ macOS fixes applied (affinity → core_affinity)"; \
	else \
		echo "⏭️  Skipping macOS fixes (not on macOS)"; \
	fi

build-geyser-plugin:
	@echo "🔨 Building plugin (this may take a few minutes)..."
	@cd integration/.yellowstone-grpc/yellowstone-grpc-geyser && \
		cargo build --release --no-default-features
	@echo "📋 Copying plugin to test_utils/geyser/..."
	@mkdir -p test_utils/geyser
	@if [ -f integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.dylib ]; then \
		cp integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.dylib \
			test_utils/geyser/libyellowstone_grpc_geyser.dylib; \
		echo "✅ Geyser plugin built: test_utils/geyser/libyellowstone_grpc_geyser.dylib"; \
	elif [ -f integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.so ]; then \
		cp integration/.yellowstone-grpc/target/release/libyellowstone_grpc_geyser.so \
			test_utils/geyser/libyellowstone_grpc_geyser.so; \
		echo "✅ Geyser plugin built: test_utils/geyser/libyellowstone_grpc_geyser.so"; \
	else \
		echo "❌ Error: Plugin binary not found after build"; \
		exit 1; \
	fi

clean-geyser:
	@echo "🧹 Cleaning Yellowstone Geyser build artifacts..."
	@rm -rf integration/.yellowstone-grpc
	@rm -f test_utils/geyser/libyellowstone_grpc_geyser.dylib
	@rm -f test_utils/geyser/libyellowstone_grpc_geyser.so
	@echo "✅ Geyser artifacts cleaned"

#############
# Common
#############
generate-operator-keypair:
	@if [ ! -f keypairs/operator-keypair.json ]; then \
		echo "🔑 Generating operator keypair..."; \
		mkdir -p keypairs; \
		solana-keygen new -o keypairs/operator-keypair.json -s --no-bip39-passphrase; \
		echo "✅ Operator keypair generated at keypairs/operator-keypair.json"; \
	else \
		echo "✅ Operator keypair already exists at keypairs/operator-keypair.json"; \
	fi

#############
# Localnet
#############
build-localnet:
	@echo "🚀 Building all programs for localnet..."
	@$(MAKE) -C contra-escrow-program build-localnet
	@$(MAKE) -C contra-withdraw-program build-localnet
	@$(MAKE) generate-operator-keypair
	@echo "📝 Updating .env.local with operator pubkey and private key..."
	@OPERATOR_PUBKEY=$$(solana-keygen pubkey keypairs/operator-keypair.json); \
	OPERATOR_PRIVATE_KEY=$$(cat keypairs/operator-keypair.json); \
	if [ -f .env.local ]; then \
		sed -i.bak "s/^CONTRA_ADMIN_KEYS=.*/CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY/" .env.local && \
		sed -i.bak "s|^ADMIN_PRIVATE_KEY=.*|ADMIN_PRIVATE_KEY=$$OPERATOR_PRIVATE_KEY|" .env.local && \
		rm .env.local.bak; \
		echo "✅ Updated .env.local with CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY"; \
		echo "✅ Updated .env.local with ADMIN_PRIVATE_KEY"; \
	else \
		echo "CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY" >> .env.local; \
		echo "ADMIN_PRIVATE_KEY=$$OPERATOR_PRIVATE_KEY" >> .env.local; \
		echo "✅ Created .env.local with CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY"; \
		echo "✅ Created .env.local with ADMIN_PRIVATE_KEY"; \
	fi

#############
# Devnet
#############
build-devnet:
	@echo "🚀 Building all programs for devnet..."
	@$(MAKE) -C contra-escrow-program build-devnet
	@$(MAKE) -C contra-withdraw-program build-devnet
	@$(MAKE) generate-operator-keypair
	@echo "📝 Updating .env.devnet with operator pubkey and private key..."
	@OPERATOR_PUBKEY=$$(solana-keygen pubkey keypairs/operator-keypair.json); \
	OPERATOR_PRIVATE_KEY=$$(cat keypairs/operator-keypair.json); \
	if [ -f .env.devnet ]; then \
		sed -i.bak "s/^CONTRA_ADMIN_KEYS=.*/CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY/" .env.devnet && \
		sed -i.bak "s|^ADMIN_PRIVATE_KEY=.*|ADMIN_PRIVATE_KEY=$$OPERATOR_PRIVATE_KEY|" .env.devnet && \
		rm .env.devnet.bak; \
		echo "✅ Updated .env.devnet with CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY"; \
		echo "✅ Updated .env.devnet with ADMIN_PRIVATE_KEY"; \
	else \
		echo "CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY" >> .env.devnet; \
		echo "ADMIN_PRIVATE_KEY=$$OPERATOR_PRIVATE_KEY" >> .env.devnet; \
		echo "✅ Created .env.devnet with CONTRA_ADMIN_KEYS=$$OPERATOR_PUBKEY"; \
		echo "✅ Created .env.devnet with ADMIN_PRIVATE_KEY"; \
	fi

deploy-devnet:
	@echo "🚀 Deploying all programs to devnet..."
	@$(MAKE) -C contra-escrow-program deploy-devnet DEPLOYER_KEY=$(DEPLOYER_KEY)
	@$(MAKE) -C contra-withdraw-program deploy-devnet DEPLOYER_KEY=$(DEPLOYER_KEY)

profile:
	@echo "🔥 Generating CU profiling report..."
	python3 generate_profiling.py
	@echo "✅ CU profiling report generated: profiling_report.md"

help:
	@echo "Contra Programs - Available targets:"
	@echo ""
	@echo "📦 Dependencies:"
	@echo "  install              - Install dependencies for all projects"
	@echo ""
	@echo "🔨 Build:"
	@echo "  build                - Build all programs"
	@echo "  generate-idl         - Generate IDL for all programs"
	@echo "  generate-clients     - Generate clients for all programs"
	@echo ""
	@echo "✨ Code Quality:"
	@echo "  fmt                  - Format all code"
	@echo ""
	@echo "🧪 Testing:"
	@echo "  unit-test            - Run unit tests for all projects"
	@echo "  unit-test-ci         - Run CI unit tests for core + indexer"
	@echo "  integration-test     - Run integration tests for all projects"
	@echo "  integration-test-ci  - Build prod artifacts, build test-tree, and run CI integration suites"
	@echo "  integration-test-ci-build-test-tree - Run contra integration, then build test-tree artifact and run indexer integration"
	@echo "  integration-test-ci-prebuilt - Run contra integration using prebuilt production artifacts only"
	@echo "  integration-test-ci-indexer - Build test-tree artifact and run indexer integration only"
	@echo "  integration-test-ci-no-build - Deprecated alias to integration-test-ci-build-test-tree"
	@echo "  all-test             - Run all tests for all projects"
	@echo ""
	@echo "📊 Coverage:"
	@echo "  unit-coverage        - Unit test coverage"
	@echo "  integration-coverage - Integration test coverage"
	@echo "  coverage-html        - Generate HTML coverage reports"
	@echo "  all-coverage         - Run all coverage tasks"
	@echo ""
	@echo "🔧 Integration Test Setup:"
	@echo "  download-yellowstone-grpc - Download & patch Yellowstone for Agave 3.0"
	@echo "  build-geyser-plugin       - Build Yellowstone Geyser plugin"
	@echo "  clean-geyser              - Clean Geyser build artifacts"
	@echo ""
	@echo "🚀 Devnet:"
	@echo "  build-devnet         - Build programs for devnet"
	@echo "  deploy-devnet        - Deploy programs to devnet (requires DEPLOYER_KEY)"
	@echo ""
	@echo "🔥 Profiling:"
	@echo "  profile              - Generate CU profiling report"
