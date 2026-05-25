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
 * @property {boolean} exited
 * @property {() => Promise<void>} kill
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
        String(opts.port ?? 4000),
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

  const linePromise = new Promise((resolve, reject) => {
    const rl = createInterface({ input: child.stdout });
    let settled = false;
    rl.once("line", (line) => {
      settled = true;
      rl.close();
      if (!line.startsWith(PREFIX)) {
        reject(new Error(`unexpected first stdout line (prefix mismatch): ${line}`));
        return;
      }
      resolve(line.slice(PREFIX.length).trim());
    });
    rl.once("close", () => {
      if (!settled) {
        reject(new Error("sidecar stdout closed before first line"));
      }
    });
  });

  let bootTimeoutId;
  const timeout = new Promise((_, reject) => {
    bootTimeoutId = setTimeout(
      () => reject(new Error(`sidecar boot timeout after ${timeoutMs}ms`)),
      timeoutMs,
    );
  });

  let address;
  try {
    address = await Promise.race([linePromise, errPromise, timeout]);
    errPromise.catch(() => {});
    clearTimeout(bootTimeoutId);
  } catch (err) {
    try {
      child.kill("SIGTERM");
    } catch {}
    await new Promise((resolve) => {
      logStream.end(() => resolve());
    });
    throw err;
  }

  // Single, persistent exit-observer attached BEFORE returning the handle.
  // Without this, a child that exits in the window between the kill() guard
  // check and the kill()-local "exit" listener registration would leave the
  // kill() promise pending forever, hanging app shutdown.
  let exited = child.exitCode !== null || child.signalCode !== null;
  const exitPromise = new Promise((resolve) => {
    if (exited) {
      resolve();
      return;
    }
    child.once("exit", () => {
      exited = true;
      resolve();
    });
  });

  /** @type {SidecarHandle} */
  const handle = {
    address,
    get exited() {
      return exited;
    },
    async kill() {
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
    },
  };
  return handle;
}
