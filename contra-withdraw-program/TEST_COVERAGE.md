# Withdraw Program — Test Coverage Analysis

> This is a **semantic coverage estimate** produced by analyzing test assertions
> against the program's testable surface. It is not instrumented line coverage —
> Solana SBF programs do not support LLVM coverage instrumentation.

## Summary

| Category | Coverage | Details |
|----------|----------|---------|
| Instruction handlers | 100% (1/1) | WithdrawFunds tested |
| Account validation paths | 40% (2/5) | Signer + mint validated; ATA/token/ATA-program untested |
| Business logic error branches | 60% (3/5) | Zero amount, insufficient funds, wrong mint tested |
| Custom error codes exercised | 100% (2/2) | InvalidMint, ZeroAmount |
| State & trait coverage (unit) | 0% (0/0) | No unit tests exist |
| Event coverage | 0% (0/1) | Event emission not verified |
| Security edge cases | 50% (1/2) | Non-signer tested; program substitution untested |
| **Overall (risk-weighted)** | **~45%** | |

## Test Inventory

**0 unit tests** + **7 integration tests** (LiteSVM) + **7 TypeScript SDK tests**.

### WithdrawFunds — Integration Tests (7 tests)

#### Happy Path
- `test_withdraw_funds_success` — basic withdrawal, balance verified
- `test_withdraw_funds_with_destination` — optional destination parameter

#### Error Paths
- `test_withdraw_funds_insufficient_funds` — SPL Token insufficient funds
- `test_withdraw_funds_zero_amount` — ZeroAmount custom error
- `test_withdraw_funds_invalid_instruction_data_too_short` — malformed data rejected
- `test_withdraw_funds_wrong_mint` — InvalidMint custom error
- `test_withdraw_funds_non_signer_user` — MissingRequiredSignature

### WithdrawFunds — TypeScript SDK Tests (7 tests)

#### Instruction Data Validation (4 tests)
- Encodes discriminator, amount, and destination correctly
- Handles u64 amounts (0, 1, 1M, 1B, max safe integer, max u64)
- Handles optional destination (None/Some variants)
- Round-trip encode/decode verification

#### Account Requirements (3 tests)
- All 5 required accounts present in correct order
- Account permissions correct (READONLY_SIGNER, WRITABLE, READONLY)
- Program addresses correct (contra program, token program, ATA program)

## Documented Gaps

### Missing Unit Tests
The withdraw program has **no `#[cfg(test)]` blocks**. The following should be added:
- `process_instruction_data` parsing — valid data with/without destination
- `process_instruction_data` — insufficient length, empty data
- `validate_ata` — PDA derivation, empty data check
- `verify_signer` / `verify_mint_account` — error path isolation
- `WithdrawFundsEvent::to_bytes` — event encoding

### Untested Error Paths
| Error Path | Status | Notes |
|-----------|--------|-------|
| NotEnoughAccountKeys | Not tested | No test with insufficient accounts |
| InvalidAccountOwner | Not tested | Token program owner check on mint |
| IncorrectProgramId | Not tested | Wrong ATA or token program address |
| InvalidSeeds (ATA derivation) | Not tested | ATA PDA mismatch |

### Untested Account Validations
| Validation | Function | Status |
|-----------|----------|--------|
| ATA program address | `verify_ata_program` | Not tested |
| Token program address | `verify_token_program` | Not tested |
| Token program account ownership | `verify_token_program_account` | Not tested |
| ATA PDA derivation | `validate_ata` | Not tested |
| ATA non-empty data check | `validate_ata` | Not tested |

### Untested Business Logic
- Event emission and encoding — no log verification
- Invalid discriminator routing — no test for wrong instruction type
- Token burn failure propagation — only insufficient funds tested
- Boundary amounts — max u64 withdrawal not tested on-chain
- Token2022 support — not tested (if applicable)

### Priority Recommendations
1. **High**: Add unit tests for instruction data parsing and account validation helpers
2. **High**: Add integration test for wrong ATA/token program addresses
3. **Medium**: Add integration test for insufficient accounts
4. **Medium**: Add Token2022 withdrawal test
5. **Low**: Add event emission verification
