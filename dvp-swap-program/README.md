# DvP Swap Program (vendored)

The DvP swap program's canonical source now lives in
[`solana-foundation/dvp`](https://github.com/solana-foundation/dvp). This
directory is **not** the program source: it is a vendored snapshot of the two
artifacts this repo consumes at build and runtime.

- `clients/rust/` — the generated Rust client crate `dvp-swap-program-client`
  (program ID, instruction builders, `SwapDvp` account layout). `core`,
  `gateway`, and `integration` depend on it by path. The generated code under
  `clients/rust/src/generated/` is committed here (it is git-ignored in the
  upstream repo, where it is regenerated on demand).
- `../core/precompiles/dvp_swap_program.so` — the compiled program, embedded
  into the node runtime as a precompile (`core/src/accounts/precompiles.rs`).

## Program ID

`dvp34bdbcEm4f4FCUjGV4mDAkDshaQR4LkK8fdcsyZq` — the devnet deployment. The
mainnet ID will differ and is not set here yet.

## Re-syncing from upstream

Vendored at `solana-foundation/dvp` commit
`39bf82373a6311fd42cc5b9343adc4568da60ef2`. To refresh:

```bash
# in a checkout of solana-foundation/dvp
make generate-clients                 # regenerate the Rust/TS clients from the IDL
(cd program && cargo-build-sbf)       # build target/deploy/dvp_swap_program.so

# then, in this repo
cp -R <dvp>/clients/rust/src/.  dvp-swap-program/clients/rust/src/
cp    <dvp>/clients/rust/Cargo.toml dvp-swap-program/clients/rust/Cargo.toml
cp    <dvp>/target/deploy/dvp_swap_program.so core/precompiles/dvp_swap_program.so
```

If the `SwapDvp` layout changes, update the hand-mirrored size and field offsets
in `gateway/src/auth.rs` (`SWAP_DVP_SIZE`, `SWAP_DVP_OWNER_FIELDS`) to match.
