import { useMemo } from 'react';
import { useWallet } from './useWallet';
import { useWallets } from '@wallet-standard/react';
import type { UiWalletAccount } from '@wallet-standard/react';

/**
 * Bridge hook that gets the wallet-standard account for the currently connected
 * Anza wallet adapter wallet
 */
export function useWalletStandardAccount(): UiWalletAccount | null {
  const { publicKey, wallet: adapterWallet } = useWallet();
  const standardWallets = useWallets();

  return useMemo(() => {
    if (!publicKey || !adapterWallet) return null;

    // Find the matching wallet-standard wallet by name
    const standardWallet = standardWallets.find(w =>
      w.name === adapterWallet.adapter?.name
    );

    if (!standardWallet || !standardWallet.accounts || standardWallet.accounts.length === 0) {
      return null;
    }

    // Return the first account from the wallet-standard wallet
    return standardWallet.accounts[0];
  }, [publicKey, adapterWallet, standardWallets]);
}
