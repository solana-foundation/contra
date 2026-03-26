.PHONY: install integration-test fmt
.PHONY: integration-coverage integration-coverage-contra integration-coverage-indexer integration-coverage-report

COVERAGE_DIR ?= ../coverage

install:
	@echo "Installing integration test dependencies..."
	@cargo install cargo-nextest --locked

# Run integration tests
integration-test:
	@echo "Building escrow with production settings for contra integration tests..."
	@$(MAKE) -C ../contra-escrow-program build
	@echo "Running integration tests..."
	@cargo nextest run --test contra_integration
	@echo "Building escrow with test-tree for indexer/operator-lifecycle tests..."
	@$(MAKE) -C ../contra-escrow-program build-test
	@cargo test --features test-tree --test indexer_integration -- --nocapture
	@echo "Running reconciliation integration tests..."
	@cargo test --test reconciliation_integration -- --nocapture
	@echo "Running mint idempotency integration tests..."
	@cargo test --test mint_idempotency_integration -- --nocapture
	@echo "Running gap detection integration tests..."
	@cargo test --test gap_detection_integration -- --nocapture
	@echo "Running truncate integration tests..."
	@cargo test --test truncate_integration -- --nocapture
	@echo "Running operator lifecycle integration tests (requires test-tree build)..."
	@cargo test --features test-tree --test operator_lifecycle_integration -- --nocapture
	@echo "Running resync integration tests..."
	@cargo test --test resync_integration -- --nocapture
	@echo "Running reconciliation e2e tests..."
	@cargo test --test reconciliation_e2e_test -- --nocapture

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
	@cargo llvm-cov nextest --no-report --workspace --test contra_integration

integration-coverage-indexer:
	@echo "Running indexer integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --features test-tree --test indexer_integration -- --nocapture
	@echo "Running reconciliation integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --test reconciliation_integration -- --nocapture
	@echo "Running mint idempotency integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --test mint_idempotency_integration -- --nocapture
	@echo "Running gap detection integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --test gap_detection_integration -- --nocapture
	@echo "Running truncate integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --test truncate_integration -- --nocapture
	@echo "Running operator lifecycle integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --features test-tree --test operator_lifecycle_integration -- --nocapture
	@echo "Running resync integration tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --test resync_integration -- --nocapture
	@echo "Running reconciliation e2e tests with coverage instrumentation..."
	@cargo llvm-cov test --no-report --workspace --test reconciliation_e2e_test -- --nocapture

integration-coverage-report:
	@echo "Generating combined integration E2E coverage report..."
	@mkdir -p $(COVERAGE_DIR)
	@cargo llvm-cov report --lcov \
		-p contra-core -p contra-indexer -p contra-integration \
		--ignore-filename-regex '(^tests/|/test_utils/|\.cargo/registry|src/bin/|shutdown_utils\.rs|metrics\.rs|test_helpers\.rs|scheduler/greedy\.rs|core/src/client\.rs|accounts/error\.rs|operator/constants\.rs)' \
		--output-path $(COVERAGE_DIR)/coverage-integration-e2e.lcov
	@echo "Coverage report: $(COVERAGE_DIR)/coverage-integration-e2e.lcov"

# Local convenience target: builds the required .so artifacts before running coverage.
# CI calls integration-coverage-contra and integration-coverage-indexer directly,
# with .so files pre-built in a separate job (do NOT add build steps to those targets).
integration-coverage:
	@echo "Building escrow with production settings for contra integration tests..."
	@$(MAKE) -C ../contra-escrow-program build
	@$(MAKE) integration-coverage-contra
	@echo "Building escrow with test-tree for indexer/operator-lifecycle tests..."
	@$(MAKE) -C ../contra-escrow-program build-test
	@$(MAKE) integration-coverage-indexer
	@$(MAKE) integration-coverage-report
