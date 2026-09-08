const fs = require("node:fs");
const path = require("node:path");

// Ship the installer from this extension's source revision. CLI releases can
// lag behind extension fixes (for example, macOS support and shell PATH setup).
const extension = path.resolve(__dirname, "..");
fs.mkdirSync(path.join(extension, "assets"), { recursive: true });
for (const script of ["install.sh", "install.ps1"]) {
  fs.copyFileSync(
    path.join(extension, "..", "..", script),
    path.join(extension, "assets", script)
  );
}
fs.copyFileSync(path.join(extension, "..", "..", "LICENSE"), path.join(extension, "LICENSE"));
