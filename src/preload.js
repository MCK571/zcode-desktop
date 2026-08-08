'use strict';

// ZCode 用量监控组件 — preload 桥。
// contextBridge 暴露 window.zapi，接口与 pywebview 版一致（status/getPos/
// moveWindow/resizeWindow/openTask/setOpacity/quit + onBlur 失焦折叠）。

const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('zapi', {
  status: () => ipcRenderer.invoke('api:status'),
  quit: () => ipcRenderer.send('win:quit'),
  getPos: () => ipcRenderer.invoke('win:getPos'),
  moveWindow: (x, y) => ipcRenderer.send('win:move', x, y),
  resizeWindow: (w, h) => ipcRenderer.send('win:resize', w, h),
  openTask: (workspacePath) => ipcRenderer.send('app:openTask', workspacePath),
  setOpacity: (v) => ipcRenderer.send('win:setOpacity', v),
  // 失焦自动折叠：主进程 blur 事件推给前端（__toggleExpand(true)）
  onBlur: (callback) => {
    const listener = () => {
      try { callback(); } catch (_) { /* 前端异常不阻塞 */ }
    };
    ipcRenderer.on('window:blur', listener);
    return () => ipcRenderer.removeListener('window:blur', listener);
  }
});
