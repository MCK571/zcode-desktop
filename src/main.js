'use strict';

// ZCode 用量监控组件 — Electron 主进程。
// 真磨砂方案（token-monitor 同款）：窗口不透明 + backgroundMaterial acrylic
// （Win11 原生 Acrylic，页面透明像素显示窗口背景的模糊），Win10 回退
// windowsBackdrop.js 的 Accent recipe。数据层在 src/data/，经 IPC 暴露给前端。

const { app, BrowserWindow, ipcMain, shell } = require('electron');
const path = require('path');
const os = require('os');
const { applyWindowsAccentBlur } = require('./windowsBackdrop');
const { applyWindowsChrome } = require('./windowsChrome');
const { createApi, startAll } = require('./data/index');

const WINDOW_W = 322;
const WINDOW_H = 840;
const WIN_X = 1260;
const WIN_Y = 80;
const ICON_W = 48;
const ICON_H = 48;

// Win11 22000+ 才支持 Electron backgroundMaterial（原生 Acrylic）
const IS_WIN11 = process.platform === 'win32' &&
  parseInt((os.release().split('.')[2] || '0'), 10) >= 22000;

// 不设 AppUserModelID（2026-08-14 任务栏图标排查）：'Oct1AtJoe.ZCodeUsageWidget'
// 在本机遗留图标失效的旧应用身份，任务栏按钮显示空白文档占位图标；
// 组件无通知/跳转列表需求，删掉后任务栏取窗口/exe 图标（正常）。


// 单实例锁：重复双击 bat 不叠窗，聚焦已有窗口
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (win) {
      if (win.isMinimized()) win.restore();
      win.show();
      win.focus();
    }
  });
}

let win = null;
let api = null;

function createWindow() {
  api = createApi();

  win = new BrowserWindow({
    width: WINDOW_W,
    height: WINDOW_H,
    x: WIN_X,
    y: WIN_Y,
    frame: false,
    transparent: false, // 关键：不透明窗口 + acrylic 背景（真磨砂，非 CSS 假透明）
    resizable: false,
    alwaysOnTop: true,
    show: false,
    backgroundColor: '#00000000', // 页面透明 → 显示窗口背景（acrylic 模糊）
    icon: path.join(__dirname, '..', 'icon.ico'),
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false
    }
  });

  // 背景模糊：全部走 Accent recipe（token-monitor windowsBackdrop）——
  // Electron backgroundMaterial 的 DWM 1px 边框（DWMWA_BORDER_COLOR=NONE
  // 控制不了，实测折叠图标右侧下方出现白边）；Accent 试无边框。
  if (process.platform === 'win32') {
    console.log('[main] applying Accent blur (no DWM border)');
    applyWindowsAccentBlur(win);
  }

  // 系统圆角（抗锯齿）+ 去边框细线（token-monitor windowsChrome）
  applyWindowsChrome(win, { round: true });

  // 桥/加载错误排查日志
  win.webContents.on('preload-error', (_e, _p, error) => {
    console.error('[main] preload-error:', error);
  });
  win.webContents.on('console-message', (_e, _level, message) => {
    console.log('[renderer]', message);
  });

  win.loadFile(path.join(__dirname, 'renderer', 'index.html'));
  win.once('ready-to-show', () => {
    // 窗口显示后再设一次 DWM 边框/圆角：创建时设置会被显示流程重置
    //（实测折叠图标右侧下方出现 1px 白边框线 = DWM 窗口边框）
    applyWindowsChrome(win, { round: true });
    // 探针：读 DWM backdrop 状态（3=Transient/Acrylic 生效，2=MainWindow，1=None，0=AUTO）
    const readBackdrop = () => {
      try {
        const koffi = require('koffi');
        const dwmapi = koffi.load('dwmapi.dll');
        const getAttr = dwmapi.func('int DwmGetWindowAttribute(uintptr_t hwnd, uint attr, void *pv, uint cb)');
        const hwndBuf = win.getNativeWindowHandle();
        const hwnd = hwndBuf.length >= 8 ? hwndBuf.readBigUInt64LE() : BigInt(hwndBuf.readUInt32LE());
        const buf = Buffer.alloc(4);
        getAttr(hwnd, 38, buf, 4);
        return buf.readUInt32LE();
      } catch {
        return -1;
      }
    };
    if (IS_WIN11) {
      // options 里 backgroundMaterial 实测不生效（backdrop=0），
      // 运行时 setBackgroundMaterial 再试一次
      try {
        win.setBackgroundMaterial('acrylic');
      } catch (e) {
        console.log('[main] setBackgroundMaterial fail:', e.message);
      }
    }
    console.log('[main] DWM backdrop type:', readBackdrop(), '(3=Acrylic 2=Mica 1=None 0=AUTO)');
    console.log('[main] window ready, backgroundMaterial:', IS_WIN11 ? 'acrylic' : 'accent-fallback');
    win.show();
  });

  // 失焦自动折叠 → 前端 __toggleExpand(true)（Electron 原生 blur 事件，
  // 比 pywebview Deactivate 干净）
  win.on('blur', () => {
    if (win && !win.isDestroyed()) win.webContents.send('window:blur');
  });
}

// ---- IPC 桥（对前端 window.api，preload.js 转发） ----
ipcMain.on('win:quit', () => app.quit());

ipcMain.on('win:move', (_e, x, y) => {
  if (win) win.setPosition(Math.round(x), Math.round(y));
});

ipcMain.on('win:resize', (_e, w, h) => {
  if (!win) return;
  const size = Math.max(ICON_W, Math.round(w));
  const height = Math.max(ICON_H, Math.round(h));
  // resizable:false 窗口 setSize 无效（Electron 已知限制），临时切 resizable 再切回
  win.setResizable(true);
  win.setSize(size, height);
  win.setResizable(false);
  // resize 后 DWM 边框/圆角可能恢复（实测折叠后出现白边），重设
  applyWindowsChrome(win, { round: true });
});

ipcMain.handle('win:getPos', () => {
  if (!win) return { x: 0, y: 0, w: 0, h: 0 };
  const [x, y] = win.getPosition();
  const [w, h] = win.getSize();
  return { x, y, w, h };
});

ipcMain.on('win:setOpacity', (_e, v) => {
  // Electron 窗口透明度（0-1）；注意与 acrylic 共存时观感，100% 时恢复原样
  if (win) win.setOpacity(Math.max(0.3, Math.min(1.0, Number(v) || 1)));
});

ipcMain.on('app:openTask', (_e, workspacePath) => {
  if (!workspacePath) return;
  const url = `zcode://open-project?directory=${encodeURIComponent(workspacePath)}`;
  shell.openExternal(url);
});

// 数据桥：status() 聚合所有数据（data/index.js 组装，返回结构同 pywebview 版）
ipcMain.handle('api:status', () => {
  try {
    return api.status();
  } catch (err) {
    return { error: String(err) };
  }
});

app.whenReady().then(() => {
  // 后台刷新调度（火山/DeepSeek/opencode，60s 周期，首次立即执行）
  startAll();
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  app.quit();
});
