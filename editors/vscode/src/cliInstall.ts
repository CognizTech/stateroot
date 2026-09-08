import * as cp from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

const DOCS_URL = "https://stateroot.dev/docs/getting-started/installation";
const INSTALL_TIMEOUT_MS = 600_000;
let pendingInstall: Promise<InstallResult> | undefined;

export type SupportedPlatform = {
  supported: true;
  label: string;
  installDest: string;
  scriptName: "install.sh" | "install.ps1";
};

export type UnsupportedPlatform = {
  supported: false;
  reason: string;
};

export type PlatformInfo = SupportedPlatform | UnsupportedPlatform;

export type InstallResult =
  | { ok: true; binaryPath: string }
  | { ok: false; error: string };

/** Extension-host platform detection (stable release installers only). */
export function detectPlatform(): PlatformInfo {
  const { platform, arch } = process;
  if (platform === "linux" && arch === "x64") {
    return {
      supported: true,
      label: "Linux x64",
      installDest: path.join(os.homedir(), ".local", "bin", "stateroot"),
      scriptName: "install.sh",
    };
  }
  if (platform === "win32" && arch === "x64") {
    const local = process.env.LOCALAPPDATA || path.join(os.homedir(), "AppData", "Local");
    return {
      supported: true,
      label: "Windows x64",
      installDest: path.join(local, "Programs", "stateroot", "stateroot.exe"),
      scriptName: "install.ps1",
    };
  }
  if (platform === "darwin" && arch === "arm64") {
    return {
      supported: true,
      label: "macOS aarch64",
      installDest: path.join(os.homedir(), ".local", "bin", "stateroot"),
      scriptName: "install.sh",
    };
  }
  if (platform === "darwin") {
    return {
      supported: false,
      reason:
        "macOS on Intel is not shipped (Apple Silicon only) — install the CLI manually, see the StateRoot docs.",
    };
  }
  return {
    supported: false,
    reason: `Unsupported extension host: ${platform} ${arch}. Install the CLI manually — see the StateRoot docs.`,
  };
}

/** Known default install locations for this extension host. */
export function defaultCliCandidates(): string[] {
  if (process.platform === "win32") {
    const local = process.env.LOCALAPPDATA;
    return local ? [path.join(local, "Programs", "stateroot", "stateroot.exe")] : [];
  }
  return [
    path.join(os.homedir(), ".local", "bin", "stateroot"),
    path.join(os.homedir(), ".cargo", "bin", "stateroot"),
  ];
}

export function probeCli(binaryPath: string): Promise<boolean> {
  return new Promise((resolve) => {
    cp.execFile(binaryPath, ["--version"], { timeout: 8_000 }, (err) => {
      resolve(!err);
    });
  });
}

/** First working binary among configured path, PATH name, and defaults. */
export async function findWorkingCli(configuredPath: string): Promise<string | undefined> {
  const seen = new Set<string>();
  const candidates = [
    configuredPath.trim(),
    "stateroot",
    ...defaultCliCandidates(),
  ].filter((entry) => {
    if (!entry || seen.has(entry)) {
      return false;
    }
    seen.add(entry);
    return true;
  });
  for (const candidate of candidates) {
    if (await probeCli(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function execInstaller(
  scriptPath: string,
  scriptName: SupportedPlatform["scriptName"],
  output: vscode.OutputChannel
): Promise<void> {
  return new Promise((resolve, reject) => {
    const opts = {
      maxBuffer: 8 * 1024 * 1024,
      timeout: INSTALL_TIMEOUT_MS,
      env: { ...process.env, STATEROOT_INSTALL_VIA: "extension" },
    };
    const windows = scriptName === "install.ps1";
    const command = windows ? "powershell.exe" : "sh";
    const args = windows
      ? ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", scriptPath]
      : [scriptPath];
    const child = cp.execFile(command, args, opts, (err, stdout, stderr) => {
      if (err) {
        const reason = err.killed
          ? `Installer timed out after ${INSTALL_TIMEOUT_MS / 60_000} minutes. Check connectivity to GitHub and retry.`
          : typeof err.code === "number"
            ? `Installer exited with code ${err.code}.`
            : err.message;
        const details = (stderr || stdout).trim().slice(-4_000);
        reject(new Error([reason, details].filter(Boolean).join("\n")));
        return;
      }
      resolve();
    });
    child.stdout?.on("data", (chunk: Buffer) => output.append(chunk.toString()));
    child.stderr?.on("data", (chunk: Buffer) => output.append(chunk.toString()));
  });
}

/** Share one install between activation and commands in this extension host. */
export function installCli(output: vscode.OutputChannel): Promise<InstallResult> {
  if (!pendingInstall) {
    pendingInstall = performInstall(output).finally(() => {
      pendingInstall = undefined;
    });
  }
  return pendingInstall;
}

/** Run the bundled installer; it downloads and verifies the stable CLI release. */
async function performInstall(output: vscode.OutputChannel): Promise<InstallResult> {
  const platform = detectPlatform();
  if (!platform.supported) {
    return { ok: false, error: platform.reason };
  }

  const scriptPath = path.join(__dirname, "..", "assets", platform.scriptName);
  if (!fs.existsSync(scriptPath)) {
    return { ok: false, error: `Bundled ${platform.scriptName} is missing. Reinstall the StateRoot extension.` };
  }

  try {
    output.appendLine(`$ run bundled ${platform.scriptName} (latest stable CLI)`);
    await execInstaller(scriptPath, platform.scriptName, output);
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    return { ok: false, error: message };
  }

  if (!(await probeCli(platform.installDest))) {
    return {
      ok: false,
      error: `Installer finished but ${platform.installDest} is not runnable yet.`,
    };
  }
  return { ok: true, binaryPath: platform.installDest };
}

/** Install the latest stable release with no confirmation gate — the default
 * when the CLI is absent. The extension is dead weight without the CLI, so
 * the first moment we notice it missing is the moment we fetch it. */
export async function autoInstallCli(output: vscode.OutputChannel): Promise<string | undefined> {
  const platform = detectPlatform();
  if (!platform.supported) {
    output.appendLine(`auto-install skipped: ${platform.reason}`);
    return undefined;
  }
  const result = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "StateRoot CLI not found — installing latest stable release",
      cancellable: false,
    },
    async () => installCli(output)
  );
  if (!result.ok) {
    const pick = await vscode.window.showErrorMessage(
      `StateRoot CLI auto-install failed: ${result.error}`,
      "Open docs"
    );
    if (pick === "Open docs") {
      await vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
    }
    output.appendLine(`auto-install failed: ${result.error}`);
    output.show(true);
    return undefined;
  }
  output.appendLine(`auto-installed: ${result.binaryPath}`);
  return result.binaryPath;
}

export async function confirmAndInstallCli(output: vscode.OutputChannel): Promise<string | undefined> {
  const platform = detectPlatform();
  if (!platform.supported) {
    await vscode.window.showErrorMessage(platform.reason, "Open docs").then((pick) => {
      if (pick === "Open docs") {
        void vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
      }
    });
    return undefined;
  }

  const choice = await vscode.window.showWarningMessage(
    [
      `StateRoot CLI not found on this ${platform.label} extension host.`,
      `Install the latest stable release to:`,
      platform.installDest,
      "",
      "The official installer also runs global harness integration (hooks, persona).",
    ].join("\n"),
    { modal: true },
    "Install",
    "Open docs",
    "Cancel"
  );

  if (choice === "Open docs") {
    await vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
    return undefined;
  }
  if (choice !== "Install") {
    return undefined;
  }

  const result = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "Installing StateRoot CLI (stable)",
      cancellable: false,
    },
    async () => installCli(output)
  );

  if (!result.ok) {
    const pick = await vscode.window.showErrorMessage(
      `StateRoot CLI install failed: ${result.error}`,
      "Open docs"
    );
    if (pick === "Open docs") {
      await vscode.env.openExternal(vscode.Uri.parse(DOCS_URL));
    }
    output.appendLine(`install failed: ${result.error}`);
    output.show(true);
    return undefined;
  }

  output.appendLine(`installed: ${result.binaryPath}`);
  return result.binaryPath;
}
