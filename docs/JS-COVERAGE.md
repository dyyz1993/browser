# JS 能力覆盖矩阵（单一事实来源）

> 本文档是 JS 渲染能力的**权威清单**。每项标注实测状态，照此补齐测试。
> 边补边观察二进制大小/内存变化（更多桥代码 = 体积增长，需监控）。
> 冲突时以此文档为准，并顺手修正 FEATURES.md。

## 测量方法

每项用 `render-script` 跑最小用例，判定：
- ✅ = 渲染出预期结果
- ❌ = 报错/panic/空输出
- ⚠️ = 部分工作（降级/no-op/语义偏差）
- ❓ = 未测

## 基线指标（边补边对比）

| 指标 | 当前值 | 补测试后 |
|------|--------|---------|
| 二进制大小（release） | 14MB | 14MB（28 项测试 + 5 API polyfill，仍不变） |
| 静态站峰值 RSS（example.com） | 14MB | _待测_ |
| CSR 站峰值 RSS（seo.box） | 95MB | _待测_ |
| 测试总数 | 781（+34 JS 特性） | 稳定增长 |

---

## 一、ES 语言特性（boa 引擎层）

### ES5（应该全支持）
| 特性 | 状态 | 测试 |
|------|------|------|
| 闭包/作用域 | ✅ | 5 SPA fixture 隐式覆盖 |
| try/catch/finally | ✅ | 隐式覆盖 |
| JSON.parse/stringify | ✅ | integration_js_features |
| Array.prototype.* | ✅ | M62 移除包装后原生 |
| String.prototype.* | ✅ | M62 移除包装后原生 |
| RegExp | ✅ | integration_js_features |

### ES2015+（关键能力）
| 特性 | 状态 | 测试 | 备注 |
|------|------|------|------|
| let/const | ✅ | integration_js_features | |
| 箭头函数 `=>` | ✅ | integration_js_features | |
| 模板字符串 `` `x${y}` `` | ✅ | integration_js_features | |
| 解构 `{a, b} = obj` | ✅ | integration_js_features | |
| 默认参数 `fn(x=1)` | ✅ | integration_js_features | |
| 展开运算符 `...arr` | ✅ | integration_js_features | |
| for...of | ✅ | integration_js_features | |
| class 语法 | ✅ | integration_js_features | |
| class 继承 extends | ✅ | integration_js_features | |
| **Promise** | ✅ | integration_promise (5) | M30 |
| **async/await** | ✅ | 5 SPA fixture | M60 升 boa 0.21 |
| **Symbol** | ✅ | integration_js_features | React `$$typeof` 硬依赖已满足 |
| **Map/Set** | ✅ | integration_js_features | React reconciler 硬依赖已满足 |
| **WeakMap/WeakSet** | ✅ | integration_js_features | |
| **Proxy** | ✅ | integration_js_features | Vue 3 响应式硬依赖已满足 |
| **Reflect** | ✅ 原生 | 无测试 | M62 移除包装 |
| 迭代器协议 | ✅ | for...of 隐式验证 | |
| 生成器 function* | ✅ | integration_js_features | |
| ES Modules import() | ❌ | boa 0.21 动态 import 需 ModuleLoader，卡死，defer | |

---

## 二、Web API（手写 shim 层）

### DOM document
| API | 状态 | 测试 |
|------|------|------|
| getElementById | ✅ | 多处 |
| querySelector | ✅ | 多处 |
| querySelectorAll | ✅ 全部（M62） | |
| createElement | ✅ | lazy-load fixture |
| createTextNode | ✅ | |
| getElementsByTagName | ✅ 全部（M62） | integration_js_features |
| addEventListener | ✅ 存回调 | integration_js_features |
| removeEventListener | ✅ | integration_js_features |
| write | ⚠️ no-op | 低频，现代 SPA 不用 | |
| body/head/title 等 getter | ✅ | |

### DOM element
| API | 状态 | 测试 |
|------|------|------|
| appendChild | ✅ | lazy-load fixture |
| insertBefore | ✅ | |
| removeChild | ✅ | |
| setAttribute/getAttribute | ✅ | |
| textContent | ✅ | |
| innerHTML | ✅ | async-data fixture |
| **querySelectorAll** | ✅ 全部（M62，简化版从 document 根搜索） | integration_js_features |
| cloneNode | ✅ | |
| classList | ✅ | M62（真实现 add/remove/contains/toggle） | |
| dataset | ✅ | M62（动态遍历常见 key + 驼峰转 kebab） | |
| style | ⚠️ 部分 | getPropertyValue/setProperty 有，CSS 不影响渲染 | |
| getClientRects | ⚠️ 返回 [] | 布局尺寸，爬虫不需要 | |
| addEventListener | ✅ 存回调 | integration_js_features |

### 网络
| API | 状态 | 测试 |
|------|------|------|
| fetch（Promise） | ✅ | integration_fetch |
| XMLHttpRequest | ✅ | integration_xhr |
| WebSocket | ✅ | integration_ws |
| setRequestHeader(XHR) | ⚠️ no-op | 爬虫场景 headers 不关键 | |

### 存储/导航/定时器
| API | 状态 | 测试 |
|------|------|------|
| localStorage | ✅ | storage_shim 测试 |
| sessionStorage | ✅ | 共享后端 |
| history.* | ✅ | navigation_shim 测试 |
| location.* | ✅ | |
| setTimeout/clearTimeout | ✅ | integration_settimeout (6) |
| **setInterval/clearInterval** | ✅ | 手测（100 次硬上限防死循环） |
| Image | ✅ | image_shim 测试 |

### 事件系统
| API | 状态 | 备注 |
|------|------|------|
| Event 构造器 | ✅ | integration_js_features |
| EventTarget | ✅ | integration_js_features |
| CustomEvent | ✅ | integration_js_features |
| dispatchEvent | ✅ | integration_js_features（含 DOMContentLoaded 自动 dispatch） |
| addEventListener（真实现） | ✅ | document/element 存回调 + dispatchEvent 触发 |

### 编码/加密/二进制
| API | 状态 | 备注 |
|------|------|------|
| **atob** | ✅ | integration_js_features | M62 修真 Base64 |
| **btoa** | ✅ | integration_js_features | M62 修真 Base64 |
| TextEncoder | ✅ | integration_js_features（UTF-8 polyfill） |
| TextDecoder | ✅ | integration_js_features（UTF-8 polyfill） |
| crypto.getRandomValues | ✅ | compat_shim | |
| crypto.randomUUID | ✅ | compat_shim | |

### 其他 Web API
| API | 状态 | 备注 |
|------|------|------|
| URL/URLSearchParams | ✅ | compat_shim |
| structuredClone | ✅ | integration_js_features | |
| performance.now | ✅ | |
| requestAnimationFrame | ✅ | compat_shim | |
| queueMicrotask | ✅ | integration_js_features | |
| MutationObserver | ✅ | M62（Vue 3 响应式，存回调不 observe） | |
| AbortController | ✅ | compat_shim（简化桩） | |
| Headers/FormData/Blob | ✅ | integration_js_features（简化 polyfill） |
| console.* | ✅ | |

---

## 三、补齐计划（按 ROI 排序）

每补一项：加 fixture + 测试 + 更新本表状态 + 对比基线指标。

| 优先级 | 项目 | 原因 |
|--------|------|------|
| ~~P0~~ | ~~async/await 回归测试~~ | ✅ M62 已入库 |
| ~~P0~~ | ~~setInterval 实装~~ | ✅ M62 已实装（100 次上限） |
| P1 | ES6 语言特性批量测（let/const/箭头/模板/解构/class） | 框架基础语法 |
| ~~P1~~ | ~~Symbol/Map/Set/Proxy 实测~~ | ✅ M62 全通过 |
| ~~P2~~ | ~~atob/btoa 修真 Base64~~ | ✅ M62 已修 |
| ~~P2~~ | ~~Event/EventTarget 基础事件~~ | ✅ M62 已补（含 DOMContentLoaded 自动 dispatch） |
| ~~P3~~ | ~~Element.querySelectorAll~~ | ✅ M62 已补真实现 |
| ~~P3~~ | ~~queueMicrotask/MutationObserver~~ | ✅ M62 已补 |

---

## 四、补齐规划与约束（自愈自循环机制）

> 这套机制已写入 AGENTS.md 第六章「遇到 JS 报错时」。核心原则：
> **报错驱动，不凭猜测补 API。**

### 自愈循环五步（每次遇到新报错都走一遍）

1. **采集报错** —— `browser fetch <url> 2>&1 | grep "\[js\]"`，按频率排序找高频缺口
2. **定位根因** —— 报错模式反查缺什么（见下表）
3. **补 API + 加测试** —— 对应 shim 补 + `integration_js_features.rs` 加最小用例
4. **验证提升** —— 重跑 `csr_compare.sh` 看评分/报错数，监控二进制大小
5. **更新本矩阵** —— 状态 ❓→✅，同步 FEATURES.md

### 常见报错模式 → 根因速查表

| 报错模式 | 根因 | 补什么 | 例子（M62 已修） |
|---------|------|--------|-----------------|
| `X is not defined` | 缺全局对象/构造器 | compat_shim 加构造器 | MutationObserver/Event/CustomEvent |
| `not a callable function` | 某函数/方法未定义 | 对应 shim 加方法 | matchMedia/ga/createDocumentFragment |
| `React error #299` 等框架错误码 | DOM 检查属性缺失 | element_shim 加属性 | nodeType/Node.ELEMENT_NODE 常量 |
| `[compat] X threw` | compat_shim 包装问题 | 移除包装（boa 0.21 原生） | Array.prototype.forEach/String.includes |
| `cannot convert null/undefined` | 空值未处理 | 加 null guard | 各 shim 加防御 |

### 补 API 的约束（避免无脑补）

1. **报错闭环（核心）**：每个 JS 功能点必须有单独测试用例覆盖。**报错就继续补
   测试用例，直到没有任何问题。** 这是原则一（SPA 全覆盖）+ 原则二（自研优先）
   的落地——不靠运气，靠测试固化每个能力点。
2. **爬虫够用原则**：事件/动画/MediaQuery 不需真实现，no-op 或桩即可
2. **纯 JS polyfill 优先**：用 compat_shim 的 JS 字符串，不引 Rust 依赖（保二进制不涨）
3. **boa 天花板认知**：纯 CSR 无 SSR 的站不硬刚，走 `--no-js` 或标注需 Chrome
4. **不追 100%**：深层 bundle 报错（lodash/template）不影响核心功能则停止深挖
5. **补完必测**：每补一个 API 必须加 `integration_js_features.rs` 入库测试

### 测试工具箱

| 工具 | 用途 |
|------|------|
| `crates/cli/tests/integration_js_features.rs` | 30 项 JS 特性入库回归（补 API 必加） |
| `crates/cli/tests/integration_spa_patterns.rs` | 5 种 SPA 模式 fixture（async/route/lazy/form/redirect） |
| `tests/benchmarks/csr_compare.sh` | 纯 CSR 站严格对比（多维评分） |
| `tests/benchmarks/spa_compare.sh` | 全站对比（含 SSR，--smart 模式） |

### 已知天花板（不硬刚）

- **纯 CSR 无 SSR**（bark/vue-playground）：boa 跑不出数据，需 Chrome
  - bark（docsify）：boa 引擎内部 `cannot convert null to object`（非 Object.keys，
    是引擎层 for...in/spread/解构 null）。已试 Object.keys null guard 无效。
    docsify 初始化链复杂（路由+fetch md+compiler+Vue 集成），属 boa 引擎天花板。
  - vue-playground：`SyntaxError: expected ';'`（boa 解析器不支持 Vue bundle 某语法）
- **boa 引擎 panic**（owid）：catch_unwind 兜底降级（M62 已修）
- **base.js/lodash _.template 深层报错**：不影响核心功能，停止深挖
- **ES Modules import()**：boa 0.21 动态 import 卡死（需 ModuleLoader），defer
