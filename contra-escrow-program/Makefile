.PHONY: install build fmt generate-idl generate-clients
.PHONY: unit-test integration-test all-test
.PHONY: unit-coverage integration-coverage coverage-html all-coverage
.PHONY: build-devnet deploy-devnet

# Install dependencies
install:
	pnpm install

# Build the program
build:
	make generate-idl
	make generate-clients
	cargo-build-sbf

# Build the program with test-tree feature for integration tests
build-test:
	make generate-idl
	make generate-clients
	cd program && cargo-build-sbf --features test-tree

# Format / lint code
fmt:
	cargo fmt --all
	@cd program && cargo clippy --all-targets -- -D warnings
	@cd tests/integration-tests && cargo clippy --all-targets -- -D warnings
	pnpm format

# Generate IDL from Shank annotations
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

# Run integration tests (includes build)
integration-test: build
	@echo "Running integration tests for escrow program..."
	@cd tests/integration-tests && cargo test -- --nocapture

all-test: unit-test integration-test

# Run unit tests with coverage
unit-coverage:
	@echo "Running unit tests with coverage..."
	@cd program && cargo llvm-cov --lib --tests --lcov --output-path ../../coverage-escrow-unit.lcov

# Run integration tests with coverage
integration-coverage: build
	@echo "Running integration tests with coverage..."
	@cd tests/integration-tests && cargo llvm-cov --lcov --output-path ../../../coverage-escrow-integration.lcov -- --nocapture

# Generate HTML coverage report
coverage-html:
	@echo "Generating HTML coverage report..."
	@cd program && cargo llvm-cov --html --output-dir ../../coverage-escrow-html

# Run all coverage tasks
all-coverage: unit-coverage integration-coverage coverage-html

#############
# Localnet
#############
build-localnet:
	make generate-idl
	make generate-clients
	cargo-build-sbf
	@echo "Updating .env.local with program IDs..."
	@ESCROW_ID=$$(solana-keygen pubkey ../target/deploy/contra_escrow_program-keypair.json); \
	if [ -f ../.env.local ]; then \
		sed -i.bak "s/^ESCROW_PROGRAM_ID=.*/ESCROW_PROGRAM_ID=$$ESCROW_ID/" ../.env.local && \
		rm ../.env.local.bak; \
		echo "✅ Updated .env.local with ESCROW_PROGRAM_ID=$$ESCROW_ID"; \
	else \
		echo "ESCROW_PROGRAM_ID=$$ESCROW_ID" > ../.env.local; \
		echo "✅ Created .env.local with ESCROW_PROGRAM_ID=$$ESCROW_ID"; \
	fi

#############
# Devnet
#############

# Build the program with devnet feature
build-devnet:
	make generate-idl
	make generate-clients
	cargo-build-sbf

# Deploy to devnet
deploy-devnet: build-devnet
	@echo "Deploying to devnet..."
	@if [ -z "$(DEPLOYER_KEY)" ]; then \
		echo "Error: DEPLOYER_KEY parameter is required. Usage: make deploy-devnet DEPLOYER_KEY=/path/to/deployer-keypair.json"; \
		exit 1; \
	fi
	solana program deploy ../target/deploy/contra_escrow_program.so \
		--program-id ../target/deploy/contra_escrow_program-keypair.json \
		--keypair $(DEPLOYER_KEY) \
		--url devnet

