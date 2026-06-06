# Browser — 自研浏览器项目计划

## 项目定位
跨平台、L1 级自研浏览器（自研架构，底层组件复用 Rust crate）。
核心用途：渲染并爬取 SPA 页面，控制内存占用。
兼具学习目的——通过造轮子深入理解浏览器内部原理。

## 自研边界（明确）
- **自研：** 浏览器架构、DOM、事件系统、布局引擎、JS-DOM 桥、资源加载编排、Page 生命周期
- **复用：** HTML 解析（html5ever）、JS 引擎（boa/deno_core）、TLS（rustls）、字体（swash）、2D 绘制（tiny-skia）

## 明确放弃的能力
Service Worker、WebRTC、WebAudio、WebGL/WebGPU、视频音频播放、
CSS grid/table/float/3D 动画。这些对 SPA 爬取无影响。

## 技术栈
| 层 | 选型 |
|---|------|
| 语言 | Rust (stable, 锁定 toolchain) |
| 异步运行时 | tokio 1.x |
| HTML 解析 | html5ever 0.27 |
| CSS 解析 | cssparser + selectors |
| JS 引擎 | M1-M3: boa_engine；M4+: 评估切 deno_core |
| HTTP | hyper + hyper-rustls |
| WebSocket | tungstenite |
| 2D 绘制 | tiny-skia |
| 窗口 | winit + softbuffer |
| CLI | clap |
| 测试 | wiremock + assert_cmd |

## Workspace 结构
```
browser/
├── crates/
│   ├── net/                # HTTP/WS/TLS
│   ├── dom/                # DOM 树 + 事件
│   ├── html-parser/        # html5ever 封装
│   ├── css-engine/         # 选择器 + 样式
│   ├── layout/             # 布局引擎
│   ├── render/             # tiny-skia 光栅化
│   ├── js-runtime/         # JS 引擎 + DOM 桥
│   ├── page/               # Page/Frame/Navigation
│   └── cli/                # 二进制入口
└── examples/
```

## 计划原则
1. 每个 step = 一个 commit，机器可验证（具体 cargo 命令 + 输出断言）
2. Fixture HTML 文件纳入 git（快照，不重新生成）
3. 小步前进，每 commit 独立可 revert
4. CI 三平台矩阵（Windows/macOS/Linux）每个 commit 全绿
5. M2-M5 在前一里程碑收尾后再细化

---

## M0 — 项目初始化（1~2 天，1 个 commit）

**目标：** 建立可用的项目地基

**产出：** workspace 骨架 + ROADMAP + ARCHITECTURE + CI

**验收命令（必须全绿）：**
```bash
cargo build --workspace
cargo fmt --check --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

**Commit：** `chore: initial workspace skeleton with 9 crates`

---

## M1 — 能看到 HTML（2 周，7 个 atomic step）

**总目标：** `cargo run -p browser-cli -- get https://example.com` 输出可读的 DOM 树

**Fixture 文件（位于 `crates/html-parser/tests/fixtures/`，纳入 git）：**
- `simple.html` — `<html><head></head><body><p>hello</p></body></html>`
- `with-doctype.html` — `<!DOCTYPE html><html>...`
- `unclosed.html` — `<p>foo<p>bar`
- `nested.html` — 5 层 div 嵌套
- `attrs.html` — `<a href="x" class="c">`
- `example.com.html` — 真实快照

### Step M1.1 — net crate：HTTPS GET 客户端
- **产出：** `crates/net/src/{lib.rs, client.rs, error.rs}` + `examples/fetch_url.rs`
- **API：** `pub async fn get(url: &str) -> Result<Vec<u8>, NetError>`
- **依赖：** hyper, hyper-rustls, tokio, url, thiserror
- **单测：** `test_invalid_url_returns_err` 传 `"not a url"` 应返回 `Err(InvalidUrl)`
- **集成测试：** wiremock mock 返回 `"hello"`，断言 `get(url) == b"hello"`
- **验收：** `cargo test -p browser-net` 全绿
- **Commit：** `feat(net): add HTTPS client with rustls`

### Step M1.2 — dom crate：arena-backed DOM
- **产出：** `crates/dom/src/{lib.rs, node.rs, tree.rs, document.rs}` + `docs/decisions/0001-arena-vs-refcell.md`
- **关键决策（ADR-0001）：** arena（`Vec<Node>` + `NodeId(usize)`），不用 `Rc<RefCell>`
- **Commit：** `feat(dom): arena-backed DOM tree (ADR-0001)`

### Step M1.3 — html-parser crate：html5ever 封装
- **产出：** `crates/html-parser/src/{lib.rs, parser.rs}` + 6 fixture + `tests/parser_tests.rs`
- **API：** `pub fn parse(html: &str) -> dom::Tree`
- **验收：** `cargo test -p browser-html-parser`（5 测试全绿）
- **Commit：** `feat(html-parser): wrap html5ever with 5 edge-case fixtures`

### Step M1.4 — dom crate：DOM 美化打印器
- **产出：** `crates/dom/src/print.rs`
- **API：** `pub fn pretty_print(tree: &Tree) -> String`
- **Commit：** `feat(dom): add tree pretty-printer`

### Step M1.5 — cli crate：get / parse 子命令
- **CLI：** `browser get <url>` / `browser parse <file>`
- **Commit：** `feat(cli): add get and parse subcommands`

### Step M1.6 — 端到端集成测试
- **产出：** `crates/cli/tests/integration_get_url.rs`
- **Commit：** `test(cli): add e2e tests for get/parse subcommands`

### Step M1.7 — M1 收尾
- **产出：** 更新 ROADMAP.md（M1 ✅）；新增 `docs/postmortems/M1.md`
- **Commit：** `docs: M1 complete, postmortem added`

---

## M2 — 文本流渲染（3 周）

**最终验收：** 渲染 Hacker News 首页（fixture 快照），stdout 肉眼可识别 30 条新闻标题

### Step M2.1 — css-engine：CSS 解析（cssparser 封装）
- **产出：** `crates/css-engine/src/{lib.rs, parser.rs}`
- **API：** `pub fn parse(css: &str) -> Stylesheet`，`Stylesheet{rules}`，`Rule{selectors, declarations}`，`Declaration{property, value, important}`
- **依赖：** cssparser
- **验收：** `cargo test -p browser-css-engine`（含 4+ fixture 测试）
- **Commit：** `feat(css-engine): wrap cssparser for stylesheets and declarations`

### Step M2.2 — css-engine：选择器引擎（tag/.class/#id）
- **产出：** `crates/css-engine/src/selector.rs`
- **API：** `pub fn matches(selector: &Selector, elem_data: &NodeData) -> bool`，`Selector::parse(s: &str) -> Result<Selector>`
- **依赖：** selectors crate
- **验收：** `cargo test -p browser-css-engine`（tag/class/id/descendant 测试）
- **Commit：** `feat(css-engine): tag/class/id selector matching`

### Step M2.3 — css-engine：computed style
- **产出：** `crates/css-engine/src/computed.rs`
- **API：** `pub fn compute_styles(tree: &Tree, sheet: &Stylesheet) -> HashMap<NodeId, Vec<Declaration>>`
- **验收：** `cargo test -p browser-css-engine -- computed`
- **Commit：** `feat(css-engine): compute_styles traverses DOM applying matching rules`

### Step M2.4 — layout：数据结构 + Box 构造
- **产出：** `crates/layout/src/{lib.rs, box.rs, construct.rs}`
- **API：** `LayoutBox`、`BoxType{Block, Inline, Anonymous}`、`Dimensions`、`construct_layout_tree(tree, styles) -> LayoutTree`
- **验收：** `cargo test -p browser-layout`
- **Commit：** `feat(layout): layout tree construction (block/inline/anonymous)`

### Step M2.5 — layout：block 布局算法
- **产出：** `crates/layout/src/block.rs`
- **验收：** block 子元素从上到下堆叠，y 单调递增
- **Commit：** `feat(layout): block-level layout algorithm`

### Step M2.6 — layout：inline + 文本折行
- **产出：** `crates/layout/src/inline.rs`
- **验收：** 一段文本宽度限制 → 正确折成多行
- **Commit：** `feat(layout): inline layout + char-level text wrapping`

### Step M2.7 — render：ASCII 渲染器
- **产出：** `crates/render/src/{lib.rs, ascii.rs}`
- **API：** `pub fn render_ascii(layout: &LayoutTree, width: usize) -> String`
- **验收：** `cargo test -p browser-render`
- **Commit：** `feat(render): terminal ASCII renderer`

### Step M2.8 — cli：render-file 子命令
- **产出：** 更新 `crates/cli/src/main.rs`
- **CLI：** `browser render-file <file>`
- **验收：** 手测 fixture 输出可读
- **Commit：** `feat(cli): add render-file subcommand`

### Step M2.9 — cli：集成测试 + HN fixture
- **产出：** `crates/cli/tests/integration_render.rs` + `tests/fixtures/news.ycombinator.com.html`
- **验收：** `cargo test -p browser-cli -- render`
- **Commit：** `test(cli): e2e render-file against HN snapshot`

### Step M2.10 — M2 收尾
- **产出：** 更新 ROADMAP.md、写 `docs/postmortems/M2.md`
- **Commit：** `docs: M2 complete, postmortem added`

## M3 — JS 执行（4 周，待 M2 后细化）

**最终验收：** 解析 `<script>document.body.innerHTML='<h1>'+new Date().getFullYear()+'</h1>'</script>` 页面，DOM 含当年年份

框架：
- M3.1 嵌入 boa_engine
- M3.2 JS-DOM 桥 v1（读）
- M3.3 JS-DOM 桥 v2（写）
- M3.4 异步任务桥：setTimeout → tokio timer
- M3.5 DOM Level 0 事件：onclick
- M3.6 MutationObserver 桩

**风险点：** M3 结束评估是否切 deno_core

## M4 — SPA 渲染（5 周，待 M3 后细化）

**最终验收：** 跑通 create-react-app 官方 demo

框架：
- M4.1 JS fetch API
- M4.2 XMLHttpRequest
- M4.3 History.pushState + popstate
- M4.4 MutationObserver 完整实现
- M4.5 requestAnimationFrame 空实现
- M4.6 localStorage / sessionStorage
- M4.7 Cookie jar 跨请求
- M4.8 networkidle 算法
- M4.9 资源拦截器（block image/font/media，省 60% 内存）
- M4.10 SPA 注入数据提取
- M4.11 输出"渲染后 HTML" API

## M5 — GUI 窗口（持续，待 M4 后细化）

## 低内存优化（M4 后独立阶段）
- 共享浏览器进程 + 多 BrowserContext（-50%）
- 资源拦截（-60% 单页）
- Page 用完 close + 强制 GC
- JS 引擎内存上限
- cgroup 限制（兜底）

---

## 风险登记簿
| # | 风险 | 概率 | 影响 | 缓解 |
|---|------|------|------|------|
| R1 | boa ES6+ 不完整，SPA 跑不起来 | 高 | 项目受阻 | M3 后评估切 deno_core |
| R2 | JS-DOM 桥异步语义复杂 | 高 | M3 延期 | 参考 Servo script crate + boa examples |
| R3 | html5ever TreeSink trait 学习曲线陡 | 中 | M1.3 延期 | 参考 Servo/scraper 实现 |
| R4 | 单人 14+ 周失动力 | 高 | 半途而废 | 每 step 有可演示产出 |
| R5 | 三平台 CI 偶发失败 | 中 | 阻塞 commit | target_os cfg；actions/cache |

## 工程纪律
- 每个 commit 必须 `cargo build --workspace` 通过
- 测试失败禁止 commit
- 不要 `git commit --amend`，失败用 `git revert`
- 每个 milestone 写 postmortem → `docs/postmortems/`
- 每个"为什么这么选"的决策写 ADR → `docs/decisions/`
- 提交信息遵循 conventional commits

## 学习资源
- 《Web Browser Engineering》Pavel Panchekha（免费在线）
- Andreas Kling YouTube（Ladybird 作者）
- Servo 源码（script/layout/net crate）
- whatwg.org HTML5 规范
- Chrome DevTools Protocol（爬虫对外协议参考）
