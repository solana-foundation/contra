export type Transaction = {
  id: number
  signature: string
  slot: number
  transaction_type: "Deposit" | "Withdrawal"
  status: "Completed" | "Processing" | "Failed" | "Pending"
  initiator: string
  recipient: string
  mint: string
  mint_symbol: string
  amount: number
  amount_display: string
  decimals: number
  memo: string | null
  counterpart_signature: string | null
  withdrawal_nonce: number | null
  created_at: string
  processed_at: string | null
  updated_at: string
  latency_ms: number | null
}

export type ProgramCheckpoint = {
  program_type: string
  label: string
  last_indexed_slot: number | null
  chain_head_slot: number
  slot_lag: number
  lag_status: "ok" | "warning" | "critical"
}

export type PipelineSummary = {
  pending: number
  processing: number
  completed_24h: number
  failed_24h: number
}

export type OverviewData = {
  status: "healthy" | "degraded" | "critical"
  programs: ProgramCheckpoint[]
  pipeline_summary: PipelineSummary
}

export type TransactionListData = {
  page: number
  per_page: number
  total_count: number
  transactions: Transaction[]
}

export type ThroughputWindow = {
  completed: number
  failed: number
  avg_latency_ms: number
}

export type FailureItem = {
  id: number
  signature: string
  transaction_type: "Deposit" | "Withdrawal"
  mint_symbol: string
  amount_display: string
  failed_at: string
  created_at: string
}

export type StuckItem = {
  id: number
  signature: string
  status: "Pending" | "Processing"
  transaction_type: "Deposit" | "Withdrawal"
  mint_symbol: string
  amount_display: string
  stuck_since: string
  age_seconds: number
}

export type PipelineData = {
  by_status: Record<string, number>
  by_type: Record<string, number>
  throughput: {
    last_1h: ThroughputWindow
    last_24h: ThroughputWindow
    last_7d: ThroughputWindow
  }
  recent_failures: FailureItem[]
  stuck_transactions: StuckItem[]
}

export type ReconciliationMint = {
  mint_address: string
  mint_symbol: string
  token_program: string
  decimals: number
  total_deposits: number
  total_withdrawals: number
  indexed_deposits_display: string
  completed_withdrawals_display: string
  expected_balance_display: string
  actual_onchain_balance: number | null
  actual_onchain_balance_display: string
  difference: number
  difference_display: string
  reconciliation_status: "balanced" | "warning" | "mismatch" | "unknown"
}

export type ReconciliationData = {
  checked_at: string
  mints: ReconciliationMint[]
}

export type CheckpointProgram = {
  program_type: string
  label: string
  last_indexed_slot: number | null
  slot_lag: number
  estimated_time_lag_seconds: number
  status: "ok" | "warning" | "critical"
}

export type CheckpointsData = {
  chain_head_slot: number
  programs: CheckpointProgram[]
}
