.PHONY: build fmt unit-test
.PHONY: unit-coverage coverage-html

# Build the contra-core library and binaries
build:
	@echo "Building contra-core..."
	@cargo build --release

# Format and lint code
fmt:
	@echo "Formatting contra-core..."
	@cargo fmt --all
	@cargo clippy --all-targets -- -D warnings

# Run unit tests
unit-test:
	@echo "Running unit tests for contra-core..."
	@cargo test --lib

# Run unit tests with coverage
unit-coverage:
	@echo "Running unit tests with coverage for contra-core..."
	@cargo llvm-cov --lib --lcov --output-path ../coverage/coverage-core-unit.lcov

# Generate HTML coverage report
coverage-html:
	@echo "Generating HTML coverage report for contra-core..."
	@cargo llvm-cov --lib --html --output-dir ../coverage/coverage-core-html
