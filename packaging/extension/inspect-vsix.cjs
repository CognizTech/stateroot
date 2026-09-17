#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const zlib = require("node:zlib");

const REQUIRED = [
  "extension/out/extension.js",
  "extension/assets/install.sh",
  "extension/assets/install.ps1",
  "extension/package.json",
];

function fail(message) {
  console.error(`inspect-vsix: ${message}`);
  process.exit(1);
}

function findEocd(buf) {
  for (let i = buf.length - 22; i >= 0; i -= 1) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      return i;
    }
  }
  return -1;
}

function listZip(buf) {
  const eocd = findEocd(buf);
  if (eocd < 0) {
    fail("not a zip/vsix (missing EOCD)");
  }
  const entries = buf.readUInt16LE(eocd + 10);
  let offset = buf.readUInt32LE(eocd + 16);
  const names = [];
  const index = new Map();
  for (let n = 0; n < entries; n += 1) {
    if (buf.readUInt32LE(offset) !== 0x02014b50) {
      fail("corrupt zip central directory");
    }
    const method = buf.readUInt16LE(offset + 10);
    const compSize = buf.readUInt32LE(offset + 20);
    const nameLen = buf.readUInt16LE(offset + 28);
    const extraLen = buf.readUInt16LE(offset + 30);
    const commentLen = buf.readUInt16LE(offset + 32);
    const localOffset = buf.readUInt32LE(offset + 42);
    const name = buf.subarray(offset + 46, offset + 46 + nameLen).toString("utf8");
    names.push(name);
    index.set(name, { method, compSize, localOffset });
    offset += 46 + nameLen + extraLen + commentLen;
  }
  return { names, index };
}

function extract(buf, meta) {
  const local = meta.localOffset;
  if (buf.readUInt32LE(local) !== 0x04034b50) {
    fail("corrupt zip local header");
  }
  const nameLen = buf.readUInt16LE(local + 26);
  const extraLen = buf.readUInt16LE(local + 28);
  const start = local + 30 + nameLen + extraLen;
  const compressed = buf.subarray(start, start + meta.compSize);
  if (meta.method === 0) {
    return compressed;
  }
  if (meta.method === 8) {
    return zlib.inflateRawSync(compressed);
  }
  fail(`unsupported zip method ${meta.method}`);
}

function main() {
  const vsixPath = process.argv[2];
  if (!vsixPath) {
    fail("usage: inspect-vsix.cjs <file.vsix>");
  }
  const resolved = path.resolve(vsixPath);
  if (!fs.existsSync(resolved)) {
    fail(`missing ${resolved}`);
  }
  const buf = fs.readFileSync(resolved);
  const { names, index } = listZip(buf);
  const missing = REQUIRED.filter((item) => !index.has(item));
  if (missing.length) {
    fail(`missing ${missing.join(", ")}`);
  }
  const pkg = JSON.parse(extract(buf, index.get("extension/package.json")).toString("utf8"));
  const version = String(pkg.version || "");
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    fail(`invalid extension version in VSIX package.json: ${version}`);
  }
  const vsce = pkg.vsce || {};
  if (vsce.dependencies !== false) {
    fail('package.json must keep "vsce": { "dependencies": false }');
  }
  const digest = crypto.createHash("sha256").update(buf).digest("hex");
  const filename = path.basename(resolved);
  const expectedName = `stateroot-vscode-${version}.vsix`;
  if (filename !== expectedName) {
    fail(`filename ${filename} does not match ${expectedName}`);
  }
  const manifest = {
    schema_version: 1,
    extension_id: "CognizTech.stateroot",
    extension_version: version,
    vsix_filename: filename,
    digest,
  };
  const manifestPath = path.join(path.dirname(resolved), "stateroot-extension.json");
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`inspect-vsix: ok ${filename} ${version} sha256:${digest}`);
  console.log(`inspect-vsix: entries ${names.length}`);
  console.log(`inspect-vsix: wrote ${manifestPath}`);
}

main();
