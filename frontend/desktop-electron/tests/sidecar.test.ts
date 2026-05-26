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
    `echo "cairn-desktop token <none>"`,
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

  it("onExit fires when the child exits after discovery", async () => {
    // Fake binary: print the discovery line, then exit 0 after 50ms.
    const p = join(dir, "die-after-discovery");
    writeFileSync(
      p,
      [
        "#!/bin/sh",
        'echo "cairn-desktop listening on http://127.0.0.1:54322"',
        'echo "cairn-desktop token <none>"',
        "sleep 0.05",
        "exit 0",
      ].join("\n"),
    );
    chmodSync(p, 0o755);
    handle = await spawnSidecar({
      binary: p,
      vault: "/tmp/v",
      logPath: join(dir, "log"),
    });
    expect(handle.address).toBe("127.0.0.1:54322");
    const exitInfo = await new Promise<{ code: number | null; signal: string | null }>(
      (resolve) => handle.onExit((code, signal) => resolve({ code, signal })),
    );
    expect(handle.exited).toBe(true);
    expect(exitInfo.code).toBe(0);
    handle = null;
  });

  it("boot-timeout cleanup waits for child exit (does not leak)", async () => {
    // Fake binary: trap TERM, sleep forever. spawnSidecar must wait for
    // the SIGKILL escalation rather than throwing while the child is
    // still alive.
    const p = join(dir, "trap-term");
    writeFileSync(
      p,
      [
        "#!/bin/sh",
        "trap '' TERM",
        "sleep 30",
      ].join("\n"),
    );
    chmodSync(p, 0o755);
    const start = Date.now();
    await expect(
      spawnSidecar({
        binary: p,
        vault: "/tmp/v",
        logPath: join(dir, "log"),
        bootTimeoutMs: 200,
      }),
    ).rejects.toThrow(/timeout/i);
    const elapsed = Date.now() - start;
    // Must wait long enough for the 5s SIGKILL escalation in terminate().
    // Allow generous bounds (CI runners are slow); the key check is
    // that we don't return immediately at ~200ms timeout.
    expect(elapsed).toBeGreaterThanOrEqual(5_000);
    expect(elapsed).toBeLessThan(10_000);
  }, 15_000);
});
