import React from 'react';
import { motion } from 'framer-motion';
import { Wallet as WalletIcon, Loader } from 'lucide-react';
import type { Wallet } from '../types/index';

interface WalletVisualizerProps {
  title: string;
  wallets: Wallet[];
  type: 'sender' | 'receiver';
}

export const WalletVisualizer: React.FC<WalletVisualizerProps> = ({
  title,
  wallets,
  type,
}) => {
  const formatAddress = (address: string) => {
    return `${address.slice(0, 4)}...${address.slice(-4)}`;
  };

  const formatBalance = (balance: number) => {
    if (balance >= 1000000) {
      return `${(balance / 1000000).toFixed(2)}M`;
    } else if (balance >= 1000) {
      return `${(balance / 1000).toFixed(2)}K`;
    } else if (balance >= 100) {
      return balance.toFixed(0);
    } else if (balance >= 1) {
      return balance.toFixed(2);
    } else if (balance > 0) {
      // For very small amounts, show more decimal places
      return balance.toFixed(6);
    }
    return '0';
  };

  return (
    <div className={`wallet-visualizer ${type}`}>
      <h3>{title}</h3>
      <div className="wallets-list">
        {wallets.map((wallet, index) => (
          <motion.div
            key={wallet.id}
            className="wallet-item"
            initial={{ opacity: 0, x: type === 'sender' ? -50 : 50 }}
            animate={{ opacity: 1, x: 0 }}
            transition={{ delay: index * 0.05 }}
          >
            <div className="wallet-icon">
              <WalletIcon size={14} />
            </div>
            <div className="wallet-address">
              {formatAddress(wallet.address.toString())}
            </div>
            <div className="wallet-balance">
              {wallet.isLoading ? (
                <Loader className="spinner" size={12} />
              ) : (
                formatBalance(wallet.balance)
              )}
            </div>
            {wallet.lastUpdate && (
              <motion.div
                className="balance-update-indicator"
                initial={{ scale: 1.5, opacity: 1 }}
                animate={{ scale: 1, opacity: 0 }}
                transition={{ duration: 0.5 }}
              />
            )}
          </motion.div>
        ))}
      </div>
    </div>
  );
};