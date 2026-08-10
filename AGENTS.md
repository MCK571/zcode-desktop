# AGENTS.md

## 项目记忆（跨 session 持久，新 session 自动加载；有新事实请让我追加到本节）

> ZCode 用量监控桌面组件（Electron 版）：无边框置顶悬浮窗，每 ~1.5s 轮询 `window.zapi.status()`（preload contextBridge 桥），读取 `~/.zcode` 本地数据（tasks-index.sqlite / db.sqlite 的 model_usage / JSONL 日志）+ 火山方舟 OpenAPI（后台 scheduler 60s 刷新缓存），展示模型 token 用量 / 云端套餐额度 / 任务列表。pywebview 旧版在 `../agentDesktop/`（结构不同，勿混用）。

### 目录结构概览
- `src/main.js` -- 主进程：窗口创建（frameless + alwaysOnTop 322x840）、DWM Acrylic 背景模糊 + 圆角去边框、IPC 桥、单实例锁
- `src/preload.js` -- contextBridge 暴露 `window.zapi`（status/quit/getPos/moveWindow/resizeWindow/openTask/setOpacity/onBlur）
- `src/renderer/index.html` -- 前端单文件（HTML+内联 CSS+内联 JS，~2090 行）：界面 + 轮询渲染 + 折叠/展开 + 8 方向 resize
- `src/windowsBackdrop.js` -- Windows DWM Accent Acrylic 模糊（koffi 调 SetWindowCompositionAttribute / DwmEnableBlurBehindWindow）
- `src/windowsChrome.js` -- DWM 圆角（DWMWA_WINDOW_CORNER_PREFERENCE）+ 去 1px 边框（DWMWA_BORDER_COLOR=NONE）
- `src/data/` -- 数据层：`index.js`（createApi().status() 聚合）、`sqlite.js`（读库+日志）、`volc.js`（SigV4 + 套餐解析）、`deepseek.js` / `opencode.js`（余额/Go 套餐，后台刷新 + 缓存）、`scheduler.js`（60s 周期后台刷新）
- `package.json` -- electron ^35 + koffi ^2.9 + electron-builder portable；无其他运行依赖（SQLite 用内置 node:sqlite）
- `icon.ico`、`README.md`、`启动组件.bat`（双击启动，含错误提示）

### 关键文件入口
- 主进程入口: `src/main.js` 的 `createWindow()`（第 44 行）-- `npm start` / 打包 exe 均走这里
- 数据聚合: `src/data/index.js` 的 `createApi().status()` -- 所有前端数据源，**零网络只读缓存**（网络调用全在 scheduler 后台）
- 前端轮询: `src/renderer/index.html` 的 `startPolling()`（每 1500ms 调 `api.status()`）
- 打包配置: `package.json` 的 `build` 字段 -- portable 单文件 exe，asarUnpack koffi

### 功能模块
- **窗口/桥接层** (`main.js`): frameless + `alwaysOnTop`；`transparent:false` + `backgroundColor:'#00000000'` → 页面透明区显示 DWM Acrylic（真磨砂，非 CSS 假透明）；`resizable:false` + 前端自实现 8 方向 resize（`win:resize` 临时切 resizable 再切回）；失焦自动折叠（`blur` → 前端 `__toggleExpand(true)`）
- **拖拽**（重要，分两态）: 展开态 header 用系统级 `-webkit-app-region: drag`（OS 原生拖，不走 JS/IPC，渲染器收不到事件）；折叠态用 JS `pointerdown + setPointerCapture + moveWindow()`（窗口 48x48 太小，指针捕获保证移出窗口仍收事件）。logo 点击展开由 JS pointerup 的移动阈值区分点击/拖动
- **数据采集** (`data/`): `sqlite.js` 的 `readTasks`/`readTaskTokens`/`readUsage`/`readProviderUsage`/`readLiveActivity`（读 `~/.zcode/v2/tasks-index.sqlite` + `~/.zcode/cli/db/db.sqlite` 的 model_usage 表 + tail JSONL 日志）；`volc.js` SigV4（open.volcengineapi.com / SERVICE=ark / REGION=cn-beijing / VERSION=2024-01-01）
- **前端渲染** (`index.html`): 轮询分区渲染（usage/plan/provider/opencode/tasks/clock）；SVG 渐变数字（`getComputedTextLength()` 测宽，display:none 容器估算宽度、切 tab 后 remeasure）；玻璃通透度滑块只改 CSS 变量 `--glass-alpha`（映射 (115-v)/100，min=30 时填充按轨道比例归一化）；任务卡点击 `openTask` 走 `zcode://open-project?directory=` 协议
- **性能设计**（2.0.3 后）: `status()` 每轮主进程阻塞 ~10ms（15s 聚合缓存 + 日志尾部读）；渲染端 `refreshSig` 稳定签名，数据未变跳过整块 DOM 重建；时钟有独立 1s tick

### 技术栈与工程配置
- 语言/运行时: Node + Electron ^35（实测 35.7.5）；前端原生 HTML/CSS/JS 单文件，无框架
- 依赖: `koffi`（原生 DLL 调用：user32/dwmapi/gdi32，用于 Accent 模糊、圆角、DWM 属性）；`node:sqlite` DatabaseSync（内置，readOnly 打开）；devDeps: electron + electron-builder
- 打包: electron-builder `portable` 单文件 exe（~78MB），产物 `dist/ZCode Usage Widget <版本>.exe`；发布时复制一份到仓库根目录（两者都提交，已跟踪）
- 平台: 仅 Windows（DWM/user32/gdi32 均为 Win 特有；Win11 22000+ 才支持 backgroundMaterial，但本项目统一走 Accent recipe）
- 凭证 (`.volc.env`): `VOLC_AK_ID`/`VOLC_AK_SECRET`（火山）/`VOLC_PLAN_TYPE`（coding 默认/agent）/`VOLC_PLAN_TIER`（手填档位）/`VOLC_PLAN_START`（套餐开通时间，倒推窗口分模型 token）；打包后从 `PORTABLE_EXECUTABLE_DIR` 读取兜底
- 网络注意: `github.com` 主站间歇不可达（DNS 抖动/被墙），已把 `20.205.243.166` 钉入 `C:\Windows\System32\drivers\etc\hosts`（备份在 `/tmp/hosts.bak`）；`api.github.com` 一直通；SSH 密钥 `id_ed25519` 未注册到 GitHub 账号（SSH 推不了）

### 运行/构建/发布命令
- 开发运行: `npm start`（electron .）
- 打包: `npm run dist`（electron-builder --win --x64，portable）
- 发布链路（2.0.3 起的既定流程）:
  ```
  bump package.json version
  npm run dist                     # 产物 dist/ZCode Usage Widget <v>.exe
  cp "dist/ZCode Usage Widget <v>.exe" "ZCode Usage Widget <v>.exe"
  git add package.json src/ 两个exe
  git commit -m "fix/build: ..."
  git tag v<v> && git push origin main && git push origin v<v>
  ```

### 历史决策与已知坑
- **展开态拖动卡顿根因链**（2.0.3 修复，最重要的坑）: 展开态拖动是系统原生 `app-region: drag`，卡顿源不是 JS 拖拽代码。真因是每 1.5s 轮询 `status()` 时主进程同步执行：① `readLogLines()` 无 tail 参数时**整文件** `readFileSync + split + 逐行 JSON.parse`（日志几十 MB 时阻塞数百 ms）；② `readUsage`/`readProviderUsage` 的 SUM 全表扫描聚合（每个 ~20ms，一轮 6+ 个 ≈ 180ms）。主进程阻塞 → 窗口消息泵停转 → 原生拖动周期冻结。修复：① `readLogLines` 改尾部 chunk 读（seek size-512KB，`STATUS_LOG_TAIL=2000`）；② `readUsage`/`readProviderUsage` 加 15s TTL 缓存（`ttlMemo`，复用 deepseek/opencode 的 `{ts,payload}` 缓存模式）；③ 渲染端 `refreshSig` 稳定签名（剔除每次变化的 `s.now`/`usage.updatedAt`），数据未变跳过 DOM 重建 [来源: 本会话实测诊断]
- **Electron backgroundMaterial 不生效**（main.js 注释）：Win11 22000+ 的 `backgroundMaterial:'acrylic'` 实测 backdrop=0，统一走 `windowsBackdrop.js` 的 Accent ACRYLICBLURBEHIND recipe；`ready-to-show` 后重设一次 DWM 圆角/去边框（显示流程会重置，实测折叠后出现 1px 白边）
- **resizable:false 窗口 setSize 无效**（Electron 已知限制）：`win:resize` 先 `setResizable(true)` → setSize → 切回，之后重设 `applyWindowsChrome`
- **页面内双重毛玻璃未处理**（候选优化）：DWM Accent 整窗模糊之上，页面 `.widget::before` 还有 `backdrop-filter: blur(32px)`，拖动时每帧重采样是持续开销；2.0.3 只解决了周期冻结，若用户仍觉"发闷"可考虑降/去页面内 blur
- **SVG 渐变数字测宽坑**（规范 §5）：display:none 容器里 `getComputedTextLength()` 返回 0，用估算宽度占位，切 provider tab 后 `remeasureView` 实测修正；SVG 无文本基线，flex 对齐用 center 不用 baseline
- **opacity-btn 用 div 不用 button**：button 内容模型不允许 div，pop 会被踢出按钮导致点击错位
- **单实例锁**（main.js）：重复双击 bat 不叠窗，聚焦已有窗口
- **滑块填充归一化**（2.0.2）：range min=30 时 `--op-fill` 按 `(v-min)/(max-min)` 计算，否则原点右侧凸出
- **打包后凭证读取**（2.0.1）：portable 单文件 exe 的 `process.cwd()` 不可靠，用 `PORTABLE_EXECUTABLE_DIR` 兜底
- **exe 大文件**：78MB 超 GitHub 50MB 推荐线（仅警告仍可推；未来量大考虑 LFS）
- **遗留脏文件**（未提交，勿误删/误提交）: 根 + `dist/` 的 `ZCode Usage Widget 2.0.1.exe`（已跟踪被改动）、`dist/ZCode Usage Widget 2.0.2.exe`（未跟踪）；发布时 `git add` 只加当版本的两个 exe
- **测试体系薄弱**：无自动化测试/CI，仅有本会话的临时 node 自检脚本（tail 逻辑、ttlMemo 行为、真实库计时冒烟）[待补充]

---

## 角色定义
你是我的开发协作助手，主要帮助我开发这个 Electron 桌面组件项目，前端部分为 `src/renderer/index.html` 单文件（原生 HTML/CSS/JS，无框架）。

## 默认技术栈与偏好
- 主进程用 Node/Electron 35，遵循现有 `src/` 各文件的代码风格与封装方式（主进程 / preload / 数据层分层明确）。
- 前端 `index.html` 为单文件内联 CSS/JS，不引入框架；改动时保持单文件结构。
- 不要随意引入新依赖；确实需要时先说明原因（SQLite 用内置 `node:sqlite`，原生调用用已装的 `koffi`）。
- 修改代码前先阅读相关文件，避免凭空假设。
- 回答问题时尽量给出可直接落地的实现方案，而不是只讲概念。
- 涉及 DWM/Accent/圆角/拖拽/打包等坑点时，优先参考「历史决策与已知坑」小节，不要重蹈覆辙。
- 性能问题（拖动卡顿、轮询阻塞等）优先定位根因并给验证方式：主进程同步阻塞用真实库计时冒烟验证，前端帧率用 DevTools Performance 录制。
- 保持良好的代码注释习惯，关键决策与坑点注释要写清原因。
- 涉及 `index.html` 界面 UI 设计的，按前端 skill 决策树调用 frontend-design / impeccable；纯逻辑改动按 ponytail 最小化。
- 收到用户需求后，先列出明确的执行计划，包括：需要修改的文件清单、每个文件的修改内容概述、将要采用的技术方案。执行计划列出后，必须等待用户确认后才能开始执行。

## 回答习惯
- 用中文回答。
- 结论先行，必要时再解释原因。
- 代码示例尽量完整但不要冗长。
- 如果需要修改文件，直接执行修改，并在最后简要说明改了什么和如何验证。
- 如果需求不明确，必须先向我确认，得到确认后再执行，不要自行假设关键需求。

## 代码改动收尾约束（必须遵守）
- **每次改动完代码并验证通过后，必须主动向用户确认：是否需要提交（git commit）？** 不要自行提交。
- 本项目改动需要重新打包才能生效（产物是 `dist/ZCode Usage Widget <版本>.exe`），所以：
  - 用户确认要提交时，进一步确认是否需要先打包（`npm run dist`）。
  - 通常提交应包含重新打包后的 exe（根 + dist 各一份），即 **打包 -> 提交** 的顺序。
- 本项目还涉及发布 release：用户确认提交后，进一步确认是否需要发布（打 tag `v<版本>` + `git push origin main` + `git push origin v<版本>`）。发布动作必须由用户明确授权，不要自行执行。
- 整条链路：改动 -> 验证 -> [确认] 打包 -> [确认] 提交 -> [确认] 发布 release。每一步都要等用户确认再往下走，不要合并确认。

## Skills 使用要求
- 当任务明显适合某个已安装 skill 时，主动调用对应 skill。
- 如果我明确说"使用某个 skill"，必须优先尝试调用该 skill。
- 每次回答结束时，都必须附上一行："本次对话调用的 skills：xxx"
- 如果没有调用任何 skill，也必须写："本次对话调用的 skills：无"
- 如果某个 skill 被请求但不可用，要明确说明："请求的 skill 不可用：xxx"

## 可用 Skills 列表
- `/frontend-design` - `index.html` 界面设计与美化
- `/impeccable` - UI 审查 / 优化 / 性能诊断（critique / audit / optimize）
- `/ponytail` - 代码最小化（纯逻辑文件：data/、preload.js、main.js 强制适用）
- `/browser-use` - 浏览器调试（前端渲染验证）
- `/caveman` - 简洁回复风格
