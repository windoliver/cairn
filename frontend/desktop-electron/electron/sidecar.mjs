// Sidecar — spawn the cairn binary, parse its bound address from the
// first stdout line, tee stderr to a log file, and own the child's
// lifecycle. The first stdout line is the canonical "cairn-desktop
// listening on http://HOST:PORT" emitted by `cairn serve`.

import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { promises as fs } from "node:fs";
import { dirname } from "node:path";
import { createInterface } from "node:readline";

const PREFIX = "cairn-desktop listening on http://";
const DEFAULT_BOOT_TIMEOUT_MS = 10_000;

/**
 * @typedef {object} SpawnOpts
 * @property {string} binary       Absolute path to the `cairn` binary
 * @property {string} vault        Vault path (passed via --vault; informational in alpha)
 * @property {string} logPath      Where to tee stderr
 * @property {string|number} [port]    TCP port (default "4000"; "0" for ephemeral, used by tests)
 * @property {number} [bootTimeoutMs]
 */

/**
 * @typedef {object} SidecarHandle
 * @property {string} address      e.g. "127.0.0.1:54321"
 * @property {string|null} token   Bearer token for non-/health routes;
 *   null when auth is disabled (sidecar was started with
 *   CAIRN_DESKTOP_TOKEN="").
 * @property {boolean} exited
 * @property {(cb: (code: number|null, signal: string|null) => void) => void} onExit
 *   Subscribe to child exit. Fires once with the exit code + signal. If
 *   the child has already exited, the callback fires on the next tick.
 * @property {() => Promise<void>} kill
 *   SIGTERM, escalate to SIGKILL after 5s, resolve when the child has
 *   actually exited. Idempotent.
 */

/**
 * @param {SpawnOpts} opts
 * @returns {Promise<SidecarHandle>}
 */
export async function spawnSidecar(opts) {
  const timeoutMs = opts.bootTimeoutMs ?? DEFAULT_BOOT_TIMEOUT_MS;
  await fs.mkdir(dirname(opts.logPath), { recursive: true });
  const logStream = createWriteStream(opts.logPath, { flags: "a" });

  let child;
  try {
    child = spawn(
      opts.binary,
      [
        "serve",
        "--port",
        // Default to ephemeral (port 0) so two Cairn instances or a
        // stale sidecar on port 4000 don't cause a hard launch failure.
        // The discovered address flows back via the stdout first-line
        // protocol and is injected into the renderer through preload.
        // Tests can pin a port via opts.port for deterministic asserts.
        String(opts.port ?? 0),
        "--vault",
        opts.vault,
        // The alpha doesn't bind --vault to a real repository; serve refuses
        // to start with --vault unless this ack flag is also set. main.mjs
        // is the one place that knows it's launching the alpha desktop, so
        // it's the right place to assert the acknowledgement.
        "--alpha-fixture",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
  } catch (err) {
    await new Promise((resolve) => {
      logStream.end(() => resolve());
    });
    throw err;
  }

  child.stderr.pipe(logStream);

  // ENOENT shows up as a process error event, not a spawn throw.
  const errPromise = new Promise((_, reject) => {
    child.on("error", reject);
  });

  const TOKEN_PREFIX = "cairn-desktop token ";
  // Sidecar emits two stdout lines on boot:
  //   1. "cairn-desktop listening on http://HOST:PORT"
  //   2. "cairn-desktop token TOKEN"  (or "... token <none>")
  // Resolves with { address, token }; token is null when sidecar
  // disabled auth (test/dev with CAIRN_DESKTOP_TOKEN="").
  const linePromise = new Promise((resolve, reject) => {
    const rl = createInterface({ input: child.stdout });
    let settled = false;
    let address = null;
    rl.on("line", (line) => {
      if (settled) return;
      if (address === null) {
        if (!line.startsWith(PREFIX)) {
          settled = true;
          rl.close();
          reject(new Error(`unexpected first stdout line (prefix mismatch): ${line}`));
          return;
        }
        address = line.slice(PREFIX.length).trim();
        return;
      }
      if (!line.startsWith(TOKEN_PREFIX)) {
        settled = true;
        rl.close();
        reject(new Error(`unexpected second stdout line (token prefix mismatch): ${line}`));
        return;
      }
      const raw = line.slice(TOKEN_PREFIX.length).trim();
      const token = raw === "<none>" ? null : raw;
      settled = true;
      rl.close();
      resolve({ address, token });
    });
    rl.once("close", () => {
      if (!settled) {
        reject(new Error("sidecar stdout closed before discovery completed"));
      }
    });
  });

  // Persistent exit-observer attached BEFORE the boot race so both
  // boot-failure cleanup AND post-boot kill() share the same await
  // path. Without this, a child that exits between any guard check
  // and a later-attached "exit" listener would leave the cleanup
  // promise pending forever.
  //
  // Resolves on either "exit" (normal child lifecycle) OR "error"
  // (ENOENT — spawn produced a child object but it never actually
  // started; no "exit" event will ever arrive in that case).
  let exited = child.exitCode !== null || child.signalCode !== null;
  const exitPromise = new Promise((resolve) => {
    if (exited) {
      resolve();
      return;
    }
    const done = () => {
      exited = true;
      resolve();
    };
    child.once("exit", done);
    child.once("error", done);
  });

  /** Send SIGTERM, escalate to SIGKILL after 5s, await actual exit. */
  async function terminate() {
    if (exited) return;
    try {
      child.kill("SIGTERM");
    } catch {}
    const grace = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch {}
    }, 5000);
    try {
      await exitPromise;
    } finally {
      clearTimeout(grace);
    }
  }

  let bootTimeoutId;
  const timeout = new Promise((_, reject) => {
    bootTimeoutId = setTimeout(
      () => reject(new Error(`sidecar boot timeout after ${timeoutMs}ms`)),
      timeoutMs,
    );
  });

  let discovery;
  try {
    discovery = await Promise.race([linePromise, errPromise, timeout]);
    errPromise.catch(() => {});
    clearTimeout(bootTimeoutId);
  } catch (err) {
    clearTimeout(bootTimeoutId);
    // Use the same awaited terminate path; do not throw until the
    // child has actually exited, otherwise port/log resources can leak.
    await terminate();
    await new Promise((resolve) => {
      logStream.end(() => resolve());
    });
    throw err;
  }

  /** @type {SidecarHandle} */
  const handle = {
    address: discovery.address,
    token: discovery.token,
    get exited() {
      return exited;
    },
    /** Resolves when the child has actually exited. */
    onExit(callback) {
      if (exited) {
        // Already exited before the consumer attached — fire next tick
        // so consumers can install other handlers first.
        setImmediate(callback);
      } else {
        child.once("exit", (code, signal) => callback(code, signal));
      }
    },
    async kill() {
      await terminate();
    },
  };
  return handle;
}
