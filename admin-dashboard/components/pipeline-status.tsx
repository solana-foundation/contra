"use client"

import { AlertTriangle, Clock, XCircle } from "lucide-react"
import { Skeleton } from "@/components/ui/skeleton"
import { TruncatedAddress } from "@/components/copy-button"
import type { PipelineData } from "@/lib/types"

function DonutChart({ data }: { data: PipelineData }) {
  const byStatus = data.by_status
  const total = Object.values(byStatus).reduce((a, b) => a + b, 0)
  if (total === 0) return null

  const segments = [
    { label: "Completed", value: byStatus["Completed"] ?? 0, color: "var(--success)" },
    { label: "Pending", value: byStatus["Pending"] ?? 0, color: "var(--warning)" },
    { label: "Processing", value: byStatus["Processing"] ?? 0, color: "var(--chart-2)" },
    { label: "Failed", value: byStatus["Failed"] ?? 0, color: "var(--danger)" },
  ]

  const radius = 42
  const cx = 50
  const cy = 50
  const circumference = 2 * Math.PI * radius

  let cumulativePercent = 0
  const paths = segments.map((seg) => {
    const percent = seg.value / total
    const offset = cumulativePercent * circumference
    const length = percent * circumference
    cumulativePercent += percent

    return (
      <circle
        key={seg.label}
        cx={cx}
        cy={cy}
        r={radius}
        fill="none"
        stroke={seg.color}
        strokeWidth="10"
        strokeDasharray={`${length} ${circumference - length}`}
        strokeDashoffset={-offset}
        className="transition-all duration-500"
      />
    )
  })

  return (
    <div className="flex items-center gap-6">
      <div className="relative">
        <svg width="110" height="110" viewBox="0 0 100 100" className="-rotate-90">
          {paths}
        </svg>
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span className="text-lg font-semibold font-mono text-foreground">
            {total.toLocaleString()}
          </span>
          <span className="text-[10px] text-muted-foreground">total</span>
        </div>
      </div>
      <div className="flex flex-col gap-2">
        {segments.map((seg) => (
          <div key={seg.label} className="flex items-center gap-2">
            <span
              className="size-2 rounded-full"
              style={{ backgroundColor: seg.color }}
            />
            <span className="text-xs text-muted-foreground w-20">{seg.label}</span>
            <span className="font-mono text-xs text-foreground">
              {seg.value.toLocaleString()}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}

function ThroughputCard({
  label,
  completed,
  failed,
  avgLatency,
}: {
  label: string
  completed: number
  failed: number
  avgLatency: number
}) {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-card p-4">
      <span className="text-xs font-medium text-muted-foreground">{label}</span>
      <div className="flex flex-col gap-1">
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Completed</span>
          <span className="font-mono text-sm font-medium text-success">
            {completed.toLocaleString()}
          </span>
        </div>
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Failed</span>
          <span
            className={`font-mono text-sm font-medium ${failed > 0 ? "text-danger" : "text-muted-foreground"}`}
          >
            {failed}
          </span>
        </div>
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Avg Latency</span>
          <span className="font-mono text-sm text-foreground">
            {avgLatency > 0 ? `${(avgLatency / 1000).toFixed(1)}s` : "—"}
          </span>
        </div>
      </div>
    </div>
  )
}

export function PipelineStatus({
  data,
  loading,
}: {
  data: PipelineData | null
  loading: boolean
}) {
  if (loading || !data) {
    return (
      <section aria-label="Pipeline Status">
        <Skeleton className="h-4 w-32" />
        <div className="mt-3 grid grid-cols-1 gap-6 lg:grid-cols-2">
          <Skeleton className="h-48 rounded-lg" />
          <div className="flex flex-col gap-3">
            <Skeleton className="h-24 rounded-lg" />
            <Skeleton className="h-24 rounded-lg" />
            <Skeleton className="h-24 rounded-lg" />
          </div>
        </div>
      </section>
    )
  }

  return (
    <section aria-label="Pipeline Status">
      <h2 className="text-sm font-semibold text-foreground tracking-wide uppercase">
        Pipeline Health
      </h2>

      <div className="mt-3 grid grid-cols-1 gap-6 lg:grid-cols-2">
        {/* Donut chart */}
        <div className="rounded-lg border border-border bg-card p-4">
          <span className="text-xs font-medium text-muted-foreground">Status Distribution</span>
          <div className="mt-3">
            <DonutChart data={data} />
          </div>
          <div className="mt-3 flex gap-4 border-t border-border pt-3">
            <div>
              <span className="text-xs text-muted-foreground">Deposits</span>
              <p className="font-mono text-sm font-medium text-foreground">
                {(data.by_type["Deposit"] ?? 0).toLocaleString()}
              </p>
            </div>
            <div>
              <span className="text-xs text-muted-foreground">Withdrawals</span>
              <p className="font-mono text-sm font-medium text-foreground">
                {(data.by_type["Withdrawal"] ?? 0).toLocaleString()}
              </p>
            </div>
          </div>
        </div>

        {/* Throughput */}
        <div className="flex flex-col gap-3">
          <ThroughputCard
            label="Last 1 Hour"
            completed={data.throughput.last_1h.completed}
            failed={data.throughput.last_1h.failed}
            avgLatency={data.throughput.last_1h.avg_latency_ms}
          />
          <ThroughputCard
            label="Last 24 Hours"
            completed={data.throughput.last_24h.completed}
            failed={data.throughput.last_24h.failed}
            avgLatency={data.throughput.last_24h.avg_latency_ms}
          />
          <ThroughputCard
            label="Last 7 Days"
            completed={data.throughput.last_7d.completed}
            failed={data.throughput.last_7d.failed}
            avgLatency={data.throughput.last_7d.avg_latency_ms}
          />
        </div>
      </div>

      {/* Recent failures & stuck transactions */}
      <div className="mt-6 grid grid-cols-1 gap-3 lg:grid-cols-2">
        {/* Recent failures */}
        <div className="rounded-lg border border-border bg-card p-4">
          <div className="flex items-center gap-2">
            <XCircle className="size-4 text-danger" />
            <span className="text-xs font-medium text-foreground">Recent Failures</span>
            <span className="ml-auto rounded-full bg-danger/15 px-1.5 py-0.5 text-[10px] font-medium text-danger">
              {data.recent_failures.length}
            </span>
          </div>
          <div className="mt-3 flex flex-col gap-2">
            {data.recent_failures.length === 0 && (
              <p className="text-xs text-muted-foreground py-2">No recent failures</p>
            )}
            {data.recent_failures.map((f) => (
              <div
                key={f.id}
                className="flex items-center justify-between rounded-md bg-background px-3 py-2"
              >
                <div className="flex flex-col gap-0.5">
                  <div className="flex items-center gap-2">
                    <span
                      className={`text-xs font-medium ${
                        f.transaction_type === "Deposit"
                          ? "text-success"
                          : "text-chart-2"
                      }`}
                    >
                      {f.transaction_type}
                    </span>
                    <span className="font-mono text-xs text-foreground">
                      {f.amount_display} {f.mint_symbol}
                    </span>
                  </div>
                  <TruncatedAddress address={f.signature} chars={6} className="text-muted-foreground" />
                </div>
                <span className="text-[11px] text-muted-foreground">
                  {new Date(f.failed_at).toLocaleTimeString()}
                </span>
              </div>
            ))}
          </div>
        </div>

        {/* Stuck transactions */}
        <div className="rounded-lg border border-border bg-card p-4">
          <div className="flex items-center gap-2">
            <AlertTriangle className="size-4 text-warning" />
            <span className="text-xs font-medium text-foreground">Stuck Transactions</span>
            <span className="ml-auto rounded-full bg-warning/15 px-1.5 py-0.5 text-[10px] font-medium text-warning">
              {data.stuck_transactions.length}
            </span>
          </div>
          <div className="mt-3 flex flex-col gap-2">
            {data.stuck_transactions.length === 0 && (
              <p className="text-xs text-muted-foreground py-2">No stuck transactions</p>
            )}
            {data.stuck_transactions.map((s) => (
              <div
                key={s.id}
                className="flex items-center justify-between rounded-md bg-background px-3 py-2"
              >
                <div className="flex flex-col gap-0.5">
                  <div className="flex items-center gap-2">
                    <span className={`text-xs font-medium ${
                      s.transaction_type === "Deposit"
                        ? "text-success"
                        : "text-chart-2"
                    }`}>
                      {s.transaction_type}
                    </span>
                    <span className="font-mono text-xs text-foreground">
                      {s.amount_display} {s.mint_symbol}
                    </span>
                  </div>
                  <TruncatedAddress address={s.signature} chars={6} className="text-muted-foreground" />
                </div>
                <div className="flex items-center gap-1.5">
                  <Clock className="size-3 text-warning" />
                  <span className="font-mono text-xs text-warning">
                    {Math.floor(s.age_seconds / 60)}m {s.age_seconds % 60}s
                  </span>
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  )
}
