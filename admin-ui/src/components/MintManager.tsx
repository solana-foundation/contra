import { useState } from "react";
import { useSolana } from "../hooks/useSolana";
import { useWalletStandardAccount } from "../hooks/useWalletStandardAccount";
import { useCluster } from "../hooks/useCluster";
import { address, type Address } from "@solana/addresses";
import { useWalletAccountTransactionSendingSigner } from "@solana/react";
import type { UiWalletAccount } from "@wallet-standard/react";
import { getBase58Decoder } from "@solana/codecs-strings";
import {
  pipe,
  createTransactionMessage,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  appendTransactionMessageInstruction,
  signAndSendTransactionMessageWithSigners,
  assertIsTransactionMessageWithSingleSendingSigner,
} from "@solana/kit";
import {
  getMintToInstruction,
  getCreateAssociatedTokenIdempotentInstruction,
  findAssociatedTokenPda,
  TOKEN_PROGRAM_ADDRESS,
} from "@solana-program/token";

interface MintData {
  supply: bigint;
  decimals: number;
  mintAuthority: Address | null;
  freezeAuthority: Address | null;
}

// Component for minting tokens - only rendered when wallet is connected
function MintTokensSection({
  account,
  chainId,
  mintData,
  mintAddress,
  walletAddress,
  onSuccess,
  onError,
}: {
  account: UiWalletAccount;
  chainId: `solana:${string}`;
  mintData: MintData;
  mintAddress: string;
  walletAddress: string;
  onSuccess: (message: string) => void;
  onError: (error: string) => void;
}) {
  const { rpc } = useSolana();
  const [mintToAddress, setMintToAddress] = useState("");
  const [mintAmount, setMintAmount] = useState("");
  const [minting, setMinting] = useState(false);
  const transactionSigner = useWalletAccountTransactionSendingSigner(
    account,
    chainId
  );

  const handleMintTokens = async () => {
    if (!mintData || !walletAddress || !mintAddress || !transactionSigner)
      return;

    try {
      setMinting(true);
      onError("");

      // Calculate raw amount
      const uiAmount = parseFloat(mintAmount);
      if (isNaN(uiAmount) || uiAmount <= 0) {
        throw new Error("Invalid amount");
      }

      const rawAmount = BigInt(
        Math.floor(uiAmount * Math.pow(10, mintData.decimals))
      );

      // Get the recipient's associated token account
      const recipientAddress = address(mintToAddress);
      const [ata] = await findAssociatedTokenPda({
        mint: address(mintAddress),
        owner: recipientAddress,
        tokenProgram: TOKEN_PROGRAM_ADDRESS,
      });

      // Check if the ATA exists
      const ataInfo = await rpc
        .getAccountInfo(ata, { encoding: "base64" })
        .send();

      // Get recent blockhash
      const { value: latestBlockhash } = await rpc
        .getLatestBlockhash({ commitment: "confirmed" })
        .send();

      // Build the MintTo instruction
      const mintInstruction = getMintToInstruction({
        mint: address(mintAddress),
        token: ata,
        mintAuthority: transactionSigner,
        amount: rawAmount,
      });

      // Build transaction message
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      let transactionMessage: any = pipe(
        createTransactionMessage({ version: 0 }),
        (m) => setTransactionMessageFeePayerSigner(transactionSigner, m),
        (m) => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, m)
      );

      // If ATA doesn't exist, create it first using idempotent instruction
      if (!ataInfo.value) {
        console.log("ATA does not exist, will create it first");

        const createAtaInstruction =
          getCreateAssociatedTokenIdempotentInstruction({
            payer: transactionSigner,
            ata,
            owner: recipientAddress,
            mint: address(mintAddress),
          });

        transactionMessage = pipe(
          transactionMessage,
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (m: any) =>
            appendTransactionMessageInstruction(createAtaInstruction, m),
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          (m: any) => appendTransactionMessageInstruction(mintInstruction, m)
        );
      } else {
        transactionMessage = appendTransactionMessageInstruction(
          mintInstruction,
          transactionMessage
        );
      }

      console.log("Transaction message:", transactionMessage);

      // Assert single sending signer
      assertIsTransactionMessageWithSingleSendingSigner(transactionMessage);

      // Sign and send the transaction
      const signatureBytes = await signAndSendTransactionMessageWithSigners(
        transactionMessage
      );

      // Convert signature bytes to base58 string
      const signature = getBase58Decoder().decode(signatureBytes);

      console.log("Transaction sent with signature:", signature);

      onSuccess(`Tokens minted successfully! Signature: ${signature}`);
      setMintAmount("");
      setMintToAddress("");
    } catch (err) {
      console.error("Error minting tokens:", err);
      onError(err instanceof Error ? err.message : "Failed to mint tokens");
    } finally {
      setMinting(false);
    }
  };

  const isUserMintAuthority = mintData?.mintAuthority === walletAddress;

  if (!isUserMintAuthority) {
    return (
      <div
        style={{
          marginTop: "1.5rem",
          padding: "1rem",
          backgroundColor: "rgba(255, 152, 0, 0.1)",
          borderRadius: "8px",
        }}
      >
        <p className="info-text" style={{ color: "#ff9800" }}>
          You are not the mint authority. Only the mint authority can mint new
          tokens.
        </p>
      </div>
    );
  }

  return (
    <div
      style={{
        marginTop: "1.5rem",
        padding: "1rem",
        backgroundColor: "rgba(76, 175, 80, 0.1)",
        borderRadius: "8px",
      }}
    >
      <h3>Mint Tokens</h3>
      <p
        className="info-text"
        style={{ marginBottom: "1rem", color: "#4caf50" }}
      >
        You are the mint authority for this token
      </p>

      <div className="form-group">
        <label>Recipient Address</label>
        <input
          type="text"
          value={mintToAddress}
          onChange={(e) => setMintToAddress(e.target.value)}
          placeholder="Enter recipient address"
          className="input"
        />
      </div>

      <div className="form-group">
        <label>Amount (UI Amount)</label>
        <input
          type="text"
          value={mintAmount}
          onChange={(e) => setMintAmount(e.target.value)}
          placeholder={`Enter amount (e.g., 100.5)`}
          className="input"
        />
        {mintAmount && !isNaN(parseFloat(mintAmount)) && (
          <p
            className="info-text"
            style={{ marginTop: "0.5rem", fontSize: "0.85rem" }}
          >
            Raw amount:{" "}
            {Math.floor(
              parseFloat(mintAmount || "0") * Math.pow(10, mintData.decimals)
            )}
          </p>
        )}
      </div>

      <button
        onClick={handleMintTokens}
        disabled={minting || !mintToAddress || !mintAmount}
        className="button button-success"
      >
        {minting ? "Processing..." : "Mint Tokens"}
      </button>
    </div>
  );
}

export function MintManager() {
  const { rpc } = useSolana();
  const { network } = useCluster();
  const [mintAddress, setMintAddress] = useState("");
  const [loading, setLoading] = useState(false);
  const [mintData, setMintData] = useState<MintData | null>(null);
  const [balance, setBalance] = useState<{
    amount: string;
    decimals: number;
    uiAmount: string;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [mintSuccess, setMintSuccess] = useState<string | null>(null);

  const chainId = (
    network === "localnet" ? "solana:devnet" : `solana:${network}`
  ) as `solana:${string}`;

  const account = useWalletStandardAccount();
  const walletAddress = account?.address;

  const decodeMintAccount = (data: Uint8Array): MintData => {
    // SPL Token Mint account is exactly 82 bytes
    if (data.length < 82) {
      throw new Error(
        `Invalid mint account data: expected 82 bytes, got ${data.length} bytes. ` +
          "This may not be a valid SPL token mint account."
      );
    }

    // Helper function to convert bytes to base58 address
    const bytesToAddress = (bytes: Uint8Array): Address => {
      const ALPHABET =
        "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
      let num = 0n;
      for (let i = 0; i < bytes.length; i++) {
        num = num * 256n + BigInt(bytes[i]);
      }

      let result = "";
      while (num > 0n) {
        const remainder = num % 58n;
        num = num / 58n;
        result = ALPHABET[Number(remainder)] + result;
      }

      for (let i = 0; i < bytes.length && bytes[i] === 0; i++) {
        result = "1" + result;
      }

      return address(result);
    };

    let offset = 0;

    // Read mint authority option (4 bytes)
    const mintAuthorityOption = new DataView(
      data.buffer,
      data.byteOffset + offset,
      4
    ).getUint32(0, true);
    offset += 4;

    // Read mint authority (32 bytes)
    const mintAuthorityBytes = data.slice(offset, offset + 32);
    const mintAuthority =
      mintAuthorityOption === 1 ? bytesToAddress(mintAuthorityBytes) : null;
    offset += 32;

    // Read supply (8 bytes, little-endian u64)
    const supply = new DataView(
      data.buffer,
      data.byteOffset + offset,
      8
    ).getBigUint64(0, true);
    offset += 8;

    // Read decimals (1 byte)
    const decimals = data[offset];
    offset += 1;

    // Skip is_initialized (1 byte)
    offset += 1;

    // Read freeze authority option (4 bytes)
    const freezeAuthorityOption = new DataView(
      data.buffer,
      data.byteOffset + offset,
      4
    ).getUint32(0, true);
    offset += 4;

    // Read freeze authority (32 bytes)
    const freezeAuthorityBytes = data.slice(offset, offset + 32);
    const freezeAuthority =
      freezeAuthorityOption === 1 ? bytesToAddress(freezeAuthorityBytes) : null;

    return {
      supply,
      decimals,
      mintAuthority,
      freezeAuthority,
    };
  };


  const loadMintData = async () => {
    if (!mintAddress) return;

    try {
      setLoading(true);
      setError(null);
      setMintData(null);
      setBalance(null);

      // Fetch mint account
      const mintPubkey = address(mintAddress);
      const mintAccountInfo = await rpc
        .getAccountInfo(mintPubkey, { encoding: "base64" })
        .send();

      if (!mintAccountInfo.value) {
        throw new Error("Mint account not found");
      }

      // Check if this is owned by the Token Program
      if (mintAccountInfo.value.owner !== TOKEN_PROGRAM_ADDRESS) {
        throw new Error(
          `This account is not owned by the SPL Token Program. ` +
            `Owner: ${mintAccountInfo.value.owner}`
        );
      }

      // Decode base64 data
      const mintDataBase64 = mintAccountInfo.value.data[0];
      const mintDataBinary = atob(mintDataBase64);
      const mintDataBytes = new Uint8Array(mintDataBinary.length);
      for (let i = 0; i < mintDataBinary.length; i++) {
        mintDataBytes[i] = mintDataBinary.charCodeAt(i);
      }

      console.log("Mint account data length:", mintDataBytes.length);
      console.log("First 10 bytes:", Array.from(mintDataBytes.slice(0, 10)));

      const mint = decodeMintAccount(mintDataBytes);

      setMintData(mint);

      // Fetch user's token account balance only if wallet is connected
      if (walletAddress) {
        try {
          const [ata] = await findAssociatedTokenPda({
            mint: mintPubkey,
            owner: address(walletAddress),
            tokenProgram: TOKEN_PROGRAM_ADDRESS,
          });
          const tokenAccountBalance = await rpc
            .getTokenAccountBalance(ata)
            .send();

          if (tokenAccountBalance.value) {
            const uiAmount = (
              Number(tokenAccountBalance.value.amount) /
              Math.pow(10, mint.decimals)
            ).toFixed(mint.decimals);

            setBalance({
              amount: tokenAccountBalance.value.amount.toString(),
              decimals: mint.decimals,
              uiAmount,
            });
          } else {
            // Token account doesn't exist
            setBalance({
              amount: "0",
              decimals: mint.decimals,
              uiAmount: "0",
            });
          }
        } catch (err) {
          console.log("Error fetching token account:", err);
          setBalance({
            amount: "0",
            decimals: mint.decimals,
            uiAmount: "0",
          });
        }
      }
    } catch (err) {
      console.error("Error loading mint data:", err);
      setError(err instanceof Error ? err.message : "Failed to load mint data");
    } finally {
      setLoading(false);
    }
  };

  const formatNumber = (value: string, decimals: number) => {
    const num = BigInt(value);
    const divisor = BigInt(Math.pow(10, decimals));
    const wholePart = num / divisor;
    const remainder = num % divisor;
    const fractionalPart = remainder.toString().padStart(decimals, "0");
    return `${wholePart}.${fractionalPart}`;
  };

  const handleMintSuccess = async (message: string) => {
    setMintSuccess(message);
    // Reload mint data to update balance
    await loadMintData();
  };

  const handleMintError = (errorMessage: string) => {
    setError(errorMessage);
    setMintSuccess(null);
  };

  return (
    <div className="card">
      <h2>Mint Manager</h2>
      <p className="card-description">
        Load a mint to view authorities and your balance
      </p>

      <div className="function-section">
        <div className="input-group">
          <input
            type="text"
            value={mintAddress}
            onChange={(e) => setMintAddress(e.target.value)}
            placeholder="Enter mint address"
            className="input"
          />
          <button
            onClick={loadMintData}
            disabled={loading || !mintAddress}
            className="button button-primary"
          >
            {loading ? "Loading..." : "Load Mint"}
          </button>
        </div>

        {error && (
          <div className="error-message" style={{ marginTop: "1rem" }}>
            {error}
          </div>
        )}

        {mintData && (
          <div style={{ marginTop: "1.5rem" }}>
            <h3>Mint Information</h3>

            <div className="info-row">
              <span className="info-label">Decimals:</span>
              <span className="info-value">{mintData.decimals}</span>
            </div>

            <div className="info-row">
              <span className="info-label">Total Supply:</span>
              <span className="info-value mono">
                {formatNumber(mintData.supply.toString(), mintData.decimals)}
              </span>
            </div>

            <div className="info-row">
              <span className="info-label">Mint Authority:</span>
              <span className="info-value mono" style={{ fontSize: "0.85rem" }}>
                {mintData.mintAuthority || "None (Frozen)"}
              </span>
            </div>

            <div className="info-row">
              <span className="info-label">Freeze Authority:</span>
              <span className="info-value mono" style={{ fontSize: "0.85rem" }}>
                {mintData.freezeAuthority || "None"}
              </span>
            </div>
          </div>
        )}

        {balance && (
          <div style={{ marginTop: "1.5rem" }}>
            <h3>Your Balance</h3>

            <div className="info-row">
              <span className="info-label">Amount:</span>
              <span
                className="info-value"
                style={{ fontSize: "1.2rem", fontWeight: "bold" }}
              >
                {balance.uiAmount}
              </span>
            </div>

            <div className="info-row">
              <span className="info-label">Raw Amount:</span>
              <span className="info-value mono" style={{ fontSize: "0.85rem" }}>
                {balance.amount}
              </span>
            </div>
          </div>
        )}

        {mintData && account && walletAddress && (
          <MintTokensSection
            account={account}
            chainId={chainId}
            mintData={mintData}
            mintAddress={mintAddress}
            walletAddress={walletAddress}
            onSuccess={handleMintSuccess}
            onError={handleMintError}
          />
        )}

        {error && (
          <div className="error-message" style={{ marginTop: "1rem" }}>
            {error}
          </div>
        )}

        {mintSuccess && (
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
              Tokens minted successfully!
            </p>
            <p
              style={{ margin: 0, fontSize: "0.85rem", wordBreak: "break-all" }}
            >
              Signature: {mintSuccess.split("Signature: ")[1]}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
