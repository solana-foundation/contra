.PHONY: integration-test fmt

# Run integration tests
integration-test:
	@echo "Running integration tests..."
	@cargo test --test contra_integration -- --nocapture
	@cargo test --features test-tree --test indexer_integration -- --nocapture

# Format and lint code
fmt:
	@echo "Formatting integration tests..."
	@cargo fmt --all
	@cargo clippy --all-targets -- -D warnings
