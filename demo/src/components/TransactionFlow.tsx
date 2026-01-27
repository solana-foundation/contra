import React, { useEffect, useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { Send, CheckCircle, XCircle, Loader } from 'lucide-react';
import type { Transaction } from '../types/index';

interface TransactionFlowProps {
  transactions: Transaction[];
}

export const TransactionFlow: React.FC<TransactionFlowProps> = ({ transactions }) => {
  const [visibleTransactions, setVisibleTransactions] = useState<Transaction[]>([]);

  useEffect(() => {
    // Keep only the last 10 transactions visible
    const recent = transactions.slice(-10);
    setVisibleTransactions(recent);
  }, [transactions]);

  const getStatusIcon = (status: Transaction['status']) => {
    switch (status) {
      case 'pending':
        return <Send size={16} />;
      case 'polling':
        return <Loader className="spinner" size={16} />;
      case 'confirmed':
        return <CheckCircle size={16} color="#4ade80" />;
      case 'failed':
        return <XCircle size={16} color="#f87171" />;
    }
  };

  const getStatusColor = (status: Transaction['status']) => {
    switch (status) {
      case 'pending':
        return '#fbbf24';
      case 'polling':
        return '#60a5fa';
      case 'confirmed':
        return '#4ade80';
      case 'failed':
        return '#f87171';
    }
  };

  return (
    <div className="transaction-flow">
      <h3>Transaction Flow</h3>
      <div className="transactions-container">
        <AnimatePresence mode="popLayout">
          {visibleTransactions.map((tx) => (
            <motion.div
              key={tx.id}
              className="transaction-item"
              initial={{ opacity: 0, scale: 0.8, x: -100 }}
              animate={{
                opacity: 1,
                scale: 1,
                x: 0,
                backgroundColor: getStatusColor(tx.status) + '20',
              }}
              exit={{ opacity: 0, scale: 0.8, x: 100 }}
              transition={{ duration: 0.3 }}
            >
              <motion.div
                className="transaction-arrow"
                initial={{ width: 0 }}
                animate={{ width: '100%' }}
                transition={{ duration: 0.5, delay: 0.2 }}
                style={{
                  background: `linear-gradient(90deg, transparent, ${getStatusColor(tx.status)})`,
                }}
              />

              <div className="transaction-details">
                <div className="transaction-parties">
                  <span className="from">{tx.from}</span>
                  <span className="arrow">→</span>
                  <span className="to">{tx.to}</span>
                </div>
                <div className="transaction-amount">{tx.amount.toFixed(2)}</div>
                <div className="transaction-status">
                  {getStatusIcon(tx.status)}
                </div>
              </div>

              {tx.sendLatency && (
                <div className="transaction-latency">
                  {tx.sendLatency}ms
                </div>
              )}

              {tx.pollCount && tx.status === 'confirmed' && (
                <div className="transaction-polls">
                  {tx.pollCount} polls
                </div>
              )}
            </motion.div>
          ))}
        </AnimatePresence>

        {transactions.length === 0 && (
          <div className="no-transactions">
            <p>No transactions yet</p>
            <p className="subtitle">Start the test to see transactions flow</p>
          </div>
        )}
      </div>
    </div>
  );
};