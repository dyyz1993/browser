# M48 Puppeteer E2E — 全链路打通报告

> 状态：**已完成**（2026-06-17 真实 Puppeteer 25.1 实测，e2e 3/3 全过）
> 配套计划：[docs/plans/M-cls-spa.md](../plans/M-cls-spa.md)（cls.cn SPA 渲染 + 内存护栏，已交付）。

## 定位
本项目是**通用 SPA 爬虫浏览器 + 兼容 CDP**。本里程碑用真实 Puppeteer 驱动
CDP server，验证 CDP 握手 + 导航 + 截图 + DOM 取数 + evaluate 全链路。

## 最终结果（实测，`run-all.js` 3/3 全过）

```
[basic: connect + navigate + screenshot] 5/5 passed
[evaluate: Runtime.evaluate (boa)]        4/4 passed
[dom-via-evaluate]                        4/4 passed
=== run-all: 3/3 scenarios passed ===
```

| 能力 | 状态 | 验证 |
|---|---|---|
| `/json/version` 发现 + WS 握手 | ✅ | basic.js connect |
| `browser.newPage()` → createTarget + session | ✅ | 全部场景 |
| `Page.navigate` + lifecycle 事件 | ✅ | goto(example.com) |
| `page.title()`（走 isolated world） | ✅ | "Example Domain" |
| `page.screenshot()` → PNG | ✅ | 64628 字节 base64 |
| `page.url()` | ✅ | https://example.com |
| `page.evaluate(() => 2+2)`（箭头函数） | ✅ | 4 |
| `page.evaluate(() => document.title)` | ✅ | 读真实 DOM |
| `page.evaluate(() => location.href)` | ✅ | navigation shim |
| `page.evaluate(() => document.querySelector('h1').textContent)` | ✅ | "Example Domain" |
| `page.evaluate(() => document.querySelectorAll('a').length)` | ✅ | 1 |

## 环境搭建（已验证可用）
```bash
# node 经 nvm
export NVM_DIR="$HOME/.nvm"; . "$NVM_DIR/nvm.sh"; nvm use 20
cd tests/e2e && npm install   # puppeteer-core 25.1（不下载 Chromium）
# 起服务
cd ../.. && ./target/release/browser cdp --port 9223
# 跑测试（另一终端）
cd tests/e2e && CDP_PORT=9223 node run-all.js
```
e2e 脚本：`tests/e2e/{_helper,basic,evaluate,dom-via-evaluate,dom,run-all}.js`。
`.gitignore` 已忽略 `tests/e2e/node_modules/`。

## 本次交付的 6 个真实修复

### 修复 1：`target_object` 缺 `targetId` 字段 ✅
CDP 的 `Target.targetInfo` 用 **`targetId`**（不是 `id`）。puppeteer 的
`TargetManager` 读 `event.targetInfo.targetId` 建 target；缺这字段 → target 永不创建。
`discovery.rs` 的 `target_object` 现同时含 `id`（给 /json HTTP 发现）和
`targetId`（给 CDP targetInfo）。

### 修复 2：`createTarget` 只发 `targetCreated`（不发重复 attachedToTarget）✅
早期为加速 newPage 同时发了 `targetCreated` + `attachedToTarget`，但 puppeteer 的
autoAttach 流程会**自己**发 `attachedToTarget` 并建 session，重复事件导致 session
被覆盖、回调孤儿化、newPage() 永久挂起。`createTarget` 现只发 `targetCreated`。

### 修复 3：catch-all 扩到覆盖 puppeteer 初始化期所有 `*.enable` ✅
puppeteer 页面初始化发大批 `Audits/Log/WebMCP/Storage/Performance/...enable`。
现扩到 ~25 个域统一 no-op ack（避免 -32601 让批量 await 卡住）。
`Emulation` 域的 `_` arm 也从 `MethodNotFound` 改为 `ok_empty`（puppeteer 发很多
`Emulation.set*` 必须 ack，否则握手失败）。

### 修复 4：`Runtime.enable` 发 `executionContextCreated`（main world）✅
puppeteer 的 FrameManager 靠它把 context 绑到 frame（否则 mainWorld 永不就绪）。
发单 main-world context（id=1，frameId=主 frame，isDefault=true）。

### 修复 5：isolated world 的 `name` 用 puppeteer 传的 `worldName` ✅（关键卡点）
`Page.createIsolatedWorld` 必须返回 `{executionContextId}` **并**发
`Runtime.executionContextCreated`。但 context 的 `name` 字段最初设成空串——
puppeteer 的 FrameManager 用 `contextPayload.name === '__puppeteer_utility_world__<ver>'`
把 context 映射到 PUPPETEER_WORLD（即 `isolatedRealm()`）；名字不匹配 context 被
忽略 → `page.title()`（跑在 isolatedRealm 上）永久卡住。
现用 puppeteer 传来的 `worldName` 参数填 name，title() 立即解通。

### 修复 6：`Runtime.callFunctionOn` + `eval_in_tree`（evaluate 接真实 DOM）✅
puppeteer 的 `page.evaluate`/`title()` 实际走 **`Runtime.callFunctionOn`**（不是
`Runtime.evaluate`），在指定 executionContext 里执行一个函数声明。
- 新增 `Runtime.callFunctionOn` handler：构造 `(functionDeclaration)(args)` IIFE eval。
- **关键**：原 evaluate 用空 `JsRuntime::new()`（无 DOM），`document.title` 报
  "document is not defined"。js-runtime 新增 `eval_in_tree(tree, url, expr)`——
  构建装好全部 shims（document/window/navigator/...）的 ctx 并绑定到当前页面 DOM tree。
  evaluate/callFunctionOn 现在能读真实 `document`/`location`。
- 抽出 `build_shimmed_context()` 让 `run_scripts_with_base` 与 CDP eval 共享 shim 安装逻辑。

## 已知边界（非协议 bug，是 boa 引擎限制）

### `page.$()` / `page.$$()` 不支持
puppeteer 的 `page.$()`/`$$()` 内部用 `QueryHandler.queryOne`，发送的函数体含
**ES2018+ 语法**：`async function*`（异步生成器）、`for await...of`、`using`
（ disposable）、`yield*`。boa 0.20 不支持这些 → "not a callable function"。
`dom.js` 测试即此限制的对照（保留作记录）。

**爬虫不受影响**：SPA 爬虫用 `page.evaluate(() => document.querySelector(...))`
手写取数逻辑（简单函数，boa 完全支持）——见 `dom-via-evaluate.js`（4/4 全过）。
puppeteer 的 ElementHandle 体系是给自动化测试用的，爬虫不需要。

## 工程状态
- ✅ 代码编译、fmt、clippy（cdp/js-runtime/cli）0 warnings。
- ✅ browser-cdp 80 tests pass、browser-js-runtime 135 tests pass。
- ✅ e2e `run-all.js` 3/3 scenarios 全过（12/12 断言）。
- ✅ 6 个 CDP 修复落地，均为正向改进。

## 教训
1. CDP 兼容不只是"方法覆盖齐全"——puppeteer 对**事件序列 + 字段名**有严格期待
   （如 isolated world 的 `name` 必须等于 `worldName`）。
2. `page.evaluate` 实际走 `Runtime.callFunctionOn`，且必须能访问真实 DOM——
   evaluate 的 ctx 必须绑定到当前页面的 tree，不能是空壳。
3. boa 0.20 的 ES6 支持（箭头函数、let/const、模板字符串）够跑爬虫手写的
   evaluate 逻辑，但跑不了 puppeteer 内部的 ES2018+ 查询函数。这是引擎边界，
   想突破需升级 boa 或加 ES6→ES5 转译层。
