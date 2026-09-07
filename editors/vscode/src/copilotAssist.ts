//! Copilot continuity assist — VS Code only.
//!
//! Offers to write `.github/hooks/stateroot.json` so the built-in Copilot
//! agent fires StateRoot lifecycle hooks in this workspace. Host-gated:
//! Cursor has no Copilot and must never see any of this. The workspace file
//! is opt-in because it is committable — anyone who clones gets hooks that
//! run our command. The user-level default (`~/.copilot/hooks/`) is written
//! by `stateroot install`; this assist is the per-workspace twin.
//!
//! The event set and entry shape mirror the CLI's CopilotJson writer
//! (stateroot-core/src/harness_install/hooks.rs) — keep them in sync.

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import { cliPath } from "./cli";

/** True only on real VS Code with the Copilot Chat extension installed. */
export function isVSCodeWithCopilot(): boolean {
  return (
    vscode.env.appName === "Visual Studio Code" &&
    vscode.extensions.getExtension("github.copilot-chat") !== undefined
  );
}

/** The six lifecycle events StateRoot registers (PascalCase), with the same
 *  timeout policy as the CLI writer: SessionStart carries the digest (30s),
 *  SessionEnd must not stall a window close (8s). No PreToolUse: Copilot
 *  command hooks are fail-closed there — we must never block a tool call. */
const COPILOT_EVENTS: Array<[string, string, number]> = [
  ["SessionStart", "session_start", 30],
  ["UserPromptSubmit", "user_prompt_submit", 15],
  ["PostToolUse", "post_tool_use", 15],
  ["PreCompact", "pre_compact", 15],
  ["Stop", "stop", 15],
  ["SessionEnd", "session_end", 8],
];

/** The workspace hook file content (Copilot/VS Code shared format). */
export function copilotHooksDocument(cliCommand: string): string {
  const hooks: Record<string, unknown> = {};
  for (const [event, canonical, timeout] of COPILOT_EVENTS) {
    hooks[event] = [
      {
        type: "command",
        command: `${cliCommand} hook ${canonical} --harness vscode-copilot`,
        timeout,
      },
    ];
  }
  return JSON.stringify({ version: 1, hooks }, null, 2) + "\n";
}

export function workspaceHooksPath(projectRoot: string): string {
  return path.join(projectRoot, ".github", "hooks", "stateroot.json");
}

/** Copy the user-level StateRoot agent (written by `stateroot install`)
 *  into the workspace, so Copilot users can pick it per project. */
function copyAgentFile(projectRoot: string, output: vscode.OutputChannel): void {
  const src = path.join(os.homedir(), ".copilot", "agents", "StateRoot.agent.md");
  if (!fs.existsSync(src)) {
    return;
  }
  const dest = path.join(projectRoot, ".github", "agents", "StateRoot.agent.md");
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.copyFileSync(src, dest);
  output.appendLine(`copilot agent → ${dest}`);
}

/** Write the workspace hook file (idempotent overwrite — it is ours) and the
 *  StateRoot agent file when the user-level one exists. */
export function enableCopilotHooks(
  projectRoot: string,
  output: vscode.OutputChannel
): void {
  const dest = workspaceHooksPath(projectRoot);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, copilotHooksDocument(cliPath()));
  output.appendLine(`copilot hooks → ${dest}`);
  copyAgentFile(projectRoot, output);
  void vscode.window.showInformationMessage(
    "StateRoot enabled for Copilot in this workspace — hooks plus the StateRoot agent. Pick 'StateRoot' in the agent dropdown for the full persona."
  );
}

const dismissedKey = (projectRoot: string) =>
  `stateroot.copilotHooks.dismissed:${projectRoot}`;

/** One gentle offer, only where it makes sense: VS Code + Copilot Chat +
 *  initialized project + no workspace hook file + not dismissed. */
export async function maybeOfferCopilotHooks(
  context: vscode.ExtensionContext,
  projectRoot: string,
  output: vscode.OutputChannel
): Promise<void> {
  if (!isVSCodeWithCopilot()) {
    return;
  }
  if (context.globalState.get<boolean>(dismissedKey(projectRoot))) {
    return;
  }
  if (!fs.existsSync(path.join(projectRoot, ".stateroot", "manifest.json"))) {
    return;
  }
  if (fs.existsSync(workspaceHooksPath(projectRoot))) {
    return;
  }
  const choice = await vscode.window.showInformationMessage(
    "Enable StateRoot continuity hooks for GitHub Copilot in this workspace? " +
      "Writes .github/hooks/stateroot.json (committable — cloners get the hooks too).",
    "Enable",
    "Later",
    "Don't ask again"
  );
  if (choice === "Enable") {
    enableCopilotHooks(projectRoot, output);
  } else if (choice === "Don't ask again") {
    await context.globalState.update(dismissedKey(projectRoot), true);
  }
}
