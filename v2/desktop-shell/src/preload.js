const { contextBridge, ipcRenderer } = require("electron");

function subscribe(channel, handler) {
  const wrapped = (_event, payload) => handler(payload);
  ipcRenderer.on(channel, wrapped);
  return () => ipcRenderer.removeListener(channel, wrapped);
}

contextBridge.exposeInMainWorld("desktopShell", {
  getBootstrapData: () => ipcRenderer.invoke("desktop:getBootstrapData"),
  saveConfig: (cfg) => ipcRenderer.invoke("desktop:saveConfig", cfg),
  openModule: (moduleId) => ipcRenderer.invoke("desktop:openModule", moduleId),
  getHealthSnapshot: () => ipcRenderer.invoke("desktop:healthSnapshot"),
  recoverNow: () => ipcRenderer.invoke("desktop:recoverNow"),
  openExternal: (url) => ipcRenderer.invoke("desktop:openExternal", url),
  showLauncher: () => ipcRenderer.invoke("desktop:showLauncher"),
  getZoom: () => ipcRenderer.invoke("desktop:getZoom"),
  setZoom: (value) => ipcRenderer.invoke("desktop:setZoom", value),
  onUpdateStatus: (handler) => subscribe("desktop:update-status", handler),
  onHealthUpdated: (handler) => subscribe("desktop:health-updated", handler),
  onConfigUpdated: (handler) => subscribe("desktop:config-updated", handler)
});
