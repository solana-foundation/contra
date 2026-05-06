.PHONY: build fmt unit-test
.PHONY: unit-coverage coverage-html all-coverage

# Build the private-channel-gateway binary
build:
	@echo "Building private-channel-gateway..."
	@cargo build --release

# Format and lint code
fmt:
	@echo "Formatting private-channel-gateway..."
	@cargo fmt --all
	@cargo clippy --all-targets -- -D warnings

# Run unit tests
unit-test:
	@echo "Running unit tests for private-channel-gateway..."
	@cargo test --lib

# Run unit tests with coverage
unit-coverage:
	@echo "Running unit tests with coverage for private-channel-gateway..."
	@mkdir -p ../coverage
	@cargo llvm-cov --lib --tests --lcov --output-path ../coverage/coverage-gateway-unit.lcov

# Generate HTML coverage report
coverage-html:
	@echo "Generating HTML coverage report for private-channel-gateway..."
	@mkdir -p ../coverage
	@cargo llvm-cov --lib --tests --html --output-dir ../coverage/coverage-gateway-html

# Run all coverage tasks
all-coverage: unit-coverage coverage-html
