import { useContext } from 'react';
import { ClusterContext } from '../context/ClusterContext';

export const useCluster = () => {
  const context = useContext(ClusterContext);
  if (!context) {
    throw new Error('useCluster must be used within a ClusterProvider');
  }
  return context;
};
