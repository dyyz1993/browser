# M69 — 动态 Script 执行（createElement + appendChild）

> 日期：2026-06-22
> 触发：用户要求爬取 `https://open.bigmodel.cn/pricing`，渲染失败
> 修复：`document.createElement("script") + appendChild(s)` 触发 fetch + eval

---

## 一、背景：一次「不是反爬」的纠错

用户要求爬取 `open.bigmodel.cn/pricing`，渲染出 113 字符的 SPA fallback 空壳
（"We're sorry but 智谱AI开放平台 doesn't work properly without JavaScript enabled"）。

**第一反应（错误）**：页面引用了 `alicdn.com` 的 `interfaceacting.js` / `antidom.js`，
我凭名字推测是「阿里风控脚本」，归因为反爬。

**用户质疑**：「你怎么确定他是反爬？你告诉我为什么？」

**验证后的事实**：
- `curl` 这两个 URL → **content-length: 0，404 死链接**，浏览器根本不会执行
- 页面**没有任何反爬/验证码/签名校验**
- JS 报错日志**没有任何风控相关内容**

**结论**：之前「反爬」的判断**没有证据，是臆测**。真正的失败原因见下文。

---

## 二、根因定位（基于证据）

### 根因 A（已修复）：动态 script 不执行

webpack/vite 等前端工程化站点，把业务代码打包成独立 chunk，在运行时由
`runtime.js` 通过 `document.createElement("script") + head.appendChild(s)` **动态加载**
（用于 code-splitting / 路由懒加载）。

我们的 `appendChild`（`bridge.rs` / QuickJS `scripts.rs`）只做 DOM 树移动，
**完全不触发 script 的 fetch+eval**。

**最小复现**：
```html
<div id="app">BEFORE</div>
<script>
  var s = document.createElement('script');
  s.textContent = 'document.getElementById("app").innerHTML = "AFTER";';
  document.body.appendChild(s);
</script>
```
修复前：渲染输出 `BEFORE`（动态 script 没执行）。
修复后：渲染输出 `AFTER_DYNAMIC_SCRIPT_WORKED`。

### 根因 B（引擎上限，不在 M69 scope）：zod/Vue 递归

`open.bigmodel.cn/pricing` 的**主 chunk**（app/vue/elementUI/libs）是 HTML 里的
**静态 `<script src>`**（非动态加载）。这些 chunk eval 时报两个错误：

1. `not a function`（zod schema 库附近，`aeyaNG` 模块）
2. `RuntimeLimitError: reached the maximum number of recursive calls`（zod 的
   schema 解析递归超过 QuickJS 的 256 上限）

这属于 **QuickJS 引擎上限**（AGENTS.md 第十章「boa 天花板不硬刚」），不是动态
script 问题。M69 修复了根因 A，但这个站卡在根因 B。

---

## 三、实现设计

### 机制：JS shim 拦截 + Rust 桥 eval（两者结合）

1. **JS shim 层**（`appendChild`）检测到 `script` 标签时：
   - 读 `src`（外链）→ `__fetchSync(src)` 同步 fetch（相对 URL 自动解析）
   - 或读 `textContent`（inline）→ 直接拿代码
   - `__enqueueDynamicScript(code)` 入 Rust 队列
   - `setTimeout(0)` 注册 onload wrapper
2. **event loop pump** 每轮：先 drain 动态 script → `engine.eval_safe(code)` → 再 drain timer

### 关键设计决策

| 决策 | 理由 |
|------|------|
| eval 走 Rust `eval_safe` 而非 JS 间接 eval | GC 安全（AGENTS.md 第 13 条）；CaughtError 在 ctx.with 闭包内 drop |
| 同步 `__fetchSync` 阻塞 | 保证 webpack chunk loader 的 Promise.resolve 顺序正确 |
| eval 推迟到 event loop（setTimeout 语义） | 符合 HTML5：动态 script 执行不阻塞当前同步 JS 栈 |
| 队列放 bridge.rs 顶层（非 qjs_bridge mod） | 不受 quickjs feature 门控，boa/QuickJS 共用 |
| src/textContent 双 fallback 读取 | QuickJS shim 缺反射属性系统，`s.src=x` 只设 JS 属性不写 DOM attrs |

### 时序：动态 script drain 必须在 timer drain 之前

appendChild 同时入队 script 代码 + onload 的 `setTimeout(0)`。pump 必须先 eval
script（设 `window.__loaded` 等），onload 回调读这些状态才正确。

**踩过的坑**：首轮 pump 原顺序是 `run_jobs → __drainDueTimers → drain_dynamic_scripts`，
导致 onload 跑在 eval 之前（读到 undefined）。修成 `drain_dynamic_scripts` 在
`__drainDueTimers` 之前。

---

## 四、改动清单（5 文件）

| 文件 | 改动 |
|------|------|
| `bridge.rs` | 顶层 `PENDING_DYNAMIC_SCRIPTS` thread_local + `enqueue/drain_dynamic_scripts`；`install` 注册 `__enqueueDynamicScript`（boa） |
| `engine_quickjs.rs` | `install_bridge` 注册 `__enqueueDynamicScript`（QuickJS） |
| `scripts.rs` | QUICKJS_ELEMENT_SHIM appendChild 加 script 拦截；`drain_and_eval_dynamic_scripts` helper；pump 循环接入 drain；`has_ts_syntax` 去门控 |
| `element_shim.rs` | boa 版 appendChild 镜像 script 拦截 |
| `tests/fixtures/spa/` | 3 个新 fixture（inline/onload/noscript） |

---

## 五、验证

### 冒烟测试（本地 HTTP server，链式加载）

三层链式（模拟 webpack runtime→chunkA→chunkB→chunkC）：
```
入口 inline → createElement+appendChild chunkA.js
chunkA.js  → createElement+appendChild chunkB.js
chunkB.js  → createElement+appendChild chunkC.js
chunkC.js  → app.innerHTML = "CHAIN_COMPLETE"
```
QuickJS + boa 双引擎均输出 `CHAIN_COMPLETE`，`4 script(s) executed`。

### L1 单元测试（4 个，全过）

`bridge::m69_dynamic_script_tests`：drain_empty / enqueue_then_drain_fifo /
drain_clears_queue / enqueue_after_drain。

### L2 集成测试（6 个，全过）

`integration_dynamic_script.rs`：inline 执行 / onload 时序 / 外链 fetch+eval /
多层链式（webpack 式）/ onerror 回调 / 非 script 不误触发。

### 门禁

- fmt ✅ 0 diff
- clippy ✅ 0 warnings
- **814 passed**（baseline 804 + 4 单元 + 6 集成），0 failed

### 5 站回归（零退化）

M68 vs M69 CLI fetch text 字符数**逐站完全一致**：

| 站点 | M68 | M69 |
|------|----:|----:|
| baidu | 251,505 | 251,505 |
| react.dev | 7,530 | 7,530 |
| vuejs.org | 1,354 | 1,354 |
| nuxt.com | 6,126 | 6,126 |
| svelte.dev | 1,938 | 1,938 |

（这 5 站的主 chunk 都是静态加载，不触发动态 script；改动是纯增量。）

---

## 六、open.bigmodel.cn/pricing 的最终状态

动态 script 功能修复后，该站**仍然渲染失败**（0 字符），原因是**根因 B**
（zod/Vue 的递归超过 QuickJS 上限）。这是引擎限制，不在 M69 scope。

按 AGENTS.md 第十章，这类站走 `--no-js` 兜底或标注需 Chrome：
```bash
# 拿静态壳（SSR fallback）
browser fetch https://open.bigmodel.cn/pricing --no-js

# 用 Chrome 拿完整渲染
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --virtual-time-budget=10000 --dump-dom \
  "https://open.bigmodel.cn/pricing"
```

---

## 七、教训

1. **不要凭名字/直觉下「反爬」判断**——必须有证据（下载脚本看内容、查报错日志）。
   本次「antidom.js 看起来像反爬」的臆测被证伪（死链接）。
2. **度量与直觉矛盾时，先怀疑度量管道**（M68 教训重现）：动态 script 功能修复后，
   先用最小复现验证功能本身，再测真实站点——避免把引擎上限误归为功能 bug。
3. **功能边界要分清**：M69 修复「动态 script 加载」，不解决「QuickJS 跑不动 zod」。
   一个站渲染失败可能有多个独立根因，逐个定位而非打包处理。
