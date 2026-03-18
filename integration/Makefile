.PHONY: integration-test fmt
.PHONY: integration-coverage integration-coverage-contra integration-coverage-indexer integration-coverage-report

COVERAGE_DIR ?= ../coverage

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

###################################
# Coverage
###################################
# NOTE: Integration tests require pre-built program .so files in target/deploy/.
# CI builds these separately; do NOT rebuild them here.
#
# Uses --no-report for individual runs, then combines with cargo llvm-cov report.
# Excludes test/test_utils code so we measure coverage of core/indexer source only.

integration-coverage-contra:
	@echo "Running contra integration tests with coverage instrumentation..."
	@cargo llvm-cov clean --workspace
	@cargo llvm-cov test --no-report --workspace --test contra_integration -- --nocapture

integration-coverage-indexer:
	@echo "Running indexer integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --features test-tree --test indexer_integration -- --nocapture

integration-coverage-report:
	@echo "Generating combined integration E2E coverage report..."
	@mkdir -p $(COVERAGE_DIR)
	@cargo llvm-cov report --lcov \
		-p contra-core -p contra-indexer -p contra-integration \
		--ignore-filename-regex '(^tests/|/test_utils/|\.cargo/registry)' \
		--output-path $(COVERAGE_DIR)/coverage-integration-e2e.lcov
	@echo "Coverage report: $(COVERAGE_DIR)/coverage-integration-e2e.lcov"

integration-coverage: integration-coverage-contra integration-coverage-indexer integration-coverage-report
