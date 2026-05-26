#!/usr/bin/env node
// Build the cairn binary for both macOS arches and lipo into a
// universal binary under resources/bin/cairn. Called by
// electron-builder's beforeBuild hook (config in electron-builder.yml)
// and by `npm run build:sidecar` for local testing.

import { spawnSync } from "node:child_process";
import { mkdirSync, statSync, existsSync, copyFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(__dirname, "..", "..", "..");
const OUT_DIR = resolve(__dirname, "..", "resources", "bin");
const OUT = join(OUT_DIR, "cairn");

const TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

function run(cmd, args, opts = {}) {
  console.log(`$ ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { stdio: "inherit", ...opts });
  if (r.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} exited ${r.status}`);
  }
}

mkdirSync(OUT_DIR, { recursive: true });

// Build each arch.
const builtBinaries = [];
for (const t of TARGETS) {
  run("rustup", ["target", "add", t], { cwd: REPO });
  run(
    "cargo",
    ["build", "--release", "--locked", "-p", "cairn-cli", "--target", t],
    { cwd: REPO },
  );
  const bin = join(REPO, "target", t, "release", "cairn");
  if (!existsSync(bin)) throw new Error(`missing: ${bin}`);
  builtBinaries.push(bin);
}

// Try lipo. If only one arch (e.g. CI shortcut), copy that one.
if (builtBinaries.length === 2) {
  run("lipo", ["-create", "-output", OUT, ...builtBinaries]);
  run("lipo", ["-info", OUT]);
} else {
  copyFileSync(builtBinaries[0], OUT);
}

console.log(`sidecar built: ${OUT} (${statSync(OUT).size} bytes)`);
