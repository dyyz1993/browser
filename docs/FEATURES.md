# FEATURES — 能力清单

> 本文档列出浏览器当前已实现的功能。每项都标注里程碑来源和验收方式。
> 冲突时以 [GOALS.md](./GOALS.md) 为准。

---

## CLI 子命令（12 个）

| 子命令 | 功能 | 来源 |
|--------|------|------|
| `get <url>` | HTTPS GET，打印解析后的 DOM 树 | M1 |
| `parse <file>` | 解析本地 HTML，打印 DOM 树 | M1 |
| `render-file <file>` | 解析 + 布局 + ASCII 渲染（**不执行 JS**） | M2 |
| `render-script <file>` | 解析 + **执行 `<script>`** + ASCII 渲染 | M3 |
| `render-url <url>` | fetch + parse + 执行 JS + 渲染（端到端 SPA） | M4 |
| `fetch <url>` | **M59：curl 式 SPA 爬虫** — fetch+JS+等待策略+内容提取（markdown/html/text/links）。**M82 加固**：全局 60s 硬超时（不挂死）、`--json` warnings/格式尊重、data: URI 图默认丢弃（`--inline-images` 保留）、反爬壳页警告 | M59/M82 |
| `open <url>` | fetch + 渲染 + GUI 窗口显示（M81.E1 起需 `--features gui` 编译；`--check` 无需） | M5 |
| `image-ascii <file>` | PNG/JPG → ASCII art | M12.3 |
| `screenshot <url>` | fetch + JS + 布局 + 网页 PNG 截图（支持 --max-height） | M12 | 
| `cdp --port N` | 启动 Chrome DevTools Protocol server（Puppeteer/Playwright 兼容） | M42 |
| `serve` | 启动 HTTP API 服务（Fetch/GUI 模式，快速并发） | M70 |
| `spa <url>` | fetch + JS + 等待策略 + 输出完整 HTML（爬虫友好） | M57 | 

通用 flag：`--width N`（终端宽度，默认 80）、`--screenshot <path>`（输出 PNG）。

---

## 渲染管线能力

### 网络层（net crate）
- ✅ HTTPS GET（hyper + hyper-rustls）
- ✅ 真实 Chrome User-Agent（避免反爬）
- ⚠️ 跨请求 Cookie jar（未实现）
- ⚠️ HTTP 重定向跟随（部分实现）

### HTML/CSS 解析
- ✅ HTML5 解析（html5ever 封装）
- ✅ CSS 解析（cssparser + 手写）
- ✅ 选择器：tag / `.class` / `#id` / compound / `*`（后代选择器部分）
- ✅ 计算样式（DOM → 匹配规则 → computed style）

### CSS 长度单位（css-engine/src/properties.rs）
| 单位 | 支持 | 说明 |
|------|------|------|
| `px` | ✅ | 像素，按 1:1 渲染 |
| `em` | ✅ | 相对字号，向上取整 |
| `%` | ✅ | 百分比（相对父容器） |
| `rem` | ✅ | 根字号 × 16px |
| `pt` | ✅ | × 4/3 px |
| `vh/vw/vmin/vmax` | ⚠️ → Zero | ASCII 模式无 viewport，归零（M11 修复） |
| `auto` | ✅ | margin 上下文等同 Zero |

### 布局（layout crate）
- ✅ 块级布局（block，从上到下堆叠）
- ✅ 行内布局（inline + 字符级折行）
- ✅ margin / padding（真实 CSS，含 collapsing）
- ✅ UA 默认样式（css-engine/ua.rs 17 规则，M78.138：h1-h6 字号阶梯→ASCII 大写映射、
  p/ul/blockquote 间距缩进、ol 编号 `1.` / ul `•`、hr 分隔线、strong `**…**`、
  em `*…*`、pre 空白保留；页面 CSS 可覆盖——级联 UA → 页面 → inline）
- ✅ 图像占位符治理（M78.138）：URL/data-URI 不进渲染流；有尺寸的 img 输出
  `[IMG w×h]` 紧凑占位，data: 与无尺寸 img 跳过；本地文件保留 `[IMG: src]`
  （M22 真实图像 ASCII 管线不受影响）
- ⚠️ flex / grid / table / float（非目标，不做）

### 渲染（render crate + gui crate）
- ✅ ASCII 终端渲染（width × height 文本网格）
- ✅ GUI 窗口渲染（winit + softbuffer）
- ✅ 真实字体（fontdue + DejaVuSans 739KB，抗锯齿，支持中文）
- ✅ PNG 截图输出（fontdue + png）
- ✅ 图像 → ASCII art（image crate）

---

## JS 执行能力（js-runtime crate）

### 引擎
- ✅ boa_engine **0.21**（M60 升级，async/await 运行时落地，ES 一致性 ~94%）
- ✅ ES6+ 全面支持（let/const/箭头/模板/解构/class/Symbol/Map/Set/Proxy/async-await，18 项入库测试）

### JS ↔ DOM 桥（28 个 `__*` 全局函数）

**DOM 操作（M3 + M7.2 + M8）：**
| 桥 | 等价 Web API | 说明 |
|----|-------------|------|
| `__setBody(html)` | `document.body.innerHTML = ...` | 替换 body |
| `__appendBody(html)` | `document.body.insertAdjacentHTML` | 追加 body |
| `__setTitle(t)` | `document.title = ...` | 设置标题 |
| `__log(...)` | `console.log` | 打到 stderr |
| `__createEl(tag)` | `document.createElement` | 返回 NodeId |
| `__appendChild(parent, child)` | `parent.appendChild` | |
| `__setAttr(id, k, v)` | `el.setAttribute` | |
| `__getElById(id)` | `document.getElementById` | 返回 NodeId 或 -1 |
| `__qs(selector)` | `document.querySelector` | tag/.class/#id/compound |
| `__setText(id, text)` | `el.textContent = ...` | |
| `__getTag(id)` / `__getBody()` | 读取辅助 | |
| `__getValue(id)` / `__setValue(id, v)` | `el.value`（表单） | M8 |
| `__click(id)` | `el.click()` | M8 |
| `__submit(id)` | `form.submit()` | M8 |

**网络（M4）：**
| 桥 | 说明 |
|----|------|
| `__fetchSetBody(url)` | 同步 fetch → 替换 body |
| `__fetchAppendBody(url)` | 同步 fetch → 追加 body |

**Web Storage（M13，localStorage/sessionStorage 共享后端）：**
| 桥 | JS 对象方法 |
|----|-----------|
| `__storageGet(k)` | `localStorage.getItem` |
| `__storageSet(k, v)` | `localStorage.setItem` |
| `__storageRemove(k)` | `localStorage.removeItem` |
| `__storageClear()` | `localStorage.clear` |
| `__storageLen()` | `localStorage.length()`（方法形式） |
| `__storageKey(i)` | `localStorage.key(i)` |

**Navigation（M14，history + location）：**
| 桥 | JS 对象方法 |
|----|-----------|
| `__historyPush(state, title, url)` | `history.pushState` |
| `__historyReplace(state, title, url)` | `history.replaceState` |
| `__historyBack/Forward/Go(n)` | `history.back/forward/go` |
| `__historyLen/State` | `history.length/state` |
| `__locationHref/Replace/Assign` | `location.href/replace/assign` |
| `__locationParts()` | 返回 `{href,protocol,host,...}` |

### JS 全局对象（shim）
- ✅ `localStorage` + `sessionStorage`（共享后端，MVP）
- ✅ `history`（pushState/replaceState/back/forward/go）
- ✅ `location`（href/replace/assign/pathname/host/...）
- ✅ `Image`（构造器，爬虫友好不 fetch）
- ✅ `setTimeout`/`Promise`（异步执行）

> **注意**：`length`/`state`/`href` 等用**方法**形式（`localStorage.length()`）
> 而非属性，因为 boa getter API 复杂。爬虫 JS 兼容时需注意。

---

## Chrome DevTools Protocol（CDP）支持

| 域 | 方法 | 里程碑 | 说明 |
|----|------|--------|------|
| **Browser** | `getVersion` | M42 | 浏览器版本信息 |
| **Page** | `navigate` | M44 | 导航到 URL |
| | `captureScreenshot` | M44 | PNG base64 截图 |
| | `getNavigationHistory` | M44 | 导航历史 |
| **Runtime** | `evaluate` | M45 | 执行 JS 表达式 |
| | `enable/disable` | M45 | 生命周期管理 |
| **DOM** | `getDocument` | M46 | 获取文档树 |
| | `getOuterHTML` | M46 | 获取节点 HTML |
| | `querySelector` | M46 | 查询单个节点 |
| | `querySelectorAll` | M46 | 查询多个节点 |
| **Network** | `getResponseBody` | M47 | 获取响应体 |
| | `enable/disable` | M47 | 生命周期管理 |
| **Target** | `createTarget` | M49 | 创建页面目标 |
| | `activateTarget` | M49 | 激活目标 |
| | `closeTarget` | M49 | 关闭目标 |
| | `setDiscoverTargets` | M49 | 自动发现 |
| **Fetch** | `enable/disable` | M51 | 拦截启用 |
| | `continueRequest` | M51 | 继续请求 |
| | `fulfillRequest` | M51 | 模拟响应 |
| **Log** | `entryAdded` | M52 | console.log 事件 |
| | `enable/disable` | M52 | 生命周期管理 |
| **Emulation** | `setUserAgentOverride` | M53 | 覆盖 UA |
| | `setDeviceMetricsOverride` | M53 | 视口尺寸 |
| **Input** | `dispatchMouseEvent` | M54 | 鼠标点击 |
| | `dispatchKeyEvent` | M55 | 键盘输入 |
| **Page** | `addScriptToEvaluateOnNewDocument` | M56 | 早期 JS 注入 |

**服务端实现**：`browser-cdp` crate（M42），支持 HTTP discovery endpoints + WebSocket server。

---

## Web API 支持矩阵

| API | 状态 | 里程碑 |
|-----|------|--------|
| DOM Core（CRUD） | ✅ | M3, M7.2, M8 |
| `fetch`（同步） | ✅ | M4 |
| `localStorage`/`sessionStorage` | ✅ | M13 |
| `history`/`location` | ✅ | M14 |
| `XMLHttpRequest` | ✅ | M17 |
| `WebSocket` | ✅ | M18 |
| `setTimeout`/`Promise`/async-await | ✅ | M16/M30/M60 |
| `setInterval` | ✅ | M62（真实现，100 次硬上限防死循环） |
| `Image` | ✅ | M41 |
| 事件系统（Event/CustomEvent/EventTarget） | ✅ | M62（DOMContentLoaded 自动 dispatch） |
| `queueMicrotask` | ✅ | M62 |
| `atob`/`btoa`（真 Base64） | ✅ | M62 |
| 表单提交（GET/POST） | ⚠️ 部分 | M8（仅本地交互，不发网络） |

---

## 已知局限（明示，避免误用）

1. **异步 JS（M30+ 已修复）**：`setTimeout(fn, 0)` / `Promise.then` 已支持，爬虫 CLI 使用 networkidle 策略等待异步操作完成。
2. **强反爬站点**：百度等返回 `location.replace` JS 反爬页，已支持 location（M14）但 Cookie 部分实现（M32持久化）。
3. **GUI 需要显示器**：`open` 子命令在 headless/SSH 无 X 转发时会失败。用 `screenshot` 代替或使用 CDP server 模式。
4. **真实图像渲染**：GUI 只显示 `[IMG: src]` 占位符（CLI 用 image-ascii），CDP captureScreenshot 生成 PNG base64（M44）。
5. **JS 对象属性**：`length`/`state`/`href` 是方法形式（`localStorage.length()`），因为 boa getter API 复杂。爬虫 JS 兼容时需注意。
6. **JS 引擎兼容性**：boa 0.21 支持 ES6+（async/await/Proxy/Map/Set/Symbol 等全部入库测试通过）。已知局限：①某些站点 JS 触发 boa 内部 panic（fetch 命令有 catch_unwind 兜底）；②纯 CSR 无 SSR 兜底的站点需真 Chrome。详见 [JS-COVERAGE.md](./JS-COVERAGE.md)。
