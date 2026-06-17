# M60 规划：JS 覆盖率提升到 80% 路线图

> 日期：2026-06-17
> 状态：规划中（待决策）
> 前置：M59 fetch 命令 + 覆盖面调研（实测 58-80% 覆盖率，boa 0.20 为瓶颈）

## 一句话结论

> **单押 boa 0.20 硬刚，到不了 80%。务实路径是「分层混合」：
> ① boa 升 0.21（免费拿到 async/await）+ ② SSR 数据提取层（抠 `__NEXT_DATA__` 等，
> 零成本拿下大票 SSG/SSR 站）+ ③ 补缺失 Web API（setInterval/Symbol/Map/Set）。
> 纯 CSR 无 SSR 兜底的硬骨头留给 `--no-js` 或未来 Chrome CDP 兜底。**

## 现状基线（M59 实测）

| 指标 | 数据 | 来源 |
|------|------|------|
| boa 版本 | 0.20（无 async/await 运行时） | js-runtime/Cargo.toml:17 |
| ES 一致性 | ~89.92% | boa 0.20 conformance |
| 实测覆盖率 | 58%（12 站 7 持平 Chrome） | M59-coverage-survey.md |
| 有效覆盖率（排除网络/反爬） | 80%（8/10 可达站） | 同上 |
| 头号杀手 | async/await 运行时缺失 | 掘金/bark 等纯 SPA 全挂 |
| 已实现 Web API | DOM/fetch/XHR/WS/Storage/Nav/Timer 子集 | compat_shim.rs（~1700 行） |

## 关键发现（颠覆性）

### 发现 1：boa 0.21（2025-10）已完整落地 async/await
- PR #2158 在 v0.21 把 async/await 运行时打通（desugar + microtask drain）
- ES 一致性从 ~90% → **94.12%**
- **升级 boa 是免费的最大收益**：头号杀手从阻塞变可用
- 代价：API 可能有 breaking change（需测），但收益远大于成本

### 发现 2：async/await 缺失影响 70-90% 纯 CSR 站
- 现代生产 bundle 几乎 100% 含 async/await（browserslist 不再降级 ES5）
- 0.20 上：async 函数定义能过，但 await Promise 永远 pending → 页面停骨架屏
- 升 0.21 后：这 70-90% 的纯 CSR 站有望拿到渲染后 DOM

### 发现 3：框架硬依赖矩阵
| 框架 | async/await | Proxy | Symbol | Map/Set |
|------|-------------|-------|--------|---------|
| Vue 3 | hydrate 常用 | **硬依赖**（响应式） | 是 | 是 |
| React 18/19 | 业务必用 | 不依赖 | **硬依赖** | **硬依赖** |
| Svelte 5 | 常用 | **硬依赖**（runes） | 是 | 是 |
| Angular | 大量 | 是 | 是 | 是 |

> ES6 级特性（Proxy/Symbol/Map/Set）boa 0.20 基本都有。**真杀手是 async/await 运行时**。

### 发现 4：现有 compat_shim 的 fallback 不是真兼容
- `Reflect.construct`/`Array.from` 已 try/catch 包装，但 fallback 返回 `[]`/`{}`
- 对真跑业务逻辑的 SPA，fallback 破坏语义（拿到空数组后续逻辑继续错）
- 这解释了掘金为什么有 compat 包装还挂

## 务实路径：分层混合架构

```
请求进来
   │
   ├─ ① SSR 数据提取层（新增，ROI 最高）
   │   扫 HTML 找 __NEXT_DATA__ / __NUXT_DATA__ / __APOLLO_STATE__ /
   │   window.__INITIAL_STATE__ / data-server-rendered
   │   命中 → 直接抽数据注入 DOM，结束（覆盖 Next/Nuxt/Astro/Remix 等 SSG/SSR）
   │
   ├─ ② boa 0.21 JS 渲染（升级 + 补 API）
   │   SSR 没命中 → 跑 JS（async/await 可用了）
   │   补缺失 API：setInterval/Symbol.iterator/queueMicrotask/Event/...
   │   覆盖：轻 JS 站 + 升级后能跑的中等 SPA
   │
   └─ ③ 兜底（现有 --no-js + 未来 Chrome CDP）
       JS 跑空/失败 → --no-js 取 SSR 壳（gov.cn 模式）
       纯 CSR 无 SSR（掘金/bark）→ 标注「需 Chrome」，未来接 CDP
```

### 为什么不直接上 Chrome/QuickJS？

| 方案 | 体积 | 内存 | 覆盖率 | 代价 |
|------|------|------|--------|------|
| **boa 0.21 升级**（推荐先做） | 不变（13MB） | 不变 | +20-30% | 升级风险 + 补 API |
| QuickJS via rquickjs | +1-2MB | 略增 | 语言完整 | FFI unsafe + 重写 DOM 桥 |
| deno_core（V8） | +50MB+ | +300MB+ | ~100% | 违背「低内存」宗旨 |
| Headless Chrome | +300MB | +300-800MB | 100% | 违背「轻量」定位 |

**结论**：我们项目的核心卖点是**13MB 单文件 + 低内存**。上 Chrome/V8 等于自废武功。
boa 0.21 升级 + SSR 提取层是**保住卖点的同时最大化覆盖率**的路径。

## 分阶段实施（M60-M63）

### M60：boa 0.20 → 0.21 升级 ✅ 已完成
**目标**：async/await 运行时可用，ES 一致性 90% → 94%。

**结果（2026-06-17 验证）**：
- ✅ 升级成功，breaking change 极小（仅 `JsValue::String` → `JsString::from().into()` + `run_jobs()` 返回 Result）
- ✅ 全量测试 739 passed / 0 failed（零回归）
- ✅ **async/await 真正可用了**：最小用例 `async function + await Promise` 正确渲染
- ✅ 掘金从 1 字节（超时/空）→ 2717 字节（拿到导航 + 链接）
- ⚠️ 但掘金文章列表仍未出来（需登录态/更复杂 API fetch，非 async/await 问题）
- ⚠️ bark.day.app 仍 1 字节（纯 CSR 无 SSR 兜底，boa 能跑但站点无初始数据）

**结论**：async/await 解锁了"能跑"，但不等于"能拿到数据"——纯 CSR 无 SSR 站点的
数据获取还需要 M61（SSR 提取）或登录态支持。覆盖率提升需要 M61 配合。

- 升级 `boa_engine = "0.21"`
- 跑全量测试，修 breaking change
- **验收**：掘金/bark 等纯 SPA 能否拿到渲染后内容（若能，覆盖率直接跳涨）
- **风险**：boa 0.21 API 可能变（Context 构造、runtime_limits 接口等）
- **回退**：若 breaking 太大，git revert，转 M61 先做 SSR 提取

### M61：SSR 数据提取层（ROI 最高，零 JS 成本）
**目标**：不跑 JS，直接抠框架注入的全局数据。

- 新增 `crates/extractor/src/ssr_data.rs`：
  - 扫 `<script id="__NEXT_DATA__">` → JSON
  - 扫 `window.__NUXT__=` / `__NUXT_DATA__` → JSON
  - 扫 `window.__INITIAL_STATE__`（小红书等）→ JSON
  - 扫 `window.__APOLLO_STATE__`（GraphQL）→ JSON
  - 扫 Vue `data-server-rendered="true"` 属性的内容
- 从 JSON 提取正文/列表数据，注入 DOM
- **验收**：Next.js/Nuxt.js 站点不跑 JS 也能拿到正文
- **覆盖增益**：SSG/SSR 站点（占现代 Web 相当比例）

### M62：补缺失 Web API（补 compat_shim 短板）
**目标**：消除 compat fallback 的语义破坏。

按优先级补：
1. `setInterval`/`clearInterval`（文档已声明但代码缺）
2. 真 Base64 `atob`/`btoa`（现实现是错的，破坏 JWT）
3. `queueMicrotask`/`MutationObserver`（hydration 高频依赖）
4. `Event`/`EventTarget`/`CustomEvent`（框架事件系统）
5. `TextEncoder`/`TextDecoder`（fetch 配套）
6. `Headers`/`FormData`/`Blob`（fetch 配套）
7. 修 `Reflect.construct`/`Array.from` 的 fallback（从返回 `[]` 改为真实现）

### M63：spa_fallback 注册表扩充 + 兜底增强
**目标**：把 cls.cn 的 CSR 兜底模式推广到更多站。

- spa_fallback.rs 的 `HOST_FETCHERS` 加更多站（掘金→m.juejin.cn？）
- JS 跑空时自动提示用户「该站需 Chrome」（已在 fetch hint 里做了）
- 评估是否接 Chrome CDP 作为终极兜底（M64+）

## 预期覆盖率提升

| 阶段 | 预期覆盖率 | 增量来源 |
|------|-----------|---------|
| 现状（M59） | 58%（实测） | — |
| +M60 boa 0.21 | **70-80%** | async/await 解锁纯 CSR |
| +M61 SSR 提取 | **80-85%** | SSG/SSR 站零成本拿下 |
| +M62 补 API | **85-90%** | hydration 不再因缺 API 挂 |
| +M63 兜底 | **90%+** | 长尾覆盖 |

## 决策点（需用户拍板）

1. **M60 boa 升级**：同意先升 0.21 试水吗？（最高 ROI，但有升级风险）
2. **引擎路线**：长期坚持 boa（轻量卖点）vs 考虑 QuickJS（语言完整但要 FFI）？
3. **Chrome CDP 兜底**：是否接受未来加一个「重型兜底」选项（用户 opt-in，默认不用）？
4. **SSR 提取优先级**：M61 是否提到 M60 前面？（零风险，但增益不如 boa 升级大）

## 风险

1. **boa 0.21 breaking change**：API 可能大改，升级工作量未知。缓解：先在一个分支试。
2. **async/await 落地不等于 SPA 能跑**：即使 await 可用，DOM/event loop 协调仍可能挂。
   缓解：M60 验收用真实站点测，挂了就诚实记录，转 M61。
3. **SSR 提取的框架多样性**：每家框架注入位置/编码不同，适配工作量。
   缓解：先做最常见的 Next/Nuxt（占 SPA 大头）。
4. **覆盖率永远到不了 100%**：纯 CSR 无 SSR + 反爬站永远需 Chrome。诚实接受。
