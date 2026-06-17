# AGENTS.md — AI Agent 工作指南

> 本文件是给所有 AI coding agent（Claude Code / Cursor / Codex / Copilot / ZCode 等）的
> **速查入口 + 行为准则**。读这一份就能开始干活，深入细节再看 `docs/`。
>
> **权威性**：本文档是项目文档的导航与纪律摘要，不替代单一事实来源。
> 冲突优先级：**GOALS.md > FEATURES.md > ARCHITECTURE.md > AGENTS.md > 其他**。
> 若本文档与 GOALS.md 冲突，以 GOALS.md 为准，并请顺手修正本文档。

---

## 一、项目宗旨（North Star）

**用 Rust 手写一个"基本能用"的爬虫渲染浏览器。** 通过造轮子深入理解浏览器原理，
同时产出一个能处理 **SPA（单页应用）** 的可控、低依赖渲染引擎。

### 我们要做的（In Scope）

| # | 能力 | 说明 |
|---|------|------|
| ✅ | **SPA 渲染** | fetch → 执行 `<script>` → JS 改 DOM → 渲染。这是项目存在的理由 |
| ✅ | **CDP 连接** | Chrome DevTools Protocol server，让 Puppeteer/Playwright 能驱动它（M42+） |
| ✅ | **JS 执行 + Web API 子集** | boa 引擎 + DOM/XHR/fetch/WS/Storage/Nav/Timer 桥 |
| ✅ | **爬虫友好输出** | ASCII 文本 / PNG 截图 / 序列化 HTML |
| ✅ | **跨平台** | macOS / Linux / Windows 三平台 CI 全绿 |

### 我们明确不做（Out of Scope —— 别花时间在这些上面）

> 这些对 SPA 爬取**无价值或价值极低**。遇到相关需求，先问"这能让更多 SPA 被爬取吗？"
> 不能 → 直接拒绝，避免 scope creep。

| ❌ 不做 | 原因 |
|--------|------|
| **风控 / 反爬对抗** | **本项目的定位是"基本能用"的渲染引擎，不投入指纹伪造/验证码/行为模拟等对抗。** 反爬太强的站点（如需签名/验证码）走 SSR 兜底或标注为已知局限 |
| **WebRTC** | P2P 音视频，与爬虫无关 |
| **Service Worker** | 离线缓存，爬虫不需要 |
| **1:1 像素级渲染** | **不追求和 Chrome 视觉一致。** 渲染只要能让爬虫读出文本/结构即可（ASCII + 近似布局就够） |
| **WebGL / WebGPU / WebAudio / 视频解码** | 媒体/3D，爬虫不关心 |
| **CSS grid / table / float 完整布局** | 已有 flexbox 子集（M32/M33），复杂布局非目标 |
| **多进程浏览器架构** | 单进程优先，低内存优先（JS 渲染走子进程仅限内存护栏，非沙箱架构） |
| **完整 ES6+ 异步语义** | boa 0.20 限制，async/await 不支持；setTimeout/Promise 已自研（M16） |
| **风控类 CDP 增强** | 不为绕过检测做 canvas 指纹/WebGL 伪造等 |

### 决策三原则（"要不要做 X"有争议时）

1. **爬虫价值优先** —— X 能让更多 SPA 被正确爬取吗？否 → 跳过。
2. **学习价值次之** —— X 能加深对浏览器原理的理解吗？否 → 跳过。
3. **复杂度门槛** —— 实现成本超过收益吗？是 → defer。
4. **自研优先** —— 能用 < 1000 行手写吗？是 → 手写；否则评估白名单 crate。
5. **不破坏既有** —— 会让现有测试变红吗？是 → 不做或先重构。

> 完整目标/非目标见 [`docs/GOALS.md`](./docs/GOALS.md)。

---

## 二、快速上手（Agent 必读三步）

### 1. 工程门禁（每次 commit 前强制，缺一不可）

```bash
cargo fmt --all -- --check                              # 格式 0 diff
cargo clippy --workspace --all-targets -- -D warnings   # 0 warning（RUSTFLAGS=-D warnings）
cargo test --workspace --no-fail-fast                   # 全绿
```

**红线**：测试未全绿时禁止 commit；失败用 `git revert`，禁止 `--amend`。

### 2. 构建 & 运行

```bash
cargo build --release -p browser-cli
# binary: ./target/release/browser

# 端到端 SPA 爬取（核心用例）
./target/release/browser render-url https://example.com/ --width 80

# 启动 CDP server（Puppeteer/Playwright 可连）
./target/release/browser cdp --port 9222
```

### 3. 子命令速查

| 命令 | 用途 |
|------|------|
| `render-url <url>` | **核心**：fetch + JS + 渲染（端到端 SPA） |
| `spa <url>` | 爬虫友好：等待策略 + 输出完整 HTML |
| `cdp --port N` | CDP server（Puppeteer/Playwright 兼容） |
| `get / parse` | 仅取 DOM 树 |
| `render-file / render-script` | 本地 HTML 渲染（后者执行 JS） |
| `screenshot <url>` | 输出 PNG 截图（支持 `--max-height`） |
| `image-ascii <file>` | 图片转 ASCII |
| `open <url>` | GUI 窗口（需显示器，headless 用 `--check`） |

---

## 三、当前进度快照

> 真实最新数据以 `git log` 和实际 `cargo test` 输出为准；下面是文档记录的里程碑状态。

| 指标 | 值 |
|------|-----|
| HEAD | `baa42e3`（M-cls SPA 渲染 + 内存自愈护栏） |
| 当前分支 | `feat/m-cls-spa`（默认 PR 分支为 `main`） |
| 总 commits | 168+ |
| Crates | **15** 个 |
| 测试 | 682 passed（M-cls 后），0 clippy warnings |
| 核心目标 G1（SPA 爬虫） | ✅ 达成（M4） |
| 截图 G2 | ✅ 达成（M12.1） |
| 跨平台 G3 | ✅ 达成 |
| CDP（G 爬虫接入） | ✅ M42–M56 完成，Puppeteer/Playwright 可连 |

### 里程碑脉络

```
M0–M6   核心管线（net/dom/css/layout/render + JS）  ✅
M7–M14  渲染质量 + Storage/Navigation/Web API 子集   ✅
M15–M21 Cookie / 异步JS / XHR / networkidle /
        fetch / Cookie 持久化                        ✅
M22–M38 真实图像 / WebSocket / TLS / 中文字体 /
        JS 全局对象补齐 / flexbox+grid 子集          ✅
M42–M56 CDP server（11 个 domain，Puppeteer e2e）    ✅
M57+    CLI 爬虫命令 / 文档收口                      🚧
M-cls   cls.cn CSR 兜底 + 内存自愈护栏（子进程+
        RSS 监控）                                  ✅
```

> 详细路线图见 [`docs/ROADMAP.md`](./docs/ROADMAP.md)；活跃日志见 [`PROGRESS.md`](./PROGRESS.md)。

---

## 四、架构概览

```
                    ┌─────────────┐
                    │  cli (bin)  │  ← 唯一可执行入口
                    └──┬──────────┘
        ┌──────────┬───┼────┬─────────┬────────┐
        ▼          ▼   ▼    ▼         ▼        ▼
     render    gui  cdp  page    js-runtime  storage/navigation
        │       │    │           (嵌入 boa)   (后端 HashMap)
        │       │    │                │
     layout    font  ws           ┌───┴───┐
        │      (共享)              │       │
     css-engine                   dom     net → (hyper + native-tls)
        │                        (arena)       ↑
     html-parser                              cookie / eventloop
```

### 15 个 Crate

| Crate | 职责 |
|-------|------|
| `net` | HTTP/HTTPS（native-tls，兼容真实大站） |
| `dom` | arena DOM（`Vec<Node>` + `NodeId`，ADR-0001） |
| `html-parser` | html5ever → dom::Tree |
| `css-engine` | CSS 解析/选择器/computed |
| `layout` | 布局树（block/inline + 折行 + flex/grid 子集） |
| `render` | 布局树 → ASCII + 字体光栅化（fontdue） |
| `js-runtime` | boa 嵌入 + 所有 `__*` 桥（DOM/XHR/fetch/WS/Storage/Nav/Timer） |
| `storage` / `navigation` / `cookie` | localStorage/history/cookie jar 后端 |
| `eventloop` | setTimeout/WS/Network 事件循环（自研，非 deno_core） |
| `ws` | 手写 RFC 6455 WebSocket client + TLS |
| `cdp` | CDP server（11 domain，Puppeteer/Playwright 兼容） |
| `gui` | winit + softbuffer 窗口 |
| `page` | Page/Frame 编排（stub，待完善） |
| `cli` | 子命令分发 + screenshot + sandbox（内存护栏） |

### 三大架构模式（动手前必看）

1. **Arena DOM** —— 所有 DOM 访问走 `tree.get(NodeId)`，不用 `Rc<RefCell>`。JS 侧只持有 `NodeId`（f64）。
2. **Thread-local Slot 模式** —— boa 的 `NativeFunction` 不支持闭包捕获，JS 桥通过 `thread_local!`（`CURRENT_TREE`/`CURRENT_STORAGE`/`CURRENT_NAV`/`CURRENT_WS`/`CURRENT_TIMER`/`CURRENT_NETWORK`）共享状态。`TreeGuard::drop` 清理所有 slot。
3. **JS 桥两层架构** —— Web 标准 API（`localStorage`/`fetch`/`history`/`XHR`/`WebSocket`）是 **shim 全局对象**，内部 eval 调 `__*` 底层桥函数，桥再调纯 Rust crate。

> 完整依赖图/数据流见 [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md)。

---

## 五、关键约束（动手前必知）

### 代码纪律

- **`#![forbid(unsafe_code)]` 全 workspace 强制**。唯一例外：`cli` crate 的 `sandbox.rs` 用 `rlimit`（子进程 RLIMIT_AS 内存护栏，M-cls）。unsafe 只能出现在 cli crate。
- **`SharedTree = Rc<RefCell<Tree>>` 是 `!Send`** → 不能跨线程共享 DOM，跨线程只能传 `String`（如 JS fetch 在新线程跑，结果以 String 回主线程）。
- **禁止 `.unwrap()`/`.expect()`**（main 启动期可 expect）。
- **禁止 `Box<dyn Any>` 滥用**，用具体 enum。
- 文件 > 100 行考虑按职责拆分。

### 依赖白名单（自研优先 G4）

只允许"底层解析/IO 库过于复杂（≥10 万行）"时引入：`html5ever` / `hyper`+`native-tls` / `tokio` / `clap` / `boa_engine` / `winit`+`softbuffer` / `url` / `fontdue` / `png` / `image`。
**新增依赖必须：登记到 GOALS.md 白名单 + 写 ADR + commit。禁止偷偷加依赖。**

### 提交规范（Conventional Commits）

```
<type>(<scope>): M<号> <subject>   # type: feat/fix/test/docs/refactor/chore/perf
                                     # scope: crate 名
                                     # subject: 祈使句、小写、≤72 字符、含里程碑号
```

> 完整规范见 [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md)。

---

## 六、Agent 工作流程

### 收到"实现功能 X"时

1. **先查 GOALS.md** —— X 在目标里吗？在非目标里吗？（多数被拒需求在非目标表里）
2. **看 ROADMAP.md / PROGRESS.md** —— 是否已实现？属于哪个里程碑？
3. **看 ARCHITECTURE.md + 对应 crate** —— 该往哪个 crate 加？依赖方向对不对？
4. **写测试先行**（三级验收）：
   - L1 单元测试（`src/*.rs` 内 `mod tests`）
   - L2 集成测试（`tests/integration_*.rs` + fixture）
   - L3 真实命令验证（跑 CLI，检查 stdout 零报错）
5. **过门禁**：fmt + clippy(-D warnings) + test 全绿。
6. **更新文档**：PROGRESS.md（必更）+ 涉及能力则改 FEATURES.md + 架构变化则改 ARCHITECTURE.md。
7. **Conventional Commit**：一个功能 = 一个 commit + 可验证验收。

### 收到"修 bug Y"时

1. 先复现 —— 用 `tests/fixtures/` 下的真实 fixture 或最小复现 HTML。
2. 定位到 crate（看架构依赖方向）。
3. 加回归测试（先红后绿）。
4. 过门禁 + 更新 PROGRESS.md（`fix(...)` commit）。

### 遇到"boa 引擎内存爆涨 / 复杂 bundle 跑不动"时

**不要硬刚 JS。** 参考 M-cls 决策：
1. 危险 JS 走子进程（`cli/sandbox.rs`），父进程 RSS 监控超 ~400MB 立即 SIGKILL。
2. 子进程被杀 → **不重跑 JS**，改 `run_js=false` 渲染静态壳。
3. 找 SSR/移动版数据源兜底（host→fetcher 注册表 `spa_fallback`）。
4. 收紧 boa `RuntimeLimits`（loop 40K / stack 4096 / recursion 256）。

> 详见 [`docs/assessments/M-cls-spa.md`](./docs/assessments/M-cls-spa.md)。

---

## 七、文档地图（深入细节看这些）

| 文档 | 内容 | 何时读 |
|------|------|--------|
| ⭐ [`docs/GOALS.md`](./docs/GOALS.md) | **NORTH STAR**：目标/非目标/验收 | **每次开工前**（判断要不要做） |
| [`docs/FEATURES.md`](./docs/FEATURES.md) | 能力清单 + Web API 矩阵 + 已知局限 | 实现新功能前（避免重复造） |
| [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) | crate 依赖图 + 数据流 | 改架构前 |
| [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) | 提交规范 + 自研边界 + 依赖白名单 | commit 前 |
| [`docs/DIRECTORY.md`](./docs/DIRECTORY.md) | 目录结构 + crate 职责 + 依赖方向 | 找文件位置时 |
| [`docs/TESTING.md`](./docs/TESTING.md) | 三级测试分层 + 验收命令 | 写测试时 |
| [`docs/ROADMAP.md`](./docs/ROADMAP.md) | 里程碑路线图（M0–M57+） | 看进度时 |
| [`PROGRESS.md`](./PROGRESS.md) | 活跃日志（每 commit 更新） | 看最近变更时 |
| [`docs/decisions/`](./docs/decisions/) | ADR（架构决策记录 0001–0004） | 理解"为什么这么选"时 |
| [`docs/postmortems/`](./docs/postmortems/) | 里程碑复盘（M1–M6） | 学教训时 |
| [`docs/assessments/`](./docs/assessments/) | 真实站点评估（M-cls/M40/M48/M60） | 看真实场景分析时 |
| [`docs/plans/`](./docs/plans/) | 实现计划（M-cls/M49-M57/M7） | 看实施步骤时 |

### ADR 速查

| ADR | 决策 |
|-----|------|
| 0001 | DOM 用 arena，不用 `Rc<RefCell>` |
| 0002 | 用 boa 自建 setTimeout/Promise，不引 deno_core（省 300MB V8） |
| 0003 | TLS 用 native-tls（兼容百度 CDN），不用 rustls |
| 0004 | `render::font` 共享模块，单一真相源 |

---

## 八、给 Agent 的硬性约定

1. **开工前必读 GOALS.md**，确认任务在 scope 内。不在 scope 的，先和用户确认而非自行扩范围。
2. **每个 commit 过三门禁**（fmt / clippy / test），否则不要提交。
3. **不破坏既有测试**：改完跑全量 `cargo test --workspace`，红了必须修。
4. **新增依赖先登记**：白名单 + ADR，否则等于违反 G4。
5. **unsafe 只进 cli crate**，且必须是内存护栏这类不可替代场景。
6. **风控/反爬/指纹伪造类需求一律拒绝** —— 这是宗旨层面的边界，不是技术问题。
7. **文档与代码同步**：改了功能就改 FEATURES，改了架构就改 ARCHITECTURE，每 commit 更新 PROGRESS。
8. **优先复用现有管线**（fetch→parse→JS→render→serialize），而不是另起炉灶。

---

*维护者：当本文档与 GOALS.md/FEATURES.md/ARCHITECTURE.md 冲突时，修正本文档。*
