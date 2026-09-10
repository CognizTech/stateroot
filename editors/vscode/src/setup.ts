import type * as vscode from "vscode";
import { shouldRefreshCli } from "./installPing";

export interface SetupState {
  phase: "checking" | "installing" | "connecting" | "ready" | "error";
  detail: string;
  version?: string;
  configured?: string[];
}

export const SETUP_KEY = "stateroot.completedSetup";
type Receipt = { extensionVersion: string; binary: string; version: string; configured: string[] };
type Run = (args: string[], binary: string) => Promise<string>;

/** Complete setup against the exact selected binary. A failed attempt never
 * advances the receipt, so reopening the editor retries without a new release. */
export async function ensureSetup(options: {
  state: vscode.Memento;
  extensionVersion: string;
  binary?: string;
  noAutoUpdate: boolean;
  retry?: boolean;
  install: () => Promise<string>;
  run: Run;
  report: (state: SetupState) => void;
}): Promise<string> {
  const { state, extensionVersion, run, report } = options;
  const previous = state.get<Receipt>(SETUP_KEY);
  let binary = options.binary;
  const missing = !binary;
  if (!binary) {
    report({ phase: "installing", detail: "Installing StateRoot CLI…" });
    binary = await options.install();
  }
  const before = (await run(["--version"], binary)).trim();
  const needsSetup = options.retry || missing || previous?.extensionVersion !== extensionVersion ||
    previous?.binary !== binary || previous?.version !== before;
  if (!needsSetup && previous) {
    report({ phase: "ready", detail: "Ready", version: before, configured: previous.configured });
    return binary;
  }
  if (!missing && shouldRefreshCli(options.retry ? undefined : previous?.extensionVersion,
    extensionVersion, true, options.noAutoUpdate)) {
    report({ phase: "installing", detail: "Updating StateRoot CLI…" });
    // The existing updater replaces the binary actually in use, including on
    // Windows, and preserves an explicitly selected nightly channel.
    const result = await run(["self-update"], binary);
    if (!result.includes("auto-update is disabled")) {
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
    }
  }
  report({ phase: "connecting", detail: "Connecting your agents…" });
  const integration = await run(["install"], binary);
  const configured = integration.match(/^Installed for:[ \t]*([^\r\n]*)/m)?.[1]
    .split(",").map(s => s.trim()).filter(Boolean);
  if (!configured) throw new Error("CLI installed, but integration setup was not confirmed. Retry setup.");
  const version = (await run(["--version"], binary)).trim();
  await state.update(SETUP_KEY, { extensionVersion, binary, version, configured } satisfies Receipt);
  report({ phase: "ready", detail: "Ready", version, configured });
  return binary;
}

export function editorHarness(appName: string): string {
  return /cursor/i.test(appName) ? "cursor" : "vscode-copilot";
}

export const SWITCH_PROMPTS = [
  "Use StateRoot to remember our decisions as we work on this project.",
  "Save our current work and write a StateRoot handoff so I can continue in another agent.",
  "Receive the StateRoot handoff for this project and continue where the previous agent stopped.",
];
