import { useContext } from 'react';
import { SolanaContext } from '../context/SolanaContext';

export function useSolana() {
  const context = useContext(SolanaContext);
  if (!context) {
    throw new Error('useSolana must be used within SolanaProvider');
  }
  return context;
}
