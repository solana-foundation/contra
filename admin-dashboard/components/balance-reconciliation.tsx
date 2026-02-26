"use client"

import { useState, useEffect, useCallback } from "react"
import { RefreshCw, ShieldCheck, AlertTriangle } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Skeleton } from "@/components/ui/skeleton"
import { TruncatedAddress } from "@/components/copy-button"
import { fetchReconciliation } from "@/lib/api"
import type { ReconciliationData } from "@/lib/types"

function ReconciliationBadge({
  status,
}: {
  status: "balanced" | "warning" | "mismatch" | "unknown"
}) {
  const config = {
    balanced: {
      bg: "bg-success/15 text-success",
      icon: ShieldCheck,
      label: "Balanced",
    },
    warning: {
      bg: "bg-warning/15 text-warning",
      icon: AlertTriangle,
      label: "Warning",
    },
    mismatch: {
      bg: "bg-danger/15 text-danger",
      icon: AlertTriangle,
      label: "Mismatch",
    },
    unknown: {
      bg: "bg-secondary text-muted-foreground",
      icon: AlertTriangle,
      label: "Unknown",
    },
  }
  const c = config[status]

  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ${c.bg}`}
    >
      <c.icon className="size-3" />
      {c.label}
    </span>
  )
}

export function BalanceReconciliation({ refreshKey }: { refreshKey?: number }) {
  const [refreshing, setRefreshing] = useState(false)
  const [data, setData] = useState<ReconciliationData | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    const res = await fetchReconciliation()
    if (res.data) {
      setData(res.data)
      setError(null)
    } else {
      setError(res.error ?? "Failed to load reconciliation data")
    }
    setLoading(false)
  }, [])

  useEffect(() => {
    load()
  }, [load, refreshKey])

  const handleRefresh = async () => {
    setRefreshing(true)
    await load()
    setRefreshing(false)
  }

  if (loading) {
    return (
      <section aria-label="Balance Reconciliation">
        <div className="flex items-center justify-between">
          <Skeleton className="h-4 w-48" />
          <Skeleton className="h-8 w-20" />
        </div>
        <Skeleton className="mt-3 h-48 rounded-lg" />
      </section>
    )
  }

  if (error && !data) {
    return (
      <section aria-label="Balance Reconciliation">
        <div className="flex items-center justify-between">
          <h2 className="text-sm font-semibold text-foreground tracking-wide uppercase">
            Balance Reconciliation
          </h2>
          <Button variant="outline" size="sm" onClick={handleRefresh} className="gap-1.5 text-xs">
            <RefreshCw className="size-3" />
            Retry
          </Button>
        </div>
        <div className="mt-3 rounded-lg border border-border bg-card p-6 text-center text-sm text-muted-foreground">
          Failed to load reconciliation data
        </div>
      </section>
    )
  }

  if (!data) return null

  return (
    <section aria-label="Balance Reconciliation">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-sm font-semibold text-foreground tracking-wide uppercase">
            Balance Reconciliation
          </h2>
          <p className="text-xs text-muted-foreground">
            Last checked {new Date(data.checked_at).toLocaleTimeString()}
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={handleRefresh}
          className="gap-1.5 text-xs"
        >
          <RefreshCw
            className={`size-3 ${refreshing ? "animate-spin" : ""}`}
          />
          Refresh
        </Button>
      </div>

      <div className="mt-3 rounded-lg border border-border overflow-hidden">
        <Table>
          <TableHeader>
            <TableRow className="border-border hover:bg-transparent">
              <TableHead className="text-xs text-muted-foreground font-medium">Mint</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Deposits</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Withdrawals</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Expected</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">On-Chain</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Diff</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Status</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.mints.map((mint) => (
              <TableRow key={mint.mint_address} className="border-border">
                <TableCell>
                  <div className="flex flex-col gap-0.5">
                    <span className="font-medium text-sm text-foreground">{mint.mint_symbol}</span>
                    <TruncatedAddress address={mint.mint_address} chars={4} className="text-muted-foreground" />
                  </div>
                </TableCell>
                <TableCell className="text-right font-mono text-sm text-foreground">
                  {mint.indexed_deposits_display}
                </TableCell>
                <TableCell className="text-right font-mono text-sm text-foreground">
                  {mint.completed_withdrawals_display}
                </TableCell>
                <TableCell className="text-right font-mono text-sm text-foreground">
                  {mint.expected_balance_display}
                </TableCell>
                <TableCell className="text-right font-mono text-sm text-foreground">
                  {mint.actual_onchain_balance_display}
                </TableCell>
                <TableCell className="text-right font-mono text-sm">
                  <span
                    className={
                      mint.difference === 0
                        ? "text-muted-foreground"
                        : "text-warning"
                    }
                  >
                    {mint.difference === 0 ? "0.00" : mint.difference_display}
                  </span>
                </TableCell>
                <TableCell className="text-right">
                  <ReconciliationBadge status={mint.reconciliation_status} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </section>
  )
}
