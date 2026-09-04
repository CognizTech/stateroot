import * as cp from "child_process";
import * as fs from "fs";
import * as https from "https";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

const REPO = "CognizTech/stateroot";
const INSTALL_BASE = `https://github.com/${REPO}/releases/latest/download`;
const DOCS_URL = "https://stateroot.dev/docs/getting-started/installation";

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
  if (platform === "darwin") {
    return {
      supported: false,
      reason:
        "macOS release binaries are not shipped yet. Build from source or install manually — see the StateRoot docs.",
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
  return [path.join(os.homedir(), ".local", "bin", "stateroot")];
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

function downloadUrl(url: string, dest: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = (target: string, redirects = 0) => {
      if (redirects > 8) {
        reject(new Error("too many redirects"));
        return;
      }
      https
        .get(target, (res) => {
          const status = res.statusCode || 0;
          if (status >= 300 && status < 400 && res.headers.location) {
            res.resume();
            request(res.headers.location, redirects + 1);
            return;
          }
          if (status !== 200) {
            res.resume();
            reject(new Error(`download failed (${status}): ${target}`));
            return;
          }
          const file = fs.createWriteStream(dest);
          res.pipe(file);
          file.on("finish", () => {
            file.close();
            resolve();
          });
          file.on("error", reject);
        })
        .on("error", reject);
    };
    request(url);
  });
}

function execInstaller(scriptPath: string, scriptName: SupportedPlatform["scriptName"]): Promise<string> {
  return new Promise((resolve, reject) => {
    const opts = { maxBuffer: 8 * 1024 * 1024, timeout: 180_000 };
    if (scriptName === "install.ps1") {
      cp.execFile(
        "powershell.exe",
        ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", scriptPath],
        opts,
        (err, stdout, stderr) => {
          if (err) {
            reject(new Error((stderr || stdout || err.message).trim()));
            return;
          }
          resolve([stdout, stderr].filter(Boolean).join("\n"));
        }
      );
      return;
    }
    cp.execFile("sh", [scriptPath], opts, (err, stdout, stderr) => {
      if (err) {
        reject(new Error((stderr || stdout || err.message).trim()));
        return;
      }
      resolve([stdout, stderr].filter(Boolean).join("\n"));
    });
  });
}

/** Download and run the official stable installer; verify the resulting binary. */
export async function installCli(output: vscode.OutputChannel): Promise<InstallResult> {
  const platform = detectPlatform();
  if (!platform.supported) {
    return { ok: false, error: platform.reason };
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-install-"));
  const scriptPath = path.join(tmpDir, platform.scriptName);
  const scriptUrl = `${INSTALL_BASE}/${platform.scriptName}`;

  try {
    output.appendLine(`$ download ${scriptUrl}`);
    await downloadUrl(scriptUrl, scriptPath);
    if (platform.scriptName === "install.sh") {
      fs.chmodSync(scriptPath, 0o755);
    }
    output.appendLine(`$ run ${platform.scriptName} (stable release)`);
    const log = await execInstaller(scriptPath, platform.scriptName);
    if (log.trim()) {
      output.appendLine(log.trimEnd());
    }
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    return { ok: false, error: message };
  } finally {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch {
      // best-effort cleanup
    }
  }

  if (!(await probeCli(platform.installDest))) {
    return {
      ok: false,
      error: `Installer finished but ${platform.installDest} is not runnable yet.`,
    };
  }
  return { ok: true, binaryPath: platform.installDest };
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
