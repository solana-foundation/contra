/* eslint-disable react-refresh/only-export-components */
import { StrictMode, useMemo } from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import App from './App.tsx';
import { ClusterProvider } from './ClusterProvider.tsx';
import { ConnectionProvider, WalletProvider } from '@solana/wallet-adapter-react';
import { WalletModalProvider } from '@solana/wallet-adapter-react-ui';
import { PhantomWalletAdapter, SolflareWalletAdapter } from '@solana/wallet-adapter-wallets';
import { useCluster } from './hooks/useCluster';

// Import wallet adapter styles
import '@solana/wallet-adapter-react-ui/styles.css';

function WalletProviders({ children }: { children: React.ReactNode }) {
  const { endpoint } = useCluster();

  const wallets = useMemo(
    () => [
      new PhantomWalletAdapter(),
      new SolflareWalletAdapter(),
    ],
    []
  );

  return (
    <ConnectionProvider endpoint={endpoint}>
      <WalletProvider wallets={wallets} autoConnect>
        <WalletModalProvider>
          {children}
        </WalletModalProvider>
      </WalletProvider>
    </ConnectionProvider>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ClusterProvider>
      <WalletProviders>
        <App />
      </WalletProviders>
    </ClusterProvider>
  </StrictMode>,
);
