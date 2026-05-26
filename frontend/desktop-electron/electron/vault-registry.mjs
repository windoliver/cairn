// Vault registry — JSON file at ~/Library/Application Support/cairn/vault_registry.json
// Schema v1: { version: 1, vaults: [{id, path, label, last_opened}], active: id|null }

import { promises as fs } from "node:fs";

export const CURRENT_VERSION = 1;

/**
 * @typedef {{id: string, path: string, label: string, last_opened: number}} VaultEntry
 * @typedef {{
 *   version: number,
 *   vaults: VaultEntry[],
 *   active: string|null,
 *   alpha_fixture_acked?: boolean,
 * }} VaultRegistry
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
  if (parsed.version !== CURRENT_VERSION) {
    // Pre-v1 (legacy, ≤0) AND newer-than-v1 (future) both refused here.
    // v1 is the initial schema; no migration path exists yet. When v2+
    // lands, this is the function that grows a migration table.
    const err = new Error(
      `vault_registry.json reports version ${parsed.version}; this build ` +
        `only understands version ${CURRENT_VERSION}. ` +
        (parsed.version > CURRENT_VERSION
          ? "Upgrade Cairn or point at a different vault."
          : "No migration path is defined for older versions."),
    );
    err.code = "UNSUPPORTED_VERSION";
    err.version = parsed.version;
    throw err;
  }
  // Full v1 schema validation. A syntactically valid but malformed
  // payload (e.g. {"version":1} with no vaults array, or vaults missing
  // required fields) is treated as CORRUPT_REGISTRY so the recovery
  // dialog fires instead of an uncaught .length-of-undefined crash.
  if (!_isValidV1(parsed)) {
    const backupPath = `${path}.bak`;
    await fs.writeFile(backupPath, raw);
    const err = new Error(
      `vault_registry.json has invalid v1 shape; original saved to ${backupPath}`,
    );
    err.code = "CORRUPT_REGISTRY";
    err.backupPath = backupPath;
    throw err;
  }
  // No v0→v1 migration yet (v1 is the initial schema).
  return parsed;
}

/** @param {unknown} r */
function _isValidV1(r) {
  if (!r || typeof r !== "object") return false;
  if (!Array.isArray(r.vaults)) return false;
  if (r.active !== null && typeof r.active !== "string") return false;
  for (const v of r.vaults) {
    if (!v || typeof v !== "object") return false;
    if (typeof v.id !== "string") return false;
    if (typeof v.path !== "string") return false;
    if (typeof v.label !== "string") return false;
    if (typeof v.last_opened !== "number") return false;
  }
  // alpha_fixture_acked is optional (added post-launch); when present
  // must be boolean.
  if (
    r.alpha_fixture_acked !== undefined &&
    typeof r.alpha_fixture_acked !== "boolean"
  ) {
    return false;
  }
  return true;
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
