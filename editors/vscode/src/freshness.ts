/** Timestamp comparison matching the digest's `ts_newer`: unparseable sides never claim. */
export function tsNewer(a?: string, b?: string): boolean {
  if (!a || !b) {
    return false;
  }
  const left = Date.parse(a);
  const right = Date.parse(b);
  if (!Number.isFinite(left) || !Number.isFinite(right)) {
    return false;
  }
  return left > right;
}

export function handoffBoundary(handoff: {
  written_at?: string;
  created_at?: string;
}): string | undefined {
  if (typeof handoff.written_at === "string" && handoff.written_at.trim()) {
    return handoff.written_at;
  }
  if (typeof handoff.created_at === "string" && handoff.created_at.trim()) {
    return handoff.created_at;
  }
  return undefined;
}

/** Same rule as `latest_activity_section` in the CLI digest. */
export function handoffIsStale(boundary?: string, activityTs?: string): boolean {
  return tsNewer(activityTs, boundary);
}

export function staleHandoffNote(seq?: number, author?: string): string {
  return (
    `activity continues after formal handoff #${seq ?? "?"} by ${author ?? "?"} ` +
    "— the formal handoff is stale; Recent Checkpoints and observed evidence carry the work since."
  );
}
