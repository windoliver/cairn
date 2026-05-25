// Vault registry — JSON file at ~/Library/Application Support/cairn/vault_registry.json
// Schema v1: { version: 1, vaults: [{id, path, label, last_opened}], active: id|null }

import { promises as fs } from "node:fs";

export const CURRENT_VERSION = 1;

/**
 * @typedef {{id: string, path: string, label: string, last_opened: number}} VaultEntry
 * @typedef {{version: number, vaults: VaultEntry[], active: string|null}} VaultRegistry
 */

/**
 * Read the registry.
 *
 * Returns null ONLY when the file is missing (ENOENT). Throws a typed
 * error otherwise so the caller can distinguish "first launch" from
 * "user data exists but unreadable" and never silently overwrites a
 * .bak'd registry.
 *
 * @throws {Error & {code: "CORRUPT_REGISTRY", backupPath: string}}
 *   When the file exists but does not parse, the original is saved to
 *   <path>.bak first and a CORRUPT_REGISTRY error is thrown.
 * @throws {Error & {code: "UNSUPPORTED_VERSION", version: number}}
 *   When the file parses but reports a schema version newer than this
 *   build understands. The caller must show a downgrade error and not
 *   overwrite the existing file.
 *
 * @param {string} path
 * @returns {Promise<VaultRegistry|null>}
 */
export async function readRegistry(path) {
  let raw;
  try {
    raw = await fs.readFile(path, "utf8");
  } catch (err) {
    if (err.code === "ENOENT") return null;
    throw err;
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    const backupPath = `${path}.bak`;
    await fs.writeFile(backupPath, raw);
    const err = new Error(
      `vault_registry.json is unreadable; original saved to ${backupPath}`,
    );
    err.code = "CORRUPT_REGISTRY";
    err.backupPath = backupPath;
    throw err;
  }
  if (typeof parsed.version !== "number") {
    const backupPath = `${path}.bak`;
    await fs.writeFile(backupPath, raw);
    const err = new Error(
      `vault_registry.json has no version field; original saved to ${backupPath}`,
    );
    err.code = "CORRUPT_REGISTRY";
    err.backupPath = backupPath;
    throw err;
  }
  if (parsed.version > CURRENT_VERSION) {
    const err = new Error(
      `vault_registry.json was written by a newer Cairn (version ${parsed.version}). ` +
        `This build only understands version ${CURRENT_VERSION}. ` +
        `Upgrade Cairn or point at a different vault.`,
    );
    err.code = "UNSUPPORTED_VERSION";
    err.version = parsed.version;
    throw err;
  }
  // No v0→v1 migration yet (v1 is the initial schema).
  return parsed;
}

/**
 * Atomic write: write to <path>.tmp, then rename. fs.rename is atomic
 * on POSIX when source and dest are on the same filesystem.
 *
 * @param {string} path
 * @param {VaultRegistry} reg
 */
export async function writeRegistry(path, reg) {
  const tmp = `${path}.tmp`;
  await fs.writeFile(tmp, JSON.stringify(reg, null, 2));
  await fs.rename(tmp, path);
}
