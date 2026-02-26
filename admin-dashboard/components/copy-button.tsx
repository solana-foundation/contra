"use client"

import { useState, useCallback } from "react"
import { Check, Copy } from "lucide-react"
import { cn } from "@/lib/utils"

export function CopyButton({ value, className }: { value: string; className?: string }) {
  const [copied, setCopied] = useState(false)

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(value)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      // Clipboard API not available
    }
  }, [value])

  return (
    <button
      onClick={handleCopy}
      className={cn(
        "inline-flex items-center justify-center rounded-sm p-0.5 text-muted-foreground transition-colors hover:text-foreground",
        className
      )}
      aria-label="Copy to clipboard"
    >
      {copied ? (
        <Check className="size-3 text-success" />
      ) : (
        <Copy className="size-3" />
      )}
    </button>
  )
}

export function TruncatedAddress({
  address,
  chars = 4,
  className,
}: {
  address: string
  chars?: number
  className?: string
}) {
  const truncated =
    address.length > chars * 2 + 3
      ? `${address.slice(0, chars)}...${address.slice(-chars)}`
      : address

  return (
    <span className={cn("inline-flex items-center gap-1 font-mono text-xs", className)}>
      <span title={address}>{truncated}</span>
      <CopyButton value={address} />
    </span>
  )
}
