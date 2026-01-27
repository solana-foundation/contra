import { createContext } from 'react';
import { createSolanaRpc } from '@solana/rpc';
import { createSolanaRpcSubscriptions } from '@solana/rpc-subscriptions';

export interface SolanaContextType {
  rpc: ReturnType<typeof createSolanaRpc>;
  rpcSubscriptions: ReturnType<typeof createSolanaRpcSubscriptions>;
}

export const SolanaContext = createContext<SolanaContextType | null>(null);
