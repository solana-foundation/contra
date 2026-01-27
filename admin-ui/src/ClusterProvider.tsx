import { useState, useMemo } from 'react';
import type { ReactNode } from 'react';
import { ClusterContext, type NetworkType } from './context/ClusterContext';
// Cluster URLs for the new SDK
import { CLUSTER_URLS, CLUSTER_WS_URLS } from './utils/clusterUrls';

export function ClusterProvider({ children }: { children: ReactNode }) {
  const [network, setNetwork] = useState<NetworkType>('devnet');
  const [customEndpoint, setCustomEndpoint] = useState<string>('');

  const endpoint = useMemo(() => {
    if (customEndpoint) return customEndpoint;
    return CLUSTER_URLS[network];
  }, [network, customEndpoint]);

  const wsEndpoint = useMemo(() => {
    if (customEndpoint) {
      return customEndpoint.replace('https://', 'wss://').replace('http://', 'ws://');
    }
    return CLUSTER_WS_URLS[network];
  }, [network, customEndpoint]);

  return (
    <ClusterContext.Provider value={{ network, endpoint, wsEndpoint, setNetwork, customEndpoint, setCustomEndpoint }}>
      {children}
    </ClusterContext.Provider>
  );
}


