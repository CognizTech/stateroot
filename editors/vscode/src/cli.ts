import * as vscode from "vscode";
import * as cp from "child_process";
import {
  confirmAndInstallCli,
  findWorkingCli,
  type PlatformInfo,
  detectPlatform,
} from "./cliInstall";

const DOCS_URL = "https://stateroot.dev/docs/getting-started/installation";

/** Auto-installed path for this extension-host session (never overwrites settings). */
let sessionCliPath: string | undefined;
let lastProbeAvailable: boolean | undefined;

export function getPlatformInfo(): PlatformInfo {
  return detectPlatform();
}

export function isCliProbeAvailable(): boolean | undefined {
  return lastProbeAvailable;
}

export function cliPath(): string {
  if (sessionCliPath) {
    return sessionCliPath;
  }
  const configured = vscode.workspace.getConfiguration("stateroot").get<string>("cliPath");
  return configured && configured.trim() ? configured.trim() : "stateroot";
}

export function hasExplicitCliPathSetting(): boolean {
  const configured = vscode.workspace.getConfiguration("stateroot").get<string>("cliPath");
  return !!(configured && configured.trim());
}

/** Lightweight probe on activation — no install prompt. */
export async function refreshCliProbe(): Promise<boolean> {
  const working = await findWorkingCli(cliPath());
  if (working) {
    sessionCliPath = working;
    lastProbeAvailable = true;
    return true;
  }
  lastProbeAvailable = false;
  return false;
}

async function resolveCliForRun(
  allowInstall: boolean,
  output: vscode.OutputChannel,
  installAttempted: { value: boolean }
): Promise<string | undefined> {
  let working = await findWorkingCli(cliPath());
  if (working) {
    sessionCliPath = working;
    lastProbeAvailable = true;
    return working;
  }
  lastProbeAvailable = false;
  if (!allowInstall || installAttempted.value) {
    return undefined;
  }
  installAttempted.value = true;
  const installed = await confirmAndInstallCli(output);
  if (installed) {
    sessionCliPath = installed;
    lastProbeAvailable = true;
    return installed;
  }
  return undefined;
}

export function runCli(
  args: string[],
  cwd: string,
  timeoutMs = 20_000,
  binary = cliPath()
): Promise<string> {
  return new Promise((resolve, reject) => {
    cp.execFile(
      binary,
      args,
      { cwd, timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024 },
      (err, stdout, stderr) => {
        if (err) {
          const error = err as NodeJS.ErrnoException;
          if (error.code === "ENOENT") {
            reject(Object.assign(new Error("ENOENT"), { code: "ENOENT" }));
            return;
          }
          reject(new Error((stderr || err.message || String(err)).trim()));
          return;
        }
        resolve(stdout);
      }
    );
  });
}

/** Deliberate install entry point (Command Palette / status bar). */
export async function installCliCommand(output: vscode.OutputChannel): Promise<boolean> {
  const installed = await confirmAndInstallCli(output);
  if (!installed) {
    return false;
  }
  sessionCliPath = installed;
  lastProbeAvailable = true;
  output.show(true);
  vscode.window.showInformationMessage(`StateRoot CLI installed: ${installed}`);
  return true;
}

export async function runCliReport(
  args: string[],
  cwd: string,
  output: vscode.OutputChannel,
  timeoutMs?: number,
  options?: { allowInstall?: boolean }
): Promise<string | undefined> {
  const allowInstall = options?.allowInstall !== false;
  const installAttempted = { value: false };
  const binary = await resolveCliForRun(allowInstall, output, installAttempted);
  if (!binary) {
    if (allowInstall) {
      await offerDocsOnly();
    }
    return undefined;
  }

  try {
    const out = await runCli(args, cwd, timeoutMs, binary);
    output.appendLine(`$ stateroot ${args.join(" ")}`);
    if (out.trim()) {
      output.appendLine(out.trimEnd());
    }
    return out;
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    const isEnoent = message === "ENOENT" || (err as { code?: string })?.code === "ENOENT";
    if (isEnoent && allowInstall) {
      sessionCliPath = undefined;
      const retried = await resolveCliForRun(true, output, installAttempted);
      if (retried) {
        try {
          const out = await runCli(args, cwd, timeoutMs, retried);
          output.appendLine(`$ stateroot ${args.join(" ")}`);
          if (out.trim()) {
            output.appendLine(out.trimEnd());
          }
          return out;
        } catch (retryErr: unknown) {
          const retryMsg = retryErr instanceof Error ? retryErr.message : String(retryErr);
          vscode.window.showErrorMessage(`stateroot ${args[0]} failed: ${retryMsg}`);
          output.appendLine(`$ stateroot ${args.join(" ")}`);
          output.appendLine(retryMsg);
          return undefined;
        }
      }
      await offerDocsOnly();
      return undefined;
    }
    vscode.window.showErrorMessage(`stateroot ${args[0]} failed: ${message}`);
    output.appendLine(`$ stateroot ${args.join(" ")}`);
    output.appendLine(message);
    return undefined;
  }
}

async function offerDocsOnly(): Promise<void> {
  const pick = await vscode.window.showErrorMessage(
    "The `stateroot` binary is not available on this extension host.",
    "Open docs"
  );
  if (pick === "Open docs") {
    await vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
  }
}

/** Parse `stateroot delegate list` lines: `id · harness · status · task`. */
export function parseDelegateList(text: string): Array<{
  id: string;
  harness: string;
  status: string;
  task: string;
}> {
  const rows: Array<{ id: string; harness: string; status: string; task: string }> = [];
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("no delegations")) {
      continue;
    }
    const parts = trimmed.split(" · ");
    if (parts.length < 3) {
      continue;
    }
    rows.push({
      id: parts[0],
      harness: parts[1],
      status: parts[2],
      task: parts.slice(3).join(" · "),
    });
  }
  return rows;
}
