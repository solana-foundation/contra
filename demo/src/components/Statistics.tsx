import React from 'react';
import { TrendingUp, CheckCircle, XCircle, Clock, Activity, Zap, Gauge, BarChart3 } from 'lucide-react';
import type { TestStatistics } from '../types/index';

interface StatisticsProps {
  statistics: TestStatistics;
}

export const Statistics: React.FC<StatisticsProps> = ({ statistics }) => {
  const formatLatency = (latency: number) => {
    if (latency === 0) return '0ms';
    return `${latency.toFixed(0)}ms`;
  };

  const formatThroughput = (throughput: number) => {
    if (throughput === 0) return '0 tx/s';
    return `${throughput.toFixed(2)} tx/s`;
  };

  const formatRps = (rps: number) => {
    if (rps === 0) return '0 req/s';
    return `${rps.toFixed(0)} req/s`;
  };

  const getSuccessRate = () => {
    if (statistics.totalTransactions === 0) return 0;
    return ((statistics.confirmedTransactions / statistics.totalTransactions) * 100).toFixed(1);
  };

  return (
    <div className="statistics">
      <h2>Statistics</h2>

      <div className="stats-grid">
        <div className="stat-item">
          <div className="stat-icon">
            <TrendingUp size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{statistics.totalTransactions}</div>
            <div className="stat-label">Total Transactions</div>
          </div>
        </div>

        <div className="stat-item success">
          <div className="stat-icon">
            <CheckCircle size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{statistics.confirmedTransactions}</div>
            <div className="stat-label">Confirmed</div>
          </div>
        </div>

        <div className="stat-item error">
          <div className="stat-icon">
            <XCircle size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{statistics.failedTransactions}</div>
            <div className="stat-label">Failed</div>
          </div>
        </div>

        <div className="stat-item">
          <div className="stat-icon">
            <Clock size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{formatLatency(statistics.averageSendLatency)}</div>
            <div className="stat-label">Avg Send Latency</div>
          </div>
        </div>

        <div className="stat-item">
          <div className="stat-icon">
            <Zap size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{formatThroughput(statistics.throughput)}</div>
            <div className="stat-label">Live TX/s</div>
          </div>
        </div>

        <div className="stat-item">
          <div className="stat-icon">
            <Activity size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{formatThroughput(statistics.maxThroughput)}</div>
            <div className="stat-label">Max TX/s</div>
          </div>
        </div>

        <div className="stat-item">
          <div className="stat-icon">
            <Gauge size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{formatRps(statistics.rps)}</div>
            <div className="stat-label">Live RPS</div>
          </div>
        </div>

        <div className="stat-item">
          <div className="stat-icon">
            <BarChart3 size={20} />
          </div>
          <div className="stat-content">
            <div className="stat-value">{formatRps(statistics.maxRps)}</div>
            <div className="stat-label">Max RPS</div>
          </div>
        </div>
      </div>

      <div className="progress-section">
        <div className="progress-label">Test Progress</div>
        <div className="progress-bar">
          <div
            className="progress-fill"
            style={{ width: `${statistics.progress}%` }}
          />
        </div>
        <div className="progress-value">{statistics.progress.toFixed(0)}%</div>
      </div>

      <div className="success-rate">
        <div className="rate-label">Success Rate</div>
        <div className="rate-bar">
          <div
            className="rate-fill"
            style={{ width: `${getSuccessRate()}%` }}
          />
        </div>
        <div className="rate-value">{getSuccessRate()}%</div>
      </div>
    </div>
  );
};