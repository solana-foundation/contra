import { useState } from 'react';
import { useSolana } from '../hooks/useSolana';
import { address } from '@solana/addresses';
import { findAllowedMintPda, findOperatorPda } from '@contra-escrow';

interface StatusCheckerProps {
  instancePubkey: string;
}

export function StatusChecker({ instancePubkey }: StatusCheckerProps) {
  const { rpc } = useSolana();
  const [mintToCheck, setMintToCheck] = useState('');
  const [operatorToCheck, setOperatorToCheck] = useState('');
  const [mintStatus, setMintStatus] = useState<{ address: string; allowed: boolean } | null>(null);
  const [operatorStatus, setOperatorStatus] = useState<{ address: string; authorized: boolean } | null>(null);
  const [loadingMint, setLoadingMint] = useState(false);
  const [loadingOperator, setLoadingOperator] = useState(false);

  const checkMint = async () => {
    if (!mintToCheck) return;

    try {
      setLoadingMint(true);
      setMintStatus(null);

      const [allowedMintPda] = await findAllowedMintPda({
        instance: address(instancePubkey),
        mint: address(mintToCheck),
      });

      const accountInfo = await rpc.getAccountInfo(address(allowedMintPda)).send();

      setMintStatus({
        address: allowedMintPda.toString(),
        allowed: !!accountInfo.value,
      });
    } catch (error) {
      console.error('Error checking mint:', error);
      alert('Invalid mint address');
    } finally {
      setLoadingMint(false);
    }
  };

  const checkOperator = async () => {
    if (!operatorToCheck) return;

    try {
      setLoadingOperator(true);
      setOperatorStatus(null);

      const [operatorPda] = await findOperatorPda({
        instance: address(instancePubkey),
        wallet: address(operatorToCheck),
      });

      const accountInfo = await rpc.getAccountInfo(address(operatorPda)).send();

      setOperatorStatus({
        address: operatorPda.toString(),
        authorized: !!accountInfo.value,
      });
    } catch (error) {
      console.error('Error checking operator:', error);
      alert('Invalid operator address');
    } finally {
      setLoadingOperator(false);
    }
  };

  return (
    <div className="card">
      <h2>Status Checker</h2>
      <p className="card-description">Verify allowed mints and authorized operators</p>

      <div className="function-section">
        <h3>Check Mint Status</h3>
        <div className="input-group">
          <input
            type="text"
            value={mintToCheck}
            onChange={(e) => setMintToCheck(e.target.value)}
            placeholder="Enter mint address"
            className="input"
          />
          <button
            onClick={checkMint}
            disabled={loadingMint || !mintToCheck}
            className="button button-primary"
          >
            {loadingMint ? 'Checking...' : 'Check'}
          </button>
        </div>
        {mintStatus && (
          <div className={`info-row`} style={{ marginTop: '1rem', padding: '1rem', borderRadius: '8px', backgroundColor: mintStatus.allowed ? 'rgba(76, 175, 80, 0.1)' : 'rgba(255, 152, 0, 0.1)' }}>
            <span className="info-label">Status:</span>
            <span className="info-value" style={{ fontWeight: 'bold', color: mintStatus.allowed ? '#4caf50' : '#ff9800' }}>
              {mintStatus.allowed ? 'ALLOWED' : 'NOT ALLOWED'}
            </span>
          </div>
        )}
      </div>

      <div className="function-section">
        <h3>Check Operator Status</h3>
        <div className="input-group">
          <input
            type="text"
            value={operatorToCheck}
            onChange={(e) => setOperatorToCheck(e.target.value)}
            placeholder="Enter operator address"
            className="input"
          />
          <button
            onClick={checkOperator}
            disabled={loadingOperator || !operatorToCheck}
            className="button button-primary"
          >
            {loadingOperator ? 'Checking...' : 'Check'}
          </button>
        </div>
        {operatorStatus && (
          <div className={`info-row`} style={{ marginTop: '1rem', padding: '1rem', borderRadius: '8px', backgroundColor: operatorStatus.authorized ? 'rgba(76, 175, 80, 0.1)' : 'rgba(255, 152, 0, 0.1)' }}>
            <span className="info-label">Status:</span>
            <span className="info-value" style={{ fontWeight: 'bold', color: operatorStatus.authorized ? '#4caf50' : '#ff9800' }}>
              {operatorStatus.authorized ? 'AUTHORIZED' : 'NOT AUTHORIZED'}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
