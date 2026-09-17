import * as vscode from "vscode";
import { cliPath, runCli } from "./cli";
import type { DelegationRecord } from "./store";

export interface LineageRoot {
  id: string;
  ref: string;
  parents: string[];
  created_at?: string;
  created_by_harness?: string;
  created_reason?: string;
  coverage?: string;
  files_pinned?: number;
  mainline: boolean;
  fork_point: boolean;
}

export interface ParallelFork {
  name: string;
  ref: string;
  tip?: string | null;
  base_root?: string | null;
  plan?: string | null;
  created_at?: string | null;
  created_by_harness?: string | null;
  contained: boolean;
  worktree?: { registered: boolean; path: string } | null;
  cleanup?: { state: string; pending?: unknown };
}

export interface LineageProjection {
  schema_version: "stateroot.lineage.v1";
  trunk: { ref: string; tip?: string | null };
  forks: ParallelFork[];
  roots: LineageRoot[];
}

export type ParallelWorkPhase =
  | "provisioning"
  | "running"
  | "cancelling"
  | "capturing"
  | "attention"
  | "ready"
  | "merged"
  | "cleanup_pending";

export interface ParallelWorkCard {
  fork: ParallelFork;
  attempts: DelegationRecord[];
  phase: ParallelWorkPhase;
  outcomeRoot?: string;
  /** Exact retry command for a fork whose cleanup did not finish. */
  cleanupCommand?: string;
}

/** Evidence-only phase derivation. A terminal worker is never merge-ready
 * until its captured outcome root is present. */
export function deriveParallelWork(
  lineage: LineageProjection | undefined,
  delegations: DelegationRecord[]
): ParallelWorkCard[] {
  if (!lineage) return [];
  return lineage.forks.map((fork) => {
    const attempts = delegations.filter((row) => row.fork_id === fork.name);
    const latest = attempts[0];
    const outcomeRoot = attempts.find((row) => row.outcome_root)?.outcome_root;
    const captureError = attempts.some((row) => row.events?.some((event) => event.event === "capture-error"));
    const phase: ParallelWorkPhase = fork.cleanup?.state === "pending"
      ? "cleanup_pending"
      : fork.contained
        ? "merged"
        : latest?.status === "cancelling"
          ? "cancelling"
          : latest?.status === "running"
            ? "running"
            : captureError || (["failed", "lost", "timed_out"].includes(latest?.status || "") && !outcomeRoot)
              ? "attention"
              : outcomeRoot
                ? "ready"
                : latest?.outcome
                  ? "capturing"
                  : "provisioning";
    const cleanupCommand =
      phase === "cleanup_pending" ? `stateroot merge --cleanup ${fork.name}` : undefined;
    return { fork, attempts, phase, outcomeRoot, cleanupCommand };
  });
}

/** Read the CLI-owned topology; the extension never infers lineage from files. */
export async function readParallelWork(
  root: string,
  output: vscode.OutputChannel
): Promise<LineageProjection | undefined> {
  try {
    const text = await runCli(["log", "--json"], root, 20_000, cliPath());
    const value = JSON.parse(text) as LineageProjection;
    if (value.schema_version !== "stateroot.lineage.v1" || !Array.isArray(value.roots)) {
      throw new Error("unsupported lineage projection");
    }
    return value;
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    output.appendLine(`Parallel lineage unavailable: ${message}`);
    return undefined;
  }
}
