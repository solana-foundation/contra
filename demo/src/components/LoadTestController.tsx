import React from 'react';
import { Play, Square, Users, Clock, Activity } from 'lucide-react';
import type { TestParams } from '../types/index';

interface LoadTestControllerProps {
  params: TestParams;
  onParamsChange: (params: TestParams) => void;
  onStart: () => void;
  onStop: () => void;
  isRunning: boolean;
}

export const LoadTestController: React.FC<LoadTestControllerProps> = ({
  params,
  onParamsChange,
  onStart,
  onStop,
  isRunning,
}) => {
  return (
    <div className="load-test-controller">
      <h2>Test Configuration</h2>

      <div className="control-group">
        <label>
          <Users size={16} />
          <span>Users</span>
        </label>
        <input
          type="number"
          min="1"
          max="20"
          value={params.users}
          onChange={(e) =>
            onParamsChange({ ...params, users: parseInt(e.target.value) || 1 })
          }
          disabled={isRunning}
        />
      </div>

      <div className="control-group">
        <label>
          <Clock size={16} />
          <span>Duration (seconds)</span>
        </label>
        <input
          type="number"
          min="5"
          max="300"
          value={params.duration}
          onChange={(e) =>
            onParamsChange({ ...params, duration: parseInt(e.target.value) || 5 })
          }
          disabled={isRunning}
        />
      </div>

      <div className="control-group">
        <label>
          <Activity size={16} />
          <span>Request Delay (ms)</span>
        </label>
        <input
          type="number"
          min="10"
          max="5000"
          value={params.requestDelay}
          onChange={(e) =>
            onParamsChange({
              ...params,
              requestDelay: parseInt(e.target.value) || 10,
            })
          }
          disabled={isRunning}
        />
      </div>

      <div className="control-buttons">
        {!isRunning ? (
          <button
            className="start-button"
            onClick={onStart}
          >
            <Play size={20} />
            Start Test
          </button>
        ) : (
          <button
            className="stop-button"
            onClick={onStop}
          >
            <Square size={20} />
            Stop Test
          </button>
        )}
      </div>
    </div>
  );
};