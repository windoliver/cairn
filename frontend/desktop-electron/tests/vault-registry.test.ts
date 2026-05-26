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

  it("throws CORRUPT_REGISTRY and preserves a .bak on corrupt JSON", async () => {
    writeFileSync(path, "{not valid json");
    try {
      await readRegistry(path);
      throw new Error("expected throw");
    } catch (err) {
      expect(err.code).toBe("CORRUPT_REGISTRY");
      expect(err.backupPath).toBe(`${path}.bak`);
    }
    expect(existsSync(`${path}.bak`)).toBe(true);
    expect(readFileSync(`${path}.bak`, "utf8")).toBe("{not valid json");
  });

  it("throws CORRUPT_REGISTRY when version field is missing", async () => {
    writeFileSync(path, JSON.stringify({ vaults: [], active: null }));
    try {
      await readRegistry(path);
      throw new Error("expected throw");
    } catch (err) {
      expect(err.code).toBe("CORRUPT_REGISTRY");
    }
    expect(existsSync(`${path}.bak`)).toBe(true);
  });

  it("throws CORRUPT_REGISTRY for valid-JSON but missing vaults array", async () => {
    writeFileSync(path, JSON.stringify({ version: 1 }));
    try {
      await readRegistry(path);
      throw new Error("expected throw");
    } catch (err) {
      expect(err.code).toBe("CORRUPT_REGISTRY");
    }
    expect(existsSync(`${path}.bak`)).toBe(true);
  });

  it("throws CORRUPT_REGISTRY for vault entry missing required fields", async () => {
    writeFileSync(
      path,
      JSON.stringify({
        version: 1,
        vaults: [{ id: "x", path: "/p" }], // no label, no last_opened
        active: "x",
      }),
    );
    try {
      await readRegistry(path);
      throw new Error("expected throw");
    } catch (err) {
      expect(err.code).toBe("CORRUPT_REGISTRY");
    }
  });

  it("throws CORRUPT_REGISTRY for non-string active", async () => {
    writeFileSync(
      path,
      JSON.stringify({ version: 1, vaults: [], active: 42 }),
    );
    await expect(readRegistry(path)).rejects.toMatchObject({
      code: "CORRUPT_REGISTRY",
    });
  });

  it("throws UNSUPPORTED_VERSION for future schema version", async () => {
    writeFileSync(
      path,
      JSON.stringify({ version: 99, vaults: [], active: null }),
    );
    try {
      await readRegistry(path);
      throw new Error("expected throw");
    } catch (err) {
      expect(err.code).toBe("UNSUPPORTED_VERSION");
      expect(err.version).toBe(99);
      expect(err.message).toMatch(/version 99/);
    }
  });

  it("writes atomically (tmp + rename)", async () => {
    const reg: VaultRegistry = { version: 1, vaults: [], active: null };
    await writeRegistry(path, reg);
    // No leftover .tmp file
    expect(existsSync(`${path}.tmp`)).toBe(false);
  });
});
