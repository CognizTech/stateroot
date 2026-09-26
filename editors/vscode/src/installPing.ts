import * as vscode from "vscode";

/**
 * Extension version marker used to classify legacy profiles and to decide
 * whether a working CLI needs a refresh. Editor recovery telemetry lives in
 * `editorTelemetry.ts` (durable POST /api/telemetry/v2/editor).
 *
 * `STATEROOT_NO_PING=1` still opts out of network telemetry. The local
 * version marker is recovery state, not a ping.
 */

export const MARKER_KEY = "stateroot.lastSeenVersion";

/** OS token in the same vocabulary as the CLI and installer pings. */
export function osTarget(platform: string, arch: string): string {
  if (platform === "win32") return "windows-x64";
  if (platform === "darwin") return arch === "arm64" ? "macos-aarch64" : "macos-x64";
  return arch === "arm64" ? "linux-aarch64" : "linux-x64";
}

/** What kind of ping (if any) a version comparison implies. */
export function pingKind(
  lastSeen: string | undefined,
  current: string
): "install" | "update" | undefined {
  if (lastSeen === current) return undefined;
  return lastSeen === undefined ? "install" : "update";
}

/** The marker as it stood before this activation (read-only — activation
 * writes it after queueing this version's `editor_seen`). */
export function previousVersion(context: vscode.ExtensionContext): string | undefined {
  return context.globalState.get<string>(MARKER_KEY);
}

/** Refresh once per successfully completed extension version, including
 * migrations from versions with no marker. Never use the telemetry marker. */
export function shouldRefreshCli(
  previous: string | undefined,
  current: string,
  cliAvailable: boolean,
  noAutoUpdate: boolean
): boolean {
  return cliAvailable && !noAutoUpdate && previous !== current;
}
