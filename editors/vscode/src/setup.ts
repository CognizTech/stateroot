import type * as vscode from "vscode";
import { shouldRefreshCli } from "./installPing";
import type { CliStatus, FailureStage, PathClass } from "./editorTelemetry";

export interface SetupState {
  phase: "checking" | "installing" | "connecting" | "ready" | "error";
  detail: string;
  version?: string;
  configured?: string[];
}

export const SETUP_KEY = "stateroot.completedSetup";
type Receipt = {
  extensionVersion: string;
  binary: string;
  version: string;
  configured: string[];
  /** "cli_only" = no agent harness detected on this machine: the CLI is set
   * up and nothing else exists to integrate — an honest label, never agent
   * readiness. */
  setupMode?: "cli_only";
};
type Run = (args: string[], binary: string, env?: NodeJS.ProcessEnv) => Promise<string>;

export function classifyRecovery(input: {
  cliStatus: CliStatus;
  receiptCurrent: boolean;
  retry: boolean;
  staleForUpdate: boolean;
}): "install" | "self_update" | "health" {
  if (input.cliStatus !== "working") return "install";
  if (input.receiptCurrent && !input.retry && !input.staleForUpdate) return "health";
  if (input.staleForUpdate || input.retry) return "self_update";
  return "health";
}

export function allowInstallerFallback(pathClass: PathClass): boolean {
  return pathClass === "default_stable";
}

export function classifySetupFailure(message: string): FailureStage {
  const m = message.toLowerCase();
  if (/unsupported|not shipped/.test(m)) return "unsupported_platform";
  if (/timed out|timeout/.test(m)) return "timeout";
  if (/did not reach|could not check|could not be verified|self-update|offline/.test(m)) {
    return "update_failed";
  }
  if (/integration|hooks|installed for/.test(m)) return "integration_failed";
  if (/not runnable|enoent/.test(m)) return "cli_unrunnable";
  if (/\binit\b|initialization/.test(m)) return "project_init_failed";
  if (/install/.test(m)) return "install_failed";
  return "unknown";
}

const REARM_SKIP_ENV: NodeJS.ProcessEnv = {
  STATEROOT_SKIP_REARM: "1",
  STATEROOT_INSTALL_VIA: "extension",
};

/** Complete setup against the exact selected binary. A failed attempt never
 * advances the receipt, so reopening the editor retries without a new release. */
export async function ensureSetup(options: {
  state: vscode.Memento;
  extensionVersion: string;
  binary?: string;
  noAutoUpdate: boolean;
  retry?: boolean;
  pathClass?: PathClass;
  install: () => Promise<string>;
  run: Run;
  report: (state: SetupState) => void;
}): Promise<string> {
  const { state, extensionVersion, run, report } = options;
  const previous = state.get<Receipt>(SETUP_KEY);
  const pathClass: PathClass = options.pathClass ?? (options.binary ? "custom" : "unknown");
  let binary = options.binary;
  let missing = !binary;
  let before = "";

  if (binary) {
    try {
      before = (await run(["--version"], binary)).trim();
    } catch {
      missing = true;
      binary = undefined;
    }
  }

  const action = classifyRecovery({
    cliStatus: missing ? "missing" : "working",
    receiptCurrent: !!(
      previous &&
      previous.extensionVersion === extensionVersion &&
      previous.binary === binary
    ),
    retry: !!options.retry,
    staleForUpdate: shouldRefreshCli(
      options.retry ? undefined : previous?.extensionVersion,
      extensionVersion,
      !missing,
      options.noAutoUpdate
    ),
  });

  if (action === "install") {
    report({ phase: "installing", detail: "Installing StateRoot CLI…" });
    binary = await options.install();
    before = (await run(["--version"], binary)).trim();
  }

  const needsSetup = options.retry || missing || previous?.extensionVersion !== extensionVersion ||
    previous?.binary !== binary || previous?.version !== before;
  if (!needsSetup && previous && binary) {
    // A current receipt still earns a health check: integration drift
    // (missing hooks or blocks) falls through to one repair pass below
    // instead of surfacing only when the next version lands.
    if (await integrationHealthy(run, binary)) {
      report({ phase: "ready", detail: "Ready", version: before, configured: previous.configured });
      return binary;
    }
    report({ phase: "connecting", detail: "Repairing harness integration…" });
    const repaired = await runIntegrationInstall(run, binary);
    const version = (await run(["--version"], binary)).trim();
    await state.update(SETUP_KEY, {
      extensionVersion, binary, version,
      configured: repaired.configured,
      ...(repaired.cliOnly ? { setupMode: "cli_only" as const } : {}),
    } satisfies Receipt);
    report({
      phase: "ready",
      detail: repaired.cliOnly ? "Ready (CLI only — no agents detected)" : "Ready",
      version,
      configured: repaired.configured,
    });
    return binary;
  }

  if (action === "self_update" && binary) {
    report({ phase: "installing", detail: "Updating StateRoot CLI…" });
    try {
      await verifySelfUpdate(run, binary, before);
    } catch (err) {
      if (!allowInstallerFallback(pathClass)) throw err;
      report({ phase: "installing", detail: "Installing StateRoot CLI…" });
      binary = await options.install();
    }
  }

  report({ phase: "connecting", detail: "Connecting your agents…" });
  if (!binary) throw new Error("CLI is not runnable after recovery.");
  const recovered = binary;
  const outcome = await runIntegrationInstall(run, recovered);
  const version = (await run(["--version"], recovered)).trim();
  await state.update(SETUP_KEY, {
    extensionVersion, binary: recovered, version,
    configured: outcome.configured,
    ...(outcome.cliOnly ? { setupMode: "cli_only" as const } : {}),
  } satisfies Receipt);
  report({
    phase: "ready",
    detail: outcome.cliOnly ? "Ready (CLI only — no agents detected)" : "Ready",
    version,
    configured: outcome.configured,
  });
  return recovered;
}

interface IntegrationHealthDoc {
  schema_version?: string;
  harnesses?: { harness: string; status: string; detected?: boolean; problems?: string[] }[];
  install?: { configured: string[]; failed: string[]; cli_only: boolean };
}

/** The `doctor --json` document (stateroot.doctor.v1): `ok` covers base
 * checks only; integration readiness is the per-harness rows. */
interface DoctorDoc {
  schema_version?: string;
  ok?: boolean;
  integrations?: { harnesses?: { status?: string }[] } | null;
}

interface IntegrationOutcome {
  configured: string[];
  cliOnly: boolean;
}

/** True only when the CLI PROVABLY rejected `--json` at argument parsing —
 * the parser's own rejection statement must name `--json` as the rejected
 * token on the SAME line. The command we ran always contains `--json`, so a
 * rejection naming some OTHER flag (or a failure message that merely echoes
 * the command line) is never a flag rejection: no side-effectful retry.
 * Timeouts, permission errors, real install failures and malformed typed
 * output are never retried here. */
function isUnsupportedJsonFlag(message: string): boolean {
  return message.split(/\r?\n/).some((line) =>
    /(?:unexpected|unrecognized|unknown)\s+(?:argument|option|flag|subcommand)\s+['"`]?--json\b/i.test(line)
    || /found argument\s+['"`]?--json['"`]?\s+which wasn't expected/i.test(line)
  );
}

/** Run `stateroot install` and return the actual outcome. New CLIs answer
 * `--json` with the typed integration-health document whose `install`
 * outcome is authoritative; a partial integration fails explicitly and can
 * never write a Ready receipt, and a no-agent machine is an explicit
 * CLI-only success. Older CLIs (proven unsupported flag, which means the
 * rejected call ran no side effects) fall back to exactly one human-mode
 * run parsed from the `Installed for:` summary. */
async function runIntegrationInstall(run: Run, binary: string): Promise<IntegrationOutcome> {
  let out: string;
  try {
    out = await run(["install", "--json"], binary, REARM_SKIP_ENV);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (!isUnsupportedJsonFlag(message)) throw err;
    // Older CLI without --json (flag rejected BEFORE side effects) — one
    // human-mode retry. This is also why a timeout/real failure is never
    // retried: the first attempt may have wired half the integrations, and
    // a blind rerun would double side effects and telemetry.
    const human = await run(["install"], binary, REARM_SKIP_ENV);
    const configured = parseInstalledFor(human);
    return { configured, cliOnly: configured.every((c) => c === "dot") };
  }
  let doc: IntegrationHealthDoc;
  try {
    doc = JSON.parse(out) as IntegrationHealthDoc;
  } catch {
    throw new Error("Integration install returned malformed output — not recording a setup receipt. Retry setup.");
  }
  if (!doc || doc.schema_version !== "stateroot.integration-health.v1" || !Array.isArray(doc.harnesses)) {
    throw new Error("Integration install returned an unrecognized document — not recording a setup receipt. Retry setup.");
  }
  // The authoritative outcome: what the install pass actually did. The
  // typed schema always carries it — a typed document without a VALIDATED
  // outcome (missing fields, wrong shapes, unknown rows derived from thin
  // air) is not an install receipt and can never mint Ready. Unknown or
  // partial outcomes fail explicitly; only a clean, fully-typed outcome
  // writes the receipt.
  const outcome = doc.install;
  if (!outcome || !Array.isArray(outcome.configured) || !Array.isArray(outcome.failed)
      || typeof outcome.cli_only !== "boolean"
      || outcome.configured.some((c) => typeof c !== "string")
      || outcome.failed.some((c) => typeof c !== "string")) {
    throw new Error("Integration install returned an unrecognized outcome — not recording a setup receipt. Retry setup.");
  }
  if (outcome.failed.length > 0) {
    throw new Error(
      `Integration setup incomplete for: ${outcome.failed.join(", ")}. Run \`stateroot install\` to retry.`
    );
  }
  return { configured: outcome.configured, cliOnly: outcome.cli_only };
}

function parseInstalledFor(integration: string): string[] {
  const configured = integration.match(/^Installed for:[ \t]*([^\r\n]*)/m)?.[1]
    .split(",").map(s => s.trim()).filter(Boolean);
  if (!configured) throw new Error("Integration repair was not confirmed. Retry setup.");
  return configured;
}

/** Integration readiness for a current receipt. Consumes the typed
 * `doctor --json` document: base checks (`ok`) AND the detected integration
 * readiness — a detected harness whose integration row reads `missing` is
 * exactly the drift the repair pass exists to fix, and a cached receipt must
 * never skip it. A no-agent machine (no rows) is healthy CLI-only. The
 * legacy fallback (human doctor exit code) runs ONLY when the CLI provably
 * rejected `--json` at parse time; a timeout, spawn failure, or malformed
 * typed output is not proof of health and never triggers a second doctor
 * run — it falls through to the single repair pass. */
async function integrationHealthy(run: Run, binary: string): Promise<boolean> {
  let out: string;
  try {
    out = await run(["doctor", "--json"], binary, { STATEROOT_NO_AUTO_UPDATE: "1" });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (!isUnsupportedJsonFlag(message)) return false;
    try {
      await run(["doctor"], binary, { STATEROOT_NO_AUTO_UPDATE: "1" });
      return true;
    } catch {
      return false;
    }
  }
  let doc: DoctorDoc;
  try {
    doc = JSON.parse(out) as DoctorDoc;
  } catch {
    return false;
  }
  if (!doc || doc.schema_version !== "stateroot.doctor.v1" || doc.ok !== true) return false;
  const rows = doc.integrations?.harnesses;
  if (!Array.isArray(rows)) return false;
  // An unknown or malformed row is not proof that a cached integration
  // remains ready. Empty rows still support a no-agent CLI-only machine.
  return rows.every((row) => row && (row.status === "configured" || row.status === "observed_working"));
}

async function verifySelfUpdate(run: Run, binary: string, before: string): Promise<string> {
  const result = await run(["self-update"], binary, REARM_SKIP_ENV);
  if (result.includes("auto-update is disabled")) return before;
  const release = result.match(/^release:\s+v?(\d+\.\d+\.\d+)\s+\(production\)/m);
  const after = (await run(["--version"], binary)).trim();
  if (release) {
    const actual = after.match(/\b(\d+)\.(\d+)\.(\d+)\b/);
    const expected = release[1].split(".").map(Number);
    const installed = actual?.slice(1).map(Number);
    const order = installed?.map((n, i) => n - expected[i]).find(n => n !== 0) ?? 0;
    if (!installed || order < 0) throw new Error(`CLI update did not reach ${release[1]}: ${after}`);
  } else if (!result.includes("(rolling preview)") || result.includes("could not compare")) {
    throw new Error(result.trim() || "CLI update could not be verified. Retry setup when connected.");
  }
  return after;
}

export function editorHarness(appName: string): string {
  return /cursor/i.test(appName) ? "cursor" : "vscode-copilot";
}

export const SWITCH_PROMPTS = [
  "Use StateRoot to remember our decisions as we work on this project.",
  "Save our current work and write a StateRoot handoff so I can continue in another agent.",
  "Receive the StateRoot handoff for this project and continue where the previous agent stopped.",
];
