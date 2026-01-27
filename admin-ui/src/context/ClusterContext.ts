import { createContext } from 'react';

export type NetworkType = 'mainnet-beta' | 'devnet' | 'testnet' | 'localnet';

export interface ClusterContextType {
  network: NetworkType;
  endpoint: string;
  wsEndpoint: string;
  setNetwork: (network: NetworkType) => void;
  customEndpoint: string;
  setCustomEndpoint: (endpoint: string) => void;
}

export const ClusterContext = createContext<ClusterContextType>({} as ClusterContextType);
