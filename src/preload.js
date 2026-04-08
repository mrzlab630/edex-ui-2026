const {clipboard, contextBridge, ipcRenderer, webFrame} = require("electron");

const bootstrap = ipcRenderer.sendSync("bootstrap:get-sync");

const edexApi = {
    app: {
        focus: () => ipcRenderer.invoke("app:focus"),
        quit: () => ipcRenderer.invoke("app:quit"),
        relaunch: () => ipcRenderer.invoke("app:relaunch")
    },
    bootstrap,
    clipboard: {
        readText: () => clipboard.readText()
    },
    shell: {
        openExternal: target => ipcRenderer.invoke("shell:openExternal", target),
        openPath: target => ipcRenderer.invoke("shell:openPath", target)
    },
    shortcuts: {
        onAppAction: callback => {
            ipcRenderer.on("shortcut:app-action", (event, action) => callback(action));
        },
        onShellAction: callback => {
            ipcRenderer.on("shortcut:shell-action", (event, payload) => callback(payload));
        },
        registerAll: shortcuts => ipcRenderer.invoke("shortcuts:registerAll", shortcuts),
        unregisterAll: () => ipcRenderer.invoke("shortcuts:unregisterAll")
    },
    webFrame: {
        setVisualZoomLevelLimits: (min, max) => webFrame.setVisualZoomLevelLimits(min, max)
    },
    window: {
        getState: () => ipcRenderer.invoke("window:getState"),
        minimize: () => ipcRenderer.invoke("window:minimize"),
        onLeaveFullScreen: callback => {
            ipcRenderer.on("window:leave-full-screen", () => callback());
        },
        onResize: callback => {
            ipcRenderer.on("window:resize", () => callback());
        },
        setFullScreen: useFullScreen => ipcRenderer.invoke("window:setFullScreen", useFullScreen),
        setSize: (width, height) => ipcRenderer.invoke("window:setSize", width, height),
        toggleDevTools: () => ipcRenderer.invoke("window:toggleDevTools"),
        unmaximize: () => ipcRenderer.invoke("window:unmaximize")
    }
};

if (process.contextIsolated) {
    contextBridge.exposeInMainWorld("edex", edexApi);
} else {
    window.edex = edexApi;
}
