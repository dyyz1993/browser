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

### 四大基础原则（宪法级，详见 GOALS.md）

> 所有架构决策、功能取舍必须服从这四条。冲突时**基础原则 > 决策原则**。

1. **SPA/CSR 尽可能全覆盖** —— 项目存在的理由。遇到渲染不了的 SPA 走自愈循环补 API。
   **⚠️ 绝对禁止用 SSR 兜底（`--no-js`）当 CSR 问题的借口。** 这个项目的核心价值就是
   CSR——如果只需要 SSR，直接用 curl 就够了。遇到 CSR 渲染失败：定位根因 → 补 API →
   提 PR 给 boa，而不是说「走 --no-js 吧」。`--no-js` 只对"明确是静态站"的场景用。
2. **自研优先** —— 架构尽可能手写，不用别人的库（白名单例外见 G4）
3. **低内存、快运行** —— 核心卖点（对标 Chrome：内存 1/11、速度 6 倍）。任何改动监控基线
4. **反爬不重点处理** —— 不做指纹伪造/验证码，但基础请求能力（brotli/cookie/UA）要做

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
| `image-ascii <file>` | 图片转 ASCII |
| `open <url>` | GUI 窗口（需显示器，headless 用 `--check`） |
| `fetch <url>` | **核心**：fetch + JS + 提取（markdown/html/text/links） |

> **M66 双引擎**：所有 JS 相关命令支持 `--js-engine quickjs`（默认）或 `--js-engine boa`。
> QuickJS 是 ES2020 完整引擎（rquickjs 0.12），速度/内存全面优于 boa。

---

## 三、JS 引擎双后端（M66）

### 引擎对比

| 维度 | QuickJS（默认） | boa（备选） | Chrome（对标） |
|------|:-:|:-:|:-:|
| ES 兼容 | ES2020 完整 | 部分（跑不动 React） | ES2024 |
| 速度 | ⭐ 最快 | 慢 | 中等 |
| 内存 | ⭐ 21MB 中位数 | 42MB | 270MB |
| react.dev | **91% 覆盖率** | 0.7% | 100% |
| 截图 | ✅ | ✅ | ✅ |

### 切换引擎

```bash
# 默认 QuickJS
browser fetch https://nuxt.com/

# 切换 boa
browser fetch https://nuxt.com/ --js-engine boa
```

### CSR 渲染对标 Chrome 测试方法

```bash
# 1. 基本验证（文本/Markdown/HTML）
browser fetch https://nuxt.com/ --format text
browser fetch https://nuxt.com/ --format markdown

# 2. ASCII 画面渲染
browser render-url https://svelte.dev/ --width 80

# 3. PNG 截图
browser render-url https://svelte.dev/ --screenshot out.png

# 4. 多站对比（QuickJS vs boa）
for u in "https://nuxt.com/" "https://svelte.dev/"; do
  q=$(browser fetch "$u" --format text 2>/dev/null | wc -c)
  b=$(browser fetch "$u" --format text --js-engine boa 2>/dev/null | wc -c)
  echo "$u: QJS=$q boa=$b"
done

# 5. 对标 Chrome
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
"$CHROME" --headless=new --virtual-time-budget=8000 --dump-dom "$url" 2>/dev/null | \
  python3 -c "import sys,re;h=sys.stdin.read();h=re.sub(r'<script[^>]*>.*?</script>','',h,flags=re.DOTALL);t=re.sub(r'<[^>]+>',' ',h);print(len(re.sub(r'\s+',' ',t).strip()))"

# 6. 自动化基准（12 站全维度）
bash tests/benchmarks/csr_benchmark.sh

# 7. JS 错误诊断
browser fetch https://react.dev/ --format text 2>&1 >/dev/null | \
  grep -oE "message=[^|]*" | sort | uniq -c | sort -rn

# 8. 底层插桩
browser fetch https://react.dev/ --format text --profile
```

> 完整对标数据见 [`docs/assessments/M66-quickjs-csr-comparison.md`](./docs/assessments/M66-quickjs-csr-comparison.md)

---

## 三、当前进度快照

> 真实最新数据以 `git log` 和实际 `cargo test` 输出为准；下面是文档记录的里程碑状态。

| 指标 | 值 |
|------|-----|
| HEAD | M66（QuickJS 双引擎：JsEngine trait + QuickJS 后端，默认引擎） |
| 当前分支 | `main` |
| 总 commits | 250+ |
| Crates | **16** 个（含 `extractor` 后置过滤器） |
| 测试 | **830+ passed**，0 clippy warnings |
| JS 引擎 | **双引擎**：QuickJS（默认，rquickjs 0.12）+ boa（`--js-engine boa`） |
| 核心目标 G1（SPA 爬虫）| ✅ |
| 截图 G2 | ✅ |
| 跨平台 G3 | ✅ |
| CSR 对标 Chrome | 8/10 站渲染成功，平均覆盖率 67%，react.dev 91% |
| JS 覆盖矩阵 | ES6+ 28 项入库测试全通过（详见 `docs/JS-COVERAGE.md`） |
| 核心目标 G1（SPA 爬虫） | ✅ 达成（M4），`browser fetch` 命令（M59）当 curl 用 |
| 截图 G2 | ✅ 达成（M12.1） |
| 跨平台 G3 | ✅ 达成 |
| CDP（G 爬虫接入） | ✅ M42–M56 完成，Puppeteer/Playwright 可连 |
| SPA 覆盖率（实测） | 有 SSR 站 80%，纯 CSR 站 39%（boa 天花板，详见 assessment） |

### 里程碑脉络

```
M0–M6   核心管线（net/dom/css/layout/render + JS）  ✅
M7–M14  渲染质量 + Storage/Navigation/Web API 子集   ✅
M15–M21 Cookie / 异步JS / XHR / networkidle /
        fetch / Cookie 持久化                        ✅
M22–M38 真实图像 / WebSocket / TLS / 中文字体 /
        JS 全局对象补齐 / flexbox+grid 子集          ✅
M42–M56 CDP server（11 个 domain，Puppeteer e2e）    ✅
M57     CLI 爬虫命令 / 文档收口                      ✅
M-cls   cls.cn CSR 兜底 + 内存自愈护栏（子进程+
        RSS 监控）                                  ✅
M58     reqwest brotli/gzip 解码（Vercel 压缩站可爬）✅
M59     browser fetch 命令 + extractor crate（4 格式）✅
M60     boa 0.20→0.21 升级（async/await 运行时落地）  ✅
M61     fetch --smart 模式（先 SSR 后 JS，快 8 倍）   ✅
M62     JS 覆盖矩阵补齐（28 项 ES6+ + 框架 API +     ✅
        事件系统 + querySelectorAll + 报错自愈循环）
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

### 遇到"JS 报错 / SPA 渲染失败 / 框架不兼容"时（自愈自循环）

**核心机制：报错驱动 → 补 API → 验证 → 更新覆盖矩阵。** 不要凭猜测补 API，
**用真实站点的报错驱动**。这套循环已验证有效（M62 把 CSR 平均分从 29→39）。

**自愈循环五步**（每次遇到新报错都走一遍）：

1. **采集报错** —— 跑 `browser fetch <url> --format markdown`，抓 stderr 里的
   `[js]` 报错。按频率排序（`grep -oE "message=..." | sort | uniq -c | sort -rn`）。
   批量采集多个框架站点（React/Vue/Angular），找高频缺口。
2. **定位根因** —— 报错信息反查缺什么。常见模式：
   - `X is not defined` → 缺全局对象/构造器（如 `MutationObserver`/`CustomEvent`）
   - `not a callable function` → 某函数/方法未定义（如 `matchMedia`/`ga`）
   - `React error #299` / 框架特定错误码 → 查官方文档，通常是 DOM 检查属性缺失
     （如 `nodeType`/`Node.ELEMENT_NODE` 常量）
   - `[compat] X threw` → compat_shim 包装问题（boa 0.21 原生支持的应移除包装）
3. **补 API + 加测试** —— 在对应 shim（compat_shim/document_shim/element_shim/
   navigation_shim）补缺失 API。**同时加入库测试**（`integration_js_features.rs`），
   用最小用例固化"补了什么、期望什么行为"。补的类型：
   - 全局构造器（Event/CustomEvent/MutationObserver）→ compat_shim
   - document/element 方法 → document_shim/element_shim
   - location/window 属性 → navigation_shim/window_shim
   - boa 0.21 原生支持的 → **移除 compat_shim 有害包装**（而非新增）
4. **验证提升** —— 重跑 `tests/benchmarks/csr_compare.sh`（纯 CSR 站严格对比），
   看评分/报错数是否改善。基线监控：测试数 + 二进制大小（补 API 不应让二进制涨）。
5. **更新覆盖矩阵** —— 更新 [`docs/JS-COVERAGE.md`](./docs/JS-COVERAGE.md) 的状态列
   （❓→✅），同步 FEATURES.md。这是**单一事实来源**，不更新矩阵等于白补。

**约束**（避免无脑补 API）：
- **爬虫够用原则**：事件系统/动画/MediaQuery 不需要真实现，no-op 或桩即可（存回调
  但不触发）。框架初始化不报错就行，不需要真交互。
- **纯 JS polyfill 优先**：补 API 用纯 JS（compat_shim 里的 JS 字符串），不引 Rust
  依赖。这保证二进制不涨（M62 补了 10+ API，14MB 纹丝不动）。
- **boa 天花板认知**：纯 CSR 无 SSR 兜底的站（bark/vue-playground）boa 跑不动是引擎
  限制，不是缺 API。这类站不硬刚，走 `--no-js` 兜底或标注需 Chrome。
- **不追 100%**：base.js/lodash `_.template` 内部的深层报错，如果不影响核心功能
  （内容仍渲染出来），停止深挖（边际收益递减）。

**已有的 CSR 测试集与对比工具**：
- `tests/benchmarks/csr_compare.sh` —— 纯 CSR 站严格对比（多维评分：内容覆盖率 +
  错误惩罚 + 噪声）
- `tests/benchmarks/spa_compare.sh` —— 全站对比（含 SSR，`--smart` 模式）
- `tests/benchmarks/coverage_survey.sh` —— curl/我们/Chrome 三方覆盖面
- `crates/cli/tests/integration_js_features.rs` —— **30 项 JS 特性入库回归测试**
- `crates/cli/tests/integration_spa_patterns.rs` —— 5 种 SPA 模式 fixture 测试

> 详见 [`docs/JS-COVERAGE.md`](./docs/JS-COVERAGE.md)（覆盖矩阵 + 补齐计划）+
> [`docs/assessments/M62-csr-strict-comparison.md`](./docs/assessments/M62-csr-strict-comparison.md）。

### QuickJS 引擎的 CSR 自愈循环（M66）

QuickJS（默认引擎）的报错驱动补 API 循环和 boa 类似，但有以下差异：

1. **采集报错** —— `browser fetch <url> --format text 2>&1 | grep "message="`
   - QuickJS 错误格式：`[js] [quickjs] Error: <message>`
2. **定位根因** —— QuickJS 的 ES2020 支持比 boa 完整，多数错误是缺 Web API
   - `X is not defined` → 在 QuickJS shim 补（`QUICKJS_GLOBAL_SHIM` 等常量）
   - `not a function` → Element/document 原型缺方法
3. **补 API** —— 在 `scripts.rs` 的 QuickJS shim 常量里补（纯 JS，不引 Rust 依赖）
4. **GC 安全** —— 补的 API 不能存 JS 对象引用到全局变量
   （QuickJS GC 在 runtime drop 时会 assertion 检查泄漏对象）
5. **验证** —— `browser fetch <url> --format text | wc -c` 看字符数是否提升

> 完整对标报告见 [`docs/assessments/M66-quickjs-csr-comparison.md`](./docs/assessments/M66-quickjs-csr-comparison.md)。

### ⚠️ 用户重复强调的事项必须沉淀到本文件

**当用户重复强调某件事（说了 2 次以上），Agent 必须将该事项写入 AGENTS.md 对应章节。**
如果内容太长，用大纲方式（标题 + 1-2 句摘要 + 链接到详细文档）。

**目的**：防止会话上下文丢失后，后续 Agent 忘记用户的关键要求。

**已沉淀的用户强调事项**：
- 「禁止用 SSR 兜底当 CSR 问题的借口」→ 第一章基础原则 1 + 第八章第 10 条
- 「JS 报错走自愈循环，不凭猜测补 API」→ 第六章自愈循环
- 「所有改动必须有例子、有引用、有测试」→ 第六章工作流程第 4 步
- 「测试方法和步骤必须写入 AGENTS.md」→ 第三章 QuickJS 测试方法
- 「对标 Chrome 是渲染质量的最终标准」→ 第三章 + M66 对标报告

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
| [`docs/assessments/`](./docs/assessments/) | 真实站点评估（M-cls/M40/M48/M60/M62/**M66**） | 看真实场景分析时 |
| [`docs/plans/`](./docs/plans/) | 实现计划（M-cls/M49-M57/M7/M60） | 看实施步骤时 |
| ⭐ [`docs/JS-COVERAGE.md`](./docs/JS-COVERAGE.md) | **JS 能力覆盖矩阵**（ES 特性 + Web API + 实测状态） | 补 JS API 前**必读**（单一事实来源） |

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
9. **JS 报错走自愈循环**（见第六章）：报错驱动 → 补 API → 加测试 → 验证 → 更新
   `docs/JS-COVERAGE.md`。**不凭猜测补 API，用真实报错驱动。** 补完必须入库测试 +
   更新覆盖矩阵，否则等于白补。
10. **boa 天花板不硬刚**：纯 CSR 无 SSR 兜底的站 boa 跑不动是引擎限制。走 `--no-js`
    兜底或标注需 Chrome，不要无限投入补 API（边际收益递减）。
11. **纯 JS polyfill 优先**：补 Web API 用 compat_shim 的 JS 字符串，不引 Rust 依赖。
    保证二进制不涨（M62 补了 15+ API，14MB 纹丝不动）。

---

*维护者：当本文档与 GOALS.md/FEATURES.md/ARCHITECTURE.md 冲突时，修正本文档。*
