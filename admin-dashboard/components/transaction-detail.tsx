"use client"

import { useEffect, useState } from "react"
import { ArrowRight, Check, Clock, Loader2, XCircle } from "lucide-react"
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
} from "@/components/ui/sheet"
import { Skeleton } from "@/components/ui/skeleton"
import { TruncatedAddress } from "@/components/copy-button"
import { StatusBadge } from "@/components/status-indicator"
import { fetchTransactionDetail } from "@/lib/api"
import type { Transaction } from "@/lib/types"

function PipelineStep({
  label,
  timestamp,
  status,
  isLast,
}: {
  label: string
  timestamp: string | null
  status: "done" | "active" | "pending" | "failed"
  isLast?: boolean
}) {
  const iconMap = {
    done: <Check className="size-3 text-success" />,
    active: <Loader2 className="size-3 text-chart-2 animate-spin" />,
    pending: <Clock className="size-3 text-muted-foreground" />,
    failed: <XCircle className="size-3 text-danger" />,
  }

  const ringMap = {
    done: "border-success bg-success/15",
    active: "border-chart-2 bg-chart-2/15",
    pending: "border-border bg-secondary",
    failed: "border-danger bg-danger/15",
  }

  return (
    <div className="flex items-start gap-3">
      <div className="flex flex-col items-center">
        <div
          className={`flex size-7 items-center justify-center rounded-full border-2 ${ringMap[status]}`}
        >
          {iconMap[status]}
        </div>
        {!isLast && (
          <div className="w-px flex-1 min-h-6 bg-border" />
        )}
      </div>
      <div className="flex flex-col pb-4">
        <span className="text-sm font-medium text-foreground">{label}</span>
        {timestamp && (
          <span className="text-xs text-muted-foreground">
            {new Date(timestamp).toLocaleTimeString()}
          </span>
        )}
      </div>
    </div>
  )
}

export function TransactionDetailSheet({
  tx,
  open,
  onOpenChange,
}: {
  tx: Transaction | null
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const [detail, setDetail] = useState<Transaction | null>(null)
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    if (!tx || !open) {
      setDetail(null)
      return
    }

    setLoading(true)
    fetchTransactionDetail(tx.signature).then((res) => {
      if (res.data) {
        setDetail(res.data)
      } else {
        // Fall back to the transaction from the list
        setDetail(tx)
      }
      setLoading(false)
    })
  }, [tx, open])

  if (!tx) return null

  const d = detail ?? tx

  // Build pipeline steps from the transaction status
  const pipelineSteps: {
    label: string
    timestamp: string | null
    status: "done" | "active" | "pending" | "failed"
  }[] = [
    {
      label: "Source Chain (Solana)",
      timestamp: d.created_at,
      status: "done",
    },
    {
      label: "Indexed",
      timestamp: d.created_at,
      status: "done",
    },
  ]

  if (d.status === "Processing") {
    pipelineSteps.push({
      label: "Processing",
      timestamp: d.updated_at,
      status: "active",
    })
  } else if (d.status === "Failed") {
    pipelineSteps.push({
      label: "Failed",
      timestamp: d.processed_at ?? d.updated_at,
      status: "failed",
    })
  } else if (d.status === "Completed") {
    pipelineSteps.push({
      label: "Processing",
      timestamp: null,
      status: "done",
    })
    pipelineSteps.push({
      label: "Completed",
      timestamp: d.processed_at,
      status: "done",
    })
    pipelineSteps.push({
      label: d.transaction_type === "Deposit" ? "Destination (Contra)" : "Destination (Solana)",
      timestamp: d.processed_at,
      status: "done",
    })
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full sm:max-w-md overflow-y-auto bg-card border-border">
        <SheetHeader className="pb-4">
          <SheetTitle className="text-foreground flex items-center gap-2">
            Transaction Detail
          </SheetTitle>
          <SheetDescription className="flex items-center gap-2">
            <span className="font-mono text-xs">{d.signature}</span>
          </SheetDescription>
        </SheetHeader>

        {loading ? (
          <div className="flex flex-col gap-4 px-4 pb-6">
            <Skeleton className="h-8 w-48" />
            <Skeleton className="h-32 rounded-lg" />
            <Skeleton className="h-24 rounded-lg" />
            <Skeleton className="h-48 rounded-lg" />
          </div>
        ) : (
          <div className="flex flex-col gap-6 px-4 pb-6">
            {/* Header summary */}
            <div className="flex items-center gap-3">
              <span
                className={`inline-flex items-center rounded-sm px-2 py-0.5 text-xs font-medium ${
                  d.transaction_type === "Deposit"
                    ? "bg-success/10 text-success"
                    : "bg-chart-2/10 text-chart-2"
                }`}
              >
                {d.transaction_type}
              </span>
              <StatusBadge status={d.status} />
              {d.latency_ms != null && (
                <span className="text-xs text-muted-foreground font-mono">
                  {(d.latency_ms / 1000).toFixed(1)}s
                </span>
              )}
            </div>

            {/* Payment info */}
            <div className="rounded-lg border border-border bg-background p-4">
              <div className="flex items-center justify-between">
                <div className="flex flex-col gap-1">
                  <span className="text-xs text-muted-foreground">Amount</span>
                  <span className="text-xl font-semibold font-mono text-foreground">
                    {d.amount_display}
                  </span>
                </div>
                <span className="text-sm font-medium text-foreground">
                  {d.mint_symbol}
                </span>
              </div>

              {d.memo && (
                <div className="mt-3 border-t border-border pt-3">
                  <span className="text-xs text-muted-foreground">Memo</span>
                  <p className="text-sm font-mono text-foreground">{d.memo}</p>
                </div>
              )}

              <div className="mt-3 border-t border-border pt-3">
                <div className="flex items-center gap-2">
                  <div className="flex-1">
                    <span className="text-xs text-muted-foreground">From</span>
                    <TruncatedAddress address={d.initiator} chars={6} />
                  </div>
                  <ArrowRight className="size-4 text-muted-foreground shrink-0" />
                  <div className="flex-1">
                    <span className="text-xs text-muted-foreground">To</span>
                    <TruncatedAddress address={d.recipient} chars={6} />
                  </div>
                </div>
              </div>
            </div>

            {/* Signatures */}
            <div className="flex flex-col gap-3">
              <h3 className="text-xs font-semibold uppercase text-muted-foreground tracking-wider">
                Signatures
              </h3>
              <div className="flex flex-col gap-2">
                <div className="flex items-center justify-between">
                  <span className="text-xs text-muted-foreground">
                    {d.transaction_type === "Deposit" ? "Solana (Source)" : "Contra (Source)"}
                  </span>
                  <TruncatedAddress address={d.signature} chars={8} />
                </div>
                {d.counterpart_signature ? (
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-muted-foreground">
                      {d.transaction_type === "Deposit" ? "Contra (Dest)" : "Solana (Dest)"}
                    </span>
                    <TruncatedAddress address={d.counterpart_signature} chars={8} />
                  </div>
                ) : (
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-muted-foreground">
                      {d.transaction_type === "Deposit" ? "Contra (Dest)" : "Solana (Dest)"}
                    </span>
                    <span className="text-xs text-muted-foreground">Pending</span>
                  </div>
                )}
              </div>
            </div>

            {/* Pipeline stepper */}
            <div className="flex flex-col gap-3">
              <h3 className="text-xs font-semibold uppercase text-muted-foreground tracking-wider">
                Pipeline
              </h3>
              <div className="flex flex-col">
                {pipelineSteps.map((step, i) => (
                  <PipelineStep
                    key={i}
                    label={step.label}
                    timestamp={step.timestamp}
                    status={step.status}
                    isLast={i === pipelineSteps.length - 1}
                  />
                ))}
              </div>
            </div>

            {/* Metadata */}
            <div className="flex flex-col gap-3">
              <h3 className="text-xs font-semibold uppercase text-muted-foreground tracking-wider">
                Metadata
              </h3>
              <div className="grid grid-cols-2 gap-3 text-sm">
                <div>
                  <span className="text-xs text-muted-foreground">Slot</span>
                  <p className="font-mono text-foreground">{d.slot.toLocaleString()}</p>
                </div>
                <div>
                  <span className="text-xs text-muted-foreground">Created</span>
                  <p className="text-foreground">{new Date(d.created_at).toLocaleString()}</p>
                </div>
                {d.withdrawal_nonce != null && (
                  <div>
                    <span className="text-xs text-muted-foreground">Nonce</span>
                    <p className="font-mono text-foreground">{d.withdrawal_nonce}</p>
                  </div>
                )}
              </div>
            </div>
          </div>
        )}
      </SheetContent>
    </Sheet>
  )
}
