import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  spawnSidecar,
  type SidecarHandle,
} from "../electron/sidecar.mjs";

let dir: string;
let handle: SidecarHandle | null = null;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "cairn-sc-"));
});

afterEach(async () => {
  if (handle) {
    await handle.kill();
    handle = null;
  }
  rmSync(dir, { recursive: true, force: true });
});

/** Make a fake `cairn` binary that just prints the canonical first line. */
function fakeBinary(addr = "127.0.0.1:54321"): string {
  const p = join(dir, "fake-cairn");
  const sh = [
    "#!/bin/sh",
    `echo "cairn-desktop listening on http://${addr}"`,
    "while true; do sleep 1; done",
  ].join("\n");
  writeFileSync(p, sh);
  chmodSync(p, 0o755);
  return p;
}

describe("sidecar", () => {
  it("rejects when binary missing", async () => {
    await expect(
      spawnSidecar({
        binary: join(dir, "does-not-exist"),
        vault: "/tmp/v",
        logPath: join(dir, "log"),
      }),
    ).rejects.toThrow(/ENOENT|not found/i);
  });

  it("parses bound address from first stdout line", async () => {
    handle = await spawnSidecar({
      binary: fakeBinary("127.0.0.1:54321"),
      vault: "/tmp/v",
      logPath: join(dir, "log"),
    });
    expect(handle.address).toBe("127.0.0.1:54321");
  });

  it("rejects on prefix mismatch", async () => {
    const p = join(dir, "bad-cairn");
    writeFileSync(p, "#!/bin/sh\necho 'not the prefix'\nsleep 30\n");
    chmodSync(p, 0o755);
    await expect(
      spawnSidecar({
        binary: p,
        vault: "/tmp/v",
        logPath: join(dir, "log"),
        bootTimeoutMs: 500,
      }),
    ).rejects.toThrow(/prefix|unexpected/i);
  });

  it("times out if no line printed", async () => {
    const p = join(dir, "silent-cairn");
    writeFileSync(p, "#!/bin/sh\nsleep 30\n");
    chmodSync(p, 0o755);
    await expect(
      spawnSidecar({
        binary: p,
        vault: "/tmp/v",
        logPath: join(dir, "log"),
        bootTimeoutMs: 200,
      }),
    ).rejects.toThrow(/timeout/i);
  });

  it("kill() terminates the child", async () => {
    handle = await spawnSidecar({
      binary: fakeBinary(),
      vault: "/tmp/v",
      logPath: join(dir, "log"),
    });
    await handle.kill();
    expect(handle.exited).toBe(true);
    handle = null; // afterEach skip
  });
});
