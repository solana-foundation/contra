import { useState, useEffect, useRef, useCallback } from "react";
import { Keypair, PublicKey } from "@solana/web3.js";
import { getAssociatedTokenAddress } from "@solana/spl-token";
import type {
  Wallet,
  Transaction,
  TestParams,
  TestStatistics,
} from "../types/index";
import {
  getAdminKeypair,
  createMintTransaction,
  createATATransaction,
  createMintToTransaction,
  createTransferTransaction,
  sendTransaction,
  getTransaction,
  getTokenBalance,
} from "../utils/solana";

const WRITE_URL = "/api/write";
const READ_URL = "/api/read";
const INITIAL_BALANCE = 1000000000; // 1000 tokens with 6 decimals

export const useLoadTest = () => {
  const [isRunning, setIsRunning] = useState(false);
  const [senders, setSenders] = useState<Wallet[]>([]);
  const [receivers, setReceivers] = useState<Wallet[]>([]);
  const [transactions, setTransactions] = useState<Transaction[]>([]);
  const [statistics, setStatistics] = useState<TestStatistics>({
    totalTransactions: 0,
    confirmedTransactions: 0,
    failedTransactions: 0,
    averageSendLatency: 0,
    throughput: 0,
    maxThroughput: 0,
    rps: 0,
    maxRps: 0,
    progress: 0,
    transactionsWithLatency: 0,
  });

  const testStateRef = useRef<{
    isRunning: boolean;
    senderKeypairs: Keypair[];
    senderWallets: Wallet[];
    receiverWallets: Wallet[];
    mint: PublicKey | null;
    testDuration: number;
    requestsSentInLastSecond: number;
    lastRpsCalculation: number;
  }>({
    isRunning: false,
    senderKeypairs: [],
    senderWallets: [],
    receiverWallets: [],
    mint: null,
    testDuration: 0,
    requestsSentInLastSecond: 0,
    lastRpsCalculation: Date.now(),
  });

  const updateWalletBalance = useCallback(
    async (address: string, type: "sender" | "receiver") => {
      const setWallets = type === "sender" ? setSenders : setReceivers;

      // Find wallet by matching the full address from testStateRef
      const wallet =
        type === "sender"
          ? testStateRef.current.senderWallets.find((w) => w.address.toString() === address)
          : testStateRef.current.receiverWallets.find((w) => w.address.toString() === address);

      if (!wallet || !testStateRef.current.mint) {
        if (!wallet) console.warn("Wallet not found:", address);
        return;
      }

      setWallets((prev) =>
        prev.map((w) => (w.id === wallet.id ? { ...w, isLoading: true } : w))
      );

      try {
        const balance = await getTokenBalance(
          wallet.address,
          testStateRef.current.mint,
          READ_URL
        );

        setWallets((prev) =>
          prev.map((w) =>
            w.id === wallet.id
              ? { ...w, balance, isLoading: false, lastUpdate: Date.now() }
              : w
          )
        );
      } catch (error) {
        console.error("Failed to update balance:", error);
        setWallets((prev) =>
          prev.map((w) => (w.id === wallet.id ? { ...w, isLoading: false } : w))
        );
      }
    },
    []
  );

  const pollConfirmation = useCallback(
    async (txId: string, signature: string, senderPubkey: string, receiverAddress: string) => {
      const maxPolls = 20;
      const pollInterval = 1000;

      for (let pollCount = 1; pollCount <= maxPolls; pollCount++) {
        if (!testStateRef.current.isRunning) break;

        try {
          const result = await getTransaction(signature, READ_URL);
          if (result) {
            setTransactions((prev) =>
              prev.map((tx) =>
                tx.id === txId ? { ...tx, status: "confirmed", pollCount } : tx
              )
            );
            setStatistics((prev) => ({
              ...prev,
              confirmedTransactions: prev.confirmedTransactions + 1,
            }));

            // Update both balances in parallel
            updateWalletBalance(senderPubkey, "sender");
            updateWalletBalance(receiverAddress, "receiver");
            return;
          }
        } catch (error) {
          console.error("Poll error:", error);
        }

        await new Promise((resolve) => setTimeout(resolve, pollInterval));
      }

      // Failed to confirm after all polls exhausted
      setTransactions((prev) =>
        prev.map((tx) =>
          tx.id === txId ? { ...tx, status: "failed", pollCount: maxPolls } : tx
        )
      );
      setStatistics((prev) => ({
        ...prev,
        failedTransactions: prev.failedTransactions + 1,
      }));
    },
    [updateWalletBalance]
  );

  const executeTransfer = useCallback(
    async (senderIndex: number, receiverIndex: number): Promise<boolean> => {
      if (!testStateRef.current.mint || !testStateRef.current.isRunning)
        return false;

      const sender = testStateRef.current.senderKeypairs[senderIndex];
      const receiver = testStateRef.current.receiverWallets[receiverIndex];
      const amount = Math.floor(Math.random() * 1000000) + 10000;

      const txId = `tx-${Date.now()}-${Math.random()}`;
      const newTx: Transaction = {
        id: txId,
        from: sender.publicKey.toString().slice(0, 8),
        to: receiver.address.toString().slice(0, 8),
        amount: amount / 1000000,
        signature: "",
        status: "pending",
        timestamp: Date.now(),
      };

      setTransactions((prev) => [...prev, newTx]);

      try {
        const transaction = await createTransferTransaction(
          sender,
          receiver.address,
          testStateRef.current.mint,
          amount
        );

        const { signature, latency } = await sendTransaction(
          transaction,
          [sender],
          WRITE_URL,
          READ_URL
        );

        // Update transaction with signature
        setTransactions((prev) =>
          prev.map((tx) =>
            tx.id === txId
              ? { ...tx, signature, sendLatency: latency, status: "polling" }
              : tx
          )
        );

        // Update RPS tracking
        const now = Date.now();
        if (now - testStateRef.current.lastRpsCalculation >= 1000) {
          testStateRef.current.requestsSentInLastSecond = 1;
          testStateRef.current.lastRpsCalculation = now;
        } else {
          testStateRef.current.requestsSentInLastSecond++;
        }

        // Update statistics
        setStatistics((prev) => {
          const txWithLatency = prev.transactionsWithLatency || 0;
          const duration = prev.startTime ? (now - prev.startTime) / 1000 : 0;
          const newTotalTx = prev.totalTransactions + 1;
          const currentThroughput = duration > 0 ? newTotalTx / duration : 0;
          const currentRps = testStateRef.current.requestsSentInLastSecond;
          const progress = testStateRef.current.testDuration > 0
            ? Math.min(100, (duration / testStateRef.current.testDuration) * 100)
            : 0;

          return {
            ...prev,
            totalTransactions: newTotalTx,
            transactionsWithLatency: txWithLatency + 1,
            averageSendLatency:
              txWithLatency === 0
                ? latency
                : (prev.averageSendLatency * txWithLatency + latency) /
                  (txWithLatency + 1),
            throughput: currentThroughput,
            maxThroughput: Math.max(prev.maxThroughput, currentThroughput),
            rps: currentRps,
            maxRps: Math.max(prev.maxRps, currentRps),
            progress,
          };
        });

        // Fire-and-forget: poll confirmation + balance updates in background
        pollConfirmation(txId, signature, sender.publicKey.toString(), receiver.address.toString());

        return true;
      } catch (error) {
        console.error("Transfer failed:", error);
        setTransactions((prev) =>
          prev.map((tx) => (tx.id === txId ? { ...tx, status: "failed" } : tx))
        );

        setStatistics((prev) => ({
          ...prev,
          failedTransactions: prev.failedTransactions + 1,
        }));
        return false;
      }
    },
    [pollConfirmation]
  );

  const setupAccounts = async (numUsers: number) => {
    const admin = getAdminKeypair();
    if (!admin) {
      throw new Error("Admin keypair not configured");
    }

    // Generate keypairs
    const senderKeypairs: Keypair[] = [];
    const senderWallets: Wallet[] = [];
    const receiverWallets: Wallet[] = [];

    for (let i = 0; i < numUsers; i++) {
      const senderKp = Keypair.generate();
      const receiverPubkey = Keypair.generate().publicKey;

      senderKeypairs.push(senderKp);

      senderWallets.push({
        id: `sender-${i}`,
        address: senderKp.publicKey,
        balance: 0,
        isLoading: false,
        lastUpdate: Date.now(),
      });

      receiverWallets.push({
        id: `receiver-${i}`,
        address: receiverPubkey,
        balance: 0,
        isLoading: false,
        lastUpdate: Date.now(),
      });
    }

    setSenders(senderWallets);
    setReceivers(receiverWallets);
    testStateRef.current.senderKeypairs = senderKeypairs;
    testStateRef.current.senderWallets = senderWallets;
    testStateRef.current.receiverWallets = receiverWallets;

    // Create mint
    const mint = Keypair.generate().publicKey;
    testStateRef.current.mint = mint;

    console.log("Initializing mint...");
    const mintTx = createMintTransaction(admin, mint);
    await sendTransaction(mintTx, [admin], WRITE_URL, READ_URL);

    // Create all ATAs in parallel
    console.log("Creating token accounts...");
    await Promise.all(
      senderKeypairs.flatMap((kp, i) => [
        createATATransaction(kp, kp.publicKey, mint).then((tx) =>
          sendTransaction(tx, [kp], WRITE_URL, READ_URL)
        ),
        createATATransaction(kp, receiverWallets[i].address, mint).then((tx) =>
          sendTransaction(tx, [kp], WRITE_URL, READ_URL)
        ),
      ])
    );

    // Wait for ATAs to propagate
    await new Promise((resolve) => setTimeout(resolve, 1000));

    // Mint tokens to all senders in parallel
    console.log("Minting tokens to senders...");
    await Promise.all(
      senderWallets.map(async (sender) => {
        const ata = await getAssociatedTokenAddress(mint, sender.address);
        const mintToTx = createMintToTransaction(admin, mint, ata, INITIAL_BALANCE);
        return sendTransaction(mintToTx, [admin], WRITE_URL, READ_URL);
      })
    );

    // Update initial balances in parallel
    await Promise.all(
      senderWallets.map((sender) =>
        updateWalletBalance(sender.address.toString(), "sender")
      )
    );
  };

  const startTest = async (params: TestParams) => {
    setIsRunning(true);
    testStateRef.current.isRunning = true;
    testStateRef.current.testDuration = params.duration;
    testStateRef.current.requestsSentInLastSecond = 0;
    testStateRef.current.lastRpsCalculation = Date.now();
    setTransactions([]);

    setStatistics({
      totalTransactions: 0,
      confirmedTransactions: 0,
      failedTransactions: 0,
      averageSendLatency: 0,
      throughput: 0,
      maxThroughput: 0,
      rps: 0,
      maxRps: 0,
      progress: 0,
      transactionsWithLatency: 0,
      startTime: Date.now(),
    });

    try {
      // Setup accounts if not already done
      if (senders.length === 0) {
        await setupAccounts(params.users);
      }

      // Start concurrent transfer loops for each user
      const runUserTransferLoop = async (userIndex: number) => {
        while (testStateRef.current.isRunning) {
          const senderIndex = userIndex % testStateRef.current.senderKeypairs.length;
          const receiverIndex = Math.floor(
            Math.random() * testStateRef.current.receiverWallets.length
          );

          // Execute transfer and wait for confirmation + balance updates (serial for this user)
          await executeTransfer(senderIndex, receiverIndex);

          // Add delay before this user's next transfer
          if (testStateRef.current.isRunning) {
            await new Promise((resolve) =>
              setTimeout(resolve, params.requestDelay)
            );
          }
        }
      };

      // Start concurrent loops for all users
      const userLoops = [];
      for (let i = 0; i < params.users; i++) {
        userLoops.push(runUserTransferLoop(i));
      }

      // All user loops run concurrently
      Promise.all(userLoops).catch(error => {
        console.error("Transfer loop error:", error);
      });

      // Auto-stop after duration
      setTimeout(() => {
        stopTest();
      }, params.duration * 1000);
    } catch (error) {
      console.error("Failed to start test:", error);
      stopTest();
    }
  };

  const stopTest = useCallback(() => {
    setIsRunning(false);
    testStateRef.current.isRunning = false;

    // Update final statistics
    setStatistics((prev) => {
      // Only set endTime if not already set (to preserve final throughput)
      if (prev.endTime) {
        return prev;
      }

      const endTime = Date.now();
      const duration = prev.startTime ? (endTime - prev.startTime) / 1000 : 0;

      return {
        ...prev,
        endTime,
        throughput: duration > 0 ? prev.totalTransactions / duration : 0,
      };
    });
  }, []);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      stopTest();
    };
  }, [stopTest]);

  return {
    isRunning,
    senders,
    receivers,
    transactions,
    statistics,
    startTest,
    stopTest,
  };
};
