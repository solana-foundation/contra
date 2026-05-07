import { useState } from 'react';
import { useSolana } from '../hooks/useSolana';
import { useWalletStandardAccount } from '../hooks/useWalletStandardAccount';
import { useCluster } from '../hooks/useCluster';
import { address } from '@solana/addresses';
import type { Address } from '@solana/addresses';
import { decodeInstance, getCreateInstanceInstructionAsync } from '@private-channel-escrow';
import { generateKeyPairSigner } from '@solana/signers';
import { useWalletAccountTransactionSendingSigner } from '@solana/react';
import type { UiWalletAccount } from '@wallet-standard/react';
import { getBase58Decoder } from '@solana/codecs-strings';
import {
  pipe,
  createTransactionMessage,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  appendTransactionMessageInstruction,
  signAndSendTransactionMessageWithSigners,
} from '@solana/kit';

interface InstanceManagerProps {
  onInstanceSelect: (instancePubkey: string) => void;
}

interface InstanceData {
  pubkey: string;
  admin: string;
  instanceSeed: string;
  withdrawalRoot: string;
  currentTreeIndex: bigint;
}

// Separate component for create instance functionality to avoid hook issues
function CreateInstanceButton({ account, chainId, onSuccess, onError }: {
  account: UiWalletAccount;
  chainId: `solana:${string}`;
  onSuccess: (address: string) => void;
  onError: (error: string) => void;
}) {
  const { rpc } = useSolana();
  const [creating, setCreating] = useState(false);
  const transactionSigner = useWalletAccountTransactionSendingSigner(account, chainId);

  const createInstance = async () => {
    try {
      setCreating(true);
      onError('');

      // Generate a new keypair for the instance seed
      const instanceSeed = await generateKeyPairSigner();

      console.log('Generated instance seed:', instanceSeed.address);
      console.log('Wallet signer:', transactionSigner.address);

      // Get the create instance instruction
      const instruction = await getCreateInstanceInstructionAsync({
        payer: transactionSigner,
        admin: transactionSigner,
        instanceSeed: instanceSeed,
      });

      console.log('Created instruction:', instruction);

      // Get recent blockhash
      const { value: latestBlockhash } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();

      // Build transaction message using the proper pattern
      const transactionMessage = pipe(
        createTransactionMessage({ version: 0 }),
        (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
        (m) => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m),
        (m) => appendTransactionMessageInstruction(instruction, m)
      );

      console.log('Transaction message:', transactionMessage);

      // Sign and send with both signers (wallet and instance seed)
      const signatureBytes = await signAndSendTransactionMessageWithSigners(transactionMessage);

      // Convert signature bytes to base58 string
      const signature = getBase58Decoder().decode(signatureBytes);

      console.log('Transaction sent with signature:', signature);

      // Get the instance address from the instruction
      const newInstanceAddress = instruction.accounts[3].address;

      onSuccess(newInstanceAddress);

    } catch (err) {
      console.error('Error creating instance:', err);
      onError(err instanceof Error ? err.message : 'Failed to create instance');
    } finally {
      setCreating(false);
    }
  };

  return (
    <button
      onClick={createInstance}
      disabled={creating}
      className="button button-success"
    >
      {creating ? 'Creating...' : 'Create New Instance'}
    </button>
  );
}

export function InstanceManager({ onInstanceSelect }: InstanceManagerProps) {
  const { rpc } = useSolana();
  const account = useWalletStandardAccount();
  const { network } = useCluster();
  const [instanceAddress, setInstanceAddress] = useState('');
  const [instanceData, setInstanceData] = useState<InstanceData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [createSuccess, setCreateSuccess] = useState<string | null>(null);

  const chainId = (network === 'localnet' ? 'solana:devnet' : `solana:${network}`) as `solana:${string}`;

  const walletAddress = account?.address;

  const fetchInstanceData = async (instancePubkey: string) => {
    try {
      setLoading(true);
      setError('');

      const instanceAddr = address(instancePubkey);
      const accountInfo = await rpc.getAccountInfo(instanceAddr, {
        encoding: 'base64',
      }).send();

      if (!accountInfo.value) {
        throw new Error('Instance account not found');
      }

      // Decode using codama-generated decoder
      // Convert base64 to Uint8Array using browser APIs
      const base64Data = accountInfo.value.data[0];
      const binaryString = atob(base64Data);
      const accountData = new Uint8Array(binaryString.length);
      for (let i = 0; i < binaryString.length; i++) {
        accountData[i] = binaryString.charCodeAt(i);
      }

      const encodedAccount = {
        address: instancePubkey as Address,
        data: accountData,
        executable: accountInfo.value.executable,
        lamports: accountInfo.value.lamports,
        owner: accountInfo.value.owner as Address,
        programAddress: accountInfo.value.owner as Address,
        space: BigInt(accountData.length),
      };

      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const decoded = decodeInstance(encodedAccount as any);
      const data = decoded.data;

      // Convert withdrawal root bytes to hex string
      const withdrawalRootHex = Array.from(data.withdrawalTransactionsRoot)
        .map(b => b.toString(16).padStart(2, '0'))
        .join('');

      const instance: InstanceData = {
        pubkey: instancePubkey,
        admin: data.admin,
        instanceSeed: data.instanceSeed,
        withdrawalRoot: withdrawalRootHex,
        currentTreeIndex: data.currentTreeIndex,
      };

      setInstanceData(instance);
      onInstanceSelect(instancePubkey);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to fetch instance');
      console.error('Error fetching instance:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleCreateSuccess = async (newInstanceAddress: string) => {
    setCreateSuccess(`Instance created successfully! Address: ${newInstanceAddress}`);
    setInstanceAddress(newInstanceAddress);
    // Auto-load the new instance
    await fetchInstanceData(newInstanceAddress);
  };

  const handleCreateError = (errorMessage: string) => {
    setError(errorMessage);
    setCreateSuccess(null);
  };

  return (
    <div className="card">
      <h2>Instance Manager</h2>

      <div className="form-group">
        <label>Instance Address</label>
        <div className="input-group">
          <input
            type="text"
            value={instanceAddress}
            onChange={(e) => setInstanceAddress(e.target.value)}
            placeholder="Enter instance public key"
            className="input"
          />
          <button
            onClick={() => fetchInstanceData(instanceAddress)}
            disabled={loading || !instanceAddress}
            className="button button-primary"
          >
            {loading ? 'Loading...' : 'Load Instance'}
          </button>
        </div>
      </div>

      <div className="form-group">
        {account && walletAddress ? (
          <CreateInstanceButton
            account={account}
            chainId={chainId}
            onSuccess={handleCreateSuccess}
            onError={handleCreateError}
          />
        ) : (
          <button
            disabled
            className="button button-success"
          >
            Create New Instance (Connect Wallet)
          </button>
        )}
      </div>

      {error && <div className="error-message">{error}</div>}

      {createSuccess && (
        <div style={{ marginTop: '1rem', padding: '1rem', backgroundColor: 'rgba(76, 175, 80, 0.2)', borderRadius: '8px' }}>
          <p style={{ margin: 0, color: '#4caf50', fontWeight: 'bold', marginBottom: '0.5rem' }}>
            {createSuccess.split('!')[0]}!
          </p>
          {createSuccess.includes('Address:') && (
            <p style={{ margin: 0, fontSize: '0.85rem', wordBreak: 'break-all' }}>
              {createSuccess.split('Address: ')[1]}
            </p>
          )}
          {createSuccess.includes('Check status:') && (
            <p style={{ margin: 0, fontSize: '0.85rem', wordBreak: 'break-all' }}>
              Signature: {createSuccess.split('Check status: ')[1]}
            </p>
          )}
        </div>
      )}

      {instanceData && (
        <div className="instance-info">
          <h3>Instance Information</h3>
          <div className="info-row">
            <span className="info-label">Address:</span>
            <span className="info-value">{instanceData.pubkey}</span>
          </div>
          <div className="info-row">
            <span className="info-label">Admin:</span>
            <span className="info-value">{instanceData.admin}</span>
          </div>
          <div className="info-row">
            <span className="info-label">Instance Seed:</span>
            <span className="info-value">{instanceData.instanceSeed}</span>
          </div>
          <div className="info-row">
            <span className="info-label">Withdrawal Root:</span>
            <span className="info-value mono">{instanceData.withdrawalRoot}</span>
          </div>
          <div className="info-row">
            <span className="info-label">Current Tree Index:</span>
            <span className="info-value">{instanceData.currentTreeIndex.toString()}</span>
          </div>
          {walletAddress && instanceData.admin === walletAddress && (
            <div className="admin-badge">You are the admin of this instance</div>
          )}
        </div>
      )}
    </div>
  );
}
