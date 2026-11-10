/**
 * Merge-attempt contract shared with the CLI (stateroot.merge-attempt.v1).
 * The extension parses and displays these payloads as evidence; it never
 * executes, continues, or aborts a merge itself.
 */

export const MERGE_ATTEMPT_SCHEMA = "stateroot.merge-attempt.v1";

export interface MergeAttemptConflict {
  fork: string;
  path: string;
  kind?: string;
  ancestor?: string;
  ours?: string;
  theirs?: string;
}

export interface MergeAttemptFork {
  name: string;
  tip: string;
}

export interface MergeAttempt {
  schema_version: typeof MERGE_ATTEMPT_SCHEMA;
  id: string;
  created_at?: string;
  harness?: string;
  trunk_tip?: string;
  forks: MergeAttemptFork[];
  state: "ready" | "attention" | "staged";
  conflicts: MergeAttemptConflict[];
  folded_forks?: MergeAttemptFork[];
  pending_forks?: MergeAttemptFork[];
  /** Machine-local candidate, present for reconciliation and staged checks. */
  worktree?: string;
  staged_tree?: string;
}

/** Unsupported evidence is distinct from an explicitly known old schema. */
export const UNSUPPORTED_ATTEMPT_ERROR = "unsupported merge-attempt payload";
const PRE_CONTRACT_ATTEMPT_ERROR = "pre-contract merge-attempt schema";

/** Validate supported payloads. An unrecognized response is unavailable
 * evidence, not proof that the installed CLI is older than the extension. */
export function parseMergeAttempt(text: string): MergeAttempt {
  let value: MergeAttempt | undefined;
  try {
    value = JSON.parse(text) as MergeAttempt;
  } catch {
    throw new Error(UNSUPPORTED_ATTEMPT_ERROR);
  }
  if (value && (value.schema_version as string) === "stateroot.merge-attempt.v0") {
    throw new Error(PRE_CONTRACT_ATTEMPT_ERROR);
  }
  if (
    !value ||
    value.schema_version !== MERGE_ATTEMPT_SCHEMA ||
    typeof value.id !== "string" ||
    !["ready", "attention", "staged"].includes(value.state) ||
    !Array.isArray(value.forks) ||
    !Array.isArray(value.conflicts)
  ) {
    throw new Error(UNSUPPORTED_ATTEMPT_ERROR);
  }
  return value;
}

/** True when the installed CLI predates the integration contracts: either it
 * rejects the prepare invocation or explicitly identifies a known old schema. */
export function isStaleIntegrationCli(err: unknown): boolean {
  const message = err instanceof Error ? err.message : String(err);
  if (message.includes(PRE_CONTRACT_ATTEMPT_ERROR)) {
    return true;
  }
  return /unexpected argument|unrecognized (subcommand|option|argument)|unknown (option|flag|argument|subcommand)|which wasn't expected|was not expected/i.test(
    message
  );
}
