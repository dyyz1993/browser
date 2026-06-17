# JS 渲染能力现状报告（诚实版）

> 日期：2026-06-17
> 目的：如实回答"对齐了什么标准、覆盖率多少、哪些没对齐"
> ⚠️ 本文不粉饰，未测量的标"未测量"，不编数字。

## 一句话现状

> **我们对标的不是任何 ECMAScript/HTML 官方规范，而是「boa 引擎 + 手写 Web API 桥」
> 的经验主义子集。语言一致性 94% 是 boa 上游声称值（我们没自测）。Web API 覆盖
> 是 DOM/fetch/XHR/WS/Storage/Timer 的爬虫够用子集，缺事件系统、setInterval、
> querySelectorAll（Element 级）等关键 API。5 种 SPA 行为模式本地测试 100% 通过，
> 但真实公网站点覆盖率约 58-80%（受 boa 引擎天花板封顶）。**

## 我们对齐的是什么？

**不是官方规范，是两个东西的组合**：

| 层 | 对齐对象 | 一致性 |
|----|---------|--------|
| JS 语言层 | boa 0.21 引擎（Rust 实现） | 上游声称 94.12% test262，**项目未自测** |
| Web API 层 | 手写 shim（document/fetch/XHR/WS/Storage/Nav/Timer） | 无规范对标，是"百度等 SPA 实测用的 API 子集" |

**项目没有任何文档写"目标是 ESXXXX"或"对标 HTML 第 N 章"。** document_shim.rs:3 原话：
"对齐 W3C/Chrome 基础子集"——但无清单说明边界。

## 覆盖率：两个不同的数字，别混淆

### 1. 语言一致性（test262）—— 未自测
- **94.12% 是 boa 0.21 上游博客的声称值**（[boajs.dev/blog/2025/10/22/boa-release-21](https://boajs.dev/blog/2025/10/22/boa-release-21)）
- 项目**从未自己跑过 test262**，全项目 grep `test262` 零命中
- 这个数字不能作为我们项目的覆盖率证据

### 2. SPA 行为模式覆盖 —— 5/5 本地通过
我们自己设计的 5 种 SPA 模式（`tests/fixtures/spa/`），**100% 渲染成功**：

| 模式 | 测什么 | 结果 |
|------|--------|------|
| async-data | async/await + setTimeout + DOM 改写 | ✅ |
| route-switch | 客户端路由默认视图 | ✅ |
| lazy-load | createElement 动态列表 | ✅ |
| dynamic-form | 状态管理 + 条件渲染 | ✅ |
| js-redirect | 鉴权 + 重定向逻辑 | ✅ |

**但这 5 个是我们自己挑的简单用例**，不等于"所有 SPA 都能跑"。

### 3. 真实公网站点覆盖 —— 58-80%（M59 实测）
12 站三方对比（curl/我们/Chrome），排除网络/反爬后有效覆盖 8/10 = 80%。
失败硬伤：纯 CSR 无 SSR（掘金/bark）、反爬（SO）、boa 引擎 panic（owid）。

## Web API 覆盖矩阵（实测，非声称）

### ✅ 已实现（爬虫够用子集）
| 类别 | API | 备注 |
|------|-----|------|
| DOM document | getElementById/querySelector/createElement/createTextNode/... | 9 方法 + 15 getter |
| DOM element | appendChild/insertBefore/setAttribute/getAttribute/textContent/innerHTML/... | ~24 方法/getter |
| 网络 | fetch（Promise）/ XHR / WebSocket | 齐全但简化 |
| 存储 | localStorage/sessionStorage | 6 方法，length 是方法非属性 |
| 导航 | history（pushState/replaceState/...）/ location | 完整 |
| 定时器 | setTimeout/clearTimeout | **setInterval 缺失** |
| 兼容层 | Object/Reflect/Array/String 原生方法（boa 0.21 原生，M62 移除有害包装） | |

### ❌ 缺失或残缺（影响真实 SPA）
| 缺口 | 状态 | 影响 |
|------|------|------|
| **事件系统**（addEventListener/Event/EventTarget） | addEventListener 是 **no-op**（空函数） | 框架 hydration / 交互全失效 |
| **setInterval/clearInterval** | 完全未实现 | 轮询类 SPA 挂 |
| **Element.querySelectorAll** | 写死返回 `[]` | Element 级查询无效 |
| **atob/btoa** | 实现错误（非真 Base64） | JWT/Base64 场景破坏 |
| **queueMicrotask** | 未实现 | 微任务调度缺失 |
| **MutationObserver** | 未实现 | DOM 变化监听缺失 |
| **TextEncoder/Decoder** | 未实现 | fetch 配套缺失 |
| **Headers/FormData/Blob** | 未实现 | fetch 配套缺失 |
| **CSS display 解析** | 不处理 style 属性 | display:none 元素照常渲染 |

## 语言层测试覆盖 —— 接近零

**这是最大的诚实盲区**：
- ES6 语言特性（Proxy/Map/Set/Symbol/class/箭头函数/destructuring）**零 fixture 覆盖**
- async/await 语法**无入库回归测试**（M60 验收靠手测用例 + 真实站点字节数，没写进 CI）
- 唯一接近语义层的是 5 个 Promise.then 测试（`integration_promise.rs`）
- js-runtime crate 有 151 个 `#[test]`，但全集中在"DOM 桥能不能改 DOM"，不是"JS 语法支持对不对"

## 天花板在哪里（原话）

项目自己的评估文档已经诚实写明：

> `docs/assessments/M40-real-world-spa.md:55`：
> **"真实网站 JS 兼容性的天花板在 boa 引擎，不在我们的 shim 层"**

> `docs/assessments/M40-real-world-spa.md:60`：
> **"投入大量精力补 JS API 收益有限，真实 SPA 兼容性受 boa 引擎限制封顶"**

> `docs/plans/M60-js-coverage-roadmap.md:98`：
> **"async/await 解锁了'能跑'，但不等于'能拿到数据'"**

**天花板 = boa 引擎的 test262 一致性 + 我们 Web API 桥的完整度。** 两者都不是完整的，组合起来对真实 SPA 的覆盖率有硬上限。

## 文档同步问题（需要修）

| 文档 | 写的 | 实际 |
|------|------|------|
| FEATURES.md:72 | "boa 0.20，async defer" | Cargo.toml 已 0.21，async/await 已落地 |
| GOALS.md:105 | "完整 ES6+ 异步 defer 到切 deno_core" | M60 已升 boa 0.21 拿到 async/await |
| FEATURES.md:191 | "boa 0.20 不支持 ES6 shorthand" | 0.21 下是否仍成立**未验证** |

## 结论与建议

### 不要对外说
- ❌ "JS 覆盖率 94%"（那是 boa 上游声称值，我们没自测）
- ❌ "支持 ES6+"（没测过 Proxy/Map/Set/Symbol 的实际可用性）
- ❌ "SPA 全覆盖"（5 个自造 fixture 通过 ≠ 真实 SPA 全覆盖）

### 可以对外说
- ✅ "基于 boa 0.21 引擎（上游 test262 一致性 94%），手写 Web API 桥覆盖 DOM/fetch/XHR/WS/Storage/Timer 爬虫够用子集"
- ✅ "5 种核心 SPA 模式（async-data/route-switch/lazy-load/dynamic-form/js-redirect）本地测试 100% 通过"
- ✅ "真实站点实测覆盖率约 58-80%（排除反爬/网络后），受 boa 引擎天花板封顶"
- ✅ "纯 CSR 无 SSR 兜底的站点需真 Chrome（已知局限）"

### 下一步最该做的（按 ROI）
1. **补 async/await 回归测试**（M60 验收没入库，这是 CI 盲区）
2. **实装 setInterval**（文档声称 M30 已有但代码没有，是假声明）
3. **测 ES6 语言特性实际可用性**（Proxy/Map/Set/Symbol 能不能真跑，形成覆盖矩阵）
4. **修文档同步**（FEATURES/GOALS 还写 boa 0.20）
