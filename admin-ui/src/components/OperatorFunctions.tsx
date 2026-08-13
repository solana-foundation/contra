import { useState } from 'react';
import { useSolana } from '../hooks/useSolana';
import { useWallet } from '../hooks/useWallet';
import { useWalletStandardAccount } from '../hooks/useWalletStandardAccount';
import { useCluster } from '../hooks/useCluster';
import { address } from '@solana/addresses';
import { useWalletAccountTransactionSendingSigner } from '@solana/react';
import { getBase58Decoder } from '@solana/codecs-strings';
import {
  getReleaseFundsInstructionAsync,
  getRotateBitmapInstructionAsync,
  getWithdrawalBitmapDecoder,
  findWithdrawalBitmapPda,
} from '@private-channel-escrow';
import { findAssociatedTokenPda } from '@solana-program/token';
import {
  pipe,
  createTransactionMessage,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  appendTransactionMessageInstruction,
  signAndSendTransactionMessageWithSigners,
  assertIsTransactionMessageWithSingleSendingSigner,
} from '@solana/kit';

const TOKEN_PROGRAM_ADDRESS = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA' as const;

interface OperatorFunctionsProps {
  instancePubkey: string;
}

export function OperatorFunctions({ instancePubkey }: OperatorFunctionsProps) {
  const { connected } = useWallet();
  const account = useWalletStandardAccount();
  const { network } = useCluster();

  if (!connected || !account) {
    return (
      <div className="card">
        <h2>Operator Functions</h2>
        <p className="card-description">These functions require operator privileges</p>
        <div className="error-message">Please connect your wallet to use operator functions</div>
      </div>
    );
  }

  return <OperatorFunctionsContent instancePubkey={instancePubkey} account={account} network={network} />;
}

interface OperatorFunctionsContentProps {
  instancePubkey: string;
  account: Parameters<typeof useWalletAccountTransactionSendingSigner>[0];
  network: string;
}

function OperatorFunctionsContent({ instancePubkey, account, network }: OperatorFunctionsContentProps) {
  const { rpc } = useSolana();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [success, setSuccess] = useState<string | null>(null);
  const [mintAddress, setMintAddress] = useState('');
  const [userAddress, setUserAddress] = useState('');
  const [amount, setAmount] = useState('');
  const [transactionNonce, setTransactionNonce] = useState('');

  const chainId = (network === 'localnet' ? 'solana:devnet' : `solana:${network}`) as `solana:${string}`;
  const transactionSigner = useWalletAccountTransactionSendingSigner(account, chainId);

  const handleReleaseFunds = async () => {
    if (!mintAddress || !userAddress || !amount || !transactionNonce) {
      setError('Please fill in all fields');
      return;
    }

    try {
      setLoading(true);
      setError('');
      setSuccess(null);

      // Find user ATA
      const [userAta] = await findAssociatedTokenPda({
        mint: address(mintAddress),
        owner: address(userAddress),
        tokenProgram: address(TOKEN_PROGRAM_ADDRESS),
      });

      // Get the release funds instruction
      const instruction = await getReleaseFundsInstructionAsync({
        payer: transactionSigner,
        operator: transactionSigner,
        instance: address(instancePubkey),
        mint: address(mintAddress),
        userAta,
        amount: BigInt(amount),
        user: address(userAddress),
        transactionNonce: BigInt(transactionNonce),
      });

      console.log('Created release funds instruction:', instruction);

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

      setSuccess(`Funds released successfully! Signature: ${signature}`);
      setMintAddress('');
      setUserAddress('');
      setAmount('');
      setTransactionNonce('');

    } catch (err) {
      console.error('Error releasing funds:', err);
      setError(err instanceof Error ? err.message : 'Failed to release funds');
    } finally {
      setLoading(false);
    }
  };

  const handleRotateBitmap = async () => {
    try {
      setLoading(true);
      setError('');
      setSuccess(null);

      // Bind the rotation to the generation the chain is on, so a replay (e.g. an
      // ambiguously-confirmed retry) is rejected instead of skipping a generation.
      const [bitmapPda] = await findWithdrawalBitmapPda({ instance: address(instancePubkey) });
      const bitmapInfo = await rpc
        .getAccountInfo(bitmapPda, { encoding: 'base64', commitment: 'confirmed' })
        .send();
      if (!bitmapInfo.value) {
        throw new Error('Withdrawal bitmap account not found for this instance');
      }
      const binary = atob(bitmapInfo.value.data[0]);
      const bitmapBytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) {
        bitmapBytes[i] = binary.charCodeAt(i);
      }
      const bitmapHeader = getWithdrawalBitmapDecoder().decode(bitmapBytes);

      // Bits start after the fixed header; capacity comes from the account so a
      // shorter test-sized bitmap counts correctly too.
      const BITS_OFFSET = 10;
      const capacity = (bitmapBytes.length - BITS_OFFSET) * 8;
      let released = 0;
      for (let i = BITS_OFFSET; i < bitmapBytes.length; i++) {
        let byte = bitmapBytes[i];
        while (byte) {
          released += byte & 1;
          byte >>= 1;
        }
      }
      const unreleased = capacity - released;

      // Rotating clears every bit, so a nonce still unreleased in this generation
      // can never be released afterwards, and a refund still waiting on one of
      // these bits loses the only proof of whether that user was already paid.
      // This page cannot see those waiting refunds, so the operator has to be the
      // one to confirm the window is safe to clear.
      if (unreleased > 0) {
        const proceed = window.confirm(
          `${unreleased} of ${capacity} nonces in generation ${bitmapHeader.generation} have not been released.\n\n` +
            'Rotating now clears every bit and advances the generation. Those nonces can never be released afterwards, ' +
            'and any refund still waiting on one of these bits loses the only proof of whether that user was already paid.\n\n' +
            'This page cannot see refunds waiting inside the operator. Confirm with the operator before continuing.\n\n' +
            'Rotate anyway?'
        );
        if (!proceed) {
          setError('Rotation cancelled: the current generation still has unreleased nonces.');
          return;
        }
      }

      const instruction = await getRotateBitmapInstructionAsync({
        payer: transactionSigner,
        operator: transactionSigner,
        instance: address(instancePubkey),
        expectedGeneration: bitmapHeader.generation,
      });

      console.log('Created rotate bitmap instruction:', instruction);

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

      setSuccess(`Bitmap rotated successfully! Signature: ${signature}`);

    } catch (err) {
      console.error('Error rotating bitmap:', err);
      setError(err instanceof Error ? err.message : 'Failed to rotate bitmap');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="card">
      <h2>Operator Functions</h2>
      <p className="card-description">These functions require operator privileges</p>

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

      <div className="function-section">
        <h3>Release Funds</h3>
        <div className="form-group">
          <label>Mint Address</label>
          <input
            type="text"
            value={mintAddress}
            onChange={(e) => setMintAddress(e.target.value)}
            placeholder="Enter token mint address"
            className="input"
          />
        </div>
        <div className="form-group">
          <label>User Address</label>
          <input
            type="text"
            value={userAddress}
            onChange={(e) => setUserAddress(e.target.value)}
            placeholder="Enter user wallet address"
            className="input"
          />
        </div>
        <div className="form-group">
          <label>Amount</label>
          <input
            type="number"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            placeholder="Enter amount to release"
            className="input"
          />
        </div>
        <div className="form-group">
          <label>Transaction Nonce</label>
          <input
            type="number"
            value={transactionNonce}
            onChange={(e) => setTransactionNonce(e.target.value)}
            placeholder="Enter transaction nonce"
            className="input"
          />
        </div>
        <button
          onClick={handleReleaseFunds}
          disabled={loading || !mintAddress || !userAddress || !amount || !transactionNonce}
          className="button button-primary"
        >
          {loading ? 'Processing...' : 'Release Funds'}
        </button>
      </div>

      <div className="function-section">
        <h3>Rotate Bitmap</h3>
        <p className="info-text">
          Clears every consumed-nonce bit and opens the next generation. Only do this
          once the instance has reached its generation boundary: nonces from the
          generation being closed can never be released afterwards.
        </p>
        <button
          onClick={handleRotateBitmap}
          disabled={loading}
          className="button button-warning"
        >
          {loading ? 'Processing...' : 'Rotate Bitmap'}
        </button>
      </div>
    </div>
  );
}
