"use client"

import { StatusDot } from "@/components/status-indicator"
import { Skeleton } from "@/components/ui/skeleton"
import { Database } from "lucide-react"
import type { CheckpointsData } from "@/lib/types"

function formatSlot(slot: number) {
  return slot.toLocaleString()
}

export function CheckpointMonitor({
  data,
  loading,
}: {
  data: CheckpointsData | null
  loading: boolean
}) {
  if (loading || !data) {
    return (
      <section aria-label="Checkpoint Monitor">
        <div className="flex items-center justify-between">
          <Skeleton className="h-4 w-40" />
          <Skeleton className="h-4 w-48" />
        </div>
        <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
          <Skeleton className="h-36 rounded-lg" />
          <Skeleton className="h-36 rounded-lg" />
        </div>
      </section>
    )
  }

  return (
    <section aria-label="Checkpoint Monitor">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-foreground tracking-wide uppercase">
          Checkpoint Monitor
        </h2>
        <span className="text-xs text-muted-foreground font-mono">
          Chain Head: {formatSlot(data.chain_head_slot)}
        </span>
      </div>

      <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
        {data.programs.map((program) => (
            <div
              key={program.program_type}
              className="rounded-lg border border-border bg-card p-4"
            >
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Database className="size-4 text-muted-foreground" />
                  <span className="text-sm font-medium text-foreground">
                    {program.label}
                  </span>
                </div>
                <StatusDot status={program.status} />
              </div>

              <div className="mt-4 flex items-end justify-between">
                <div className="flex flex-col gap-1">
                  <div className="flex items-baseline gap-2">
                    <span className="text-xs text-muted-foreground">Indexed Slot</span>
                    <span className="font-mono text-sm font-medium text-foreground">
                      {program.last_indexed_slot != null
                        ? formatSlot(program.last_indexed_slot)
                        : "—"}
                    </span>
                  </div>
                  <div className="flex items-baseline gap-2">
                    <span className="text-xs text-muted-foreground">Lag</span>
                    <span className="font-mono text-sm font-medium text-primary">
                      {program.slot_lag} slots
                    </span>
                    <span className="text-xs text-muted-foreground">
                      ({program.estimated_time_lag_seconds}s)
                    </span>
                  </div>
                </div>
              </div>
            </div>
        ))}
      </div>
    </section>
  )
}
