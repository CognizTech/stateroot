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
type Receipt = { extensionVersion: string; binary: string; version: string; configured: string[] };
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
    report({ phase: "ready", detail: "Ready", version: before, configured: previous.configured });
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
  const integration = await run(["install"], recovered, REARM_SKIP_ENV);
  const configured = integration.match(/^Installed for:[ \t]*([^\r\n]*)/m)?.[1]
    .split(",").map(s => s.trim()).filter(Boolean);
  if (!configured) throw new Error("CLI installed, but integration setup was not confirmed. Retry setup.");
  const version = (await run(["--version"], recovered)).trim();
  await state.update(SETUP_KEY, { extensionVersion, binary: recovered, version, configured } satisfies Receipt);
  report({ phase: "ready", detail: "Ready", version, configured });
  return recovered;
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
