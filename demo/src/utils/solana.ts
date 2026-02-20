import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  sendAndConfirmRawTransaction,
} from '@solana/web3.js';
import {
  createInitializeMintInstruction,
  createMintToInstruction,
  createTransferInstruction,
  getAssociatedTokenAddress,
  createAssociatedTokenAccountInstruction,
  TOKEN_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
} from '@solana/spl-token';
import bs58 from 'bs58';

export const getAdminKeypair = (): Keypair | null => {
  const adminKeypairStr = import.meta.env.VITE_ADMIN_KEYPAIR;
  if (!adminKeypairStr) return null;

  try {
    const keypairBytes = JSON.parse(adminKeypairStr);
    return Keypair.fromSecretKey(new Uint8Array(keypairBytes));
  } catch (e) {
    console.error('Failed to parse admin keypair:', e);
    return null;
  }
};

export const createMintTransaction = (
  admin: Keypair,
  mint: PublicKey
): Transaction => {
  const transaction = new Transaction();

  transaction.add(
    createInitializeMintInstruction(
      mint,
      9, // decimals
      admin.publicKey, // mint authority
      null, // freeze authority
      TOKEN_PROGRAM_ID
    )
  );

  return transaction;
};

export const createMintToTransaction = (
  admin: Keypair,
  mint: PublicKey,
  destination: PublicKey,
  amount: number
): Transaction => {
  const transaction = new Transaction();

  transaction.add(
    createMintToInstruction(
      mint,
      destination,
      admin.publicKey,
      amount,
      [],
      TOKEN_PROGRAM_ID
    )
  );

  return transaction;
};

export const createATATransaction = async (
  payer: Keypair,
  owner: PublicKey,
  mint: PublicKey
): Promise<Transaction> => {
  const transaction = new Transaction();
  const ata = await getAssociatedTokenAddress(mint, owner);

  transaction.add(
    createAssociatedTokenAccountInstruction(
      payer.publicKey,
      ata,
      owner,
      mint,
      TOKEN_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID
    )
  );

  return transaction;
};

export const createTransferTransaction = async (
  from: Keypair,
  to: PublicKey,
  mint: PublicKey,
  amount: number
): Promise<Transaction> => {
  const transaction = new Transaction();

  const fromATA = await getAssociatedTokenAddress(mint, from.publicKey);
  const toATA = await getAssociatedTokenAddress(mint, to);

  transaction.add(
    createTransferInstruction(
      fromATA,
      toATA,
      from.publicKey,
      amount,
      [],
      TOKEN_PROGRAM_ID
    )
  );

  return transaction;
};

const BLOCKHASH_TTL_MS = 15_000;

let cachedBlockhash: { value: string; fetchedAt: number } | null = null;
let inflight: Promise<string> | null = null;

const fetchBlockhash = async (url: string): Promise<string> => {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'getLatestBlockhash',
      params: [{ commitment: 'finalized' }],
    }),
  });

  const json = await response.json();
  if (json.error) {
    throw new Error(json.error.message || 'Failed to get blockhash');
  }

  const blockhash = json.result?.value?.blockhash;
  if (!blockhash || typeof blockhash !== 'string') {
    console.error('Unexpected getLatestBlockhash response:', JSON.stringify(json.result));
    throw new Error('Invalid blockhash response from read node');
  }

  console.log('Fetched new blockhash:', blockhash);
  return blockhash;
};

const getLatestBlockhash = async (url: string): Promise<string> => {
  if (cachedBlockhash && Date.now() - cachedBlockhash.fetchedAt < BLOCKHASH_TTL_MS) {
    return cachedBlockhash.value;
  }

  // Deduplicate concurrent fetches
  if (inflight) return inflight;

  inflight = fetchBlockhash(url)
    .then((blockhash) => {
      cachedBlockhash = { value: blockhash, fetchedAt: Date.now() };
      return blockhash;
    })
    .catch((err) => {
      cachedBlockhash = null; // invalidate on error
      throw err;
    })
    .finally(() => {
      inflight = null;
    });

  return inflight;
};

export const sendTransaction = async (
  transaction: Transaction,
  signers: Keypair[],
  url: string,
  readUrl: string
): Promise<{ signature: string; latency: number }> => {
  const startTime = Date.now();

  // Fetch a real blockhash from the read node
  const blockhash = await getLatestBlockhash(readUrl);
  transaction.recentBlockhash = blockhash;
  transaction.feePayer = signers[0].publicKey;

  // Sign the transaction
  transaction.sign(...signers);

  // Serialize and send via RPC
  const serialized = transaction.serialize();
  const base64 = serialized.toString('base64');

  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'sendTransaction',
      params: [base64],
    }),
  });

  const json = await response.json();
  const latency = Date.now() - startTime;

  if (json.error) {
    throw new Error(json.error.message || 'Transaction failed');
  }

  // Get the first signature (which is the transaction signature)
  const sig = transaction.signatures[0];
  const signature = sig ? bs58.encode(sig.signature!) : '';

  return {
    signature,
    latency,
  };
};

export const getTransaction = async (
  signature: string,
  url: string
): Promise<any> => {
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'getTransaction',
      params: [signature, { encoding: 'json' }],
    }),
  });

  const json = await response.json();
  return json.result;
};

export const getTokenBalance = async (
  owner: PublicKey,
  mint: PublicKey,
  url: string
): Promise<number> => {
  const ata = await getAssociatedTokenAddress(mint, owner);

  const response = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'getTokenAccountBalance',
      params: [ata.toString()],
    }),
  });

  const json = await response.json();

  if (json.result?.value?.amount !== undefined) {
    // Return the raw amount as a number (lamports)
    // Since we have 6 decimals, divide by 10^6 to get the UI amount
    const decimals = json.result.value.decimals || 6;
    return parseInt(json.result.value.amount) / Math.pow(10, decimals);
  }

  return 0;
};