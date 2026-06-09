# Architecture — 架构与数据流

> 本文档描述 crate 依赖、数据流、职责边界。冲突时以 [GOALS.md](./GOALS.md) 为准。

---

## Crate 依赖图（M56 真实状态）

```
                              ┌─────────────┐
                              │  cli (bin)  │
                              └──┬──────────┘
                                 │
        ┌────────────┬───────────┼───────────┬─────────────┬──────────┐
        ▼            ▼           ▼           ▼             ▼          ▼
   ┌─────────┐ ┌──────────┐ ┌────────┐ ┌──────────┐  ┌─────────┐ ┌──────┐
   │  page   │ │  render  │ │  gui   │ │  image   │  │storage  │ │ cdp  │
   │ (stub)  │ │          │ │        │ │ (png/    │  │ (Rc<    │ │      │
   └────┬────┘ └────┬─────┘ └───┬────┘ │ fontdue) │  │ HashMap>)│ └──┬───┘
        │           │           │       └──────────┘  └─────────┘    │
        │           │           │                                   │
        │     ┌─────▼─────┐     │                                   │
        │     │  layout   │     │                                   │
        │     └─────┬─────┘     │                                   │
        │           │           │                                   │
        │     ┌─────▼─────┐     │                                   │
        │     │css-engine │     │                                   │
        │     └─────┬─────┘     │                                   │
        │           │           │                                   │
        ▼           ▼           ▼                                   │
   ┌──────────────────────────────┐                            ┌────┴────┐
   │         html-parser          │                            │   ws    │
   └──────────────┬───────────────┘                            └────┬────┘
                  │                                            │
                  ▼                                            │
   ┌──────────────────────────────┐                            │
   │       js-runtime             │ ◄── 嵌入 boa_engine          │
   │   (shim: DOM/XHR/fetch/WS/   │     + thread-local slot      │
   │    Storage/Nav/Timer/Image)  │     (Tree/Storage/Nav/WS   │
   │                              │      + Timer + Network)     │
   └──┬────────────────┬──────────┘                            │
      │                │                                        │
      ▼                ▼                                        ▼
   ┌────────┐    ┌──────────────┐                       ┌──────────┐
   │  dom   │    │   net        │                       │  cookie  │
   │(arena) │    │(hyper+native │                       │ (Rc<     │
   └────────┘    │  tls)       │                       │  HashMap>)│
                 └──────┬───────┘                       └──────────┘
                        │                             ┌──────────────┐
                        ▼                             │  eventloop   │
                 ┌──────────────┐                    │  (Timer/WS/  │
                 │  navigation  │                    │   Network)   │
                 │ (History/    │                    └──────────────┘
                 │  Location)   │
                 └──────────────┘
```

**新增 crate（M16-M56）**：
- `browser-cdp` — Chrome DevTools Protocol server（WebSocket + HTTP discovery）
- `browser-ws` — WebSocket client + TLS（native-tls）
- `browser-cookie` — Cookie jar（RFC 6265 子集）
- `browser-eventloop` — 异步事件循环（Timer/WS/Network）

---

## 数据流

### SPA 渲染完整管线（M4 + M13 + M14）

```
URL
 │
 ▼
page::Page::new()  →  net::get(url)  →  HTML bytes
                                         │
                                         ▼
                              html_parser::parse(html)  →  dom::Tree
                                         │
                                         ▼
              js_runtime::run_scripts_with_base(tree, base_url)
                                         │
                    ┌────────────────────┴────────────────────┐
                    ▼                                          ▼
        install thread-local slots                       execute <script>
        - CURRENT_TREE (SharedTree)                          │
        - CURRENT_STORAGE (StorageHandle)                     │
        - CURRENT_NAV (NavigationHandle)                      │
        - BASE_URL                                            │
                                                             │
                    JS 通过 __* 桥调用 ◄──────────────────────┘
                    ├─ DOM: __setBody/__createEl/...
                    ├─ fetch: __fetchSetBody/AppendBody
                    │    └─▶ net::get(api_url) ─▶ json/HTML
                    ├─ storage: __storageGet/Set/...
                    │    └─▶ browser_storage::HashMap
                    └─ history: __historyPush/...
                         └─▶ browser_navigation::HistoryStack
                                         │
                                         ▼
                              (Tree 被 JS 修改后)
                                         │
              ┌──────────────────────────┴──────────────────────┐
              ▼                                                  ▼
    css_engine::compute_styles(tree, sheet)         layout::construct_layout_tree
              │                                                  │
              └──────────────────┬───────────────────────────────┘
                                 ▼
                         layout::layout_tree
                                 │
                                 ▼
                    render::render_ascii(layout, width)
                                 │
                    ┌────────────┴────────────┐
                    ▼                         ▼
              stdout (文本)         screenshot::render_text_to_png
                                       │
                                       ▼
                                    PNG 文件
```

---

## Crate 职责边界

| Crate | LOC | 职责 | 不负责 |
|-------|-----|------|--------|
| `net` | 272 | HTTP/HTTPS GET、User-Agent、timeout（M40 修复） | HTML 解析 |
| `dom` | 747 | arena DOM（`Vec<Node>` + `NodeId`）、Node API、事件、遍历 | 解析、JS |
| `html-parser` | 429 | HTML 字符串 → `dom::Tree`（html5ever 封装） | DOM 内部结构 |
| `css-engine` | 1418 | CSS 解析、选择器匹配、计算样式、长度单位 | 布局 |
| `layout` | 1415 | DOM + 样式 → 布局树（block/inline + 折行 + margin） | 绘制 |
| `render` | 246 | 布局树 → ASCII 文本 | GUI 窗口、PNG |
| `js-runtime` | 2295 | boa 嵌入、JS↔DOM/storage/nav/WS/XHR/fetch/Image/Timer 桥 | JS 引擎本身 |
| `page` | 17 | Page/Frame 编排（stub，待完善） | 具体子能力 |
| `cli` | 1686 | 子命令分发、screenshot、image-ascii、CDP client | 业务逻辑 |
| `gui` | 528 | winit + softbuffer 窗口、URL 栏、滚动、字体 | 渲染算法 |
| `storage` | 138 | localStorage/sessionStorage 后端（HashMap） | JS 桥 |
| `navigation` | 297 | history/location 后端（HistoryStack） | JS 桥 |
| **`ws`** | **474** | **WebSocket client + TLS（native-tls）** | **HTTP** |
| **`cookie`** | **355** | **Cookie jar（RFC 6265 子集）** | **网络** |
| **`eventloop`** | **497** | **Timer/WS/Network 事件循环** | **JS 逻辑** |
| **`cdp`** | **1300+** | **CDP server（WebSocket + HTTP discovery + 11 domains）** | **前端渲染** |

---

## 关键架构模式

### 1. Arena DOM（ADR-0001）
所有 DOM 访问通过 `tree.get(NodeId)`，不用 `Rc<RefCell>`。
JS 侧只持有 `NodeId`（一个 `f64`），跨边界简单。

### 2. Thread-local Slot 模式（JS 桥核心）
boa 0.20 的 `NativeFunction` 不支持闭包捕获环境，所以用 thread-local：
```rust
thread_local! {
    static CURRENT_TREE: RefCell<Option<SharedTree>>;
    static CURRENT_STORAGE: RefCell<Option<StorageHandle>>;
    static CURRENT_NAV: RefCell<Option<NavigationHandle>>;
    static CURRENT_WS: RefCell<Option<WsBridgeHandle>>;     // M18
    static CURRENT_TIMER: RefCell<Option<TimerBridgeHandle>>; // M30
    static CURRENT_NETWORK: RefCell<Option<NetworkBridgeHandle>>; // M47
}
```
- `install_*()` 安装 handle
- `with_tree(|t| ...)` / `with_storage(|s| ...)` / `with_navigation(|h| ...)` / `with_ws(|w| ...)` / `with_timer(|t| ...)` / `with_network(|n| ...)` 访问
- `TreeGuard::drop` 清理所有 slot（防泄漏到下一次渲染）

### 3. JS 桥两层架构
```
JS 代码
  │
  ▼
shim 全局对象（localStorage/history/location/XHR/fetch/WS/Image）← Web 标准 API
  │（内部 eval）
  ▼
__* 全局函数（bridge.rs 注册）                 ← 底层桥
  │
  ▼
browser_storage / browser_navigation / browser-ws / browser-eventloop ← 纯 Rust 实现
```

### 4. MVP 简化决策（见 GOALS.md 非目标）
- localStorage/sessionStorage 共享后端
- `length`/`state`/`href` 用方法形式（非属性）
- history.pushState 不触发真实 fetch
- 异步 JS 已支持（setTimeout/Promise/setInterval，M30）

---

## 关键决策（ADR 摘要）

| ADR | 决策 | 状态 |
|-----|------|------|
| [0001](./decisions/0001-arena-vs-refcell.md) | DOM 用 arena，不用 Rc<RefCell> | accepted |
| [0002](./decisions/0002-boa-settimeout-vs-deno-core.md) | 用 boa 自建 setTimeout/Promise，不用 deno_core | accepted |
| [0003](./decisions/0003-tls-native-tls-vs-rustls.md) | TLS 用 native-tls，不用 rustls（兼容百度 CDN） | accepted |
| [0004](./decisions/0004-render-font-shared-module.md) | render::font 共享模块，单一真相源 | accepted |

> 已实现 ADR：JS 桥两层架构、thread-local slot、shim 方法形式、M30 异步 JS 实现。
