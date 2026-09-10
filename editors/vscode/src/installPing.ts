import * as vscode from "vscode";

/**
 * First-run install/update telemetry: one anonymous GET when the extension's
 * version differs from the last seen one. Independent of setup/update state.
 *
 * Contract: fire-and-forget (never blocks activation), 3s cap, every error
 * swallowed, STATEROOT_NO_PING=1 opts out. One attempt per version change per
 * machine: the marker is written before firing, so an offline machine is not
 * retried — the floor is installs that happened and could reach us, never an
 * exact census.
 */

const PING_URL = "https://stateroot.dev/api/install-ping";
const MARKER_KEY = "stateroot.lastSeenVersion";

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

export function pingUrl(current: string, kind: "install" | "update", from?: string): string {
  let url = `${PING_URL}?os=${osTarget(process.platform, process.arch)}&v=${encodeURIComponent(
    current
  )}&via=extension&kind=${kind}`;
  if (from) url += `&from=${encodeURIComponent(from)}`;
  return url;
}

/** The marker as it stood before this activation (read-only — maybePing writes it). */
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

/** Fire one fail-silent ping when the extension version changed. */
export function maybePing(context: vscode.ExtensionContext): void {
  try {
    if (process.env.STATEROOT_NO_PING) return;
    const current = String(context.extension.packageJSON.version ?? "");
    if (!current) return;
    const lastSeen = context.globalState.get<string>(MARKER_KEY);
    const kind = pingKind(lastSeen, current);
    if (!kind) return;
    // Record before firing: one attempt per version change, even offline.
    void context.globalState.update(MARKER_KEY, current);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 3000);
    void fetch(pingUrl(current, kind, kind === "update" ? lastSeen : undefined), {
      signal: controller.signal,
    })
      .catch(() => undefined)
      .finally(() => clearTimeout(timer));
  } catch {
    // fail-silent by contract
  }
}
