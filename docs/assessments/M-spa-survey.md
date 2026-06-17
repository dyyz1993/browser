# SPA 覆盖面批量测试报告：browser fetch 能爬多少真 SPA？

> 日期：2026-06-17
> 调研脚本：[`tests/benchmarks/spa_survey.sh`](../../tests/benchmarks/spa_survey.sh)
> 方法：20 个候选 SPA 站点，curl（证 CSR）/ browser fetch（我们）/ Chrome headless（真渲染）三方对比。
> 判定：`curl_kw=0`（curl 抓不到 = CSR 铁证）+ `ours_kw=1`（我们抓到 = JS 跑通）才算 ✅ SPA 成功。

## 一句话结论（诚实版）

> **20 个候选站点中，真正纯 CSR 的只有 5 个，其中 3 个 demo 站已 404，2 个因 boa 引擎
> 限制（docsify 抛错 / boa opcode panic）失败。我们的 SPA 渲染对真实公网 CSR 站点
> 的成功率：0/2。** 但更重要的发现是：**测试集本身有严重问题**——15 个"误入 SSR"
> 里，一半是关键词选错（命中 title/footer），一半是站点有 SSR 兜底。

## 调研数据（20 站点三方对比）

| 组 | tag | curl_kw | ours_kw | chrome_kw | 判定 |
|----|-----|---------|---------|-----------|------|
| main | todomvc-react | 1 | 1 | 1 | ⚪ 误入SSR（关键词命中 footer） |
| main | todomvc-vue | 1 | 1 | 1 | ⚪ 误入SSR（关键词命中 footer） |
| main | todomvc-preact | 1 | 1 | 1 | ⚪ 误入SSR（关键词命中 footer） |
| main | todomvc-angular | 0 | FAIL(1) | 0 | ⚠️ 站点 404 |
| main | realworld-ng | 0 | FAIL(1) | 0 | ⚠️ 站点 404 |
| main | realworld-react | 0 | FAIL(1) | 0 | ⚠️ 站点 404 |
| main | hn-vue | 0 | FAIL(1) | 0 | ⚠️ 站点 404 |
| main | caniuse | 1 | 1 | 1 | ⚪ 误入SSR（有 SSR 兜底） |
| main | bundlephobia | 1 | FAIL(1) | 1 | ❌ SSR+JS报错（退出码1） |
| main | bark | 1 | 0 (1B) | 1 | ❌ 真 CSR，docsify JS 抛错 |
| main | jsonplaceholder | 1 | 1 | 1 | ⚪ 有 SSR 内容 |
| main | npmtrends | 1 | FAIL(1) | 1 | ❌ SSR+JS报错（--no-js 可救） |
| stress | juejin | 1 | FAIL(1) | 1 | ❌ 有 SSR，JS 崩（--no-js 可救） |
| stress | cls | 1 | FAIL(1) | 1 | ❌ 有 SSR，JS 崩（--no-js 可救） |
| stress | svelte-repl | 1 | 1 | 1 | ⚪ 误入SSR（关键词命中 title） |
| stress | vue-playground | 1 | 0 (1B) | 1 | ❌ 真 CSR，boa 渲染空 |
| stress | solid-playground | 1 | 1 | 1 | ⚪ 误入SSR（关键词命中 meta） |
| stress | firecrawl-docs | 1 | 1 | 1 | ⚪ 有 SSR（212MB/慢） |
| stress | coingecko | 0 | FAIL(1) | 1 | ❌ 误判：实为 HTTP 000 网络问题 |
| stress | owid-grapher | 1 | FAIL(101) | 1 | 🚨 真 CSR，**boa panic** |

## 失败根因分类（5 类）

### 类型 1：候选站点已下线（4 个）—— 测试集陈旧
- **realworld-ng / realworld-react**：`demo.realworld.io` 和 `react-redux.realworld.io`
  均返回 **HTTP 404**。RealWorld 项目官方 demo 已迁移或下线。
- **todomvc-angular**：`todomvc.com/examples/angularjs/` 返回 404（AngularJS 版已移除）。
- **hn-vue**：`hnpwa-vue3.netlify.app` 返回 404（部署已删除）。
- **结论**：开源 SPA demo 站维护性差，不适合作为长期回归测试集。应改用本地 fixture。

### 类型 2：关键词误判（误入 SSR，实为 CSR）（~6 个）—— 测试方法学问题
- **todomvc-react/vue/preact**：关键词 "todo" 命中了 TodoMVC 的**静态 footer 文案**
  （`"Double-click to edit a todo"`），而非 JS 渲染的实际 todo 列表项。这些站**确实是
  CSR**（实际 todo 项 curl 拿不到），但被误判为 SSR。
- **svelte-repl / solid-playground**：关键词命中了 `<title>` 或 meta 标签。
- **结论**：SPA 测试的关键词必须选**只有 JS 渲染后才会出现的内容**（如动态数据、用户
  生成内容），不能用站点名/框架名/通用词。

### 类型 3：站点本身有 SSR 兜底（~5 个）—— 站点选错
- **juejin / cls / firecrawl-docs / npmtrends / caniuse**：这些站 curl 能拿到大量内容
  （juejin 41KB、firecrawl 1MB），说明有 SSR/SSG 预渲染。虽然也用 JS 增强，但不是"纯 CSR"。
- **juejin/cls/npmtrends 的 `--no-js` 兜底成功**（kw=1），证明 SSR 内容有效。
- **结论**：现代主流站点普遍有 SSR 兜底（SEO 需求），**纯 CSR 站点在网上很少见**。

### 类型 4：真 CSR + boa 引擎失败（2 个）—— 项目硬限制
- **bark.day.app**：用 docsify（运行时 markdown 渲染）。boa 跑了 6 个脚本，但
  `docsify@4` 和 `cloudflareinsights` 抛 `TypeError: cannot convert 'null' or 'undefined'
  to object`，页面渲染空。输出仅 1 字节。
- **owid-grapher**：`HTMLScriptElement is not defined`（我们的 DOM 桥缺这个接口）→
  **boa 引擎 panic**（`boa_engine-0.21.1/src/vm/opcode/define/mod.rs:82`，退出码 101）。
  这是 boa 内部崩溃，非我们错误处理能兜住。
- **结论**：这两类失败是已知的 boa 引擎限制（M59/M-cls 已记录），符合预期。

### 类型 5：网络环境问题（1 个）—— 非项目责任
- **coingecko**：curl 返回 **HTTP 000**（连接失败，疑似 DNS 污染/被墙）。
  被脚本误判为"JS 失败"，实为网络连不上。与 M59 的 HN/Wikipedia 同类。

## 修正后的真实结论

| 指标 | 值 | 说明 |
|------|-----|------|
| 候选站点 | 20 | |
| 有效纯 CSR 站点 | **2** | bark, owid（其余 404/有 SSR/网络问题/关键词误判） |
| SPA 渲染成功 | **0 / 2** | 两个都因 boa 引擎限制失败 |
| demo 站 404 | 4 | 测试集陈旧 |
| 有 SSR 兜底的站 | ~5 | 现代 SPA 普遍 SSR |
| 关键词误判 | ~6 | 测试方法学问题 |

## 关键发现（比"成功率"更重要）

### 发现 1：纯 CSR 站点在公网上很少
现代主流站点为 SEO 普遍加了 SSR/SSG 兜底（Next.js/Nuxt.js 默认 SSR、docsify 之外
的文档站多为静态生成）。**真正"curl 拿不到、必须 JS 渲染"的纯 CSR 站点，在主流公网
站点中占比很低**。这与项目 GOALS 的 SPA 爬虫定位有张力——需要重新定义"SPA 爬虫"
的真实目标场景。

### 发现 2：测试集需要重构为本地 fixture
公网 SPA demo 站维护性差（4/20 已 404），且无法精确控制 CSR 程度。项目自己定义的
5 类 SPA 模式（M40/M60 文档：async-data/route-switch/lazy-load/dynamic-form/js-redirect）
**尚未实现为 fixture**——这才是可控、可回归的 SPA 测试集。

### 发现 3：boa 引擎 panic 是未处理的失败模式
owid 触发的 `boa_engine panic`（退出码 101）直接让 `fetch` 进程崩溃，没有触发
`render-url` 的内存护栏自愈路径（因为 `fetch` 不走 sandbox 子进程）。
**对于公网未知站点，`fetch` 命令有 panic 风险**——建议公网爬取走 `render-url` 的
sandbox 路径，而非 `fetch` 的 in-process 路径。

### 发现 4：`--no-js` 兜底是有效的实用策略
juejin/cls/npmtrends 的 `--no-js` 兜底全部成功（kw=1）。**对于有 SSR 兜底的"伪 CSR"
站点，`--no-js` 比硬刚 JS 更实用**——这验证了 M59 `fetch --no-js` 设计的价值。

## 下一步建议（按爬虫价值排序）

1. **重构 SPA 测试集为本地 fixture**（最高价值）
   实现项目自己定义的 5 类 SPA 模式 HTML（async-data/route-switch/lazy-load/dynamic-form/
   js-redirect），作为可控回归集。公网 demo 站只做"真实世界抽检"，不做回归。

2. **关键词选取改为"JS 渲染后才有的动态内容"**
   如 TodoMVC 应检测 "todo" 列表项的**实际内容**（先 `add a todo` 再检测），而非
   检测 "todo" 这个词本身。

3. **`fetch` 命令增加 panic 防护**
   对于公网未知站点，考虑走 `render-url` 的 sandbox 子进程路径，避免 boa panic
   杀死整个进程。或在 `fetch` 里 catch boa panic。

4. **诚实定位 SPA 爬虫的真实场景**
   纯 CSR 站点在公网少见 → SPA 爬虫的核心价值可能在于：(a) 有 SSR 但 JS 增强的
   站点（如 juejin，`--no-js` 能救）；(b) 内网/私有 SPA；(c) API 驱动的数据看板。
   需要在 GOALS.md 里明确这个定位。

## 复现

```bash
cargo build --release -p browser-cli
./tests/benchmarks/spa_survey.sh
# 产物：${TMPDIR}/spa-survey-*/spa_survey.tsv
```
