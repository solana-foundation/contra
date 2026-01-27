import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend, Counter } from "k6/metrics";
import { Options } from "k6/options";

const MULTIPLIER = 10;

// Metrics
const sendDuration = new Trend("send_duration", true);
const sendSuccess = new Rate("send_success");
const requestsPerSecond = new Counter("requests_per_second");

// Test configuration for maximum throughput
export const options: Options = {
  stages: [
    { duration: "5s", target: 10 * MULTIPLIER },
    { duration: "5s", target: 20 * MULTIPLIER },
    { duration: "5s", target: 30 * MULTIPLIER },
    { duration: "5s", target: 40 * MULTIPLIER },
    { duration: "5s", target: 50 * MULTIPLIER },
    { duration: "5s", target: 60 * MULTIPLIER },
    { duration: "5s", target: 70 * MULTIPLIER },
    { duration: "5s", target: 80 * MULTIPLIER },
    { duration: "5s", target: 90 * MULTIPLIER },
    { duration: "5s", target: 100 * MULTIPLIER },
    { duration: "5s", target: 0 },
  ],
  thresholds: {
    send_duration: ["p(95)<1000"], // Relaxed to 1s for load testing
    send_success: ["rate>0.90"], // Allow 90% success under heavy load
    http_req_duration: ["p(95)<2000"], // 2s max response time
  },
  noConnectionReuse: false, // Keep connections alive for better throughput
  batch: 10, // Batch requests for efficiency
  batchPerHost: 10,
};

// RPC endpoint from environment
const RPC_URL = __ENV.RPC_URL || "http://localhost:8899";

// Multiple pre-generated valid transactions for variety
// (transaction, signature)
const TRANSACTIONS = [
  [
    "Adjp0mgjSImVcqpOF9cPnWwLA+gIjVTPjLXPHJvLz4yXsqs8ZUq3CWmw3cwCJZByCeeYwFz2mPaLgySJLcX0MAMBAAEEjyQhsBs6jOMoVmZ3uuOU8yU0U6sK6sMZWOMXZieLlqGA9xfkCdoH8N2abFxsqE4RQx5ZXAR0lhz0P4N7hUEaxvwT2zM68K6hdmXhFHrcmkjA3fVy+OSJKo7Kva/7wygFBt324ddloZPZy+FGzut5rBy0he1fWzeROoz1hX7/AKkAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEDAwECAAkDWwEAAAAAAAA=",
    "5LXyMG4yXLdnwW76npBLovJ2v8MQrhfL22FquxzojJQcn6Vx1VGzoc8AEWSKGWnAqgYcVXqNnptB6Dud7mRUvJfC",
  ],
  // Add more if you have them, or just cycle through the same one
];

export function setup() {
  // k6 doesn't have console, just return data
  return {
    rpcUrl: RPC_URL,
    testType: "Maximum Throughput Test",
  };
}

export default function () {
  // Pick a transaction (cycle through if multiple)
  const txIndex = __ITER % TRANSACTIONS.length;
  const [transaction, expectedSignature] = TRANSACTIONS[txIndex];

  const start = Date.now();

  const response = http.post(
    RPC_URL,
    JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "sendTransaction",
      params: [
        transaction,
        {
          encoding: "base64",
          skipPreflight: true,
        },
      ],
    }),
    {
      headers: { "Content-Type": "application/json" },
      timeout: "5s", // Increased timeout under load
    }
  );

  const duration = Date.now() - start;
  sendDuration.add(duration);

  const success = check(response, {
    "status is 200": (r) => r.status === 200,
    "has signature": (r) => {
      const body = JSON.parse(r.body as string);
      // Solana signatures are base58 encoded and typically 87-88 characters long
      return (
        body.result &&
        typeof body.result === "string" &&
        body.result === expectedSignature
      );
    },
  });

  sendSuccess.add(success ? 1 : 0);
  requestsPerSecond.add(1);

  // Small sleep to prevent overwhelming the server
  if (!success && response.status === 0) {
    // Connection error - back off slightly
    __ENV.K6_SLEEP && sleep(0.1);
  }
}

export function teardown(data: any) {
  // Test completed - metrics will be shown by k6
}
