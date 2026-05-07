import { useState } from 'react';
import { useSolana } from '../hooks/useSolana';
import { useWallet } from '../hooks/useWallet';
import { useWalletStandardAccount } from '../hooks/useWalletStandardAccount';
import { useCluster } from '../hooks/useCluster';
import { address } from '@solana/addresses';
import { useWalletAccountTransactionSendingSigner } from '@solana/react';
import type { UiWalletAccount } from '@wallet-standard/react';
import { getBase58Decoder } from '@solana/codecs-strings';
import { getDepositInstructionAsync } from '@private-channel-escrow';
import { getWithdrawFundsInstructionAsync } from '@private-channel-withdraw';
import { createSolanaRpc } from '@solana/rpc';
import {
  pipe,
  createTransactionMessage,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  appendTransactionMessageInstruction,
  signAndSendTransactionMessageWithSigners,
  assertIsTransactionMessageWithSingleSendingSigner,
} from '@solana/kit';

// Fallback host is a placeholder — set VITE_PRIVATE_CHANNEL_RPC_URL before building.
const rawUrl = import.meta.env.VITE_PRIVATE_CHANNEL_RPC_URL || 'https://api.example.com';
const PRIVATE_CHANNEL_RPC_URL = rawUrl.startsWith('http://') || rawUrl.startsWith('https://') ? rawUrl : `https://${rawUrl}`;

interface UserFunctionsProps {
  instancePubkey: string;
}

// Separate component for deposit functionality
function DepositSection({
  account,
  chainId,
  instancePubkey,
  onSuccess,
  onError,
}: {
  account: UiWalletAccount;
  chainId: `solana:${string}`;
  instancePubkey: string;
  onSuccess: (message: string) => void;
  onError: (error: string) => void;
}) {
  const { rpc } = useSolana();
  const [depositMintAddress, setDepositMintAddress] = useState('');
  const [depositAmount, setDepositAmount] = useState('');
  const [recipientAddress, setRecipientAddress] = useState('');
  const [depositing, setDepositing] = useState(false);
  const transactionSigner = useWalletAccountTransactionSendingSigner(account, chainId);

  const handleDeposit = async () => {
    if (!depositMintAddress || !depositAmount) return;

    try {
      setDepositing(true);
      onError('');

      const amount = BigInt(depositAmount);
      const recipient = recipientAddress ? address(recipientAddress) : null;

      // Get the deposit instruction
      const instruction = await getDepositInstructionAsync({
        payer: transactionSigner,
        user: transactionSigner,
        instance: address(instancePubkey),
        mint: address(depositMintAddress),
        amount,
        recipient,
      });

      console.log('Created deposit instruction:', instruction);

      // Get recent blockhash
      const { value: latestBlockhash } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();

      // Build transaction message
      const transactionMessage = pipe(
        createTransactionMessage({ version: 0 }),
        (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
        (m) => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m),
        (m) => appendTransactionMessageInstruction(instruction, m)
      );

      console.log('Transaction message:', transactionMessage);

      // Assert single sending signer
      assertIsTransactionMessageWithSingleSendingSigner(transactionMessage);

      // Sign and send the transaction
      const signatureBytes = await signAndSendTransactionMessageWithSigners(transactionMessage);

      // Convert signature bytes to base58 string
      const signature = getBase58Decoder().decode(signatureBytes);

      console.log('Transaction sent with signature:', signature);

      onSuccess(`Tokens deposited successfully! Signature: ${signature}`);
      setDepositAmount('');
      setDepositMintAddress('');
      setRecipientAddress('');

    } catch (err) {
      console.error('Error depositing tokens:', err);
      onError(err instanceof Error ? err.message : 'Failed to deposit tokens');
    } finally {
      setDepositing(false);
    }
  };

  return (
    <div className="function-section">
      <h3>Deposit Tokens</h3>
      <p className="info-text">
        Deposit tokens from your wallet to the escrow instance
      </p>
      <div className="form-group">
        <label>Mint Address</label>
        <input
          type="text"
          value={depositMintAddress}
          onChange={(e) => setDepositMintAddress(e.target.value)}
          placeholder="Enter token mint address"
          className="input"
        />
      </div>
      <div className="form-group">
        <label>Amount</label>
        <input
          type="number"
          value={depositAmount}
          onChange={(e) => setDepositAmount(e.target.value)}
          placeholder="Enter amount to deposit"
          className="input"
        />
      </div>
      <div className="form-group">
        <label>Recipient (optional)</label>
        <input
          type="text"
          value={recipientAddress}
          onChange={(e) => setRecipientAddress(e.target.value)}
          placeholder="Leave blank to use your address"
          className="input"
        />
      </div>
      <button
        onClick={handleDeposit}
        disabled={depositing || !depositMintAddress || !depositAmount}
        className="button button-primary"
      >
        {depositing ? 'Processing...' : 'Deposit'}
      </button>
    </div>
  );
}

// Separate component for withdraw functionality
function WithdrawSection({
  account,
  chainId,
  onSuccess,
  onError,
}: {
  account: UiWalletAccount;
  chainId: `solana:${string}`;
  onSuccess: (message: string) => void;
  onError: (error: string) => void;
}) {
  const [withdrawMintAddress, setWithdrawMintAddress] = useState('');
  const [withdrawAmount, setWithdrawAmount] = useState('');
  const [withdrawDestination, setWithdrawDestination] = useState('');
  const [withdrawing, setWithdrawing] = useState(false);
  const transactionSigner = useWalletAccountTransactionSendingSigner(account, chainId);

  const handleWithdraw = async () => {
    if (!withdrawMintAddress || !withdrawAmount) return;

    try {
      setWithdrawing(true);
      onError('');

      const amount = BigInt(withdrawAmount);
      const destination = withdrawDestination ? address(withdrawDestination) : null;

      // Create RPC connection to Solana Private Channels for the withdrawal transaction
      const privateChannelRpc = createSolanaRpc(PRIVATE_CHANNEL_RPC_URL);

      // Get the withdraw instruction
      const instruction = await getWithdrawFundsInstructionAsync({
        user: transactionSigner,
        mint: address(withdrawMintAddress),
        amount,
        destination,
      });

      console.log('Created withdraw instruction:', instruction);

      // Get recent blockhash from Solana Private Channels
      const { value: latestBlockhash } = await privateChannelRpc.getLatestBlockhash({ commitment: 'confirmed' }).send();

      // Build transaction message
      const transactionMessage = pipe(
        createTransactionMessage({ version: 'legacy' }),
        (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
        (m) => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m),
        (m) => appendTransactionMessageInstruction(instruction, m)
      );

      console.log('Transaction message:', transactionMessage);

      // Assert single sending signer
      assertIsTransactionMessageWithSingleSendingSigner(transactionMessage);

      // Sign and send the transaction
      const signatureBytes = await signAndSendTransactionMessageWithSigners(transactionMessage);

      // Convert signature bytes to base58 string
      const signature = getBase58Decoder().decode(signatureBytes);

      console.log('Withdrawal transaction sent with signature:', signature);

      onSuccess(`Tokens withdrawn successfully! Signature: ${signature}`);
      setWithdrawAmount('');
      setWithdrawMintAddress('');
      setWithdrawDestination('');

    } catch (err) {
      console.error('Error withdrawing tokens:', err);
      onError(err instanceof Error ? err.message : 'Failed to withdraw tokens');
    } finally {
      setWithdrawing(false);
    }
  };

  return (
    <div className="function-section">
      <h3>Withdraw Tokens</h3>
      <p className="info-text">
        Withdraw tokens from your token account (uses Solana Private Channels Withdraw Program)
      </p>
      <div className="form-group">
        <label>Mint Address</label>
        <input
          type="text"
          value={withdrawMintAddress}
          onChange={(e) => setWithdrawMintAddress(e.target.value)}
          placeholder="Enter token mint address"
          className="input"
        />
      </div>
      <div className="form-group">
        <label>Amount</label>
        <input
          type="number"
          value={withdrawAmount}
          onChange={(e) => setWithdrawAmount(e.target.value)}
          placeholder="Enter amount to withdraw"
          className="input"
        />
      </div>
      <div className="form-group">
        <label>Destination (optional)</label>
        <input
          type="text"
          value={withdrawDestination}
          onChange={(e) => setWithdrawDestination(e.target.value)}
          placeholder="Leave blank to withdraw to yourself"
          className="input"
        />
      </div>
      <button
        onClick={handleWithdraw}
        disabled={withdrawing || !withdrawMintAddress || !withdrawAmount}
        className="button button-success"
      >
        {withdrawing ? 'Processing...' : 'Withdraw'}
      </button>
    </div>
  );
}

export function UserFunctions({ instancePubkey }: UserFunctionsProps) {
  const { connected } = useWallet();
  const account = useWalletStandardAccount();
  const { network } = useCluster();
  const [error, setError] = useState('');
  const [success, setSuccess] = useState<string | null>(null);

  const chainId = (network === 'localnet' ? 'solana:devnet' : `solana:${network}`) as `solana:${string}`;

  const handleSuccess = (message: string) => {
    setSuccess(message);
    setError('');
  };

  const handleError = (errorMessage: string) => {
    setError(errorMessage);
    setSuccess(null);
  };

  return (
    <div className="card">
      <h2>User Functions</h2>
      <p className="card-description">These functions are available to all users</p>

      {error && <div className="error-message">{error}</div>}

      {success && (
        <div style={{ marginTop: '1rem', padding: '1rem', backgroundColor: 'rgba(76, 175, 80, 0.2)', borderRadius: '8px' }}>
          <p style={{ margin: 0, color: '#4caf50', fontWeight: 'bold', marginBottom: '0.5rem' }}>
            {success.split('!')[0]}!
          </p>
          <p style={{ margin: 0, fontSize: '0.85rem', wordBreak: 'break-all' }}>
            Signature: {success.split('Signature: ')[1]}
          </p>
        </div>
      )}

      {connected && account ? (
        <>
          <DepositSection
            account={account}
            chainId={chainId}
            instancePubkey={instancePubkey}
            onSuccess={handleSuccess}
            onError={handleError}
          />
          <WithdrawSection
            account={account}
            chainId={chainId}
            onSuccess={handleSuccess}
            onError={handleError}
          />
        </>
      ) : (
        <>
          <div className="function-section">
            <h3>Deposit Tokens</h3>
            <p className="info-text">Connect your wallet to deposit tokens</p>
            <button disabled className="button button-primary">
              Deposit (Connect Wallet)
            </button>
          </div>
          <div className="function-section">
            <h3>Withdraw Tokens</h3>
            <p className="info-text">Connect your wallet to withdraw tokens</p>
            <button disabled className="button button-success">
              Withdraw (Connect Wallet)
            </button>
          </div>
        </>
      )}
    </div>
  );
}
