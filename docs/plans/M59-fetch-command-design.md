# M59 实现计划：`browser fetch` 爬虫命令 + 独立 extractor crate

> 日期：2026-06-17
> 设计来源：Firecrawl 内容提取管线 + xbrowser `scrape` 命令（均已验证方案）。
> 本文记录设计决策（为什么）+ 实现步骤（做什么）+ 对标验收（不比别人差）。

## 目标

1. `browser fetch <url>` 作为「当 curl 用」的 SPA 爬虫命令，一行拉取 → 结构化输出。
2. 支持 4 种输出格式：`--format markdown|html|text|links`（默认 markdown）。
3. 过滤器**独立成 `extractor` crate**（后置插件），与浏览器核心解耦。
4. 噪声过滤对齐 Firecrawl（42 选择器 + force-include + 空内容兜底）。
5. **验收时对标 xbrowser/Firecrawl**，确保同站点输出质量不输它们。

## 设计依据（已验证方案的借鉴）

### 借鉴 xbrowser `scrape`（用户自己做过、跑通的命令）

| xbrowser 参数 | 我们对应 | 说明 |
|---|---|---|
| `--format markdown\|html\|text` | `--format markdown\|html\|text\|links` | 加 links，其余完全对齐 |
| `--selector "article"` | `--selector "table tr"` | CSS 选择器过滤（复用 css-engine） |
| `--only-main-content true`（默认） | `--only-main-content`（默认 true） | Firecrawl 噪声过滤 |
| `--timeout 15000` | `--timeout 15000` | networkidle 兜底 |
| `--json` | `--json` | 结构化输出 |
| `--sitemap` / `--include-paths` | ❌ 首版不做 | 属 map/crawl，YAGNI |

> 参考文档：`~/.knowledge/d8839c0emox76i8z-xbrowser-爬虫命令完整文档.md`

### 借鉴 Firecrawl 内容提取管线

| Firecrawl 机制 | 我们如何用 |
|---|---|
| `EXCLUDE_NON_MAIN_TAGS`（42 选择器） | 直接内化为 `only_main_content` 默认噪声过滤 |
| `FORCE_INCLUDE_MAIN_TAGS`（保护 `#main`/`article`） | 防止误删主内容 |
| 无条件移除 `script/style/head/meta/noscript` | 输出前必做 |
| 空内容兜底（main empty → full 重跑） | 简单可靠，加上 |
| URL 绝对化（`a[href]`/`img[src]`） | links 格式必备 |
| 后置 transformer pipeline | 正是「独立 extractor crate」模式 |

> 参考文档：`~/.knowledge/fjkz6am1cr-firecrawl-content-extraction.md`、
> `~/.knowledge/hueolyhiio-firecrawl-content-extraction-pipeline.md`

### 借鉴 agent-browser 数据提取模式（5 种模式）

| 模式 | 我们首版覆盖 |
|---|---|
| DOM 元素提取 | ✅ `--format` + `--selector` |
| JS 全局变量提取（`__INITIAL_STATE__` 等） | ⚠️ 间接覆盖（JS 执行后 DOM 已变，serialize 即可） |
| API 拦截 | ❌ 首版不做（需 wait --request，复杂） |
| 滚动加载 | ❌ 首版不做（需 actions） |
| iframe 操作 | ❌ 首版不做 |

> 参考文档：`~/.knowledge/60f27659mon2ds80-agent-browser-data-extraction-patterns.md`

## 架构：独立 `extractor` crate（用户明确要求解耦）

```
crates/extractor/          ← 新 crate，纯 Rust，后置插件
├── Cargo.toml             # 依赖：dom, css-engine, url（不碰 net/js-runtime/eventloop）
└── src/
    ├── lib.rs             # FetchOptions + run_extract(tree, opts) → String
    ├── selector.rs        # query_all(): css_engine::Selector + tree.traverse (~10 行)
    ├── clean.rs           # Firecrawl 噪声过滤（42 选择器 + force-include + 兜底）
    ├── format_html.rs     # 薄包装 dom::serialize_html
    ├── format_text.rs     # 纯文本（去标签、保留段落）
    ├── format_md.rs       # HTML→Markdown（自研 turndown 子集）
    ├── format_links.rs    # 超链接地图 + URL 绝对化
    └── tests.rs           # 每种格式 + 噪声过滤单元测试
```

**依赖方向**（严守 AGENTS.md 架构约束）：
```
cli ──→ extractor ──→ dom (SharedTree)
                  ──→ css-engine (Selector)
                  ──→ url (Url::join)
```
extractor **只吃渲染后的 `SharedTree`**，不碰 net/js-runtime/eventloop。这就是「插件」边界——浏览器核心无感，extractor 单独可测、可换、可删。

## 命令接口

```bash
browser fetch <url>
  --format markdown|html|text|links   # 默认 markdown
  --selector "table tr"               # CSS 选择器过滤（可选）
  --only-main-content true|false      # 默认 true（Firecrawl 噪声过滤）
  --timeout 15000                     # networkidle 兜底（毫秒）
  --no-js                             # 跳过 JS（已知静态页加速）
  --json                              # 结构化输出 {url,title,content,metadata}
  --width 80                          # 渲染宽度（影响 text 折行）
```

## 管线（复用现有 + extractor 后置）

```
fetch_with_jar(url)                          → HTML String       [现有]
parse_html(html)                             → Tree              [现有]
run_scripts_with_base(tree, Some(url))       → SharedTree        [现有，已含 networkidle pump]
   ↓ extractor::run_extract(shared_tree, opts)
       1. [若 --selector] query_all → 只留匹配节点
       2. [若 only_main_content] Firecrawl 42 选择器去噪 + force-include 保护
       3. 无条件移除 script/style/head/meta/noscript
       4. URL 绝对化（a[href]/img[src]，仅 links/md 格式需要）
       5. 按 --format 加工 → html / text / md / links
   → stdout（或 --json 包成 {url,title,content,metadata}）
```

**复用清单**（全部现成，零新网络/JS 代码）：
| 需求 | 复用函数 | 位置 |
|---|---|---|
| 抓取 | `fetch_with_jar` | `cli/src/main.rs:448` |
| 解析 | `parse_html` | `cli/src/main.rs:524` |
| 跑 JS + networkidle | `run_scripts_with_base` | `js-runtime/src/scripts.rs:400` |
| 序列化 HTML | `serialize_html` | `dom/src/html_ser.rs:10` |
| 选择器解析 | `Selector::parse` | `css-engine/src/selector.rs:64` |
| 选择器匹配 | `Selector::matches` | `css-engine/src/selector.rs:79` |
| 遍历节点 | `tree.traverse` | `dom/src/tree.rs:140` |

**需新写**：`query_all`（~10 行）、HTML→Markdown（~200 行，全新）、噪声过滤（~60 行）。

## 四种 format 加工逻辑

| format | 逻辑 | 复杂度 |
|---|---|---|
| `html` | `dom::serialize_html` 直接序列化（selector 过滤后子树） | 极低（现成） |
| `text` | DFS 遍历，块级元素（p/div/h*/li）后换行，inline 拼文本，折叠连续空白 | 低（~80 行） |
| `md` | turndown 子集：h1-h6→`#`、a→`[text](href)`、ul/li→`-`、strong→`**`、code→`` ` ``、pre→```` ``` ````、table→GFM 表格 | 中（~200 行） |
| `links` | 遍历 `<a href>`，`Url::join(base)` 绝对化，去重，输出 `文本 → url` | 低（~40 行） |

## 噪声过滤（Firecrawl 42 选择器 + force-include）

```rust
const EXCLUDE_NON_MAIN_TAGS: &[&str] = &[
    "header", "footer", "nav", "aside",
    ".header", ".top", ".navbar", "#header", ".footer", ".bottom", "#footer",
    ".sidebar", ".side", ".aside", "#sidebar",
    ".modal", ".popup", "#modal", ".overlay",
    ".ad", ".ads", ".advert", "#ad",
    ".lang-selector", ".language", "#language-selector",
    ".social", ".social-media", ".social-links", "#social",
    ".menu", ".navigation", "#nav",
    ".breadcrumbs", "#breadcrumbs",
    ".share", "#share", ".widget", "#widget",
    ".cookie", "#cookie",
];
const FORCE_INCLUDE: &[&str] = &["#main", "article"];  // 保护主内容（含后代）
```

**空内容兜底**（Firecrawl 验证的关键）：`only_main_content` 过滤后输出为空 → 自动回退用完整内容重跑一次。

## 实现步骤

### M59.1 extractor crate 骨架 + query_all + html/text/links 格式
- 新建 `crates/extractor/`，注册到 workspace `Cargo.toml`。
- `FetchOptions` struct + `run_extract(tree, opts)` 入口。
- `selector.rs::query_all`：`Selector::parse` + `tree.traverse`。
- `format_html.rs` / `format_text.rs` / `format_links.rs` 三种格式。
- 单元测试：fixture HTML → 期望输出。

### M59.2 噪声过滤（clean.rs）
- `EXCLUDE_NON_MAIN_TAGS` + `FORCE_INCLUDE`。
- 空内容兜底逻辑。
- 单元测试：含 nav/footer/article 的 fixture → 去噪后只剩 article；空兜底回退。

### M59.3 Markdown 格式（format_md.rs）
- turndown 子集实现（标题/链接/列表/强调/代码/表格）。
- URL 绝对化（md/links 共用）。
- 单元测试：各元素 → markdown 映射。

### M59.4 CLI 集成（cli crate）
- `Cmd::Fetch` 变体 + clap 参数定义。
- `run_cmd` 加 `Cmd::Fetch { ... }` 分支：fetch_with_jar → parse → run_scripts_with_base → extractor::run_extract → stdout/--json。
- 集成测试：wiremock 本地服务 + 各 format 组合。

### M59.5 真实站点对标验收（L3）
- 见下「验收与对标」。

## 验收与对标（不比别人差）

### L1 单元测试（extractor crate）
- 每种 format：fixture HTML → 期望字符串。
- 噪声过滤：去掉 nav/footer，保留 article；force-include 保护；空兜底回退。
- selector 过滤：`table tr` 只返回表格行。

### L2 集成测试（cli）
- `fetch` 各 `--format` 组合，wiremock 本地服务返回固定 HTML。
- `--selector` 过滤、`--only-main-content false`、`--json` 输出。

### L3 真实站点对标（核心：不比别人差）

> 用**同一批测试站点**，对比 `browser fetch` vs `xbrowser scrape`（Playwright）的输出质量。
> 判断标准：主内容提取完整性、噪声去除效果、markdown 可读性。

**对标站点矩阵**（覆盖不同类型）：
| 站点 | 类型 | 预期 | 对标点 |
|---|---|---|---|
| `https://seo.box/referring/` | CSR（Vercel brotli，刚修复） | 表格数据完整 | SPA + 压缩站点 |
| `https://example.com/` | 静态 | 纯文本 | 基线 |
| `https://docs.firecrawl.dev/` | 文档站（多 nav/footer 噪声） | 主文档提取干净 | 噪声过滤效果 |
| `https://bark.day.app/#/tutorial` | SPA（hash 路由） | 教程正文 | xbrowser 文档里的经典用例 |
| 一篇博客文章 | 长文 | 完整 markdown | md 转换质量 |

**对标方法**：
```bash
# 我们的
browser fetch <url> --format markdown > /tmp/ours.md
# xbrowser（已有，Playwright 基线）
xbrowser scrape <url> --format markdown > /tmp/xbrowser.md
# 对比
diff /tmp/ours.md /tmp/xbrowser.md
# 人工判断：主内容是否都在？噪声是否都去了？md 是否可读？
```

**通过标准**：
- ✅ 主内容（正文/数据）完整提取，不比 xbrowser 少关键信息。
- ✅ 噪声（nav/footer/ad）去除效果不输 xbrowser。
- ✅ markdown 语法正确，可读。
- ⚠️ 允许格式细节差异（如换行风格），但信息量不输。

## YAGNI 边界（首版明确不做）

| 不做 | 原因 | 何时考虑 |
|---|---|---|
| `map` / `crawl` 多页爬取 | 首版只 scrape 单页，做扎实 | fetch 稳定后 |
| `actions` 自动化（点击/填表/截图） | 需 CDP 交互层，复杂度高 | M60+ |
| LLM 内容清洗（onlyCleanContent） | 依赖外部 LLM | 非本项目宗旨（无 AI 依赖） |
| 图片提取 / OG 元数据 | 以后作为新 format 扩展 | 按需 |
| HTTP 服务模式 | 用户决策「以后再说」 | 单机 CLI 稳定后 |
| sitemap.xml 解析 | 属 map 命令 | map 命令时 |

## 里程碑脉络

```
M58 ✅ reqwest brotli/gzip 解码（Vercel 压缩站可爬）—— 本命令的前置依赖
M59 🚧 fetch 命令 + extractor crate（本文档）
M60+ ⏳ map/crawl/actions（待 fetch 稳定）
```

## 风险

1. **Markdown 转换质量**：自研 turndown 子集可能不如 turndown 库完整。缓解：首版覆盖 80% 常见元素（标题/链接/列表/代码/表格），边界 case 记录为已知局限。
2. **css-engine 不支持属性选择器**：`a[href]` 这类用不了。缓解：`--selector` 文档标注限制，links 格式内部直接遍历 `<a>` 不依赖属性选择器。
3. **对标可能暴露差距**：某些站点 xbrowser（真 Chrome）能渲染我们 boa 渲染不了。缓解：记为已知局限，标注「JS 引擎差异」，不硬刚（符合 M-cls 决策）。
