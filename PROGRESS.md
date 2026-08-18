# 自研浏览器项目 — 进度日志（活跃）

> 本文档记录每次重要变更，每 commit 后更新。
> **目标/非目标/验收** 见 [docs/GOALS.md](./docs/GOALS.md)（单一事实来源）。
> **能力清单** 见 [docs/FEATURES.md](./docs/FEATURES.md)。
> **里程碑** 见 [docs/ROADMAP.md](./docs/ROADMAP.md)。

---

## 当前状态快照

| 指标 | 值 |
|------|-----|
| HEAD | **M71.4**（boa→optional 9.4M + Web API GAP 修复 + sloppy mode） |
| 总 commits | ~264 |
| 测试 | 868 pass + 18 e2e, 0 clippy warnings |
| Crates | 16 |
| CLI 子命令 | 10 + `--js-engine boa\|quickjs`（含 `serve` HTTP API 服务） |
| JS 引擎 | **QuickJS（默认，9.4M）**；boa 改为 `--features boa` 可选（17M，纯 CSR 站天花板，保留备用） |
| CDP navigate | ✅ M68 执行页面 `<script>`（spawn_blocking + catch_unwind） |
| 动态 script | ✅ M69 appendChild(script) 触发 fetch+eval+onload（webpack/vite 兼容） |
| HTTP API | ✅ M70.12 `browser serve` 命令 + Cloudflare Worker 前端 |
| 性能 | ✅ M70.13 DOM 稳定检测 + 连接复用 + idle 优化（react.dev 17s→4s） |
| 核心目标 G1（SPA 爬虫）| ✅ |
| 截图 G2 | ✅ |
| 跨平台 G3 | ✅ |

---

## 最近变更（倒序）

### M78 — 兼容性评分基线 + 自优化循环（进行中）🎯

**目标**（[docs/plans/M78-compat-score-loop.md](./docs/plans/M78-compat-score-loop.md)）：
标准兼容性分 ≥0.85（五类加权：20% Test262 + 25% HTML/DOM + 15% CSS/Selector +
25% WebAPI/Network/EventLoop + 15% Storage/Nav/CDP），每类 ≥0.50，SPA task 保持 1.0，
性能护栏 <10% 回退。**循环不停直到达标**（对齐 AGENTS.md 评分闭环章节）。

**M78.1 评分 harness**：
- `tests/compat/run_compat.py`：test262（1556 用例，三段式 wrapper + 负向测试页内判定）
  + WPT（281 用例，testharness.js 注入 add_completion_callback 采集器）+ CDP 代理分。
- 锁定版本：test262 `3655e74` / wpt `7b4ed9f`（manifest.json 记录，`--lock` 可重建）。
- 计分对齐 AGENTS：PASS=1 / FAIL=TIMEOUT=CRASH=NOT_RUN=0，OUT_OF_SCOPE 不入分母。

**M78.2 循环 1 —— has_ts_syntax 注释误杀（WPT 全军覆没根因）**：
- 现象：testharness.js 加载但什么都没定义、无报错（`test is not defined`）。
- 根因：testharness.js 第 28 行文档注释含 `interface TestEnvironment {`，
  `has_ts_syntax` 子串匹配命中 → 整个 script 被**静默跳过**。任何注释里提到
  TS 关键字的普通 JS 都会被误杀（`: string`/`interface ` 等）。
- 修复：探测前先 `strip_js_comments`（保守状态机，处理字符串/转义/行块注释）。
- 回归测试：`ts_detection_ignores_keywords_in_comments`（先红后绿）+
  `ts_detection_still_skips_real_ts`（真 TS 仍跳过）。
- 效果：WPT testharness 链路全通，css_selector 类从"全部 harness-not-run"变为
  逐断言真实结果（暴露 :lang/:dir 伪类缺口 → 循环 2）。

门禁：fmt ✅ / clippy 0 warnings ✅ / **725 passed**（baseline 723 + 2 新）。

**M78.3 循环 2 —— :lang/:dir/:nth-child 伪类 + offsetWidth + DOMException（基线 0.333）**：

基线分（锁定 1837 用例，test262 3655e74 + wpt 7b4ed9f）：
`总分 0.3334`（js_test262 0.914 / html_dom 0.292 / css_selector 0.000 /
webapi 0.000 / storage_nav_cdp 0.517）。

- **css-engine**（selector.rs）：`Pseudo::{Lang,Dir,NthChild}` 解析+匹配。
  :lang 走 RFC4647（en 匹配 en-US 不匹配 enm；`en-*`/`*` 通配；祖先 lang 继承）；
  :dir 走 dir 属性继承（HTML 无 dir 祖先默认 ltr）；id/class 名在 `:` 处断开
  （`#box:lang(es)` 之前会把 id 解析成 `box:lang(es)`）。
- **bridge**（querySelector 路径）：token 加伪类三件套；matches_selector 换
  `(tree, id, node, tokens)` 签名（继承/兄弟匹配需要树）；`qs_syntax_error()`
  语法校验（`:dir()` 空/带引号/逗号 → SYNTAX_ERR）；`offset_width()` 桥
  （js-runtime 新增内部依赖 browser-css-engine：<style> 收集 → parse →
  compute_styles → 最后一条 width px）。
- **QuickJS shim**：DOMException 构造器（name→code 映射，SyntaxError=12，
  testharness 的 assert_throws_dom 检查 constructor 同一性）；
  querySelector/All + matches/closest 非法选择器抛 SYNTAX_ERR；
  Element.offsetWidth（mini 级联近似：显式 px 宽）。
- 测试：css-engine 8 个单测（含 `#in:lang(es)` 复合）+ cli 5 个集成测试
  （lang→offsetWidth 100/50 级联回退、:dir 命中与默认 ltr、
  SYNTAX_ERR name+code+constructor、:nth-child）。

门禁：fmt ✅ / clippy 0 warnings ✅ / **737 passed**（+12 新）。

**M78.4 循环 3 —— 属性选择器 + window 命名访问 + 身份缓存（css_selector 0.000→0.915）**：

- **css-engine 属性选择器**（M2 以来 out-of-scope 的缺口）：`[attr]` / `[attr="v"]` /
  `[attr|="v"]`（dash-match）解析+匹配；lang/xml:lang 属性值比较大小写不敏感
  （CSS Selectors 4 §4.2）；`:lang` 只看 lang 属性（HTML 语义，xml:lang 不参与）。
- **offsetWidth 尊重 display:none**（WPT :lang 控制元素断言链路）。
- **insertAdjacentText** 四位置实现（testharness.js 输出渲染依赖，之前 all_complete
  中途崩掉导致 no-results）。
- **window 命名访问**（`__allIds` 桥 + shim 惰性 getter）：WPT 大量裸引用元素 id。
- **Element 包装器身份缓存**（`__elCache`，纯 JS 数据不持原生引用）：同一节点
  getElementById/querySelector/命名访问必须 === 相等（WPT assert_equals 严格相等）。

css_selector 类分变化：0.000 → 0.273（循环 2）→ **0.915**（43/47；剩 `+` 相邻
组合器、dir=auto 内容探测两个已知非目标级缺口）。denominator 47→68 说明 dir
测试从中途崩溃推进到逐断言。
测试：css-engine +3 单测，cli +4 集成（dash-match 样式、display:none、
insertAdjacentText、命名访问+身份）。741 passed。

### M57 — 文档更新 + browser fetch --wait-strategy/--timeout flags（2026-07-05）✅

**M57.1-M57.4**：文档四件套更新
- GOALS.md：G6 从 `browser spa` → `browser fetch`，状态快照更新到 M71
- FEATURES.md：CLI 子命令表增加 `browser fetch` + `serve`，CDP 域更新到 M56
- ROADMAP.md：主表从 M14-M39+ 扩展到 M71，M57 标记 ✅
- ARCHITECTURE.md：crate 依赖图标题更新到 M71，新增 extractor/html_ser 引用

**M57.6**：`browser fetch` 新增 `--wait-strategy` 和 `--timeout-ms` flags
- `--wait-strategy {dom-ready|load|timeout}`（默认 load）控制 JS 执行等待策略
- `--timeout-ms <ms>`（默认 30000）配合 timeout 策略使用
- timeout 超时时日志警告 + 返回当前已渲染内容

**M57.7**：端到端验证通过（723 tests, 0 failed, 0 clippy warnings）
- `browser fetch example.com --format html` → 完整 HTML 输出 ✅
- `browser fetch example.com --format markdown` → markdown 输出 ✅
- `--wait-strategy` + `--timeout-ms` flags 全部正常工作 ✅

---

### M71.5 — smart fallback 同量纲比较，修复 CSR 误判（2026-07-01）✅

**根因**：`fetch` 命令的「JS 搞坏页面」检测用「JS 后纯文本」vs「原始 HTML 字节」比较
（`content_len < raw_len/5`）。**量纲不匹配**——HTML 标签开销占 80%+，正常 CSR 页面
（JS 渲染出正文）也满足此条件，被误判为'JS broke the page'，回退静态壳丢失 CSR 内容。

**修复**：同量纲比较——先提取 JS 前纯文本，再和 JS 后纯文本比。仅当 SSR 文本足够（>200B）
且 JS 后不足其 1/3 才判定 JS 搞坏页面。

**验证（三场景全过）**：正常 CSR 不再误判 / 真 JS 搞坏 fallback 仍生效 / 部分增强两者保留。

---

### M71.1–M71.4 — boa→optional + Web API 差距修复（worktree 隔离，20 commits）（2026-07-01）✅

**体积优化（M71.1）**：boa 从强制依赖降为 `--features boa` 可选。
默认构建（纯 QuickJS）**17M→9.4M（-45%）**，gzip 4.7M。`--features boa` 仍可编双引擎（17M）。

**渲染质量对比方法论（M71.2）**：自写 8 档渐进复杂度 HTML
（iframe/vdom/fragment/css/css3/canvas/webgl/performance），Chrome dump-dom 产 baseline，
逐行对比找差距。工具沉淀到 `tests/render-matrix/`。

**Web API GAP 修复（M71.3，render-matrix 42%→89%）**：
- **GAP-I（根因 bug）**：`querySelector/All` 对「以 # 开头的后代选择器」（如 `#dyn1 .p`）
  彻底失效——id 短路逻辑未排除含空格选择器。5 个回归测试固化。
- **GAP-A**：`HTMLIFrameElement` 构造器 + iframe contentDocument/contentWindow/postMessage stub。
- **GAP-B**：`DocumentFragment` nodeType=11/childNodes + 插入展开子节点。
- **GAP-D**：`CSS.supports` + `matchMedia` 视口判断。
- **GAP-E/F**：Canvas/WebGL `getContext` 返回 stub（符合项目宗旨不做真渲染）。
- **GAP-H**：`performance.navigation` + console 扩展。
- **GAP-J/K/L/M**：cloneNode/fragment/getComputedStyle/dispatchEvent 收尾。

**sloppy mode 兼容（M71.4，GAP-N 根因修复）**：rquickjs 默认 `strict:true` 导致
SvelteKit/Nuxt 裸全局赋值抛 ReferenceError中断 CSR。新增 `eval_user_script()`（sloppy）
仅对用户 script 关闭 strict。回归测试固化。

---

### M70.18 — 修正 Map/Crawl 为递归+并发批量（Firecrawl 对齐）（2026-06-30）✅

上一个版本 (M70.15) 的 Map/Crawl 是「单页 links → 挨个 scrape」，不是真正的递归全站发现。
这是深刻的教训——用户一眼就看出「没有效果」。

**3 个 bug 修正**：
1. **Map 递归缺失**：改为 BFS 多层遍历（depth 0/1/2/3+），**同层并发批量**（每批 3 个），并非串行逐个。
2. **parseLinks hash 去重**：`u.hash = ''` 把 docsify 的 hash 路由（`#/`、`#/zh-cn/`）全部归一为根 URL，
   导致 `https://docsify.js.org/#/` 和 `https://docsify.js.org/#/quickstart` 被视为同一链接被去重。
   这是 Map 对 hash 路由站完全无效果的根因。
3. **`parseInt(depth,10) || 1` falsy 陷阱**：JS 中 `0` 是 falsy，`0 || 1` = 1，
   用户选 depth=0 实际变 depth=1，导致无限递归超时。

**真实验证**（fetch.xbrowser.dev）：

| 站 | 深度 | links | 耗时 | 说明 |
|------|------|-------|------|------|
| react.dev | depth=0 | 9 | 4s | 仅根页 |
| react.dev | depth=1 | **152** | **19s** | 递归进子页 |
| docsify.js.org | depth=0 | 32 | 9s | SPA hash 路由 |
| docsify.js.org | depth=1 | 32 | 118s | 固定侧边栏 SPA（无新链接） |
| Crawl react.dev | depth=1, max=3 | 4 pages | 18s | Map→并发scrape |

**教训**：外围功能（Map/Crawl）如果实现不对等于没做。不再把核心精力分给这类东西——要么做对，要么不做。

### M70.17 — serve 并发支持（每请求一线程 + Semaphore 限流）（2026-06-30）✅

给 `serve` HTTP 服务加并发能力——之前是串行处理（`for stream in listener.incoming()` 阻塞循环），一个请求处理完才接下一个，并发能力 = 1。

**方案：per-thread std::thread + 嵌套 current_thread tokio runtime**
- 为什么是这个方案：`CookieHandle = Rc<RefCell<>>` 是 `!Send`，multi_thread runtime + `tokio::task::spawn` 连编译都过不了。每请求一个独立 `std::thread` 彻底隔离 thread_local（cookie jar / DOM slot），代码库已有先例（`prefetch_to_cache` bridge.rs:557、`fetch_external_script` scripts.rs:718）。
- 改动：提取 `handle_request(stream)` async 函数；`serve()` 改为每连接 `std::thread::spawn` + 嵌套 runtime + `tokio::sync::Semaphore` 限并发。
- CLAP 加 `--max-concurrency N`（默认 3，clamp 1-16）。

**性能验证（本地 release，concurrency=3）**：

| 指标 | 串行（改前） | 并发=3（改后） |
|------|------------|---------------|
| 8 请求总耗时 | 39.2s | **21.8s**（快 1.8 倍）|
| 并发子进程峰值 | 1 | 2-3（信号量限流生效）|
| 总 RSS 峰值 | 41MB | **66MB**（主 23 + 子 43）|
| 成功率 | 8/8 | 8/8 |
| 单请求尾延迟 | 最慢 39s | 最慢 17s |

**重型 SPA 公网验证（NAS，这才是并发真正的价值场景——curl 拿不到的 SPA 内容）**：

| 场景 | 串行 | 并发=3 | 提升 |
|------|------|--------|------|
| 5 个重型 SPA（docsify/excalidraw/bark/todomvc-vue/bb）| 36.4s | **11.6s** | 快 3.1 倍，省 24.8s |
| 6 个重型 SPA（+mithril，超过并发上限）| ~42s（推算）| **13.0s** | 快 3.2 倍，省 29s |

6 站全 200 成功，serve 并发后无崩溃（example.com 仍秒回），主进程 RSS 稳定 41MB（无泄漏）。
限流行为正确：完成时间呈阶梯（2.6→5.2→7→9.7→11→12.9s），证明 3 并发槽轮流消化。

**内存护栏**：3 并发峰值 66MB = Chrome 270MB 的 1/4。信号量保证最多 3 个 serve-child 同时运行（每个 ~22MB），防 N×22MB 内存爆。子进程仍 fork→用完即销毁。

**约束**：每线程独立 cookie jar（thread_local 惰性初始化），并发请求间 cookie 不共享——对爬虫公开页无影响（用户场景）。

### M70.16 — serve vs Chrome 全维度对标基准（8 站，7 站 A 级）（2026-06-30）✅

修复并扩充 `tests/benchmarks/serve_vs_chrome.sh`，跑本地 serve vs Chrome headless 全维度对标。

**基准脚本 2 个 bug 修复**：
1. **JSON 解析**：原用 `json.loads('''$var''')` 三引号嵌入，被内容里的引号/特殊字符破坏（serve_ms 全显示 `?`）。改用 stdin `json.load(sys.stdin)`。
2. **对比格式不对齐**：原用 `serve markdown`（纯文本）vs `chrome HTML` 对比，completeness.py 按 HTML 解析 markdown 提取不到 `<p>/<li>` 块 → blk_cov 恒 0。改成 **HTML vs HTML**（apples-to-apples）。

**修正后结果**（本地 serve vs Chrome headless，HTML 格式对比）：

| 站点 | serve | Chrome | blk_cov | word_cov | 综合 | 评级 |
|------|-------|--------|---------|----------|------|------|
| example.com | 2ms | 7.8s | 1.000 | 1.000 | 1.000 | **A** |
| react.dev | 1.0s | 24.0s | 1.000 | 1.000 | 1.000 | **A** |
| nuxt.com | 1.1s | 139.4s | 1.000 | 1.000 | 0.999 | **A** |
| vuejs.org | 0.8s | 12.6s | 1.000 | 1.000 | 1.000 | **A** |
| svelte.dev | 1.1s | 12.1s | 1.000 | 1.000 | 1.000 | **A** |
| docsify.js.org | 4.4s | 17.8s | 0.929 | 0.950 | 0.925 | **A** |
| todomvc-backbone | 2.8s | 6.1s | 1.000 | 1.000 | 1.000 | **A** |
| bark.day.app | 2.6s | 10.9s | — | 1.000 | — | 见注 |

**7/8 站 A 级**（综合 ≥0.85），平均覆盖率 0.99。serve 比 Chrome 快 6-100 倍（react 1s vs 24s，nuxt 1s vs 139s）。

**bark.day.app 特例**：serve 渲染出完整中文内容（2323 chars），Chrome 只拿到标题（coverpage 依赖 CSS 动画/交互，headless 未触发）。completeness.py 假设 Chrome 是 ground truth → 反向打低分。这其实说明 **serve 在 bark 上超越了 Chrome**。这是已知方法论局限（AGENTS 第三章）。

**已知基准方法局限**（非引擎问题）：
- go.dev：Chrome headless 做语言重定向（中文版），serve UA 拿英文版 → 跨语言不可比
- todomvc-vue：纯 CSR 自定义组件无标准 `<p>/<li>` 块 → blk_cov 度量不适用

### M70.15 — Worker UI 实现 Map + Crawl 标签（2026-06-30）✅

在 `fetch.xbrowser.dev` 前端实现 Firecrawl 风格的 Map/Crawl 功能（Search 不做，依赖外部 API 违背自研优先）。

**设计决策：纯 Worker 端编排，零后端改动。** 爬虫是 I/O 密集型编排，CF 边缘层是正确的编排位置；
后端（NAS serve）继续做单页 SPA 渲染。复用现有管线（AGENTS 第九章第 8 条）。

- **Map**：`POST /api/map` → 复用 scrape `format=links` → 解析 `text → URL` → 同域过滤+去重 → 返回结构化链接数组。react.dev 22 links @ 890ms。
- **Crawl**：`POST /api/crawl` → Map 根页 → 去重+并发分批（每批3）抓取子页 markdown → 汇总多页。react.dev 4 pages @ 6.8s。
- **UI**：tab 切换（Scrape/Map/Crawl，Search 保持 disabled）、链接列表渲染（Map）、多页卡片渲染（Crawl）、crawl-max 页数控件（3/5/10）。
- **重构**：`scrapePage`/`callBackend`/`callWasmFallback` 抽成可复用 helper（Map/Crawl/Scrape 共用）。

**修复**：links 解析分隔符偏移（` → ` 是 3 字符，`slice(idx+3)` 而非 `+4`）、crawl 根页去重（visited set + 尾斜杠规范化）。

**L3 验证**（fetch.xbrowser.dev 真实站点）：
- Scrape regression: example.com markdown/html/links 全通
- Map: react.dev 22 same-domain links, docsify.js.org SPA 渲染后 1 link
- Crawl: react.dev 4 unique pages, 0 duplicates

### M70.14 — serve HTTP API + Cloudflare Worker 前端 + 测试入库（2026-06-30）✅

**子主题：**

1. **`browser serve` HTTP API 服务**（M70.12–14）：`TcpListener` HTTP 服务器，
   fetch→JS→extract 管线封装为 `/` POST 接口，支持 markdown/html/text/links 等 7 格式。
   子进程隔离（`serve-child` + RLIMIT_AS 400MB）防 QuickJS C 层 abort 杀主进程。
2. **Cloudflare Worker 前端**（`fetch.xbrowser.dev`）：Firecrawl 风格 UI（Markdown 预览、
   复制、响应式、7 格式 tab、10 个 SPA 示例站）。**始终走 NAS 后端 JS 渲染**
   （`_source: backend-spa`），CF 边缘 wasm 仅作后端不可用时的兜底。
3. **SPA 渲染性能优化**（M70.13–14）：
   - CDN 外链并行预取（bark.day.app 8.5s→2.5s）
   - body 可见文本检测替代 HTML 大小阈值（docsify 修复）
   - URL hash fragment 去除（docsify 路由修复）
   - XHR 异步 send + CSS 过渡仿真 + fetch 超时保护
4. **测试入库**（本次 commit）：
   - `has_visible_body_content` 5 个单元测试（SSR 检测逻辑固化）
   - `integration_spa_routing.rs` 2 个集成测试（hash fragment + XHR 路由）
   - CLI `fetch` base_url hash 去除（与 serve 一致性修复）
   - 清死代码（`render_and_extract`/`RenderResult`/无效 `drop`）+ clippy 0 warning

**通用原则**（用户强调）：所有修复必须是标准化通用方案，禁止特定网站 hack。
所有流量走 NAS 后端 JS 渲染（curl 能做到的没意义）。

### M70.13 — 性能优化：DOM 稳定即退出 🚀（2026-06-29）✅

**核心思路：爬虫 ≠ 浏览器。Chrome 等"全部加载完"（8s 虚拟时间预算 + 15 个 JS 脚本 +
analytics 都跑完），我们等"内容出现了"就停。**

#### 效果
| 站点 | 优化前 | 优化后 | Chrome 对标 |
|------|--------|--------|------------|
| example.com | 0.8s | **0.8s** | ~3s |
| react.dev | **17s** | **4.2s** 🚀 | ~8s |
| nuxt.com | 8s | **2.5s** 🚀 | ~6s |

所有站点内容完整性不变（react.dev 15803 chars 全量）。

#### 7 项优化

1. **DOM 稳定检测**（最大收益）：每个 script eval 后检查 `body.children.length > 0`，
   有内容就停止执行后续脚本。react.dev 从 15→1 个脚本，省 ~10s。
   文件：`scripts.rs:934`
2. **长延迟定时器不阻塞退出**：`__hasPendingTimers()` 只关注 fireAt≤2s 的定时器，
   忽略 analytics/telemetry 的 60s setTimeout。省 ~6s 空等。
   文件：`scripts.rs:1072`
3. **事件循环 idle 检测（QuickJS）**：500ms 宽限期 + 连续 idle 5 轮→提前退出，
   硬超时从 8s→3s。之前 QuickJS 路径无 idle 检测，一直等满 8s。
   文件：`scripts.rs:960-980`
4. **HttpClient 全局复用**：`OnceLock<browser_net::HttpClient>` 跨脚本共享 TLS
   连接池，避免 15 个脚本每个新建 TLS 连接。省 ~1s。
   文件：`scripts.rs:678-697`
5. **HttpClient 预初始化**：事件循环开始前预创建，避免首脚本冷启动。省 ~0.3s。
   文件：`scripts.rs:684-686`
6. **analytics 跳过列表增强**：+sentry/doubleclick/facebook/hotjar/fullstory。
   文件：`scripts.rs:717-727`
7. **sleep 粒度 20ms→5ms**：事件循环的每轮固定 sleep 从 20ms 降到 5ms。省 ~0.5s。
   文件：`scripts.rs:949`

#### 架构变化
- 事件循环添加 `idle_start`/`idle_rounds` 状态跟踪
- `__hasPendingTimers` 加入时间窗口过滤（≤2s）
- `fetch_external_script` 使用全局 `OnceLock<HttpClient>`（不 Send 问题已验证）

---

### M70.12 — browser serve HTTP API + Cloudflare Worker 后端代理（2026-06-29）✅

**把 Rust binary 变成 HTTP API 服务，部署到 NAS（shanbox），对外提供 SPA 渲染能力。**

#### 架构
```
用户 → https://fetch.xbrowser.dev/ (Worker UI)
         POST /api/scrape
           ↓
    BROWSER_BACKEND（Cloudflare Secret）
    https://spa-render.shanbox.19930810.xyz:8443
           ↓
    NAS(shanbox) → nginx:8443 → browser serve:3021 → QuickJS 引擎
```

#### CLI 新增
- `browser serve --port 3021 --bind 0.0.0.0`：HTTP API 服务
  - 零新依赖：`std::net::TcpListener` + 手动 HTTP 解析
  - 完整 SPA 渲染管线：fetch → QuickJS → extract → JSON
  - 支持 7 种格式（markdown/html/text/links/images/highlights/branding）
- 交叉编译：macOS ARM64 → Linux x86_64（cargo-zigbuild）

#### Worker 新增
- `BROWSER_BACKEND` 环境变量控制后端代理
- 内存缓存（60s TTL，Map 实现）
- 后端不可用直接报错（不走静默回退）
- 默认 URL 改为 react.dev（SPA 标杆）
- 示例站点：React / Nuxt / Svelte / Vue.js

#### 部署
- 二进制部署到 shanbox（192.168.0.29:2200，Debian 12 容器）
- 持久化：crontab @reboot + 守护脚本
- nginx 路由（port 8443 HTTPS）

---

### M70.11 — Markdown 渲染预览 + 一键复制（2026-06-29）✅

#### UI 改进
- **Markdown → HTML 渲染器**：纯 JS 实现，无外部依赖。支持 h1-h6/粗斜体/
  链接/代码/列表/引用/图片
- **Raw / Preview 切换**：markdown 格式自动进入预览模式
- **一键复制**：Clipboard API + execCommand fallback
- **输出工具栏**：页面标题 + 格式切换 + Copy 按钮
- **Toast 通知**：复制成功/失败反馈

#### 架构改进
- `ui.html` 独立文件，通过 Wrangler Text 模块导入
- 避免模板字面量冲突（此前 `\w`/`\s` 等正则导致 wrangler 编译失败）

---

### M70.10 — 7 种格式 + 移动端响应式 UI（2026-06-29）✅

#### 新增格式
| 格式 | wasm 函数 | 作用 |
|------|-----------|------|
| Images | `extract_images()` | `alt text → URL` 每行一条 |
| Highlights | `extract_highlights()` | `[tag] 高亮文本` |
| Branding | `extract_branding()` | title/description/og:tags/icon |

共 7 种格式：`markdown` · `html` · `text` · `links` · `images` · `highlights` · `branding`

#### UI 改进
- 响应式 CSS（≤640px 纵向堆叠，触控目标 44px）
- Format 快速切换 chips
- 示例 URL 快捷填充
- 加载动画 + 键盘 Enter 提交
- 页脚

#### 后端
- wasm 新增 3 个 `#[wasm_bindgen]` 导出函数
- Worker API switch 增加对应分支

---

### M70.9 — Cloudflare Worker 部署 🚀（2026-06-29）✅

**把 8 个核心 crate 编译到 wasm，部署到 Cloudflare Workers，绑定自定义域名。**

#### 编译 wasm
- `crates/worker-wasm`：cdylib crate，依赖 dom + html-parser + extractor + wasm-bindgen
- `wasm-pack build --target web` → 833KB wasm
- 5 个导出函数：extract_markdown/text/links/html/title

#### Cloudflare Worker
- `GET /` → 前端 UI 页面（暗色主题，类似 Firecrawl）
- `POST /api/scrape` → `{ url, format }` → `{ title, content }`
- Workers fetch(url) → wasm 解析+提取 → 返回

#### 域名
- 绑定 `fetch.xbrowser.dev`（CF 自定义域名）
- `wrangler.toml` + `wrangler deploy`
- GitHub-style 暗色 UI

#### 关键决策
- eval 走 `eval_safe`（GC 安全）而非 JS 间接 eval
- 同步 `__fetchSync` 阻塞（保证 webpack Promise.resolve 顺序）
- src/textContent 双 fallback 读取（QuickJS shim 缺反射属性系统）
- 队列放 bridge.rs 顶层（不受 quickjs feature 门控，boa/QuickJS 共用）

#### 验证
- 冒烟：3 层链式加载（入口→chunkA→chunkB→chunkC）双引擎均 `CHAIN_COMPLETE`
- L1：4 单元测试（enqueue/drain FIFO + 清空 + 重入）
- L2：6 集成测试（inline/外链/链式/onload 时序/onerror/非 script 不误触发）
- 门禁：fmt ✅ / clippy 0 warnings ✅ / **814 passed**（baseline 804 + 10 新）
- 5 站回归：**零退化**（M68 vs M69 CLI fetch text 逐站完全一致）

详见 [`docs/assessments/M69-dynamic-script.md`](./docs/assessments/M69-dynamic-script.md)。

### M68 — CDP Page.navigate 执行页面 `<script>`（对齐 CLI SPA 管线）✅

**解决「CDP navigate 只解析静态 HTML、不跑 JS」的历史缺口。** 现在 puppeteer
连上 `browser cdp`，`page.goto()` 会真正执行页面自带 `<script>`，JS 改过的 DOM
反映到 `PageState.tree`，后续 `DOM.getDocument` / `getOuterHTML` /
`Runtime.evaluate` 读到的是**渲染后**的 DOM。

#### 现状（改动前）
- `page.rs:266` navigate 调 `st.render(&html, url, 80)`——只 parse+layout+render，
  不跑 JS（`page.rs:22` 注释承认「M44 does not execute JS」）。
- `PageState::render`（`page.rs:67-84`）把 parse→css→layout→render 绑死成一函数。
- `dom_domain.rs` 的 `getDocument`/`getOuterHTML` 读的是**静态解析树**，JS mutation
  不体现。

#### 改动（3 文件 + 1 e2e）
- **cdp/page.rs**：
  - **拆 `render()` 为 `parse_only` + `render_from_tree`**——给 JS 步骤留插入点。
    旧 `render()` 保留（内部调两者），向后兼容。
  - **`dispatch` 加 `engine_kind: EngineKind` 参数**。
  - **`Page.navigate` 三步管线**：
    1. `parse_only`（同步，存 tree）
    2. `spawn_blocking` + `catch_unwind` 跑 `run_scripts_with_base_engine`
       （对齐 CLI，含 timer/networkidle 驱动）。!Send 的 SharedTree 在闭包内
       clone 成 owned Tree 返回。panic 兜底回退静态树。
    3. `render_from_tree`（用 JS 改过的 tree 重新 layout+render）
- **cdp/server.rs**：`page::dispatch` 调用传 `self.engine_kind`。
- **js-runtime/scripts.rs**：QuickJS `document.title` setter 从 no-op 改为
  `__setText` 写回 `<title>` 节点（对齐浏览器：改 title 更新 `<title>` 元素，
  getter 反映新值）。**顺手补的 shim 缺口**——e2e 暴露。
- **tests/e2e/spa.js**（新）：puppeteer navigate 本地 HTTP 托管的 inline-script
  SPA，验证同步渲染 + title 覆盖 + setTimeout 异步渲染。

#### 验证
- 4 个新单元测试：`parse_only_then_render_from_tree_matches_render`（拆分等价回归）
  / `run_scripts_mutates_tree_reflected_in_page_state`（JS innerHTML 改 DOM）
  / `run_scripts_async_timer_reflected`（setTimeout 异步渲染）
  / `no_script_page_renders_static`（无 script 静态回归）。
- **e2e（tests/e2e/spa.js）5/5 全过**：同步 inline script DOM、title JS 覆盖、
  50ms setTimeout 异步内容全部验证。
- **完整 e2e 套件 18/18 全过**（basic 5 + evaluate 4 + dom-via-evaluate 4 + spa 5）。
- 三门禁：fmt ✅ / clippy 0 warnings ✅ / **804 passed**（baseline 800 + 4 新）。

#### 关键技术点
| 点 | 处理 |
|----|------|
| `!Send`（SharedTree/thread_local） | `spawn_blocking` 在固定 OS 线程跑完 run_scripts，闭包内 clone 成 owned Tree 返回 |
| run_scripts panic（OOM/栈溢出） | `catch_unwind`，panic → 回退静态树，不杀 CDP server |
| Tree 所有权 | `tree.clone()` 给 run_scripts（拿走所有权），返回后 `borrow().clone()` 回写 |
| timer/networkidle 驱动 | 复用 run_scripts 内置逻辑（boa pump_event_loop / QuickJS __drainDueTimers），无需 CDP 侧另接 eventloop |

#### 边界（本次不做）
- ❌ sandbox 子进程内存护栏（CLI sandbox 是 stdin 协议，不适合 CDP 长驻 server；
  CDP 用 catch_unwind + 静态树兜底替代）
- ❌ 动态渲染宽度（emulation device metrics 联动另议）
- ❌ `Page.addScriptToEvaluateOnNewDocument` 真正注入（目前 no-op，保持）

#### M68-fix：`DOM.getOuterHTML` 响应格式 bug

**现象**：用 puppeteer + completeness.py 跑 5 CSR 站对标 Chrome，DOM 渲染明明
追平（链接数 100% 一致），但 completeness 评分全 D/F（composite 0.33-0.49）。

**根因**：`DOM.getOuterHTML`（`dom_domain.rs`）返回裸 `Json::String(html)`，
未包成 CDP 协议要求的 `{"outerHTML":"..."}` 对象。puppeteer 把裸字符串当
iterable 解构成 `{0:'<',1:'!',...}`，`r.outerHTML` 得 undefined，HTML 全空。

**修复**（1 文件）：
- `cdp/dom_domain.rs`：响应改为 `BTreeMap{"outerHTML" → html}` 对象。
- 单元测试 `dispatch_get_outer_html` 加断言：响应必须含 `"outerHTML"` 字段。
- 同步 `/tmp/single.js` 从 `evaluate(()=>outerHTML)`（QuickJS getter 漏标签）
  改走 `DOM.getOuterHTML`（Rust `serialize_html`，完整序列化）。

**验证**：重跑 5 站 × 2 backend，completeness **5 站全 A**（composite
0.997-1.000，block_cov/struct_jaccard/word_cov 全 1.000）。详见
[`docs/assessments/M68-cdp-chrome-comparison.md`](./docs/assessments/M68-cdp-chrome-comparison.md)。

### M67.1 — CDP Runtime domain 接 EngineKind，默认 QuickJS ✅

**解决「CDP 是 workspace 唯一还硬编码 boa 的路径」问题。** M66 把 CLI 三命令
（render-url/fetch/open）默认引擎切到 QuickJS，但 CDP 的 `Runtime.evaluate` /
`callFunctionOn`（puppeteer 的 `page.evaluate`/`title`/`$` 全走这里）仍写死走 boa。

#### 现状（改动前）
- `eval_in_tree`（scripts.rs）硬编码 `build_shimmed_context()`（boa ctx），CDP 唯一
  JS 执行入口。
- `CdpServer::listen(port)` / `runtime_domain::dispatch(...)` 链路无 engine 透传点。
- `Cdp` CLI 命令只有 `--port`，无 `--js-engine`。

#### 改动（5 文件）
- **js-runtime/scripts.rs** —— 新增 `eval_in_tree_engine(tree, base_url, expr, &EngineKind)`：
  - boa 分支沿用 `eval_in_tree` 逻辑（返回 boa `display()` 格式）
  - QuickJS 分支复用 `run_scripts_quickjs` 的 setup 模式（install_shared + storage/nav/
    cookie + shim + 裸变量声明），调新方法 `engine.eval_display_string()`
  - **返回值格式约定**：两引擎统一（string 带引号模拟 boa display，number/bool/undefined/
    null 原样），`classify_value` 不分引擎
  - 旧 `eval_in_tree` 保留（内部委托 boa 分支），向后兼容
- **js-runtime/engine_quickjs.rs** —— 新增 `eval_display_string(js)`：用 IIFE 在 JS 层
  格式化结果（`typeof r==='string' ? JSON.stringify(r) : String(r)`），一次 eval 拿
  String 结果，避免 rquickjs Value 跨闭包取值复杂性。用 `CatchResultExt::catch` GC 安全。
- **js-runtime/bridge.rs** —— `qjs_bridge::get_tag_by_name(tag) -> f64`：按标签名找
  第一个匹配节点 NodeId（找不到 -1.0），复用 `find_first_element`。对齐 boa `get_tag`
  的字符串模式。
- **js-runtime/engine_quickjs.rs** —— 注册 `__findTag(tag)` 桥 + QuickJS shim 的
  `document.title` getter 从「硬编码空串」改为读真实 `<title>` 节点（`__findTag('title')`
  + `__getText`）。**这是顺手补的 shim 缺口**——CDP `document.title` 依赖它。
- **cdp/runtime_domain.rs** —— `dispatch(...)` 加 `engine_kind: &EngineKind` 参数，
  两处 `eval_in_tree` 调用改为 `eval_in_tree_engine`。`classify_value` 不动。
- **cdp/server.rs** —— `CdpSession` 加 `engine_kind` 字段，`handle`/`listen`/`accept_one`
  加参数透传，两处 `dispatch` 调用传 `&self.engine_kind`。
- **cdp/Cargo.toml** —— 加 `quickjs` feature 转发（`browser-js-runtime/quickjs`），默认开。
- **cli/main.rs** —— `Cdp` 命令加 `--js-engine`（默认 quickjs），修正 3 处过时注释。

#### 验证
- 4 个新 QuickJS 测试：`evaluate_arithmetic_quickjs` / `evaluate_string_quickjs` /
  `evaluate_boolean_quickjs` / `evaluate_reads_dom_quickjs`（document.title 读真实 DOM）。
- 端到端：`browser cdp --port N`（默认 quickjs），5 个 Runtime.evaluate 表达式全通过
  （含 `typeof Symbol` → `function`，**QuickJS 原生支持 Symbol，boa 0.20 不支持**）。
- 三门禁：fmt ✅ / clippy 0 warnings ✅ / **800 passed**（baseline 796 + 4 新）。
- boa 回退模式（`--js-engine boa`）同样 5 表达式全过。

#### 边界（本次不做）
- ❌ `Page.navigate` 执行页面 `<script>`（CDP 目前只 evaluate 注入表达式，不跑页面
  自带脚本）——更大 scope，另议。
- ❌ CDP engine 缓存/复用（性能优化）——每次 evaluate new engine，和 boa 版特征一致。

### M67 — 内容完整性度量加固（4 指标 + 测试量化阈值）✅

**解决「测试不够给力、没有值反映完整性」问题：旧 wc -c 指标假繁荣。**

核心问题：旧对标用 `wc -c` 总字符数（6124 vs 6948 → 88%）当覆盖率，极具欺骗性——
渲染全 nav/footer 噪声、正文一个字没出，总字符数照样接近 Chrome。集成测试用
`contains("Post A")` 断言，渲染丢 90% 内容只要剩一个词照样绿。

#### 新增
- `tests/benchmarks/completeness.py` —— 4 指标度量工具（纯标准库）
  - `block_cov`（块覆盖）/ `sim_ratio`（相似度）/ `struct_jaccard`（链接）/ `word_cov`（词频）
  - 去噪：nav/footer/script/style/aside 等子树不计入，测正文完整性
  - 综合评级 A-F（block_cov×0.4 + word_cov×0.3 + sim×0.2 + struct×0.1）
- `chrome_test_suite.sh` 第 3 部分升级：单字符数 → 4 指标表格 + 评级
- `integration_spa.rs` 加固：
  - `word_coverage()` helper（词覆盖率，≥0.9 阈值，替代 contains）
  - 顺序断言（Posts: 必须在 Post A 前，防乱序）
  - `spa_shell_completeness_quantified` 多块完整度测试（6 短语缺一不可）

#### 实测发现（3 站）
- 综合完整度 **0.971**（评级 A）：块覆盖 1.000 / 词频 1.000 / 相似度 0.987 / 结构 0.733
- **新指标暴露了旧指标掩盖的问题**：vite.dev 结构覆盖只有 0.200
  （QuickJS 只含 Chrome 链接 20%），旧 wc -c 显示"73% 假繁荣"看不出
- 正文完整性（块/词频）QuickJS 已 100% 对齐 Chrome

验证：796 passed（+1 新测试），0 failed，0 clippy warnings

### M66-fix — QuickJS bridge 三处关键 bug 修复（__setBody / appendBody / Promise microtask）✅

**修复 QuickJS 引擎下 SPA 渲染管线 3 个导致内容丢失的 bug，4 个测试转绿。**

根因与修复：

1. **`__setBody` 走 `set_attr("innerHTML")` 而非 `set_body_inner_html`**
   - 现象：QuickJS 下 `__setBody("text")` 执行了但渲染仍显示旧 placeholder
   - 根因：QuickJS 的 `__setBody` 注册成 `set_attr(body, "innerHTML", html)`，
     而 `set_attr_inner` 把 innerHTML 当普通 attribute 设置（只改属性表），不替换子节点
   - 修复：新增 `qjs_bridge::set_body()`（走 `set_body_inner_html`，清空子节点+插文本），
     `__setBody` 改用它（`engine_quickjs.rs:169`）

2. **`__appendBody` 误用 `set_body`（覆盖而非追加）**
   - 现象：`spa_shell_renders_combined_api_output` 只输出最后一个 fetch 结果
   - 根因：`__appendBody` 注册成 `set_body`（清空+覆盖），而非追加
   - 修复：新增 `qjs_bridge::append_body()`（走 `append_body_text`，追加到 body 末尾）

3. **`__fetchSetBody` / `__fetchAppendBody` 未注册到 QuickJS**
   - 现象：QuickJS 报 `__fetchAppendBody is not defined`
   - 修复：新增 `qjs_bridge::fetch_set_body()` / `fetch_append_body()`（镜像 boa 实现），
     注册到 QuickJS globals

4. **Promise microtask 不 drain（`run_jobs` 空实现）**
   - 现象：`render_url_async_spa_with_real_fetch` 失败——`Promise.resolve().then(fn)` 的
     fn 永远不执行，setTimeout 回调拿不到 then 准备的数据
   - 根因：`QuickJsEngine::run_jobs()` 是空函数（注释称 "ctx.with 退出自动 drain"，实际不会）
   - 修复：`run_jobs` 改为 `while ctx.execute_pending_job() {}` 循环 drain；
     并在 event loop 里**先 drain microtask 再 drain macrotask**（`run_jobs` → `__drainDueTimers`），
     匹配 JS 的 microtask-before-macrotask 语义

验证：
- 4 个失败测试转绿：`integration_render_url` / `integration_open` / `integration_spa` /
  `integration_timer_spa`（20 个测试全通过）
- 全量 **795 passed, 0 failed**，0 clippy warnings
- 顺手清理 dead-code：移除 `HttpResolver.base` / `QuickJsEngine.base_url` 未读字段

### M66 — JS 引擎双后端（JsEngine trait + QuickJS via rquickjs）✅

**引入 QuickJS 作为 boa 的替代引擎，速度/内存/兼容性全面超越。**

架构：
- `JsEngine` trait 抽象层（`engine.rs`）—— `ctx_mut()`/`supports_esm()`/`name()`/`as_any()`
- `BoaEngine`（`engine_boa.rs`）—— 默认后端，封装 boa::Context
- `QuickJsEngine`（`engine_quickjs.rs`）—— `--features quickjs`，68 个 bridge 函数 + 独立 JS shim
- `EngineKind` 工厂（`engine.rs`）—— `--js-engine boa|quickjs` 切换
- `run_scripts_with_base_engine`（`scripts.rs`）—— 根据 engine_name 走不同执行路径

QuickJS 优势（12 站实测对标 boa + Chrome）：
- **速度**：8/12 站比 boa 快（nuxt.com 快 35x，remix.run 快 5x）
- **内存**：中位数 19MB（boa 44MB / Chrome 262MB，省 93%）
- **兼容性**：react.dev 渲染 91%（boa 只有 0.7%）—— ES2020 完整让 React hydration 成功
- **渲染**：8/12 站成功（nuxt.com/docusaurus 零错误完美渲染）

QuickJS shim 集（独立精简版，避免 QuickJS 正则差异）：
- window/document/Element（classList/style/firstChild/querySelector 等完整 API）
- XHR/fetch（同步 + Promise-based）
- URL/URLSearchParams/localStorage/sessionStorage
- crypto/performance/history/MutationObserver/Event/CustomEvent
- setTimeout/setInterval 异步 event loop

### M65 — 速度优化 + 底层插桩 ✅

- HTTP/2 多路复用（并行 fetch 用单 reqwest::Client 共享连接池）
- Event loop networkidle 检测（提前退出，省 7-16s 空转）
- Net worker 连接复用（XHR 不再每次 TLS 握手）
- ESM chunk 并行 prefetch（BFS 扫描 import 依赖图）
- Vite `__vite__mapDeps` 预取（nuxt.com 35→16s）
- `--profile` 底层插桩（分阶段 RSS + 耗时）
- `boa_engine::gc::force_collect()` 手动 GC（效果不显著——活跃对象非垃圾）

### M64 — ESM 支持（HttpModuleLoader：boa Module API + 同步 HTTP fetch chunk）✅

**解决 CSR 最后一道墙：ES Modules（静态 import/export + import.meta）**

之前 vuejs.org/vite.dev/nuxt.com 的 `<script type="module">` 在 parse 阶段就 SyntaxError
（`import{...}from"..."` 语法 boa Script 模式不认），只拿到 SSR 静态壳。

**实现**：
- `crates/js-runtime/src/esm_loader.rs`：`HttpModuleLoader` 实现 boa `ModuleLoader` trait。
  `load_imported_module` 时同步 HTTP fetch chunk → 写临时文件（保留 path 供 referrer 解析）
  → `Module::parse`（Module 模式接受 import/export/import.meta）。boa 自动处理依赖图
  解析、实例化、链接、循环依赖。
- `scripts.rs`：`<script type="module">` 检测 → 走 Module 路径（非 ctx.eval）。
  预扫描有 module 脚本时用 `Context::builder().module_loader(...)` 创建 Context。
- `window/Element.addEventListener` null listener guard（Vue/React passive 检测模式）。

**验证**（全部 0 ESM 错误，真实 CSR 渲染，非静态壳）：
- **vite.dev**：102 行 markdown（"# The Build Tool for the Web" + features + npm 命令）
- **vuejs.org**：73 行（"# The Progressive JavaScript Framework" + features + sponsors）
- **nuxt.com**：323 行（"# The Full-Stack Vue Framework" + 代码示例 + 路由）
- **svelte.dev**：27 行

**入库测试**（4 项）：
- `integration_esm_module`：3 项（链式依赖 a→b→c + 循环依赖 + import.meta 语法）
- `integration_js_features`：web_api_null_event_listener_ignored（Vue passive 检测）

### M63 — CSR 自愈循环第 3 轮（append/prepend + 反射 IDL 属性 + getBoundingClientRect + URL 递归修复）✅

**自愈循环（报错驱动补 API，真实站点验证）**：

- **bark.day.app（docsify SPA）0 JS 错误渲染**—— 42 行 markdown，内容/导航/链接完整。
  残余 4 错误全部消除：
  - `[xhr] load listener threw: cannot convert null/undefined to object` ×2 → **反射 IDL 属性修复**（a.href 缺失，docsify sidebar sort 读 `b.href.length - a.href.length` 崩）
  - `[xhr] load listener threw: not a callable function` ×1 → **getBoundingClientRect 补全**（docsify K() scroll handler 读 `rect.height`）
- **svelte.dev（SvelteKit）渲染**—— `not a callable function`（Element.append 缺失）+ `cannot convert null/undefined to object`（URL 无限递归）全部修复
- **react.dev / nextjs.org**—— 完整渲染（272 / 185 行 markdown）

**补的 API（全部纯 JS polyfill，二进制 0 涨）**：
  - **Element.prototype.append / prepend**（ParentNode 标准方法，接受多参数+字符串）→ svelte.dev `document.body.append(div)`
  - **反射 IDL 属性**（href/src/value/name/type/checked/disabled 等 24 个）→ docsify `a.href.length` 排序。这些属性通过 getter/setter 反射到同名 attribute，框架直接读不走 getAttribute
  - **Element.prototype.getBoundingClientRect**（返回零值 DOMRect）→ docsify K() scroll handler
  - **URL 构造器接受 location 对象作 base** + 修复 **URL.href getter 无限递归**（getter 调 toString 调 href getter...）
  - **XHR addEventListener('load', cb) 回调 this 绑定**（cb.call(self, ev)，否则 cb 内 this.status === undefined）

**入库测试**（6 项，integration_js_features，共 43 项）：
  - web_api_element_append_node / append_string
  - web_api_url_with_location_base
  - web_api_anchor_href_reflected（含 docsify sort 复现场景）
  - web_api_get_bounding_client_rect
  - web_api_xhr_load_listener_this_binding

**修复 pre-existing 测试**（7 项，boa main 升级遗留的 script count 断言）：
  - 引入 `at_least_n_scripts(n)` 辅助谓词，断言脚本执行下限而非精确数（boa main 执行内置安装脚本，计数随版本变化）
  - integration_render_script / integration_spa / integration_render_url / integration_open / integration_cookie 全绿

**确认 boa 引擎天花板（不硬刚）**：
  - `import.meta` / 动态 `import()`（vuejs.org / vite.dev / nuxt.com / svelte.dev SvelteKit bootstrap）→ boa 无 ES Module loader
  - Web Worker（nextjs.org turbopack "chunk path empty but not in a worker"）→ out of scope（爬虫不需要），且页面仍正常渲染 185 行

### M62 — JS 覆盖矩阵补齐（28 项 ES6+ + 框架 API + 事件系统 + bark + todomvc + builder.io CSR 渲染）✅

- **bark.day.app（docsify SPA）CSR 渲染成功**—— 39 行文本内容、markdown/links 格式完整。
- **todomvc-vue（Vue 3 SPA）渲染**—— TodoMVC 完整 UI（header/toggle-all/input 渲染）
- **builder.io（React/SDK）渲染**—— 真实内容输出（7783B 文本，含产品/团队信息）
- 修复链条（自愈循环，报错驱动补 API）：
  - **window.addEventListener/removeEventListener/dispatchEvent**：docsify initRouter 调 `window.addEventListener('hashchange', cb)` 崩（`TypeError: not a callable function`）
  - **XMLHttpRequest.addEventListener/removeEventListener**：docsify `X().then` 用 `addEventListener('load', cb)` 注册回调
  - **XMLHttpRequest.response** 属性：docsify onload 读 `xhr.response`
  - **XMLHttpRequest.getResponseHeader/getAllResponseHeaders**：docsify 读 `last-modified` 做 cache
  - **innerHTML/outerHTML setter** 升级：从 `__setText`（纯文本）升级为 `__parseHtml`（html5ever 解析 + 递归创建真实 DOM 节点）。外挂的 `querySelector` 才能找到标记段元素
  - **Element.prototype.querySelector**：Vue/React createElement 后查找子元素（之前只有 querySelectorAll，Vue/React 应用全崩）
  - **document.createElementNS**：Vue/React SVG/MathML 元素创建
  - **escape/unescape**：deprecated 全局函数（builder.io 第三方依赖）
  - **TextEncoderStream/TextDecoderStream**：Stream API 构造器
  - 新增 `__parseHtml` Rust 桥函数（copy_subtree 递归复制解析树到 DOM 树）
  - docsify 源码级补丁（`fetch_external_script` 替换）：Prism DFS null guard + 事件注册 null guard
- 新增 8 个测试（3 个 window_shim + 4 个 JS features + 1 个 createElementNS）
- 工程门禁：fmt ✅ clippy 0 warnings ✅ **139 lib tests + 37 JS features tests pass** ✅
- CSR 实测对比（自研 vs Chromium 149）：
  - bark: 内容覆盖率 **100%**（1660B vs 1547B Chrome 文本）
  - todomvc-vue: 内容覆盖率 **43%**（156B vs 355B，React/Vue 组件树部分渲染）
  - amp.dev: 内容覆盖率 **61%**（13797B vs 22542B）
  - builder.io: 13961B 文本（Chrome headless 超时无法对比）
  - 速度优势：轻 SPA（todomvc 1-2s vs Chrome 14-30s，快 10×+）
- 文档更新：docs/JS-COVERAGE.md 新增 11 项 API 状态行

### M61 — fetch --smart 模式（先 SSR 后 JS，快 8 倍）✅
- 调研 SSR JSON 提取适用面窄（Next RSC 流/Nuxt 混淆函数），转向 --smart 模式。
- 实现：先 --no-js 提 SSR，够（≥500 字符）则跳过 JS，不够回退跑 JS。
- 实测 nextjs.org/blog：101s/113MB → 12.8s/18.8MB（快 8 倍省 6 倍内存）。
- 测试：741 passed（+2 smart 集成测试）。

### M60 — boa 0.20→0.21 升级（async/await 落地）✅ + 覆盖率路线图
- 目标：从实测 58% 覆盖率提升到 80%+，保住「13MB 低内存」卖点。
- 关键发现：boa 0.21（2025-10）已完整落地 async/await（0.20 的头号杀手）。
- 务实路径：分层混合 —— ① boa 升 0.21 ② SSR 数据提取层 ③ 补缺失 Web API。
- 规划文档：[docs/plans/M60-js-coverage-roadmap.md](./docs/plans/M60-js-coverage-roadmap.md)。

### M59 — `browser fetch` 爬虫命令 + 独立 extractor crate ✅
- 目标：`browser fetch <url>` 当 curl 用，支持 `--format markdown|html|text|links`。
- 设计依据：借鉴 xbrowser `scrape`（已验证）+ Firecrawl 内容提取管线（42 选择器噪声过滤）。
- 关键决策：过滤器独立成 `crates/extractor`（后置插件，不碰 net/js-runtime）。
- 实现：extractor crate（html/text/md/links 四格式 + Firecrawl 噪声过滤 + 空兜底）+
  cli Cmd::Fetch（复用 fetch→parse→run_scripts→extract 管线）+ 9 集成测试。
- 验收：L3 真实站点 2/2 通过（example.com + seo.box CSR）；workspace 739 passed 0 failed。
- 设计文档：[docs/plans/M59-fetch-command-design.md](./docs/plans/M59-fetch-command-design.md)。
- 对标结果：[docs/assessments/M59-fetch-benchmark.md](./docs/assessments/M59-fetch-benchmark.md)。

### M58 — reqwest 启用 brotli/gzip/deflate 解码（Vercel/CDN 压缩站可爬）✅
- 根因：`seo.box` 等 Vercel 托管站点默认对 `text/html` 大响应做 Brotli 压缩
  （`content-encoding: br`）。`browser-net` 的 reqwest 未启用解码 feature，收到
  压缩字节流按明文解 → `read body failed: error decoding response body`，整站爬取失败。
- 影响面：所有 Vercel/Cloudflare 类默认压缩站点（G1 爬虫硬伤）。
- 修复：`crates/net/Cargo.toml` 给 reqwest 加 `brotli`/`gzip`/`deflate` 三个官方
  解码 feature（非新 crate，属白名单 reqwest 的合理配置）。
- 验收：`render-url https://seo.box/referring/` 端到端跑通，CSR 表格（fetch JSON +
  JS 填 DOM）完整渲染出 Top Referring Websites 数据。
- 连带：顺手 `cargo fmt` 修了 `cdp/emulation_domain.rs` 一处预存格式 diff。

### M-cls — cls.cn/telegraph SPA 渲染 + 内存自愈护栏 ✅
- M-cls.1 ✅ 内存自愈护栏（子进程 + RLIMIT_AS + 父进程 RSS 监控 kill）
- M-cls.2 ✅ 收紧 boa 运行时限制（loop 250K→40K, stack 4096, recursion 256）
- M-cls.3 ✅ CSR 数据兜底（spa_fallback + host→fetcher 注册表，cls.cn 接 m.cls.cn SSR）
- M-cls.4 ✅ 大纲文档（assessment + plan）
- M-cls.5 ✅ 连带回归修复（navigation fixture location getter）

用户诉求：cls.cn/telegraph 能 SPA 渲染 + 内存"内部自愈"（免得 40GB）+ 统一大纲。

**根因**：cls.cn/telegraph 是 Next.js **CSR**，`__NEXT_DATA__` 只有 `{chooseNav}`
无正文，正文需带签名 XHR（`get_roll_list` errno 10012）。执行 `main.js`(142KB)
在 boa 0.20 里 eval 内存暴涨到 **6.6GB 被 OOM 杀**，渲染永不完成。

**解法**（纵深防御 + 数据双管）：
1. 危险 JS 跑在子进程，父进程轮询 RSS（50ms）超 ~400MB 立即 SIGKILL（self-healing）。
   macOS RLIMIT_AS 不强制，RSS 监控是实际护栏。子进程被杀 → **不重跑 JS**，
   改 `run_js=false` 渲染静态壳 + CSR 兜底。
2. CSR 兜底：发现 `m.cls.cn/telegraph` 是 **SSR**，内嵌 `roll_data[]`（20 条
   brief/ctime/level，无签名）。`spa_fallback` 抠 JSON（自研解析器）注入 body。
3. host→fetcher 注册表，新 CSR 站点加项即可。

**实测**：峰值 RSS **~415MB**（修复前 6.6GB，降 98.4%），wall **~2s**（修复前
57s 被杀），输出 20 条真实电报（日期+等级+正文，非 `-.--` 占位符）。

文档：[docs/assessments/M-cls-spa.md](docs/assessments/M-cls-spa.md)（诊断+决策）、
[docs/plans/M-cls-spa.md](docs/plans/M-cls-spa.md)（步骤+验收）。
工程门禁：fmt ✅ clippy 0 warnings ✅ **682 tests pass** ✅。

### M42-M48 — CDP 协议层 + Puppeteer 端到端全链路打通 ✅
本项目是**通用 SPA 爬虫浏览器 + 兼容 CDP**。M42 起逐步补齐 CDP 协议，
M48 用真实 **Puppeteer 25.1** 验证全链路。

- M42-M43 ✅ CDP server 骨架：WebSocket + JSON-RPC + `/json` 发现端点。
- M44 ✅ `Page` 域：navigate（fetch→parse→render→存 PageState）+ captureScreenshot。
- M45 ✅ `Runtime` 域：evaluate。
- M46 ✅ `DOM` 域：getDocument / getOuterHTML / querySelector（基于 PageState.tree）。
- M47 ✅ `Network` 域：getResponseBody + enable/disable。
- M49 ✅ `Emulation` 域（未知方法统一 ack，puppeteer 握手必需）。
- M50-M53 ✅ `Page` lifecycle 事件 + `Target` 域（flatten session）。

**M48 Puppeteer e2e 实测全链路打通**（`run-all.js` 3/3 scenarios、12/12 断言）：
握手 + newPage + navigate + title（走 isolated world）+ screenshot（PNG）
+ page.evaluate（含 document/location 真实 DOM 访问）+ DOM 取数。
6 个真实修复：targetId 字段、createTarget 事件去重、catch-all 扩展、
executionContextCreated、isolated world worldName（**title 卡点根因**）、
Runtime.callFunctionOn + `eval_in_tree`（**evaluate 接真实 DOM**）。
详见 [docs/assessments/M48-puppeteer-e2e.md](docs/assessments/M48-puppeteer-e2e.md)。
工程门禁：fmt ✅ clippy 0 warnings ✅ cdp 80 tests + js-runtime 135 tests ✅。
**已知边界**：`page.$()`/`$()` 受 boa 0.20 不支持 ES2018+（async generator/
for-await/using）限制；爬虫用 `page.evaluate(() => document.querySelector(...))`
完全够用（dom-via-evaluate.js 4/4 验证）。

### M29-M30 — ADR 文档 + 截图优化 + <a> 蓝色渲染 ✅
- M29.1 ✅ ADR-0003 TLS 后端切换（hyper-rustls → native-tls）
- M29.2 ✅ wss:// TLS 验证（代码层面确认 ws crate 只支持 ws://）
- M29.3 ✅ 截图尺寸优化（--max-height 参数，限高避免超长截图）
- M30 ✅ <a> 标签蓝色渲染（screenshot ANSI + W3C #0000EE）

ADR-0003 记录 M24.2 决策：hyper-rustls 连百度报 AlertReceived(ProtocolVersion)，
但 curl 能秒连。根因是 TLS 客户端兼容性，非网络环境问题。切换到 native-tls 解决。

截图尺寸优化：screenshot.rs render_text_to_png 加 max_height 参数，从顶部截断。
CLI render-file/render-script/render-url 加 --max-height flag。

<a> 蓝色渲染（关键设计）：LayoutBox 加 link: bool，CharBuffer 加 links mask，
render_ascii 双 API（plain + colored）。stdout 保持纯 ASCII（爬虫安全），
screenshot 用 colored（ANSI \x1b[4;34m...\x1b[0m）。screenshot.rs
strip_ansi_and_track_links 解析 ANSI → link span，渲染蓝色 (#0000EE)。
render_html_to_string_inner 返回 (plain, colored) tuple，parse+layout+JS 只跑一次
（避免重复执行 JS 的副作用风险）。

验证：3 render 单元测试 + 4 screenshot ANSI 单元测试 + 百度截图 555K（max-height 2000）。

---

### 2026-06-06 — 文档体系建立（wiki 重构）🟡 进行中
**变更**：建立结构化 wiki 文档体系，解决多文档冗余 + 过时问题。
- ⭐ `docs/GOALS.md`：NORTH STAR（目标/非目标/验收，单一事实来源）
- `docs/FEATURES.md`：能力清单（CLI/CSS/JS 桥/Web API 矩阵）
- `docs/ARCHITECTURE.md`：更新到 M14（含 storage/navigation）
- `docs/CONVENTIONS.md`：工程规范（提交/自研边界/依赖白名单）
- `docs/DIRECTORY.md`：目录结构 + crate 职责
- `docs/TESTING.md`：三级测试分层 + 验收命令
- `docs/ROADMAP.md`：M0-M14 + 前瞻
- `README.md`：重写（快速开始 + SPA 爬虫示例 + 文档导航）
- 删除根目录 `ROADMAP.md` / `ARCHITECTURE.md`（移入 docs/）
- `docs/PLAN.md`：加 superseded 标注（历史归档）

### M28 — JS 全局对象补齐（对齐 W3C/Chrome 基础子集）✅
- M28.1 ✅ navigator_shim.rs（userAgent/platform/language/languages/onLine/cookieEnabled/vendor）
- M28.2 ✅ window_shim.rs（window===globalThis 自引用 + innerWidth/innerHeight/视口数据）
- M28.3 ✅ document_shim.rs（getElementById/querySelector/createElement 包装 __* 桥 + body/head/cookie/title/location）
- M28.4 ✅ screen_shim.rs（width/height/colorDepth/orientation，响应式布局特性检测）
- M28.5 ✅ 文档同步

补齐 SPA 反爬/特性检测最常读的四大全局对象，对齐 W3C/Chrome 基础子集。
所有对象用纯 JS 对象字面量（非 NativeFunction getter），避免跨 eval this 绑定丢失。

设计决策（M28.1 关键教训）：navigator 值全是静态的（不依赖运行时 DOM），
与 location/history（值依赖运行时需方法调 __locationHref()）不同——用 JS 对象字面量
一次构造，是真正的**数据属性**，符合 W3C（`navigator.userAgent` 无括号），
且代码更简单。第一版用 ObjectInitializer::function() 注册，访问返回函数对象
而非字符串（ua=空），修为纯 JS 对象字面量解决。

window = globalThis 自引用（不复制 navigator/location 到 window，经 globalThis 自动可见），
document 方法包装现有 __* 桥，screen 全部静态默认值（无显示器环境）。

验证：4 个 shim 共 29 单元测试，端到端全部验证（navigator userAgent/platform、
window.innerWidth/self===window、document.readyState/body、screen.width/orientation），
百度 JS 错误减少（document/window/navigator 不再未定义）。workspace 481 tests。

---

### M27 — `<a href>` 链接目标渲染（G1 爬虫核心）✅
- M27.1 ✅ construct.rs inject_a_href（`text` → `text (url)`，参照 inject_li_bullet）
- M27.2 ✅ integration_anchor.rs 3 e2e（有文本/空链接/无 href）+ example.com 快照更新
- M27.3 ✅ 文档同步

让爬虫从渲染文本直接看到链接指向，无需解析 DOM。ASCII 模式无颜色概念，
内联 URL 比颜色/下划线对爬虫更直接可用。空链接（无文本子节点）seed 文本
叶子显示 href（爬虫不丢链接）。

验证：受控实验 + layout 23 单元测试 + 3 e2e + example.com 快照（预期更新）。

---

### M26 — 真实站点渲染修复（百度截图可用）✅
- M25.1+M25.2 ✅ 截图字形坐标修复（fontdue metrics 精确测量 + 坐标公式不翻转，乱码→可读）
- M26.1 ✅ textarea 加入非渲染黑名单（修复百度 CSS 泄漏，截图 50MB→1.3MB，降幅 78%）
- M26.2 ✅ 文档同步

让百度等真实站点截图真正可用。两大修复：
1. fontdue 字形坐标：旧版用硬编码 COL_WIDTH/LINE_HEIGHT 且翻转公式导致字形重叠+垂直镜像。
   改用 fontdue Metrics 精确测量（advance_width/ascent/descent）+ 正确坐标公式
   （y_origin = baseline - ymin - height + 1，不翻转）+ 纯数学锁定测试。
2. textarea CSS 泄漏：百度把 CSS 藏在 `<textarea style="display:none">` 做延迟加载，
   旧版把 textarea 当普通元素渲染导致 CSS 泄漏（69% 噪音）。加入非渲染黑名单。

验证：百度截图 2270×57100(50MB) → 2736×10600(1.3MB)，内容干净（百度首页/新闻/hao123 全在）。

---

### M24 — TLS 后端切换（百度可连）✅
- M24.1 ✅ 诊断（curl 能秒连百度，hyper-rustls 报 AlertReceived(ProtocolVersion)）
- M24.2 ✅ net crate hyper-rustls→reqwest(native-tls)，公开 API 不变
- M24.3 ✅ 实测百度可连可截图 + commit + 文档

推翻 memory 旧误判：'真实 HTTPS 连接失败 = 网络环境问题'是错的。真相是
hyper-rustls 的 ring provider 与百度 CDN TLS 不兼容。换 reqwest(native-tls，
curl 同款系统 TLS 库）解决。

---

### M23 — WebSocket（手写 RFC 6455，实时 SPA）✅
- M23.1 ✅ browser-ws crate：RFC 6455 帧编解码纯算法
  （OpCode/Frame/apply_mask/encode_frame/decode_frame，7/16/64-bit 长度，控制帧校验）
- M23.2 ✅ 握手 sha1 + base64 + handshake（纯算法，RFC 6455 §4.2.2 经典向量验证）
- M23.3 ✅ tokio TCP 连接 + 握手 + 帧读写（ws://，XorShift64 PRNG）
- M23.4 ✅ WsManager 多连接管理器（后台线程 + 命令/事件队列，修复 await_holding_lock）
- M23.5 ✅ JS WebSocket 全局对象（纯 JS 原型，修复 Close 不 push 事件 + recv_buf 丢失）
- M23.6 ✅ 文档同步

解决实时 SPA（聊天/推送）的最后一块拼图。**手写 RFC 6455**，不引入 tungstenite
（遵循 GOALS.md 自研优先）。ws:// 全链路验证（echo server e2e）。

真实 bug 修复（2 个，记录教训）：
1. manager.rs WsCmd::Close：发 Close 帧后必须 push Closed 事件，否则 pump
   ws_connection_count 永不归零 → idle 超时。
2. client.rs recv_message：recv_buf 必须是 struct 字段而非局部变量。一次
   socket.read 可能读到多帧 TCP 数据，局部 buf return 时 drop 会丢失后续帧字节。

---

### M22 — 真实图像渲染（<img> → ASCII art）✅
- M22.1 ✅ render crate image 模块（image_to_ascii_from_img + resolve_local_image_src，10 测试）
- M22.2 ✅ cli render_html_to_string post_process_images（跨行扫描，3 e2e）
- M22.3 ✅ 文档同步

解决 M9 的 [IMG: src] 纯文本占位符问题：爬虫/CLI 现在能看到 <img> 图像内容。
本地图像（file:// / 绝对路径 / 相对 cwd）解码成 ASCII art，http(s) URL 不下载
（避免渲染管线引入网络）。M12.3 的 image_to_ascii 提升到 render crate。
post_process_images 跨行扫描（src 可能因折行被拆，] 可能被 width 截断丢失）。

---

### M21 — Cookie 持久化（跨进程保留登录态）✅
- M21.1 ✅ cookie crate serialize/deserialize（TSV，不引入 serde，10 测试）
- M21.2 ✅ cli --cookie-file flag + CookieFileGuard（RAII，持有 jar owned clone + sync_jar）

解决"爬虫重启要重新登录"痛点：启动时 load cookie 文件，退出时 save。
TSV 纯文本格式（自研优先，不引入 serde）。
CookieFileGuard 持有 owned jar clone（避开 TreeGuard::drop 清空 thread-local）。

---

### M20 — fetch 增强（POST/PUT/DELETE + 真实 status code）✅
- M20.1+M20.2 ✅ net 通用 request（POST/PUT/DELETE wiremock 测试）
- M20.3 ✅ fetch(url, {method, body, headers}) JS 端 + request_full（真实 status 201）
- M20.4 ✅ 文档同步

fetch 现支持完整 HTTP method 集合：表单提交 / REST API 调用场景。
request_full 返回真实 status code（不再被 is_success() 吞掉 201/204）。
POST/PUT/DELETE 自动带 cookie jar（复用 M15）+ networkidle 计数（复用 M18）。

---

### M19 — 标准 fetch API（现代 SPA 核心）✅
- M19.1 ✅ __fetchSync 桥 + fetch_shim（Response 对象 + .text()/.json()）
- M19.2 ✅ e2e（fetch.then(text/json/catch)，5 tests）
- M19.3 ✅ 文档同步 + 真实 SPA 手动验证（res.json 用户列表渲染）

标准 fetch：fetch(url) → Promise<Response> → res.text()/json() → Promise。
React/Vue/Next.js 等 SPA 核心数据获取模式。复用 M16 Promise + M15 cookie jar +
M18 networkidle。网络错误 reject TypeError（标准行为）。

---

### M18 — networkidle 算法（爬虫渲染完整性信号）✅
- M18.1 ✅ pending_requests 计数器 + is_network_idle（5 e2e）
- M18.2 ✅ cli --assert-network-idle flag（render-script / render-url，2 e2e）

networkidle = pending_timers==0 && pending_requests==0。
爬虫用此信号判断 SPA 是否渲染完（Playwright/Puppeteer 同款能力）。
fetch_sync 用 RequestGuard（Drop）确保计数器即使失败也 -1。

---

### M17 — XMLHttpRequest（老 SPA 依赖）✅
- M17.1 ✅ XhrState + thread-local + 4 桥（__xhrCreate/Open/Send/GetResponseText）
- M17.2 ✅ XMLHttpRequest 全局构造器（纯 JS 原型，闭包捕获 self）
- M17.3 ✅ e2e（wiremock 真实 fetch + onload 渲染，3 tests）
- M17.4 ✅ 文档同步 + 真实 SPA 手动验证（Users 表格渲染）

异步 XHR 支持：new XMLHttpRequest() / open / send / onload / responseText。
设计：同步 fetch（复用 fetch_sync + cookie jar）+ setTimeout(0) 异步触发 onload。
教训：boa native fn 跨 eval 传 this 不可靠，纯 JS 原型更稳。

---

### M16 — 异步 JS（setTimeout + Promise，用 boa 自研）✅
- M16.0 ✅ ADR-0002 决策（调研推翻 M7.3 defer，用 boa 而非 deno_core）
- M16.1 ✅ browser-eventloop crate（TimerWheel 纯算法，14 tests）
- M16.2 ✅ setTimeout/clearTimeout 桥 + 全局名（thread-local + drain API）
- M16.3 ✅ event loop 接入 run_scripts（pump_event_loop，6 集成测试）
- M16.4 ✅ Promise.then 触发（ctx.run_jobs，5 集成测试）
- M16.5 ✅ e2e fixture（timer-spa.html，setTimeout+Promise 驱动 SPA 渲染）

异步 JS 支持：setTimeout(fn, 0) / Promise.resolve().then() / 递归 setTimeout
链 / Promise 链 + microtask 与 macrotask 交错。无需 300MB V8。

---

### M15 — Cookie jar（跨请求会话保持）✅
- M15.1 ✅ browser-cookie crate（RFC 6265 子集，21 tests）
- M15.2 ✅ net::get_with_headers（带 Cookie 头 + 返回 Set-Cookie）
- M15.3 ✅ JS fetch_sync 接入 jar（主线程读写，新线程传 String）
- M15.4 ✅ cli get/render-url/open 主请求共享 jar（fetch_with_jar）
- M15.5 ✅ e2e（cookie jar 跨请求会话保持，2 tests）

SPA 爬虫增强：解决百度等登录态反爬。主请求设的 cookie → JS fetch 带上。

---

### M14 — Navigation（history / location）✅
- M14.1 ✅ browser-navigation crate（HistoryStack + Location 解析，11 tests）
- M14.2 ✅ `__history*` / `__location*` bridges + install_navigation
- M14.3 ✅ history/location JS 对象 shim + 接入 run_scripts（8 tests）
- M14.4 ✅ e2e fixture navigation-spa.html（5 tests）
- M14.5 ✅ PROGRESS/memory 同步

### M13 — Web Storage（localStorage / sessionStorage）✅
- M13.1 ✅ browser-storage crate（`Rc<RefCell<HashMap>>`，6 API，8 tests）
- M13.2 ✅ `__storage*` bridges + install_storage
- M13.3 ✅ localStorage/sessionStorage JS 对象 shim（6 tests）
- M14.4 ✅ e2e fixture storage-spa.html（2 tests）

### M12 — 截图 + 图像 ASCII ✅
- M12.1 ✅ PNG screenshot（`--screenshot` flag，fontdue + png）
- M12.3 ✅ image-ascii 子命令（image crate + 10 级灰阶 ramp）

### M11 — Bug 修复 ✅
- vh/vw/vmin/vmax → Zero / rem → 16px / pt → 4/3 px

### M10 — 性能优化 ✅
- LayoutCache（get_or_compute）+ DirtyTracker（mark/mark_subtree/is_dirty/clear）

### M9 — 图片占位符 ✅
- `[IMG: src]` 占位符注入（construct.rs build_box）

### M8 — 表单交互 ✅
- `__getValue` / `__setValue` / `__click` / `__submit` 橋 + e2e

### M7 — 渲染质量 + 交互 ✅
- M7.1 CSS margin/padding（真实解析 + UA defaults + collapsing）
- M7.2 完整 DOM API（`__createEl`/`__appendChild`/`__qs`/...）
- M7.4 真实字体（fontdue + DejaVuSans 739KB + 中文）
- M7.5 URL 栏 + 键盘输入 + 滚动
- M7.3 异步 JS 🟡 defer（boa 0.20 JsObject::call 私有）

### M0-M6 — 核心管线 ✅
- M0 骨架 + CI / M1 HTTPS+DOM / M2 CSS+布局+ASCII 渲染
- M3 JS 执行（boa）/ M4 SPA 渲染（**项目目标达成**）
- M5 GUI 窗口 / M6 渲染质量修复 + 跨平台 CI

---

## 前瞻（按爬虫价值排序，见 GOALS.md 决策原则）

1. **切 deno_core** ⭐⭐⭐⭐⭐ — 解锁 setTimeout/Promise/async-await
2. **Cookie jar** ⭐⭐⭐⭐ — 跨请求会话，解决登录态反爬
3. **XMLHttpRequest** ⭐⭐⭐ — 老 SPA 依赖
4. **真实图像渲染进 GUI** ⭐⭐
5. **WebSocket** ⭐⭐
6. **networkidle 算法** ⭐⭐⭐
7. **资源拦截器** ⭐⭐⭐（省 60% 内存）

---

## 文档维护规则

- **每 commit 后**：更新本文件"最近变更"
- **目标变更**：改 docs/GOALS.md + 写 ADR
- **新增功能**：改 docs/FEATURES.md
- **架构变化**：改 docs/ARCHITECTURE.md
- **里程碑完成**：改 docs/ROADMAP.md + 写 docs/postmortems/M<n>.md
- **冲突优先级**：GOALS > FEATURES > ARCHITECTURE > 其他
