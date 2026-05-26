import { app, BrowserWindow, dialog } from "electron";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";
import { promises as fs } from "node:fs";
import { spawnSidecar } from "./sidecar.mjs";
import { readRegistry, writeRegistry, CURRENT_VERSION } from "./vault-registry.mjs";
import { parseSmokeFlags } from "./smoke-flag.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const APP_SUPPORT = join(homedir(), "Library", "Application Support", "cairn");
const REGISTRY_PATH = join(APP_SUPPORT, "vault_registry.json");
const LOG_PATH = join(APP_SUPPORT, "logs", "desktop.log");

function sidecarBinary() {
  if (app.isPackaged) {
    return join(process.resourcesPath, "bin", "cairn");
  }
  // Dev: walk up to the workspace root and find target/debug/cairn.
  return join(__dirname, "..", "..", "..", "target", "debug", "cairn");
}

async function ensureAppSupport() {
  await fs.mkdir(APP_SUPPORT, { recursive: true });
  await fs.mkdir(join(APP_SUPPORT, "logs"), { recursive: true });
  await fs.mkdir(join(APP_SUPPORT, "models"), { recursive: true });
}

async function pollHealth(address, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  const url = `http://${address}/health`;
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(url);
      if (resp.ok) return true;
    } catch {}
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

// Sidecar binds to a fixed port (4000) so preload's hardcoded apiBaseUrl works.
// Sidecar binds an ephemeral port; the discovered address is forwarded
// to the renderer via webPreferences.additionalArguments so preload can
// pick it up synchronously without an extra IPC roundtrip per fetch.
async function createWindow(apiBase) {
  const win = new BrowserWindow({
    width: 1320,
    height: 860,
    minWidth: 1024,
    minHeight: 720,
    webPreferences: {
      preload: join(__dirname, "preload.mjs"),
      contextIsolation: true,
      nodeIntegration: false,
      additionalArguments: [`--cairn-api-base=${apiBase}`],
    },
  });
  if (process.env.NODE_ENV === "development") {
    const devUrl = process.env.VITE_DEV_SERVER_URL ?? "http://127.0.0.1:5173";
    await win.loadURL(devUrl);
  } else {
    await win.loadFile(join(__dirname, "../dist/index.html"));
  }
  return win;
}

async function main() {
  const smoke = parseSmokeFlags(process.argv);
  await ensureAppSupport();

  let registry;
  try {
    registry = await readRegistry(REGISTRY_PATH);
  } catch (err) {
    // Distinguish corrupt vs unsupported-version recovery paths so we
    // never silently overwrite the user's vault index. Both block start
    // until the user takes explicit action.
    if (err.code === "CORRUPT_REGISTRY") {
      const { response } = await dialog.showMessageBox({
        type: "error",
        title: "Cairn vault index unreadable",
        message:
          `Cairn could not read its vault index. The original was saved ` +
          `to ${err.backupPath}. Choose how to proceed.`,
        detail:
          "Reset clears the index and starts a fresh first-launch (vaults " +
          "themselves are NOT deleted). Quit lets you inspect the backup.",
        buttons: ["Reset index (start fresh)", "Quit"],
        cancelId: 1,
        defaultId: 1,
      });
      if (response === 1) {
        app.exit(1);
        return;
      }
      registry = null; // Caller will treat as first-launch and write fresh.
    } else if (err.code === "UNSUPPORTED_VERSION") {
      await dialog.showMessageBox({
        type: "error",
        title: "Cairn version mismatch",
        message: err.message,
        buttons: ["Quit"],
      });
      app.exit(1);
      return;
    } else {
      throw err;
    }
  }

  let vaultPath;
  if (smoke.enabled) {
    vaultPath = smoke.vault ?? join(APP_SUPPORT, "smoke-vault");
    await fs.mkdir(vaultPath, { recursive: true });
  } else if (registry) {
    // Registry exists — resolve active vault path.
    if (registry.active) {
      vaultPath =
        registry.vaults.find((v) => v.id === registry.active)?.path ?? null;
    }
    // If active points at a vanished id with multiple vaults present, do
    // NOT silently pick one — that could open the wrong vault and persist
    // the choice. Ask the user explicitly.
    if (!vaultPath && registry.vaults.length > 1) {
      const labels = registry.vaults.map(
        (v, i) => `${i + 1}. ${v.label || v.path}`,
      );
      const { response } = await dialog.showMessageBox({
        type: "warning",
        title: "Cairn vault selection needed",
        message:
          "The vault registry references a vault id that no longer exists. " +
          "Pick which vault to open. Your registry will not be overwritten " +
          "until you confirm.",
        buttons: [...labels, "Quit"],
        cancelId: labels.length,
        defaultId: 0,
      });
      if (response === labels.length) {
        app.exit(0);
        return;
      }
      vaultPath = registry.vaults[response].path;
      registry.active = registry.vaults[response].id;
      await writeRegistry(REGISTRY_PATH, registry);
    }
    // Single-vault case: stale active is unambiguous, fall back without
    // a dialog.
    if (!vaultPath && registry.vaults.length === 1) {
      vaultPath = registry.vaults[0].path;
      registry.active = registry.vaults[0].id;
      await writeRegistry(REGISTRY_PATH, registry);
    }
  }

  if (!vaultPath) {
    // First launch (non-smoke) OR registry exists but has zero vaults.
    // Minimal blocking flow for v1 of this packaging slice — a richer
    // React onboarding lands in a sibling issue. Default ~/Documents/cairn.
    vaultPath = join(homedir(), "Documents", "cairn");
    await fs.mkdir(vaultPath, { recursive: true });
    const entry = {
      id: crypto.randomUUID(),
      path: vaultPath,
      label: "Default",
      last_opened: Date.now(),
    };
    registry = registry ?? { version: CURRENT_VERSION, vaults: [], active: null };
    registry.vaults.push(entry);
    registry.active = entry.id;
    await writeRegistry(REGISTRY_PATH, registry);
  }

  // Alpha-fixture acknowledgement. The sidecar serves canned data
  // regardless of which vault is selected, so EVERY non-smoke launch
  // must surface that to the user before we spawn. Ack is persisted in
  // the registry so we only nag once per machine; subsequent launches
  // pass through silently. Smoke (CI) skips the prompt by design.
  if (!smoke.enabled) {
    if (registry && registry.alpha_fixture_acked === true) {
      // already acknowledged on a prior launch
    } else {
      const { response } = await dialog.showMessageBox({
        type: "info",
        title: "Cairn alpha — fixture data only",
        message:
          "This Cairn build is an alpha. The window will show example data, " +
          "NOT the contents of any vault listed in your registry. Real-vault " +
          "binding lands in a follow-up release.",
        detail:
          "Selected vault: " + vaultPath + "\n\n" +
          "Nothing you do in this window will modify your vault files. " +
          "Acknowledge to continue; this prompt will not repeat.",
        buttons: ["Continue", "Quit"],
        cancelId: 1,
        defaultId: 0,
      });
      if (response === 1) {
        app.exit(0);
        return;
      }
      if (registry) {
        registry.alpha_fixture_acked = true;
        await writeRegistry(REGISTRY_PATH, registry);
      }
    }
  }

  let handle;
  try {
    handle = await spawnSidecar({
      binary: sidecarBinary(),
      vault: vaultPath,
      logPath: LOG_PATH,
    });
  } catch (err) {
    if (!smoke.enabled) {
      dialog.showErrorBox("Cairn backend failed to start", String(err));
    } else {
      console.error("smoke: sidecar failed:", err);
    }
    app.exit(1);
    return;
  }

  app.on("before-quit", async (event) => {
    if (handle && !handle.exited) {
      event.preventDefault();
      await handle.kill();
      app.exit(0);
    }
  });

  if (smoke.enabled) {
    const ok = await pollHealth(handle.address, 30_000);
    await handle.kill();
    app.exit(ok ? 0 : 1);
    return;
  }

  // Normal launch: verify the sidecar is actually serving requests
  // before opening the window. spawnSidecar resolves on the first
  // stdout line; a sidecar that prints the address then dies leaves
  // the renderer pointed at a dead backend.
  const healthy = await pollHealth(handle.address, 15_000);
  if (!healthy) {
    dialog.showErrorBox(
      "Cairn backend not responding",
      `The sidecar started on ${handle.address} but did not answer /health ` +
        `within 15 seconds. See ${LOG_PATH} for details.`,
    );
    await handle.kill();
    app.exit(1);
    return;
  }

  // Post-launch crash recovery: if the sidecar dies after we open the
  // window, attempt one silent restart with a fresh /health poll; on a
  // second crash OR an unhealthy restart, surface a blocking dialog
  // with the log path. Quitting via app.quit suppresses restart (the
  // exit was intentional).
  let intentionalShutdown = false;
  app.on("before-quit", () => {
    intentionalShutdown = true;
  });
  let restartAttempted = false;
  let openWindow = null;
  function showCrashDialogAndExit(code, signal, why) {
    dialog.showMessageBoxSync({
      type: "error",
      title: "Cairn backend stopped",
      message:
        `The sidecar exited unexpectedly (code=${code}, signal=${signal}).` +
        (why ? ` ${why}` : ""),
      detail: `Log: ${LOG_PATH}`,
      buttons: ["Quit"],
    });
    app.exit(1);
  }
  function wireCrashHandler(h) {
    h.onExit(async (code, signal) => {
      if (intentionalShutdown) return;
      if (restartAttempted) {
        showCrashDialogAndExit(code, signal, "Automatic restart failed.");
        return;
      }
      restartAttempted = true;
      let next;
      try {
        next = await spawnSidecar({
          binary: sidecarBinary(),
          vault: vaultPath,
          logPath: LOG_PATH,
        });
      } catch (err) {
        console.error("sidecar restart spawn failed:", err);
        showCrashDialogAndExit(code, signal, "Restart spawn failed.");
        return;
      }
      const ok = await pollHealth(next.address, 15_000);
      if (!ok) {
        await next.kill();
        showCrashDialogAndExit(
          code,
          signal,
          "Restart bound a port but /health did not respond.",
        );
        return;
      }
      handle = next;
      wireCrashHandler(next);
      // Tell the renderer the address changed so it can re-target fetch.
      if (openWindow && !openWindow.isDestroyed()) {
        openWindow.webContents.send(
          "cairn:api-base",
          `http://${next.address}`,
        );
      }
    });
  }
  wireCrashHandler(handle);

  openWindow = await createWindow(`http://${handle.address}`);
}

app.whenReady().then(() => {
  void main();
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
