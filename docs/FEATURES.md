# FEATURES — 能力清单

> 本文档列出浏览器当前已实现的功能。每项都标注里程碑来源和验收方式。
> 冲突时以 [GOALS.md](./GOALS.md) 为准。

---

## CLI 子命令（8 个）

| 子命令 | 功能 | 来源 |
|--------|------|------|
| `get <url>` | HTTPS GET，打印解析后的 DOM 树 | M1 |
| `parse <file>` | 解析本地 HTML，打印 DOM 树 | M1 |
| `render-file <file>` | 解析 + 布局 + ASCII 渲染（**不执行 JS**） | M2 |
| `render-script <file>` | 解析 + **执行 `<script>`** + ASCII 渲染 | M3 |
| `render-url <url>` | fetch + parse + 执行 JS + 渲染（端到端 SPA） | M4 |
| `open <url>` | fetch + 渲染 + GUI 窗口显示 | M5 |
| `image-ascii <file>` | PNG/JPG → ASCII art | M12.3 |

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
- ✅ UA 默认样式（`<body>` margin 8px 等）
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
- ✅ boa_engine 0.20（嵌入式 JS 引擎）
- ⚠️ ES6+ 异步（Promise/setTimeout/async-await）**defer**——boa 0.20 限制

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

> **注意**：`length`/`state`/`href` 等用**方法**形式（`localStorage.length()`）
> 而非属性，因为 boa getter API 复杂。爬虫 JS 兼容时需注意。

---

## Web API 支持矩阵

| API | 状态 | 里程碑 |
|-----|------|--------|
| DOM Core（CRUD） | ✅ | M3, M7.2, M8 |
| `fetch`（同步） | ✅ | M4 |
| `localStorage`/`sessionStorage` | ✅ | M13 |
| `history`/`location` | ✅ | M14 |
| `XMLHttpRequest` | ❌ | 未实现 |
| `setTimeout`/`Promise` | ❌ defer | M7.3（切 deno_core 后） |
| WebSocket | ❌ | 未实现 |
| 表单提交（GET/POST） | ⚠️ 部分 | M8（仅本地交互，不发网络） |

---

## 已知局限（明示，避免误用）

1. **异步 JS**：`setTimeout(fn, 0)` / `Promise.then` 会抛 ReferenceError。
   SPA 必须在主 JS 流中完成 DOM 操作。
2. **强反爬站点**：百度等返回 `location.replace` JS 反爬页，需 location 支持
   + Cookie jar（location 已支持 M14，Cookie 未实现）。
3. **GUI 需要显示器**：`open` 子命令在 headless/SSH 无 X 转发时会失败。
   用 `--check` flag 或 `render-url` 代替。
4. **真实图像渲染**：GUI 只显示 `[IMG: src]` 占位符（CLI 用 image-ascii）。
5. **JS 对象属性**：`length`/`state`/`href` 是方法形式。
