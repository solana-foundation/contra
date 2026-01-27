# Contra Integration Tests

This package contains integration tests for the Contra stack.

## Structure

- `tests/integration.rs` - Main integration test file with end-to-end tests
- `tests/helpers.rs` - Helper functions for RPC interactions
- `tests/setup.rs` - Transaction builder helpers

## Running Tests

Run all integration tests:
```bash
cargo test -p contra-integration
```

Run a specific test:
```bash
cargo test -p contra-integration test_with_redis
cargo test -p contra-integration test_with_postgres
```

Run with output:
```bash
cargo test -p contra-integration -- --nocapture
```

## Test Coverage

The integration tests cover:

1. **SPL Token Operations**
   - Mint creation and initialization
   - Token account creation
   - Minting tokens
   - Token transfers
   - Fund withdrawals

2. **Security Validation**
   - Non-admin users cannot send admin instructions (InitializeMint)
   - Empty transactions are rejected
   - Mixed transactions (admin + non-admin instructions) are rejected

3. **Transaction Replay Protection**
   - Duplicate transactions with same blockhash are only executed once

4. **Database Backends**
   - Tests run against both PostgreSQL and Redis backends

## Future Enhancements

The next goal is to extend these tests to include the indexer and other components of the Contra stack for full end-to-end testing.
