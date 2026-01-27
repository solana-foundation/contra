# Devnet Setup Commands

## Prerequisites
```bash
cp .env.example .env
# Edit .env with your keys
```

Run all commands from project root:

## 1. Create Instance
```bash
cargo run --bin create_instance -- \
  https://api.devnet.solana.com \
  ./keypairs/admin.json
```

## 2. Add Operator
```bash
cargo run --bin add_operator -- \
  https://api.devnet.solana.com \
  ./keypairs/admin.json \
  <INSTANCE_ID> \
  <OPERATOR_PUBKEY>
```

## 3. Allow Mint
```bash
cargo run --bin allow_mint -- \
  https://api.devnet.solana.com \
  ./keypairs/admin.json \
  <INSTANCE_ID> \
  <MINT_ADDRESS>
```

## 4. Deposit (Solana → Contra)
```bash
cargo run --bin deposit -- \
  https://api.devnet.solana.com \
  ./keypairs/user.json \
  <INSTANCE_ID> \
  <MINT_ADDRESS> \
  <AMOUNT>
```

## 5. Withdraw (Contra → Solana)
```bash
cargo run --bin withdraw -- \
  http://localhost:8898 \
  ./keypairs/user.json \
  <MINT_ADDRESS> \
  <AMOUNT>
```

## Monitor
```bash
# Watch deposit processing
docker logs -f contra-operator-solana

# Watch withdrawal processing
docker logs -f contra-operator-contra
```
