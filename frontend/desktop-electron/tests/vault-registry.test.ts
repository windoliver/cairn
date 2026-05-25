import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, rmSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  readRegistry,
  writeRegistry,
  type VaultRegistry,
} from "../electron/vault-registry.mjs";

let dir: string;
let path: string;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "cairn-reg-"));
  path = join(dir, "vault_registry.json");
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("vault-registry", () => {
  it("returns null when file missing", async () => {
    expect(await readRegistry(path)).toBeNull();
  });

  it("round-trips a v1 registry", async () => {
    const reg: VaultRegistry = {
      version: 1,
      vaults: [{ id: "abc", path: "/home/u/v", label: "v", last_opened: 0 }],
      active: "abc",
    };
    await writeRegistry(path, reg);
    expect(await readRegistry(path)).toEqual(reg);
  });

  it("preserves a .bak on corrupt JSON", async () => {
    writeFileSync(path, "{not valid json");
    const result = await readRegistry(path);
    expect(result).toBeNull();
    expect(existsSync(`${path}.bak`)).toBe(true);
    expect(readFileSync(`${path}.bak`, "utf8")).toBe("{not valid json");
  });

  it("rejects unknown future schema version", async () => {
    writeFileSync(
      path,
      JSON.stringify({ version: 99, vaults: [], active: null }),
    );
    await expect(readRegistry(path)).rejects.toThrow(/version 99/);
  });

  it("writes atomically (tmp + rename)", async () => {
    const reg: VaultRegistry = { version: 1, vaults: [], active: null };
    await writeRegistry(path, reg);
    // No leftover .tmp file
    expect(existsSync(`${path}.tmp`)).toBe(false);
  });
});
