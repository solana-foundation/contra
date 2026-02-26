"use client"

import { useState, useEffect, useCallback } from "react"
import { Filter, ArrowUpDown, ChevronLeft, ChevronRight, AlertCircle } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Skeleton } from "@/components/ui/skeleton"
import { TruncatedAddress } from "@/components/copy-button"
import { StatusBadge } from "@/components/status-indicator"
import { fetchTransactions } from "@/lib/api"
import type { Transaction, TransactionListData } from "@/lib/types"

export function TransactionFeed({
  refreshKey,
  onSelectTransaction,
}: {
  refreshKey: number
  onSelectTransaction: (tx: Transaction) => void
}) {
  const [statusFilter, setStatusFilter] = useState<string>("all")
  const [typeFilter, setTypeFilter] = useState<string>("all")
  const [showFilters, setShowFilters] = useState(false)
  const [page, setPage] = useState(1)
  const [perPage] = useState(25)
  const [data, setData] = useState<TransactionListData | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    setLoading(true)
    const res = await fetchTransactions({
      page,
      per_page: perPage,
      status: statusFilter,
      type: typeFilter,
    })
    if (res.data) {
      setData(res.data)
      setError(null)
    } else {
      setError(res.error ?? "Failed to load transactions")
    }
    setLoading(false)
  }, [page, perPage, statusFilter, typeFilter])

  useEffect(() => {
    load()
  }, [load, refreshKey])

  // Reset to page 1 when filters change
  const handleStatusFilter = (val: string) => {
    setStatusFilter(val)
    setPage(1)
  }
  const handleTypeFilter = (val: string) => {
    setTypeFilter(val)
    setPage(1)
  }

  const totalPages = data ? Math.ceil(data.total_count / perPage) : 0

  return (
    <section aria-label="Recent Transactions">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-sm font-semibold text-foreground tracking-wide uppercase">
            Recent Transactions
          </h2>
          <p className="text-xs text-muted-foreground">
            {data ? data.total_count.toLocaleString() : "—"} total
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => setShowFilters(!showFilters)}
          className="gap-1.5 text-xs"
        >
          <Filter className="size-3" />
          Filters
        </Button>
      </div>

      {showFilters && (
        <div className="mt-3 flex flex-wrap gap-3">
          <Select value={statusFilter} onValueChange={handleStatusFilter}>
            <SelectTrigger className="w-[140px] h-8 text-xs bg-card">
              <SelectValue placeholder="Status" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All Status</SelectItem>
              <SelectItem value="Completed">Completed</SelectItem>
              <SelectItem value="Processing">Processing</SelectItem>
              <SelectItem value="Pending">Pending</SelectItem>
              <SelectItem value="Failed">Failed</SelectItem>
            </SelectContent>
          </Select>
          <Select value={typeFilter} onValueChange={handleTypeFilter}>
            <SelectTrigger className="w-[140px] h-8 text-xs bg-card">
              <SelectValue placeholder="Type" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All Types</SelectItem>
              <SelectItem value="Deposit">Deposit</SelectItem>
              <SelectItem value="Withdrawal">Withdrawal</SelectItem>
            </SelectContent>
          </Select>
        </div>
      )}

      {/* Inline error */}
      {error && !loading && (
        <div className="mt-3 flex items-center gap-2 rounded-lg border border-danger/30 bg-danger/10 px-4 py-3 text-sm text-danger">
          <AlertCircle className="size-4 shrink-0" />
          <span>Failed to load transactions: {error}</span>
        </div>
      )}

      <div className="mt-3 rounded-lg border border-border overflow-hidden">
        <Table>
          <TableHeader>
            <TableRow className="border-border hover:bg-transparent">
              <TableHead className="text-xs text-muted-foreground font-medium w-[60px]">ID</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium">
                <span className="flex items-center gap-1">
                  Type
                  <ArrowUpDown className="size-3" />
                </span>
              </TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium">Status</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium">Signature</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium">Mint</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Amount</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium hidden lg:table-cell">Initiator</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Latency</TableHead>
              <TableHead className="text-xs text-muted-foreground font-medium text-right">Time</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {loading ? (
              Array.from({ length: 5 }).map((_, i) => (
                <TableRow key={i} className="border-border">
                  <TableCell><Skeleton className="h-4 w-10" /></TableCell>
                  <TableCell><Skeleton className="h-5 w-16" /></TableCell>
                  <TableCell><Skeleton className="h-5 w-20" /></TableCell>
                  <TableCell><Skeleton className="h-4 w-24" /></TableCell>
                  <TableCell><Skeleton className="h-4 w-10" /></TableCell>
                  <TableCell><Skeleton className="h-4 w-16" /></TableCell>
                  <TableCell className="hidden lg:table-cell"><Skeleton className="h-4 w-20" /></TableCell>
                  <TableCell><Skeleton className="h-4 w-10" /></TableCell>
                  <TableCell><Skeleton className="h-4 w-16" /></TableCell>
                </TableRow>
              ))
            ) : (
              data?.transactions.map((tx) => (
                <TableRow
                  key={tx.id}
                  className="border-border cursor-pointer transition-colors hover:bg-accent/50"
                  onClick={() => onSelectTransaction(tx)}
                >
                  <TableCell className="font-mono text-xs text-muted-foreground">
                    {tx.id}
                  </TableCell>
                  <TableCell>
                    <span
                      className={`inline-flex items-center rounded-sm px-1.5 py-0.5 text-xs font-medium ${
                        tx.transaction_type === "Deposit"
                          ? "bg-success/10 text-success"
                          : "bg-chart-2/10 text-chart-2"
                      }`}
                    >
                      {tx.transaction_type}
                    </span>
                  </TableCell>
                  <TableCell>
                    <StatusBadge status={tx.status} />
                  </TableCell>
                  <TableCell>
                    <TruncatedAddress address={tx.signature} chars={6} />
                  </TableCell>
                  <TableCell>
                    <span className="text-sm font-medium text-foreground">{tx.mint_symbol}</span>
                  </TableCell>
                  <TableCell className="text-right font-mono text-sm text-foreground">
                    {tx.amount_display}
                  </TableCell>
                  <TableCell className="hidden lg:table-cell">
                    <TruncatedAddress address={tx.initiator} chars={4} className="text-muted-foreground" />
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs">
                    {tx.latency_ms != null ? (
                      <span className="text-foreground">{(tx.latency_ms / 1000).toFixed(1)}s</span>
                    ) : (
                      <span className="text-muted-foreground">-</span>
                    )}
                  </TableCell>
                  <TableCell className="text-right text-xs text-muted-foreground">
                    {new Date(tx.created_at).toLocaleTimeString()}
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>

      {/* Pagination */}
      {data && totalPages > 1 && (
        <div className="mt-3 flex items-center justify-between">
          <span className="text-xs text-muted-foreground">
            Page {page} of {totalPages}
          </span>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={page <= 1}
              onClick={() => setPage((p) => p - 1)}
              className="gap-1 text-xs"
            >
              <ChevronLeft className="size-3" />
              Prev
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setPage((p) => p + 1)}
              className="gap-1 text-xs"
            >
              Next
              <ChevronRight className="size-3" />
            </Button>
          </div>
        </div>
      )}
    </section>
  )
}
