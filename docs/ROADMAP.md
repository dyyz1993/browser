# Roadmap — 里程碑路线图

> 状态图例：⚪ 未开始 / 🟡 进行中 / ✅ 完成 / ⏸ 暂停（defer）
> 详细功能见 [FEATURES.md](./FEATURES.md)，目标/非目标见 [GOALS.md](./GOALS.md)。

---

## 总览

| 里程碑 | 状态 | 目标 | 测试数 |
|--------|------|------|--------|
| M0 | ✅ | 项目骨架 + CI | — |
| M1 | ✅ | 能看到 HTML（curl + parse + 打印 DOM） | — |
| M2 | ✅ | 文本流渲染（HN fixture 看出标题列表） | 111 |
| M3 | ✅ | JS 执行（动态创建元素进 DOM） | 140 |
| M4 | ✅ | **SPA 渲染（项目目标达成）** | 160 |
| M5 | ✅ | GUI 窗口（跨平台开窗浏览） | 179 |
| M6 | ✅ | 渲染质量修复 + 黄金对比 + 跨平台 CI | 211 |
| M7 | ✅ | 渲染质量 + 交互（margin/font/URL 栏/滚动） | 217 |
| M8 | ✅ | 表单交互（input/textarea/button/submit） | 217 |
| M9 | ✅ | 图片占位符渲染（[IMG: src]） | 217 |
| M10 | ✅ | 性能优化（LayoutCache + DirtyTracker） | 217 |
| M11 | ✅ | Bug 修复（vh/vw/rem/pt） | 217 |
| M12 | ✅ | 截图 + 图像 ASCII（--screenshot / image-ascii） | 225 |
| M13 | ✅ | Web Storage（localStorage/sessionStorage） | 241 |
| M14 | ✅ | Navigation（history/location） | 260（M14.1-3 完成） |
| M15 | ✅ | Cookie jar（跨请求会话保持） | 291 |
| M16 | ✅ | 异步 JS（setTimeout + Promise，boa 自研） | 318 |
| M17 | ✅ | XMLHttpRequest（老 SPA 依赖） | 325 |
| M18 | ✅ | networkidle 算法（爬虫渲染完整性信号） | 332 |
| M19 | ✅ | 标准 fetch API（现代 SPA 核心） | 339 |
| M20 | ✅ | fetch 增强（POST/PUT/DELETE + 真实 status） | 350 |
| M21 | ✅ | Cookie 持久化（跨进程登录态） | 363 |
| M22 | ✅ | 真实图像渲染（<img> → ASCII art） | 366 |
| M23 | ✅ | WebSocket（手写 RFC 6455，实时 SPA） | 444 |
| M24 | ✅ | TLS 后端切换（百度等中信大站可连） | 444 |
| M25 | ✅ | 截图字形坐标修复（fontdue metrics，乱码→可读） | 446 |
| M26 | ✅ | textarea CSS 泄漏修复（百度截图 50MB→1.3MB） | 449 |
| M27 | ✅ | <a href> 链接目标渲染（G1 爬虫核心） | 452 |
| M28 | ✅ | JS 全局对象补齐（navigator/window/document/screen） | 481 |
| M29 | ✅ | ADR-0003 + wss:// 验证 + 截图 --max-height | 482 |
| M30 | ✅ | <a> 蓝色渲染（screenshot ANSI + #0000EE） | 489 |
| M31 | ✅ | wss:// TLS 支持（ws crate + native-tls） | 489 |
| M32 | ✅ | CSS flexbox 子集（row/column + grow + justify + gap） | 498 |
| M33 | ✅ | CSS grid 子集（grid-template-columns + gap + auto-placement） | 508 |
| M34 | ✅ | GUI 像素级渲染（render::font 共享，修复 'X' bug） | 508 |
| M35 | ✅ | flex-wrap/align-items/grid-col-row + ADR-0004 | 494 |
| M36 | ✅ | 中文字体渲染（双字体回退，NotoSansSC GB2312 1.6MB） | 538 |
| M37 | ✅ | render-file 异步 JS + 完整 Element 对象 | 538 |
| M38 | ✅ | XHR status/readyState/onreadystatechange 补齐 | 538 |
| M39 | ✅ | border + background-color ASCII 渲染 | 546 |
| M40 | ✅ | 真实 SPA 站点评估 + timeout 修复 | 554 |
| M41 | ✅ | Image 构造器 shim | 576 |
| M42 | ✅ | CDP WebSocket server + JSON-RPC framework | 576 |
| M43 | ✅ | CDP HTTP discovery endpoints | 587 |
| M44 | ✅ | CDP Page domain（navigate + captureScreenshot） | 593 |
| M45 | ✅ | CDP Runtime domain（evaluate） | 600+ |
| M46 | ✅ | CDP DOM domain（querySelector + getOuterHTML） | 608 |
| M47 | ✅ | CDP Network domain（getResponseBody） | 615+ |
| M48 | ✅ | Puppeteer e2e 全链路打通（6 CDP fixes + eval_in_tree） | 625+ |
| M49-M56 | ✅ | 真实 SPA 爬虫 / 事件补齐 / flatten session / FrameTree / lifecycleEvent | 625+ |
| M57 | ✅ | CLI 命令 + 文档更新（GOALS/FEATURES/ROADMAP/ARCHITECTURE） | — |
| M58 | ✅ | brotli/gzip 解码 + Interceptor trait | — |
| **M59** | ✅ | **`browser fetch` CLI 爬虫命令 + extractor crate（4 格式）** | 776+ |
| M60 | ✅ | boa 0.20→0.21 升级（async/await） | 776+ |
| M61 | ✅ | fetch --smart 模式（先 SSR 后 JS） | — |
| M62 | ✅ | JS 覆盖矩阵补齐（28 项 ES6+ 入库测试） | 776+ |
| M65 | ✅ | Profile 模式（RSS + 耗时监控） | — |
| M66 | ✅ | QuickJS 引擎（默认） + 双引擎 CLI 切换 | — |
| M67 | ✅ | CDP QuickJS + 内容完整性度量（4 指标） | — |
| M70 | ✅ | HTTP API serve 命令 + 并发进程护栏 | — |
| M71 | ✅ | boa 降为 optional feature | — |

---

## 已完成里程碑详情

### M0-M6（核心管线）
- **M0** ✅ workspace 骨架 + CI
- **M1** ✅ HTTPS GET + arena DOM + html5ever + CLI + e2e
- **M2** ✅ 手写 CSS parser/selector/computed + 布局（block/inline/折行）+ ASCII 渲染器
- **M3** ✅ boa_engine JS 执行 + JS↔DOM 桥（`__setBody` 等）
- **M4** ✅ SPA 渲染（同步 fetch + 相对 URL resolve）— **项目目标达成**
- **M5** ✅ winit + softbuffer GUI + 5x7 bitmap font
- **M6** ✅ 长段落折行 / `<head>` 不渲染 / `<li>` 前缀 / 黄金对比 / 跨平台 CI

复盘见 `docs/postmortems/M1.md` ~ `M6.md`。

### M7（渲染质量 + 交互）✅
| Sub | 状态 | 内容 |
|-----|------|------|
| M7.1 | ✅ | CSS margin/padding（真实解析 + UA defaults + collapsing） |
| M7.2 | ✅ | 完整 DOM API（`__createEl`/`__appendChild`/`__qs`/...） |
| M7.4 | ✅ | 真实字体（fontdue + DejaVuSans 739KB + 中文支持） |
| M7.5 | ✅ | URL 栏 + 键盘输入 + 滚动（MouseWheel/PageUp/Down/Home/End） |
| M7.3 | 🟡 defer | 异步 JS（boa 0.20 `JsObject::call` 私有） |

### M8-M11（增强 + bug 修复）✅
- **M8** ✅ 表单交互（`__getValue`/`__setValue`/`__click`/`__submit`）
- **M9** ✅ 图片占位符渲染（`[IMG: src]`）
- **M10** ✅ 性能优化（LayoutCache `get_or_compute` + DirtyTracker）
- **M11** ✅ Bug 修复（vh/vw/vmin/vmax → Zero / rem → 16px / pt → 4/3 px）

### M12（截图 + 图像 ASCII）✅
- **M12.1** ✅ PNG screenshot（`--screenshot` flag，fontdue + png）
- **M12.3** ✅ image-ascii 子命令（image crate + 10 级灰阶 ramp）

### M13（Web Storage）✅
- **M13.1** ✅ browser-storage crate（`Rc<RefCell<HashMap>>`，6 API）
- **M13.2** ✅ `__storage*` bridges + `install_storage`
- **M13.3** ✅ localStorage/sessionStorage JS 对象 shim（Web 标准 API）
- **M13.4** ✅ e2e fixture（storage-spa.html）

---

## M14（Navigation）✅

| Sub | 状态 | 内容 |
|-----|------|------|
| M14.1 | ✅ | browser-navigation crate（HistoryStack + Location 解析） |
| M14.2 | ✅ | `__history*` / `__location*` bridges + `install_navigation` |
| M14.3 | ✅ | history/location JS 对象 shim + 接入 run_scripts |
| M14.4 | ✅ | e2e fixture（navigation-spa.html，SPA 路由 + 百度场景） |
| M14.5 | ✅ | PROGRESS.md + memory 同步 |

---

### M15（Cookie jar）✅

| Sub | 状态 | 内容 |
|-----|------|------|
| M15.1 | ✅ | browser-cookie crate（RFC 6265 子集，domain/path/secure 匹配） |
| M15.2 | ✅ | net::get_with_headers（带 Cookie 头 + 返回 Set-Cookie） |
| M15.3 | ✅ | JS fetch_sync 接入 jar（主线程读写，新线程传 String） |
| M15.4 | ✅ | cli get/render-url/open 主请求共享 jar（fetch_with_jar） |
| M15.5 | ✅ | e2e（cookie jar 跨请求会话保持） |

核心链路：主请求 Set-Cookie → jar → JS `__fetchSetBody` 带上 Cookie 头
（解决百度等登录态反爬）。

---

## M16（异步 JS）✅ — setTimeout + Promise（用 boa 自研，非 deno_core）

> 决策见 docs/decisions/0002-boa-settimeout-vs-deno-core.md：调研发现
> boa 0.20 的 JobQueue/enqueue_job/NativeFunction::call/JsFunction::call
> 全是 pub，推翻 M7.3 defer 理由。用现有 boa 自研，避免 300MB V8。

| Sub | 状态 | 内容 |
|-----|------|------|
| M16.0 | ✅ | ADR-0002 决策（调研 + deno_core spike + kill） |
| M16.1 | ✅ | browser-eventloop crate（TimerWheel 纯算法，14 tests） |
| M16.2 | ✅ | setTimeout/clearTimeout 桥 + 全局名 |
| M16.3 | ✅ | event loop 接入 run_scripts（pump_event_loop） |
| M16.4 | ✅ | Promise.then 触发（ctx.run_jobs） |
| M16.5 | ✅ | e2e fixture（timer-spa.html） |
| M16.6 | ✅ | PROGRESS + ROADMAP + ADR 引用同步 |

---

## 前瞻（M17+，已排序）

> **M16-M56 已全部完成** ✅（2025-06）
> M57 计划：`browser fetch <url>` CLI 爬虫命令（networkidle 等待 + HTML 序列化输出）

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M15 | Cookie jar（RFC 6265 子集） | ✅ |
| M16 | 异步 JS（setTimeout/Promise/setInterval，自研） | ✅ |
| M17 | XMLHttpRequest（纯 JS 原型） | ✅ |
| M18 | WebSocket（client + TLS native-tls） | ✅ |
| M42 | CDP WebSocket server + JSON-RPC framework | ✅ |
| M43 | CDP HTTP discovery endpoints | ✅ |
| M44 | CDP Page domain（navigate + captureScreenshot） | ✅ |
| M45 | CDP Runtime domain（evaluate） | ✅ |
| M46 | CDP DOM domain（querySelector + getOuterHTML） | ✅ |
| M47 | CDP Network domain（getResponseBody） | ✅ |
| M48 | Puppeteer e2e 联调 | ✅ |
| M49-M51 | CDP Target + Fetch 域 | ✅ |
| M52 | CDP Log domain（console.log） | ✅ |
| M53 | CDP Emulation domain（UA + viewport） | ✅ |
| M54 | CDP Input domain（鼠标点击） | ✅ |
| M55 | CDP Input domain（键盘输入） | ✅ |
| M56 | CDP Page domain（早期 JS 注入） | ✅ |
| **M57** | **CLI 爬虫命令**（`browser fetch <url>`） | 🚧 进行中 |

---

### M57 计划：CLI 爬虫命令（G6）

| Sub | 任务 |
|-----|------|
| M57.1 | 更新 GOALS.md — 新增 G6 + 90% 覆盖率标准 + 状态快照更新到 M56 |
| M57.2 | 更新 FEATURES.md — CLI 子命令表 + 能力清单 + CDP 域更新到 M56 |
| M57.3 | 更新 ROADMAP.md — M15-M56 全部补齐 + M57 计划 |
| M57.4 | 更新 ARCHITECTURE.md — crate 依赖图 + 数据流 + 新增 crate（cdp/ws/cookie/eventloop） |
| M57.5 | 实现 `browser fetch <url>` 子命令 — fetch+parse+JS+wait+serialize HTML+stdout |
| M57.6 | 添加 `--wait/--timeout` flags — domcontentloaded/load/networkidle/none |
| M57.7 | 端到端验证 — example.com + baidu + fixture SPA |

**实现要点**：
- 复用 CLI 现有管线（fetch → parse → JS → networkidle → serialize）
- 模块化等待策略（`wait.rs`）：4 种模式（none/domcontentloaded/load/networkidle）
- HTML 序列化器（`serializer.rs`）：`Tree → String`（CDP `DOM.getOuterHTML` 的非 CDP 版本）
- 测试覆盖：3 个 e2e（example.com + baidu + fixture SPA）+ 5 个 unit tests

---
