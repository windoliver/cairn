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
// Issue #XXX (file follow-up) will introduce IPC port discovery to support ephemeral binding.
async function createWindow() {
  const win = new BrowserWindow({
    width: 1320,
    height: 860,
    minWidth: 1024,
    minHeight: 720,
    webPreferences: {
      preload: join(__dirname, "preload.mjs"),
      contextIsolation: true,
      nodeIntegration: false,
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

  let registry = await readRegistry(REGISTRY_PATH);

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

  await createWindow();
}

app.whenReady().then(() => {
  void main();
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
