.PHONY: build fmt unit-test
.PHONY: unit-coverage coverage-html all-coverage

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
	@mkdir -p ../coverage
	@cargo llvm-cov --lib --lcov --output-path ../coverage/coverage-core-unit.lcov

# Generate HTML coverage report
coverage-html:
	@echo "Generating HTML coverage report for contra-core..."
	@mkdir -p ../coverage
	@cargo llvm-cov --lib --html --output-dir ../coverage/coverage-core-html

# Run all coverage tasks
all-coverage: unit-coverage coverage-html
