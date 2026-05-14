import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import packageJson from "../package.json";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

describe("Electron entrypoints", () => {
  it("uses runtime JavaScript files that Electron can load directly", () => {
    expect(packageJson.main).toBe("electron/main.mjs");

    const mainPath = resolve(packageRoot, packageJson.main);
    expect(existsSync(mainPath)).toBe(true);

    const mainSource = readFileSync(mainPath, "utf8");
    expect(mainSource).toContain("preload.mjs");
    expect(mainSource).not.toContain("preload.ts");
  });
});
