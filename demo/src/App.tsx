import { useState } from 'react';
import { LoadTestController } from './components/LoadTestController';
import { WalletVisualizer } from './components/WalletVisualizer';
import { TransactionFlow } from './components/TransactionFlow';
import { Statistics } from './components/Statistics';
import { useLoadTest } from './hooks/useLoadTest';
import './App.css';

function App() {
  const [testParams, setTestParams] = useState({
    users: 5,
    duration: 30,
    requestDelay: 100,
  });

  const {
    isRunning,
    senders,
    receivers,
    transactions,
    statistics,
    balances,
    startTest,
    stopTest,
  } = useLoadTest();

  return (
    <div className="app">
      <header className="app-header">
        <h1>Contra Load Test Visualizer</h1>
        <p>Interactive load testing for Contra deployment</p>
      </header>

      <div className="main-content">
        <div className="control-panel">
          <LoadTestController
            params={testParams}
            onParamsChange={setTestParams}
            onStart={() => startTest(testParams)}
            onStop={stopTest}
            isRunning={isRunning}
          />
          <Statistics statistics={statistics} />
        </div>

        <div className="visualization-area">
          <div className="wallets-container">
            <WalletVisualizer
              title="Senders"
              wallets={senders}
              balances={balances}
            />
            <TransactionFlow transactions={transactions} />
            <WalletVisualizer
              title="Receivers"
              wallets={receivers}
              balances={balances}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;