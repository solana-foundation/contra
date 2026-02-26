"use client"

import { Activity, Clock, AlertTriangle, CheckCircle2 } from "lucide-react"
import { StatusDot } from "@/components/status-indicator"
import { Skeleton } from "@/components/ui/skeleton"
import type { OverviewData } from "@/lib/types"

function MetricCard({
  label,
  value,
  icon: Icon,
  accent,
}: {
  label: string
  value: string | number
  icon: React.ElementType
  accent?: string
}) {
  return (
    <div className="flex items-center gap-3 rounded-lg border border-border bg-card px-4 py-3">
      <div className={`flex size-9 items-center justify-center rounded-md ${accent ?? "bg-secondary"}`}>
        <Icon className="size-4 text-foreground" />
      </div>
      <div className="flex flex-col">
        <span className="text-xs text-muted-foreground">{label}</span>
        <span className="text-lg font-semibold leading-tight tracking-tight font-mono">{value}</span>
      </div>
    </div>
  )
}

function MetricCardSkeleton() {
  return (
    <div className="flex items-center gap-3 rounded-lg border border-border bg-card px-4 py-3">
      <Skeleton className="size-9 rounded-md" />
      <div className="flex flex-col gap-1">
        <Skeleton className="h-3 w-16" />
        <Skeleton className="h-6 w-12" />
      </div>
    </div>
  )
}

export function SystemOverview({
  data,
  loading,
}: {
  data: OverviewData | null
  loading: boolean
}) {
  if (loading || !data) {
    return (
      <section aria-label="System Overview">
        <div className="flex items-center gap-3">
          <Skeleton className="size-2.5 rounded-full" />
          <Skeleton className="h-5 w-48" />
        </div>
        <div className="mt-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
          <MetricCardSkeleton />
          <MetricCardSkeleton />
          <MetricCardSkeleton />
          <MetricCardSkeleton />
        </div>
      </section>
    )
  }

  const statusLevel =
    data.status === "healthy" ? "healthy" : data.status === "degraded" ? "warning" : "critical"
  const statusLabel =
    data.status === "healthy" ? "All Systems Operational" : data.status === "degraded" ? "Degraded" : "Critical"

  return (
    <section aria-label="System Overview">
      {/* Status banner */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-3">
          <StatusDot status={statusLevel} />
          <div>
            <h2 className="text-sm font-semibold text-foreground">{statusLabel}</h2>
          </div>
        </div>
      </div>

      {/* Metric cards */}
      <div className="mt-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
        <MetricCard
          label="Pending"
          value={data.pipeline_summary.pending}
          icon={Clock}
          accent="bg-warning/15"
        />
        <MetricCard
          label="Processing"
          value={data.pipeline_summary.processing}
          icon={Activity}
          accent="bg-chart-2/15"
        />
        <MetricCard
          label="Completed (24h)"
          value={data.pipeline_summary.completed_24h.toLocaleString()}
          icon={CheckCircle2}
          accent="bg-success/15"
        />
        <MetricCard
          label="Failed (24h)"
          value={data.pipeline_summary.failed_24h}
          icon={AlertTriangle}
          accent="bg-danger/15"
        />
      </div>
    </section>
  )
}
