import { useState } from "react";
import { useWalletStandardAccount } from "../hooks/useWalletStandardAccount";
import { useCluster } from "../hooks/useCluster";
import { address } from "@solana/addresses";
import { useWalletAccountTransactionSendingSigner } from "@solana/react";
import type { UiWalletAccount } from "@wallet-standard/react";
import { getBase58Decoder } from "@solana/codecs-strings";
import { getWithdrawFundsInstructionAsync } from "@private-channel-withdraw";
import { createSolanaRpc } from "@solana/rpc";
import {
  findAssociatedTokenPda,
  getCreateAssociatedTokenIdempotentInstruction,
  getTransferInstruction,
} from "@solana-program/token";
import {
  pipe,
  createTransactionMessage,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  appendTransactionMessageInstruction,
  signAndSendTransactionMessageWithSigners,
  assertIsTransactionMessageWithSingleSendingSigner,
} from "@solana/kit";

const rawUrl =
  import.meta.env.VITE_PRIVATE_CHANNEL_RPC_URL || "https://api.example.com";
const PRIVATE_CHANNEL_RPC_URL =
  rawUrl.startsWith("http://") || rawUrl.startsWith("https://")
    ? rawUrl
    : `https://${rawUrl}`;
const TOKEN_PROGRAM_ADDRESS =
  "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA" as const;

interface TokenBalance {
  amount: string;
  decimals: number;
  uiAmount: string;
}

interface WithdrawSectionProps {
  account: UiWalletAccount;
  chainId: `solana:${string}`;
  privateChannelBalance: TokenBalance | null;
  mintAddress: string;
  onSuccess: (message: string) => void;
  onError: (error: string) => void;
}

function WithdrawSection({
  account,
  chainId,
  privateChannelBalance,
  mintAddress,
  onSuccess,
  onError,
}: WithdrawSectionProps) {
  const [withdrawAmount, setWithdrawAmount] = useState("");
  const [withdrawDestination, setWithdrawDestination] = useState("");
  const [withdrawing, setWithdrawing] = useState(false);
  const transactionSigner = useWalletAccountTransactionSendingSigner(
    account,
    chainId
  );

  const handleWithdraw = async () => {
    if (!withdrawAmount || !mintAddress) return;

    try {
      setWithdrawing(true);
      onError("");

      const amount = BigInt(withdrawAmount);
      const destination = withdrawDestination
        ? address(withdrawDestination)
        : null;

      // Create RPC connection to Solana Private Channels for the withdrawal transaction
      const privateChannelRpc = createSolanaRpc(PRIVATE_CHANNEL_RPC_URL);

      // Get the withdraw instruction
      const instruction = await getWithdrawFundsInstructionAsync({
        user: transactionSigner,
        mint: address(mintAddress),
        amount,
        destination,
      });

      console.log("Created withdraw instruction:", instruction);

      // Get recent blockhash from Solana Private Channels
      const { value: latestBlockhash } = await privateChannelRpc
        .getLatestBlockhash({ commitment: "confirmed" })
        .send();

      // Build transaction message
      const transactionMessage = pipe(
        createTransactionMessage({ version: "legacy" }),
        (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
        (m) => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m),
        (m) => appendTransactionMessageInstruction(instruction, m)
      );

      console.log("Transaction message:", transactionMessage);

      // Assert single sending signer
      assertIsTransactionMessageWithSingleSendingSigner(transactionMessage);

      // Sign and send the transaction to Solana Private Channels
      const signatureBytes = await signAndSendTransactionMessageWithSigners(
        transactionMessage
      );

      // Convert signature bytes to base58 string
      const signature = getBase58Decoder().decode(signatureBytes);

      console.log("Withdrawal transaction sent with signature:", signature);

      onSuccess(`Tokens withdrawn successfully! Signature: ${signature}`);
      setWithdrawAmount("");
      setWithdrawDestination("");
    } catch (err) {
      console.error("Error withdrawing tokens:", err);
      onError(err instanceof Error ? err.message : "Failed to withdraw tokens");
    } finally {
      setWithdrawing(false);
    }
  };

  const handleMaxClick = () => {
    if (privateChannelBalance) {
      setWithdrawAmount(privateChannelBalance.amount);
    }
  };

  return (
    <div className="function-section">
      <h3>Withdraw to Mainnet</h3>
      <p className="info-text">
        Withdraw tokens from Solana Private Channels back to Solana mainnet
      </p>
      {privateChannelBalance && (
        <div
          style={{
            marginBottom: "1rem",
            padding: "0.5rem",
            backgroundColor: "rgba(0, 0, 0, 0.2)",
            borderRadius: "4px",
          }}
        >
          <p style={{ margin: 0, fontSize: "0.9rem" }}>
            <strong>Solana Private Channels Balance:</strong> {privateChannelBalance.uiAmount}
          </p>
        </div>
      )}
      <div className="form-group">
        <label>Amount (in smallest units)</label>
        <div style={{ display: "flex", gap: "0.5rem" }}>
          <input
            type="text"
            value={withdrawAmount}
            onChange={(e) => setWithdrawAmount(e.target.value)}
            placeholder="Enter amount to withdraw"
            className="input"
            style={{ flex: 1 }}
          />
          <button
            onClick={handleMaxClick}
            disabled={!privateChannelBalance || withdrawing}
            className="button"
            style={{ padding: "0.5rem 1rem" }}
          >
            Max
          </button>
        </div>
      </div>
      <div className="form-group">
        <label>Destination (optional)</label>
        <input
          type="text"
          value={withdrawDestination}
          onChange={(e) => setWithdrawDestination(e.target.value)}
          placeholder="Leave blank to withdraw to your address"
          className="input"
        />
      </div>
      <button
        onClick={handleWithdraw}
        disabled={withdrawing || !withdrawAmount || !mintAddress}
        className="button button-success"
      >
        {withdrawing ? "Processing..." : "Withdraw to Mainnet"}
      </button>
    </div>
  );
}

interface TransferSectionProps {
  account: UiWalletAccount;
  chainId: `solana:${string}`;
  privateChannelBalance: TokenBalance | null;
  mintAddress: string;
  walletAddress: string;
  onSuccess: (message: string) => void;
  onError: (error: string) => void;
}

function TransferSection({
  account,
  chainId,
  privateChannelBalance,
  mintAddress,
  walletAddress,
  onSuccess,
  onError,
}: TransferSectionProps) {
  const [transferAmount, setTransferAmount] = useState("");
  const [recipientAddress, setRecipientAddress] = useState("");
  const [transferring, setTransferring] = useState(false);
  const transactionSigner = useWalletAccountTransactionSendingSigner(
    account,
    chainId
  );

  const handleTransfer = async () => {
    if (!transferAmount || !recipientAddress || !mintAddress) return;

    try {
      setTransferring(true);
      onError("");

      const amount = BigInt(transferAmount);
      const recipient = address(recipientAddress);

      // Create RPC connection to Solana Private Channels for the transfer transaction
      const privateChannelRpc = createSolanaRpc(PRIVATE_CHANNEL_RPC_URL);

      // Find the source ATA (user's token account)
      const [sourceAta] = await findAssociatedTokenPda({
        mint: address(mintAddress),
        owner: address(walletAddress),
        tokenProgram: address(TOKEN_PROGRAM_ADDRESS),
      });

      // Find the destination ATA (recipient's token account)
      const [destinationAta] = await findAssociatedTokenPda({
        mint: address(mintAddress),
        owner: recipient,
        tokenProgram: address(TOKEN_PROGRAM_ADDRESS),
      });

      console.log("Source ATA:", sourceAta);
      console.log("Destination ATA:", destinationAta);

      // Create transfer instruction
      const transferInstruction = getTransferInstruction({
        source: sourceAta,
        destination: destinationAta,
        authority: transactionSigner,
        amount,
      });

      console.log("Created transfer instruction:", transferInstruction);

      // Get recent blockhash from Solana Private Channels
      const { value: latestBlockhash } = await privateChannelRpc
        .getLatestBlockhash({ commitment: "confirmed" })
        .send();

      // Build transaction message

      const destinationAtaInfo = await privateChannelRpc
        .getAccountInfo(destinationAta, { encoding: "base64" })
        .send();

      const transactionMessage = !destinationAtaInfo.value
        ? (() => {
            console.log("Destination ATA does not exist, will create it first");
            const createAtaInstruction =
              getCreateAssociatedTokenIdempotentInstruction({
                payer: transactionSigner,
                ata: destinationAta,
                owner: recipient,
                mint: address(mintAddress),
              });
            return pipe(
              createTransactionMessage({ version: "legacy" }),
              (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
              (m) =>
                setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m),
              (m) =>
                appendTransactionMessageInstruction(createAtaInstruction, m),
              (m) =>
                appendTransactionMessageInstruction(transferInstruction, m)
            );
          })()
        : pipe(
            createTransactionMessage({ version: "legacy" }),
            (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
            (m) =>
              setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m),
            (m) =>
              appendTransactionMessageInstruction(transferInstruction, m)
          );

      console.log("Transaction message:", transactionMessage);

      // Assert single sending signer
      assertIsTransactionMessageWithSingleSendingSigner(transactionMessage);

      // Sign and send the transaction to Solana Private Channels
      const signatureBytes = await signAndSendTransactionMessageWithSigners(
        transactionMessage
      );

      // Convert signature bytes to base58 string
      const signature = getBase58Decoder().decode(signatureBytes);

      console.log("Transfer transaction sent with signature:", signature);

      onSuccess(`Tokens transferred successfully! Signature: ${signature}`);
      setTransferAmount("");
      setRecipientAddress("");
    } catch (err) {
      console.error("Error transferring tokens:", err);
      onError(err instanceof Error ? err.message : "Failed to transfer tokens");
    } finally {
      setTransferring(false);
    }
  };

  const handleMaxClick = () => {
    if (privateChannelBalance) {
      setTransferAmount(privateChannelBalance.amount);
    }
  };

  return (
    <div className="function-section">
      <h3>Transfer on Solana Private Channels</h3>
      <p className="info-text">
        Transfer tokens to another address on Solana Private Channels (regular SPL transfer)
      </p>
      {privateChannelBalance && (
        <div
          style={{
            marginBottom: "1rem",
            padding: "0.5rem",
            backgroundColor: "rgba(0, 0, 0, 0.2)",
            borderRadius: "4px",
          }}
        >
          <p style={{ margin: 0, fontSize: "0.9rem" }}>
            <strong>Solana Private Channels Balance:</strong> {privateChannelBalance.uiAmount}
          </p>
        </div>
      )}
      <div className="form-group">
        <label>Recipient Address</label>
        <input
          type="text"
          value={recipientAddress}
          onChange={(e) => setRecipientAddress(e.target.value)}
          placeholder="Enter recipient's Solana address"
          className="input"
        />
      </div>
      <div className="form-group">
        <label>Amount (in smallest units)</label>
        <div style={{ display: "flex", gap: "0.5rem" }}>
          <input
            type="text"
            value={transferAmount}
            onChange={(e) => setTransferAmount(e.target.value)}
            placeholder="Enter amount to transfer"
            className="input"
            style={{ flex: 1 }}
          />
          <button
            onClick={handleMaxClick}
            disabled={!privateChannelBalance || transferring}
            className="button"
            style={{ padding: "0.5rem 1rem" }}
          >
            Max
          </button>
        </div>
      </div>
      <button
        onClick={handleTransfer}
        disabled={
          transferring || !transferAmount || !recipientAddress || !mintAddress
        }
        className="button button-primary"
      >
        {transferring ? "Processing..." : "Transfer"}
      </button>
    </div>
  );
}

export function PrivateChannelManagement() {
  const { network } = useCluster();
  const [mintAddress, setMintAddress] = useState("");
  const [privateChannelBalance, setPrivateChannelBalance] = useState<TokenBalance | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [success, setSuccess] = useState<string | null>(null);

  const chainId = (
    network === "localnet" ? "solana:devnet" : `solana:${network}`
  ) as `solana:${string}`;

  const account = useWalletStandardAccount();
  const walletAddress = account?.address;

  const fetchPrivateChannelBalance = async () => {
    if (!mintAddress || !walletAddress) return;

    try {
      setLoading(true);
      setError("");
      setPrivateChannelBalance(null);

      // Create RPC connection to Solana Private Channels
      const privateChannelRpc = createSolanaRpc(PRIVATE_CHANNEL_RPC_URL);

      // Find the associated token account for the user on Solana Private Channels
      const [ata] = await findAssociatedTokenPda({
        mint: address(mintAddress),
        owner: address(walletAddress),
        tokenProgram: address(TOKEN_PROGRAM_ADDRESS),
      });

      console.log("Fetching balance from Solana Private Channels for ATA:", ata);

      // Fetch the token account balance (includes decimals!)
      const tokenAccountBalance = await privateChannelRpc
        .getTokenAccountBalance(ata)
        .send();

      if (tokenAccountBalance.value) {
        // The getTokenAccountBalance response includes decimals and uiAmount
        const decimals = tokenAccountBalance.value.decimals;
        const uiAmount =
          tokenAccountBalance.value.uiAmountString ||
          tokenAccountBalance.value.uiAmount?.toString() ||
          (
            Number(tokenAccountBalance.value.amount) / Math.pow(10, decimals)
          ).toFixed(decimals);

        setPrivateChannelBalance({
          amount: tokenAccountBalance.value.amount.toString(),
          decimals,
          uiAmount,
        });
      } else {
        setError("Token account not found on Solana Private Channels or balance is zero");
      }
    } catch (err) {
      console.error("Error fetching Solana Private Channels balance:", err);
      setError(
        err instanceof Error
          ? err.message
          : "Failed to fetch balance from Solana Private Channels"
      );
    } finally {
      setLoading(false);
    }
  };

  const handleSuccess = (message: string) => {
    setSuccess(message);
    setError("");
    // Refresh balance after successful withdrawal
    fetchPrivateChannelBalance();
  };

  const handleError = (errorMessage: string) => {
    setError(errorMessage);
    setSuccess(null);
  };

  return (
    <div className="card">
      <h2>Solana Private Channels Management</h2>
      <p className="card-description">
        Check your token balance on Solana Private Channels, transfer tokens, and withdraw back
        to mainnet
      </p>
      <p
        className="info-text"
        style={{ fontSize: "0.85rem", marginTop: "0.5rem" }}
      >
        Connected to Solana Private Channels RPC: {PRIVATE_CHANNEL_RPC_URL}
      </p>

      {error && <div className="error-message">{error}</div>}

      {success && (
        <div
          style={{
            marginTop: "1rem",
            padding: "1rem",
            backgroundColor: "rgba(76, 175, 80, 0.2)",
            borderRadius: "8px",
          }}
        >
          <p
            style={{
              margin: 0,
              color: "#4caf50",
              fontWeight: "bold",
              marginBottom: "0.5rem",
            }}
          >
            {success.split("!")[0]}!
          </p>
          <p style={{ margin: 0, fontSize: "0.85rem", wordBreak: "break-all" }}>
            Signature: {success.split("Signature: ")[1]}
          </p>
        </div>
      )}

      <div className="function-section">
        <h3>Check Balance on Solana Private Channels</h3>
        <p className="info-text">
          Enter a token mint address to check your balance on Solana Private Channels
        </p>
        <div className="form-group">
          <label>Token Mint Address</label>
          <input
            type="text"
            value={mintAddress}
            onChange={(e) => setMintAddress(e.target.value)}
            placeholder="Enter token mint address"
            className="input"
          />
        </div>
        <button
          onClick={fetchPrivateChannelBalance}
          disabled={loading || !mintAddress || !walletAddress}
          className="button button-primary"
        >
          {loading ? "Fetching..." : "Check Balance"}
        </button>

        {privateChannelBalance && (
          <div
            style={{
              marginTop: "1rem",
              padding: "1rem",
              backgroundColor: "rgba(33, 150, 243, 0.2)",
              borderRadius: "8px",
            }}
          >
            <h4 style={{ margin: "0 0 0.5rem 0" }}>Solana Private Channels Balance</h4>
            <p
              style={{
                margin: "0.25rem 0",
                fontSize: "1.2rem",
                fontWeight: "bold",
              }}
            >
              {privateChannelBalance.uiAmount}
            </p>
            <p
              style={{
                margin: "0.25rem 0",
                fontSize: "0.85rem",
                color: "rgba(255, 255, 255, 0.7)",
              }}
            >
              Raw amount: {privateChannelBalance.amount} (decimals:{" "}
              {privateChannelBalance.decimals})
            </p>
          </div>
        )}
      </div>

      {account && walletAddress && privateChannelBalance && (
        <>
          <TransferSection
            account={account}
            chainId={chainId}
            privateChannelBalance={privateChannelBalance}
            mintAddress={mintAddress}
            walletAddress={walletAddress}
            onSuccess={handleSuccess}
            onError={handleError}
          />
          <WithdrawSection
            account={account}
            chainId={chainId}
            privateChannelBalance={privateChannelBalance}
            mintAddress={mintAddress}
            onSuccess={handleSuccess}
            onError={handleError}
          />
        </>
      )}

      {(!account || !walletAddress) && (
        <>
          <div className="function-section">
            <h3>Transfer on Solana Private Channels</h3>
            <p className="info-text">Connect your wallet to transfer tokens</p>
            <button disabled className="button button-primary">
              Transfer (Connect Wallet)
            </button>
          </div>
          <div className="function-section">
            <h3>Withdraw to Mainnet</h3>
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
