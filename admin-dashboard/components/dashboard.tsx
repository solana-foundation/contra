"use client"

import { useState, useEffect, useCallback, useRef } from "react"
import { RefreshCw, WifiOff } from "lucide-react"
import { Button } from "@/components/ui/button"
import { SystemOverview } from "@/components/system-overview"
import { CheckpointMonitor } from "@/components/checkpoint-monitor"
import { BalanceReconciliation } from "@/components/balance-reconciliation"
import { TransactionFeed } from "@/components/transaction-feed"
import { TransactionDetailSheet } from "@/components/transaction-detail"
import { PipelineStatus } from "@/components/pipeline-status"
import { fetchOverview, fetchPipeline, fetchCheckpoints } from "@/lib/api"
import type { Transaction, OverviewData, PipelineData, CheckpointsData } from "@/lib/types"

const isDemo = process.env.NEXT_PUBLIC_DEMO === "true"

export function Dashboard() {
  const [selectedTx, setSelectedTx] = useState<Transaction | null>(null)
  const [sheetOpen, setSheetOpen] = useState(false)
  const [secondsUntilRefresh, setSecondsUntilRefresh] = useState(10)
  const [refreshing, setRefreshing] = useState(false)
  const [refreshKey, setRefreshKey] = useState(0)
  const refreshingRef = useRef(false)

  // Data state
  const [overview, setOverview] = useState<OverviewData | null>(null)
  const [pipeline, setPipeline] = useState<PipelineData | null>(null)
  const [checkpoints, setCheckpoints] = useState<CheckpointsData | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  const loadData = useCallback(async () => {
    const [overviewRes, pipelineRes, checkpointsRes] = await Promise.all([
      fetchOverview(),
      fetchPipeline(),
      fetchCheckpoints(),
    ])

    // Show error when ANY fetch fails (collect per-endpoint errors)
    const errors: string[] = []
    if (overviewRes.error) errors.push(`overview: ${overviewRes.error}`)
    if (pipelineRes.error) errors.push(`pipeline: ${pipelineRes.error}`)
    if (checkpointsRes.error) errors.push(`checkpoints: ${checkpointsRes.error}`)

    if (errors.length > 0) {
      setError(errors.join("; "))
    } else {
      setError(null)
    }

    if (overviewRes.data) setOverview(overviewRes.data)
    if (pipelineRes.data) setPipeline(pipelineRes.data)
    if (checkpointsRes.data) setCheckpoints(checkpointsRes.data)
    setLoading(false)
  }, [])

  useEffect(() => {
    loadData()
  }, [loadData])

  const handleRefresh = useCallback(() => {
    if (refreshingRef.current) return
    refreshingRef.current = true
    setRefreshing(true)
    setRefreshKey((k) => k + 1)
    setSecondsUntilRefresh(10)
    loadData().finally(() => {
      setRefreshing(false)
      refreshingRef.current = false
    })
  }, [loadData])

  // Auto-refresh countdown
  useEffect(() => {
    const interval = setInterval(() => {
      setSecondsUntilRefresh((prev) => {
        if (prev <= 1) {
          handleRefresh()
          return 10
        }
        return prev - 1
      })
    }, 1000)
    return () => clearInterval(interval)
  }, [handleRefresh])

  const handleSelectTransaction = (tx: Transaction) => {
    setSelectedTx(tx)
    setSheetOpen(true)
  }

  return (
    <div className="min-h-screen bg-background">
      {/* Header */}
      <header className="sticky top-0 z-40 border-b border-border bg-background/95 backdrop-blur-sm">
        <div className="mx-auto flex max-w-7xl items-center justify-between px-4 py-3 sm:px-6">
          <div className="flex items-center gap-3">
            <div className="flex size-8 items-center justify-center rounded-md bg-primary/15">
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <path
                  d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"
                  stroke="var(--primary)"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </div>
            <div>
              <h1 className="text-sm font-semibold text-foreground tracking-tight">
                Contra Admin
                {isDemo && (
                  <span className="ml-2 inline-flex items-center rounded-sm bg-warning/15 px-1.5 py-0.5 text-[10px] font-medium text-warning">
                    DEMO
                  </span>
                )}
              </h1>
              <p className="text-[11px] text-muted-foreground">
                Operator Dashboard
              </p>
            </div>
          </div>
          <div className="flex items-center gap-3">
            <span className="hidden text-xs text-muted-foreground sm:inline-block font-mono tabular-nums">
              Refresh in {secondsUntilRefresh}s
            </span>
            <Button
              variant="outline"
              size="sm"
              onClick={handleRefresh}
              className="gap-1.5 text-xs"
            >
              <RefreshCw
                className={`size-3 ${refreshing ? "animate-spin" : ""}`}
              />
              <span className="hidden sm:inline">Refresh All</span>
            </Button>
          </div>
        </div>
      </header>

      {/* Error banner */}
      {error && !loading && (
        <div className="mx-auto max-w-7xl px-4 pt-4 sm:px-6">
          <div className="flex items-center gap-2 rounded-lg border border-danger/30 bg-danger/10 px-4 py-3 text-sm text-danger">
            <WifiOff className="size-4 shrink-0" />
            <span>API unreachable: {error}</span>
          </div>
        </div>
      )}

      {/* Main content */}
      <main className="mx-auto max-w-7xl px-4 py-6 sm:px-6">
        <div className="flex flex-col gap-8">
          <SystemOverview data={overview} loading={loading} />
          <CheckpointMonitor data={checkpoints} loading={loading} />
          <BalanceReconciliation refreshKey={refreshKey} />
          <TransactionFeed
            refreshKey={refreshKey}
            onSelectTransaction={handleSelectTransaction}
          />
          <PipelineStatus data={pipeline} loading={loading} />
        </div>
      </main>

      {/* Transaction detail sheet */}
      <TransactionDetailSheet
        tx={selectedTx}
        open={sheetOpen}
        onOpenChange={setSheetOpen}
      />
    </div>
  )
}
