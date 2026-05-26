import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

let appSupport: string;
let vault: string;

const SCRIPT = join(__dirname, "..", "..", "..", "scripts", "uninstall.sh");

beforeEach(() => {
  appSupport = mkdtempSync(join(tmpdir(), "cairn-as-"));
  vault = mkdtempSync(join(tmpdir(), "cairn-v-"));
  // Seed app-support
  mkdirSync(join(appSupport, "models"), { recursive: true });
  mkdirSync(join(appSupport, "logs"), { recursive: true });
  writeFileSync(join(appSupport, "models", "bge.onnx"), "stub");
  writeFileSync(join(appSupport, "logs", "desktop.log"), "stub");
  writeFileSync(
    join(appSupport, "vault_registry.json"),
    JSON.stringify({
      version: 1,
      vaults: [{ id: "x", path: vault, label: "v", last_opened: 0 }],
      active: "x",
    }),
  );
  // Seed vault
  writeFileSync(join(vault, "purpose.md"), "important");
});

afterEach(() => {
  rmSync(appSupport, { recursive: true, force: true });
  rmSync(vault, { recursive: true, force: true });
});

describe("uninstall.sh", () => {
  it("removes models + logs, preserves registry by default", () => {
    execFileSync("bash", [SCRIPT, "--yes", "--app-support", appSupport]);
    expect(existsSync(join(appSupport, "models"))).toBe(false);
    expect(existsSync(join(appSupport, "logs"))).toBe(false);
    expect(existsSync(join(appSupport, "vault_registry.json"))).toBe(true);
  });

  it("never touches vault files", () => {
    execFileSync("bash", [SCRIPT, "--yes", "--app-support", appSupport]);
    expect(readFileSync(join(vault, "purpose.md"), "utf8")).toBe("important");
  });

  it("removes registry when --full", () => {
    execFileSync("bash", [SCRIPT, "--yes", "--full", "--app-support", appSupport]);
    expect(existsSync(join(appSupport, "vault_registry.json"))).toBe(false);
  });

  it("script contains no destructive command against $VAULT", () => {
    const src = readFileSync(SCRIPT, "utf8");
    // Allow $VAULT in echo lines; reject in rm/find/cp -rf etc.
    const lines = src.split("\n");
    for (const line of lines) {
      const stripped = line.trim();
      if (stripped.startsWith("#")) continue;
      if (/\b(rm|find|mv|cp)\b.*\$\{?VAULT\}?/i.test(stripped)) {
        throw new Error(`destructive command references VAULT: ${line}`);
      }
    }
  });
});
