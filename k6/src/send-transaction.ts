import http from "k6/http";
import { check } from "k6";
import { Rate, Trend } from "k6/metrics";
import { Options } from "k6/options";

// Metrics
const sendDuration = new Trend("send_duration", true);
const sendSuccess = new Rate("send_success");

// Test configuration
export const options: Options = {
  vus: 10,
  duration: "30s",
  thresholds: {
    send_duration: ["p(95)<500"],
    send_success: ["rate>0.95"],
  },
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
  return { rpcUrl: RPC_URL };
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
      params: [transaction, { encoding: "base64", skipPreflight: true }],
    }),
    { headers: { "Content-Type": "application/json" }, timeout: "2s" }
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
}
