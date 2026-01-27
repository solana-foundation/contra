import { useState, useMemo } from "react";
import "./App.css";
import { ConnectWalletButton } from "./components/ConnectWalletButton";
import { InstanceManager } from "./components/InstanceManager";
import { AdminFunctions } from "./components/AdminFunctions";
import { OperatorFunctions } from "./components/OperatorFunctions";
import { UserFunctions } from "./components/UserFunctions";
import { StatusChecker } from "./components/StatusChecker";
import { MintManager } from "./components/MintManager";
import { ContraManagement } from "./components/ContraManagement";
import { useWallet } from "./hooks/useWallet";
import { useCluster } from "./hooks/useCluster";
import type { NetworkType } from "./context/ClusterContext";
import { createSolanaRpc } from "@solana/rpc";
import { createSolanaRpcSubscriptions } from "@solana/rpc-subscriptions";
import { SolanaContext } from "./context/SolanaContext";

type TabType = "escrow" | "mint" | "contra";

function AppContent() {
  const { connected, publicKey } = useWallet();
  const { network, setNetwork } = useCluster();
  const [instancePubkey, setInstancePubkey] = useState<string>("");
  const [activeTab, setActiveTab] = useState<TabType>("escrow");

  return (
    <div className="app-container">
      <header className="app-header">
        <h1>Contra Admin UI</h1>
        <div
          className="header-actions"
          style={{ display: "flex", gap: "1rem", alignItems: "center" }}
        >
          <select
            value={network}
            onChange={(e) => setNetwork(e.target.value as NetworkType)}
            className="input"
            style={{ padding: "0.5rem", minWidth: "120px" }}
          >
            <option value="devnet">Devnet</option>
            <option value="testnet">Testnet</option>
            <option value="mainnet-beta">Mainnet</option>
            <option value="localnet">Localnet</option>
          </select>
          <div style={{ minWidth: "200px" }}>
            <ConnectWalletButton />
          </div>
        </div>
      </header>

      <main className="app-main">
        {!connected ? (
          <div className="connect-prompt">
            <h2>Connect your wallet to manage your Contra instance</h2>
            <p>Use the button above to connect your Solana wallet</p>
            <p className="info-text">Powered by Anza Wallet Adapter</p>
          </div>
        ) : (
          <div className="dashboard">
            <div className="wallet-info">
              <h3>Connected Wallet</h3>
              <p className="wallet-address">{publicKey?.toBase58()}</p>
            </div>

            <div className="tabs">
              <button
                className={`tab ${activeTab === "escrow" ? "active" : ""}`}
                onClick={() => setActiveTab("escrow")}
              >
                Escrow Management
              </button>
              <button
                className={`tab ${activeTab === "mint" ? "active" : ""}`}
                onClick={() => setActiveTab("mint")}
              >
                Mint Management
              </button>
              <button
                className={`tab ${activeTab === "contra" ? "active" : ""}`}
                onClick={() => setActiveTab("contra")}
              >
                Contra Management
              </button>
            </div>

            <div className="tab-content">
              <div
                style={{ display: activeTab === "escrow" ? "block" : "none" }}
              >
                <InstanceManager onInstanceSelect={setInstancePubkey} />

                {instancePubkey && (
                  <>
                    <AdminFunctions instancePubkey={instancePubkey} />
                    <StatusChecker instancePubkey={instancePubkey} />
                    <OperatorFunctions instancePubkey={instancePubkey} />
                    <UserFunctions instancePubkey={instancePubkey} />
                  </>
                )}
              </div>

              <div style={{ display: activeTab === "mint" ? "block" : "none" }}>
                <MintManager />
              </div>

              <div
                style={{ display: activeTab === "contra" ? "block" : "none" }}
              >
                <ContraManagement />
              </div>
            </div>
          </div>
        )}
      </main>

      <footer className="app-footer">
        <p>Contra Escrow & Withdraw Management Interface</p>
        <p className="footer-note">
          Built with Anza Wallet Adapter & @solana/kit - Modern Solana tooling!
        </p>
      </footer>
    </div>
  );
}

function App() {
  const { endpoint, wsEndpoint } = useCluster();

  const rpc = useMemo(() => createSolanaRpc(endpoint), [endpoint]);
  const rpcSubscriptions = useMemo(() => {
    const wsUrl =
      wsEndpoint ||
      endpoint.replace("https://", "wss://").replace("http://", "ws://");
    return createSolanaRpcSubscriptions(wsUrl);
  }, [endpoint, wsEndpoint]);

  return (
    <SolanaContext.Provider value={{ rpc, rpcSubscriptions }}>
      <AppContent />
    </SolanaContext.Provider>
  );
}

export default App;
