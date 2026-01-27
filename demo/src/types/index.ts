import { PublicKey } from '@solana/web3.js';

export interface Wallet {
  id: string;
  address: PublicKey;
  balance: number;
  isLoading: boolean;
  lastUpdate: number;
}

export interface Transaction {
  id: string;
  from: string;
  to: string;
  amount: number;
  signature: string;
  status: 'pending' | 'polling' | 'confirmed' | 'failed';
  timestamp: number;
  sendLatency?: number;
  pollCount?: number;
}

export interface TestParams {
  users: number;
  duration: number;
  requestDelay: number;
}

export interface TestStatistics {
  totalTransactions: number;
  confirmedTransactions: number;
  failedTransactions: number;
  averageSendLatency: number;
  throughput: number;
  maxThroughput: number;
  rps: number;  // Current requests per second
  maxRps: number;  // Maximum RPS achieved
  progress: number;  // Progress percentage (0-100)
  transactionsWithLatency?: number;
  startTime?: number;
  endTime?: number;
}