import * as path from "path";
import type * as vscode from "vscode";

/** Use VS Code's terminal environment API instead of overwriting PATH settings. */
export function terminalPathUpdater(collection: vscode.EnvironmentVariableCollection): (binary: string) => void {
  let lastDirectory: string | undefined;
  return (binary: string) => {
    if (!path.isAbsolute(binary)) return;
    const directory = path.dirname(binary);
    if (directory === lastDirectory) return;
    collection.description = "Makes the StateRoot CLI available in terminals.";
    collection.prepend("PATH", directory + path.delimiter);
    lastDirectory = directory;
  };
}
