import { cn } from "@/lib/utils"

type StatusLevel = "ok" | "healthy" | "warning" | "critical" | "balanced"

const statusColors: Record<StatusLevel, string> = {
  ok: "bg-success",
  healthy: "bg-success",
  balanced: "bg-success",
  warning: "bg-warning",
  critical: "bg-danger",
}

export function StatusDot({
  status,
  pulse = true,
  className,
}: {
  status: StatusLevel
  pulse?: boolean
  className?: string
}) {
  return (
    <span className={cn("relative flex size-2.5", className)}>
      {pulse && (
        <span
          className={cn(
            "absolute inline-flex size-full animate-ping rounded-full opacity-50",
            statusColors[status]
          )}
        />
      )}
      <span
        className={cn(
          "relative inline-flex size-2.5 rounded-full",
          statusColors[status]
        )}
      />
    </span>
  )
}

export function StatusBadge({
  status,
  label,
  className,
}: {
  status: "Completed" | "Processing" | "Failed" | "Pending"
  label?: string
  className?: string
}) {
  const config = {
    Completed: { bg: "bg-success/15 text-success", dot: "bg-success" },
    Processing: { bg: "bg-chart-2/15 text-chart-2", dot: "bg-chart-2" },
    Failed: { bg: "bg-danger/15 text-danger", dot: "bg-danger" },
    Pending: { bg: "bg-warning/15 text-warning", dot: "bg-warning" },
  }

  const c = config[status]

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium",
        c.bg,
        className
      )}
    >
      <span className={cn("size-1.5 rounded-full", c.dot)} />
      {label ?? status}
    </span>
  )
}
