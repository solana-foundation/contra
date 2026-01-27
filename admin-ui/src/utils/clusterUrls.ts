export const CLUSTER_URLS = {
  'mainnet-beta': 'https://api.mainnet-beta.solana.com',
  'devnet': 'https://api.devnet.solana.com',
  'testnet': 'https://api.testnet.solana.com',
  'localnet': 'http://127.0.0.1:8899',
} as const;

export const CLUSTER_WS_URLS = {
  'mainnet-beta': 'wss://api.mainnet-beta.solana.com',
  'devnet': 'wss://api.devnet.solana.com',
  'testnet': 'wss://api.testnet.solana.com',
  'localnet': 'ws://127.0.0.1:8900',
} as const;
