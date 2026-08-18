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

### 四大基础原则 + 生命线（宪法级，详见 GOALS.md）

> 所有架构决策、功能取舍必须服从这五条。冲突时**生命线 > 基础原则 > 决策原则**。

🩸 **生命线：JS 必须能跑通** —— 项目存在的根。任何时候必须有**至少一个能执行 `<script>`、
   能跑通 SPA 的 JS 引擎**。引擎可以替换/升级（QuickJS → 跟进 Bellard 2026-06 的 42% 提速；
   甚至换实现），但**不得移除 JS 执行能力**。没有 JS 引擎 = 没有 CSR 爬取 = 项目失去存在
   理由。引擎层现状：**QuickJS（默认，rquickjs 0.12，ES2020 完整）+ boa（备选 feature，
   test262 90%+）**，双引擎通过 `trait JsEngine` 共存，5400 行 JS shim 引擎无关。

1. **SPA/CSR 尽可能全覆盖** —— 项目存在的理由。遇到渲染不了的 SPA 走自愈循环补 API。
   **⚠️ 绝对禁止用 SSR 兜底（`--no-js`）当 CSR 问题的借口。** 这个项目的核心价值就是
   CSR——如果只需要 SSR，直接用 curl 就够了。遇到 CSR 渲染失败：定位根因 → 补 API →
   提 PR 给 boa，而不是说「走 --no-js 吧」。`--no-js` 只对"明确是静态站"的场景用。
2. **自研优先** —— 架构尽可能手写，不用别人的库（白名单例外见 G4）
3. **低内存、快运行** —— 核心卖点（对标 Chrome：内存 1/11、速度 6 倍）。任何改动监控基线。
   **⚠️ 因此不引入 V8/rusty_v8**——V8 二进制 ~30MB+、运行时几百 MB，直接违背低内存卖点。
4. **反爬不重点处理** —— 不做指纹伪造/验证码，但基础请求能力（brotli/cookie/UA）要做

### 我们要做的（In Scope）

| # | 能力 | 说明 |
|---|------|------|
| ✅ | **SPA 渲染** | fetch → 执行 `<script>` → JS 改 DOM → 渲染。这是项目存在的理由 |
| ✅ | **CDP 连接** | Chrome DevTools Protocol server，让 Puppeteer/Playwright 能驱动它（M42+） |
| ✅ | **JS 执行 + Web API 子集** | **QuickJS（默认）+ boa（备选）** 双引擎 + DOM/XHR/fetch/WS/Storage/Nav/Timer 桥。详见生命线 |
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
| ESM module | ✅ HttpLoader + eval_module_with_imports | ✅ HttpModuleLoader | 原生 |
| 截图 | ✅ ASCII + PNG | ✅ | ✅ |
| TypeScript | 自动跳过（`: string`/`as Type` 检测） | N/A | N/A |

### QuickJS 关键实现要点（大纲）

- **bridge 函数**：68 个 `Function::new`，复用 `bridge.rs` 的 `qjs_bridge` 模块（调同样的 thread_local DOM 后端）
- **ESM module**：`Module::declare` + `eval` + `promise.finish`，HttpResolver/HttpLoader 自动 fetch 远程 chunk
- **GC 安全**：`eval_safe`（`CatchResultExt::catch`）安全捕获错误，不泄漏 JS 对象。**禁止** try/catch 包装（wrap_script）——会触发 QuickJS C 层 GC assertion
- **TypeScript 检测**：inline/external script 含 `: string`/`: "literal" |`/`as Type` 的跳过（QuickJS 不支持 TS）
- **import.meta 补丁**：无静态 import 的 module 用 `try_strip_esm_for_eval` 替换 `import.meta.url` → URL 字符串后普通 eval
- **customElements**：no-op `define`（不存构造器引用，避免 GC 泄漏）
- **代码位置**：`engine_quickjs.rs`（引擎）+ `bridge.rs qjs_bridge`（DOM 包装）+ `scripts.rs` QuickJS shim 常量

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

### 内容完整性度量（M67，4 指标）

**⚠️ 不要用 `wc -c` 总字符数当完整性指标**——它极具欺骗性：渲染全 nav/footer
噪声、正文一个字没出，总字符数照样接近 Chrome。M67 引入 4 指标去噪度量，
工具 `tests/benchmarks/completeness.py`（纯标准库，不引第三方依赖）。

```bash
# 用法：对比两份 HTML 产物，输出 4 指标
python3 tests/benchmarks/completeness.py --ours qjs.html --theirs chrome.html --grade -v

# 典型流程：QuickJS fetch → HTML，Chrome dump-dom → HTML，再比
browser fetch https://nuxt.com/ --format html --only-main-content=false > qjs.html
"$CHROME" --headless=new --virtual-time-budget=8000 --dump-dom https://nuxt.com/ > chrome.html
python3 tests/benchmarks/completeness.py --ours qjs.html --theirs chrome.html -v

# 自动化：chrome_test_suite.sh 第 3 部分已集成
SITES_COUNT=3 bash tests/benchmarks/chrome_test_suite.sh
```

**4 个指标**（值域 [0,1]，越高越好，综合评级 A-F）：

| 指标 | 算法 | 反映什么 |
|------|------|---------|
| `block_cov` | Chrome 文本块（p/li/h1-6/article/td）里 ours 命中多少（`|交集|/|Chrome|`） | 正文段落爬全没 |
| `sim_ratio` | 双方 HTML→纯文本归一化后 `difflib.SequenceMatcher.ratio` | 综合像不像 |
| `struct_jaccard` | 链接集 a[href] 归一化（去 fragment/query）后 Jaccard `|交集|/|并集|` | DOM 结构完整度 |
| `word_cov` | Chrome 正文高频词 top40（len≥4 去停用词）里 ours 命中比例 | 正文核心词覆盖 |

**关键去噪**：提取文本块/词频时，过滤 nav/footer/script/style/aside/header/svg/
iframe/form/button 等噪声子树（复用 extractor clean.rs 的 Firecrawl 思路），
保证测的是**正文完整性**而非页面总字节数。

**综合分**：`block_cov×0.4 + word_cov×0.3 + sim_ratio×0.2 + struct×0.1`。
块覆盖权重最高（最贴近"正文爬全没"）。评级：≥0.85 A / 0.70 B / 0.55 C / 0.35 D。

**集成测试**（`integration_spa.rs`）也用量化阈值断言，不再用 `contains`：
- `word_coverage()` helper —— 期望词覆盖率（容许丢 1 个不能丢一半），阈值 ≥ 0.9
- 顺序断言 —— Posts: 必须在 Post A 前（防乱序）
- `spa_shell_completeness_quantified` —— 多块完整度测试（6 个关键短语缺一不可）

### 浏览器兼容性评分、优化闭环与停止条件

**先区分三种问题，禁止用一个分数混在一起：**

1. **标准正确性**：Web API/DOM/CSS/JavaScript 的行为是否符合标准。
2. **项目任务成功率**：SPA 是否真正完成加载、路由、异步请求和最终内容提取。
3. **性能与资源**：速度、峰值 RSS、二进制大小、并发吞吐和稳定性。

#### 外部在线测试页面的定位

用户可以直接让浏览器访问这些页面，页面会在浏览器内执行 JS 并显示结果：

| 测试 | 用途 | 项目中的地位 |
|------|------|--------------|
| [HTML5test](https://html5test.co/) | 特性探测和直观分数 | 外部冒烟测试，不是最终门禁；它不保证每项功能语义正确，也不是 W3C 官方认证 |
| [Acid3](https://www.webstandards.org/action/acid3/index.html) | 老的 DOM/CSS/ECMAScript 综合冒烟测试 | 只记录结果，不作为现代 Web 兼容性总分；100/100 也不代表完整浏览器兼容 |
| [BrowserBench](https://browserbench.org/) | Speedometer/JetStream/MotionMark 性能 | 只用于性能报告，不计入标准兼容性分 |

**外部网页分数不是本项目的最终目标。** 页面探测可能受测试版本、UA、视口、等待时间和缺失 API 影响；测试页面能显示分数，只说明它完成了自己的探测，不代表真实 SPA 一定可用。

#### 最终标准：两条主线

**A. 标准兼容性主线**

- JavaScript 语言层使用锁定版本的 [Test262](https://github.com/tc39/test262) 子集。
- HTML/DOM/CSS/Fetch/Storage/Events/Navigation 使用锁定 revision 的 [Web Platform Tests](https://web-platform-tests.org/) 子集。
- WPT/Test262 只把项目 `target_profile=crawler-spa` 范围内的测试纳入分母；明确的非目标（WebGL 真渲染、WebRTC、Service Worker、媒体解码等）标记 `OUT_OF_SCOPE`，不能伪装成 PASS。
- Chrome/Firefox/Safari 只作为差分和故障定位参考；标准测试的期望结果优先来自规范测试断言，不从 Chrome 输出反推标准。

**B. 项目任务主线**

每个 SPA fixture/真实站点必须有机器可验证的关键断言：最终文本、DOM 结构、请求结果、路由状态、Cookie/Storage 状态和错误数。关键断言失败即任务失败，不能用“输出了很多无关文字”抵消。

#### 项目内部评分公式

每项测试按以下结果计分：`PASS=1.0`、`PARTIAL=0.5`、`FAIL/TIMEOUT/CRASH=0`、`OUT_OF_SCOPE` 不入分母。`PARTIAL` 必须在 manifest 中预先说明，不能测试失败后临时降级。

标准兼容性分固定为：

```text
20% JavaScript/Test262
25% HTML/DOM
15% CSS/Selector/Layout
25% Web API/Network/EventLoop
15% Storage/Navigation/CDP
```

项目任务分按关键断言和可选断言计算：

```text
SPA task score = critical_assertions × 0.7 + optional_assertions × 0.3
```

`tests/benchmarks/completeness.py` 的 `block_cov/sim_ratio/struct_jaccard/word_cov` 继续保留，但名称和结论必须理解为 **Content Completeness（内容完整度）**，不能称作浏览器标准兼容性分。`wc -c` 只能作为诊断信息，不能作为评分依据。

#### 每轮开发的固定闭环

1. 从 WPT/Test262、HTML5test/Acid3 或真实 SPA 采集失败现象。
2. 将失败归因到 `html-parser`、`dom`、`css-engine`、`layout`、`js-runtime`、`eventloop`、`net`、`storage/cookie/navigation` 或 `cdp`。
3. 先加最小回归测试，再修改实现；不能只补一个网站专用 hack。
4. 重新跑：目标 profile 测试、SPA fixture、真实站点矩阵、Chrome 差分和性能基线。
5. 记录分数变化、失败数变化、错误变化、RSS/耗时变化，并更新 `docs/JS-COVERAGE.md`、`FEATURES.md`、`PROGRESS.md`。
6. 只有当改动改善了目标测试或修复了明确的回归，才进入下一个缺口。

#### 什么时候可以停止一个阶段

一个版本达到以下条件，才可以停止当前兼容性阶段并转入新能力：

- 目标 profile 的关键 WPT/Test262 测试 **100% 通过**。
- 目标 profile 的标准兼容性分 **≥95/100**；低于 95 的失败必须都有明确的非目标、上游缺陷或已记录的实现计划。
- 核心 SPA 任务成功率 **≥95%**，且 fetch、Promise/Timer、DOM 更新、路由、Storage/Cookie 这类生命线场景 **100% 通过**。
- 关键测试无 crash；新增改动不得让既有测试、核心 SPA 任务或 CDP 回归。
- 内容完整度作为爬虫质量指标，核心站点综合分 **≥0.90**；不能只看字符总数。
- 性能基线无未解释回退：峰值 RSS、冷启动和中位耗时任一项恶化超过 **10%**，不得直接宣布阶段完成。

达到上述条件后，停止的是“当前 target profile 的兼容性补齐”，不是停止项目。后续只有在新增 Web 标准进入 target profile、真实 SPA 暴露新高频缺口、或出现回归时才重新开启该闭环。对明确的非目标和边际收益过低的深层 bundle 报错，记录原因后停止投入。

---

## 四、当前进度快照

> 真实最新数据以 `git log` 和实际 `cargo test` 输出为准；下面是文档记录的里程碑状态。

| 指标 | 值 |
|------|-----|
| HEAD | M68（CDP Page.navigate 执行页面 `<script>`，对齐 CLI SPA 管线） |
| 当前分支 | `main` |
| 总 commits | 250+ |
| Crates | **16** 个（含 `extractor` 后置过滤器） |
| 测试 | **804 passed** + 18 e2e，0 clippy warnings |
| JS 引擎 | **双引擎**：QuickJS（默认，rquickjs 0.12，CLI + CDP）+ boa（`--js-engine boa`） |
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

## 五、架构概览

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

### 16 个 Crate

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

## 六、关键约束（动手前必知）

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

## 七、Agent 工作流程

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
5. **验证** —— 重跑目标 SPA 矩阵和 `tests/benchmarks/completeness.py`；`wc -c` 只能辅助观察，不能作为完整性评分

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
- 「标准正确性以 WPT/Test262、项目价值以 SPA 任务成功率为最终门禁；Chrome 只做差分参考」→ 第三章兼容性评分闭环
- 「外部在线评分必须写清用途和停止条件」→ 第三章兼容性评分闭环

---

## 八、文档地图（深入细节看这些）

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

## 九、给 Agent 的硬性约定

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
12. **QuickJS 是默认引擎**（M66 + M67.1）。所有新功能先确保 QuickJS 下可用，再确认 boa 兼容。
    `--js-engine boa` 可切换回旧引擎调试。
    - **CLI**（render-url/fetch/open）默认 QuickJS（M66）。
    - **CDP**（`browser cdp`）默认 QuickJS（M67.1）：`Runtime.evaluate`/`callFunctionOn`
      走 `eval_in_tree_engine(..., &EngineKind)`，不再硬编码 boa。`cdp --js-engine boa` 回退。
    - **CDP 边界**：`Page.navigate`（M68）**执行页面自带 `<script>`**——复用 CLI 的
      `run_scripts_with_base_engine` 管线（含 timer/networkidle 驱动，覆盖异步 SPA）。
      spawn_blocking 隔离 !Send DOM，catch_unwind 防 JS panic 杀 server。JS 改过的
      DOM 反映到 `PageState.tree`，后续 `DOM.getDocument`/`getOuterHTML`/`Runtime.evaluate`
      读到渲染后的 DOM。
13. **QuickJS GC 安全**（生死规则）：
    - ❌ 禁止用 `wrap_script`（try/catch 包装）——会触发 QuickJS C 层 GC assertion
    - ✅ 用 `eval_safe`（`CatchResultExt::catch`）捕获错误
    - ❌ 禁止在全局变量上存 JS 函数/对象引用（如 `window._cb = fn`）——runtime drop 时 GC 断言
    - ✅ customElements.define 用 no-op（不存构造器引用）
    - ✅ `eval_module_with_imports` 全部在 `ctx.with` 闭包内完成
14. **TypeScript 自动跳过**：QuickJS 不支持 TS。inline/external script 含
    `: string`/`: "literal" |`/`as Type` 的自动跳过（见第三章 TS 检测）。
15. **所有改动必须有例子、有引用、有测试**：
    - 例子：`browser fetch <url> --format text` 的实际输出
    - 引用：AGENTS.md 对应章节 + docs/assessments 报告链接
    - 测试：`integration_js_features.rs` 入库回归测试
16. **测试方法和步骤必须写入 AGENTS.md**（第三章 QuickJS 测试方法）。
17. **标准正确性以 WPT/Test262 为最终标准，SPA 价值以任务成功率为最终标准**；Chrome 只用于内容/渲染差分，HTML5test/Acid3 只用于外部冒烟（见本章「浏览器兼容性评分、优化闭环与停止条件」）。
18. **QuickJS bridge 函数必须走真实 DOM 操作，不能走 set_attr 当 attribute**（M66-fix）：
    - ❌ `__setBody` 不能注册成 `set_attr(body, "innerHTML", html)`——`set_attr_inner`
      把 innerHTML 当普通 attribute（只改属性表），**不替换子节点**，渲染仍读旧 DOM
    - ✅ `__setBody` 必须走 `qjs_bridge::set_body()` → `set_body_inner_html`（清空子节点+插文本）
    - ✅ `__appendBody` 必须走 `append_body_text`（追加，不清空）
    - ✅ `__fetchSetBody`/`__fetchAppendBody` 必须注册（镜像 boa 的 fetch_set_body/fetch_append_body）
    - 新增 bridge 函数时，先看 boa 的 `bridge.rs` 同名函数怎么实现，再在 `qjs_bridge` 镜像
19. **QuickJS Promise microtask 必须显式 drain**（M66-fix）：
    - ❌ `run_jobs()` 不能是空函数——rquickjs 的 Promise microtask（`.then` 回调）**不会**
      在 `ctx.with` 退出时自动 drain，必须显式调用 `ctx.execute_pending_job()`
    - ✅ `run_jobs` 实现：`while ctx.execute_pending_job() {}`（带上限 1000 防御死循环）
    - ✅ event loop 里**先 drain microtask 再 drain macrotask**（`run_jobs` → `__drainDueTimers`），
      匹配 JS 的 microtask-before-macrotask 语义。否则 `setTimeout(0)` 回调跑得比
      `Promise.then` 早，拿不到 then 里准备的数据（`integration_timer_spa` 回归测试覆盖）
20. **动态 script 执行的时序与 GC 约束**（M69）：
    - ✅ `appendChild(scriptEl)` 的 JS shim 检测 script 标签后，把代码（inline textContent
      或 `__fetchSync(src)` 拿到的外链源码）塞进 `bridge::PENDING_DYNAMIC_SCRIPTS` 队列
    - ✅ event loop pump 每轮**先 drain 动态 script（eval_safe）再 drain timer**——否则
      onload 的 `setTimeout(0)` 会跑在 script eval 之前，读到 undefined 状态
    - ❌ 禁止用 JS 间接 `eval(code)` 执行动态 script——会触发 QuickJS GC 风险；必须走
      Rust 侧 `engine.eval_safe(code)`（`CatchResultExt::catch`，CaughtError 在 ctx.with
      闭包内 drop）
    - ✅ QuickJS shim 缺反射属性系统：`s.src = x` 只设 JS 属性不写 DOM attrs，appendChild
      里读 src 必须**双 fallback**（先 `__getAttr(nodeId,'src')` 再 `child.src`）
    - ✅ 同步 `__fetchSync` 阻塞是期望行为——保证 webpack chunk loader 的 Promise.resolve
      顺序正确；eval 推迟到 event loop（setTimeout 语义）不阻塞当前同步 JS 栈
21. **JS 引擎选型底线（生命线落地）**：
    - 🩸 **任何改动不得移除 JS 执行能力**。至少保留一个能跑通 SPA 的 JS 引擎。
    - ✅ 当前最优解 = **QuickJS（Bellard 原版，rquickjs 0.12）**。2026-06 原版发“比上版快 42%”
      更新，是嵌入式 JS 引擎的体积/性能最佳点。
    - ❌ **不引入 V8/rusty_v8**——二进制 ~30MB+、运行时几百 MB，直接违背 G3 低内存卖点。
    - ❌ **不换 quickjs-ng**——2025-09 实测比 Bellard 原版慢 ~3%，字符串场景慢最多 80x，
      纯性能负收益（NG 的优势是 Windows/社区，不是性能）。
    - ✅ 引擎升级走“跟进 rquickjs 版本 / Bellard 上游提速”，不走换引擎。
    - ✅ boa 从强制依赖降为 **optional feature**（M71），不传 `--features boa` 不编译，省 ~5MB。
    - ✅ boa 跑不动纯 CSR 站是引擎天花板，走 `--no-js` 兜底或标注，不硬刚（详见第十条）。

---

*维护者：当本文档与 GOALS.md/FEATURES.md/ARCHITECTURE.md 冲突时，修正本文档。*
