// Vault registry — JSON file at ~/Library/Application Support/cairn/vault_registry.json
// Schema v1: { version: 1, vaults: [{id, path, label, last_opened}], active: id|null }

import { promises as fs } from "node:fs";

export const CURRENT_VERSION = 1;

/**
 * @typedef {{id: string, path: string, label: string, last_opened: number}} VaultEntry
 * @typedef {{version: number, vaults: VaultEntry[], active: string|null}} VaultRegistry
 */

/**
 * Read the registry. Returns null when the file is missing OR corrupted
 * (in the corrupt case, the original is preserved as <path>.bak before
 * returning null so the caller can re-run onboarding). Throws when the
 * file exists, parses, but reports an unknown schema version — that's
 * a downgrade case and the caller must NOT silently overwrite it.
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
    await fs.writeFile(`${path}.bak`, raw);
    return null;
  }
  if (typeof parsed.version !== "number") {
    await fs.writeFile(`${path}.bak`, raw);
    return null;
  }
  if (parsed.version > CURRENT_VERSION) {
    throw new Error(
      `vault_registry.json was written by a newer Cairn (version ${parsed.version}). ` +
        `This build only understands version ${CURRENT_VERSION}. ` +
        `Upgrade Cairn or point at a different vault.`,
    );
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
