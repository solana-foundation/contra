import type {
  OverviewData,
  TransactionListData,
  Transaction,
  PipelineData,
  ReconciliationData,
  CheckpointsData,
} from "./types"
import {
  mockOverview,
  mockTransactionList,
  mockTransactionDetail,
  mockPipeline,
  mockReconciliation,
  mockCheckpoints,
} from "./mock-data"

const DEMO = process.env.NEXT_PUBLIC_DEMO === "true"

const BASE_URL =
  process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:3001"

// Token is exposed to the client bundle — acceptable because the dashboard
// is an internal operator tool, not a public-facing app. The operator sets
// both ADMIN_API_TOKEN on the API server and this value on the dashboard.
const API_TOKEN = process.env.NEXT_PUBLIC_ADMIN_API_TOKEN

type ApiResult<T> = { data: T; error: null } | { data: null; error: string }

function ok<T>(data: T): ApiResult<T> {
  return { data, error: null }
}

async function apiFetch<T>(path: string): Promise<ApiResult<T>> {
  try {
    const headers: Record<string, string> = {}
    if (API_TOKEN) {
      headers["Authorization"] = `Bearer ${API_TOKEN}`
    }
    const res = await fetch(`${BASE_URL}${path}`, {
      cache: "no-store",
      headers,
    })
    if (!res.ok) {
      return { data: null, error: `HTTP ${res.status}: ${res.statusText}` }
    }
    const data = (await res.json()) as T
    return { data, error: null }
  } catch (e) {
    return {
      data: null,
      error: e instanceof Error ? e.message : "Unknown error",
    }
  }
}

export async function fetchOverview(): Promise<ApiResult<OverviewData>> {
  if (DEMO) return ok(mockOverview)
  return apiFetch<OverviewData>("/api/overview")
}

export async function fetchTransactions(params?: {
  page?: number
  per_page?: number
  status?: string
  type?: string
}): Promise<ApiResult<TransactionListData>> {
  if (DEMO) {
    let txs = mockTransactionList.transactions
    if (params?.status && params.status !== "all")
      txs = txs.filter((tx) => tx.status === params.status)
    if (params?.type && params.type !== "all")
      txs = txs.filter((tx) => tx.transaction_type === params.type)
    return ok({
      ...mockTransactionList,
      transactions: txs,
      total_count: txs.length,
    })
  }
  const searchParams = new URLSearchParams()
  if (params?.page) searchParams.set("page", String(params.page))
  if (params?.per_page) searchParams.set("per_page", String(params.per_page))
  if (params?.status && params.status !== "all")
    searchParams.set("status", params.status)
  if (params?.type && params.type !== "all")
    searchParams.set("type", params.type)
  const qs = searchParams.toString()
  return apiFetch<TransactionListData>(
    `/api/transactions${qs ? `?${qs}` : ""}`
  )
}

export async function fetchTransactionDetail(
  signature: string
): Promise<ApiResult<Transaction>> {
  if (DEMO) {
    const tx = mockTransactionDetail(signature)
    return tx ? ok(tx) : { data: null, error: "Not found" }
  }
  return apiFetch<Transaction>(`/api/transactions/${signature}`)
}

export async function fetchPipeline(): Promise<ApiResult<PipelineData>> {
  if (DEMO) return ok(mockPipeline)
  return apiFetch<PipelineData>("/api/pipeline")
}

export async function fetchReconciliation(): Promise<
  ApiResult<ReconciliationData>
> {
  if (DEMO) return ok(mockReconciliation)
  return apiFetch<ReconciliationData>("/api/reconciliation")
}

export async function fetchCheckpoints(): Promise<
  ApiResult<CheckpointsData>
> {
  if (DEMO) return ok(mockCheckpoints)
  return apiFetch<CheckpointsData>("/api/checkpoints")
}
