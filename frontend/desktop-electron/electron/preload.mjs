import { contextBridge } from "electron";

contextBridge.exposeInMainWorld("cairnDesktop", {
  apiBaseUrl: process.env.CAIRN_DESKTOP_API ?? "http://127.0.0.1:4000",
});
