.PHONY: build fmt unit-test
.PHONY: unit-coverage coverage-html all-coverage

# Build the private-channel-core library and binaries
build:
	@echo "Building private-channel-core..."
	@cargo build --release

# Format and lint code
fmt:
	@echo "Formatting private-channel-core..."
	@cargo fmt --all
	@cargo clippy --all-targets -- -D warnings

# Run unit tests
unit-test:
	@echo "Running unit tests for private-channel-core..."
	@cargo test --lib

# Files excluded from coverage: thin RPC wrappers, error boilerplate, and infra
# that are better validated by integration tests than unit tests.
COV_IGNORE := --ignore-filename-regex 'accounts/(error|redis)\.rs|rpc/(error|rpc_impl|server|.*_impl)\.rs'

# Run unit tests with coverage
unit-coverage:
	@echo "Running unit tests with coverage for private-channel-core..."
	@mkdir -p ../coverage
	@cargo llvm-cov --lib --lcov --output-path ../coverage/coverage-core-unit.lcov $(COV_IGNORE)

# Generate HTML coverage report
coverage-html:
	@echo "Generating HTML coverage report for private-channel-core..."
	@mkdir -p ../coverage
	@cargo llvm-cov --lib --html --output-dir ../coverage/coverage-core-html $(COV_IGNORE)

# Run all coverage tasks
all-coverage: unit-coverage coverage-html
