.PHONY: build fmt unit-test

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
