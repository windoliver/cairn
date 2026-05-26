import { contextBridge, ipcRenderer } from "electron";

// Main injects the discovered sidecar address into a process.argv
// switch so the renderer doesn't have to make an async IPC call before
// every fetch. Env var still wins for local dev (CAIRN_DESKTOP_API).
function discoverApiBase() {
  if (process.env.CAIRN_DESKTOP_API) return process.env.CAIRN_DESKTOP_API;
  const flag = "--cairn-api-base=";
  const fromArgv = process.argv.find((a) => a.startsWith(flag));
  if (fromArgv) return fromArgv.slice(flag.length);
  return "http://127.0.0.1:4000"; // last-resort fallback for legacy dev
}

contextBridge.exposeInMainWorld("cairnDesktop", {
  apiBaseUrl: discoverApiBase(),
  // Allow the renderer to subscribe to address changes (e.g. after a
  // sidecar crash + restart on a different port).
  onApiBaseChange: (callback) => {
    ipcRenderer.on("cairn:api-base", (_event, url) => callback(url));
  },
});
