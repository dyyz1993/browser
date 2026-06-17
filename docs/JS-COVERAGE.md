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
| 二进制大小（release） | 14MB | 14MB（不变） |
| 静态站峰值 RSS（example.com） | 14MB | _待测_ |
| CSR 站峰值 RSS（seo.box） | 95MB | _待测_ |
| 测试总数 | 764（+18 JS 特性含事件/qsa/microtask） | 稳定增长 |

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
| 生成器 function* | ❓ | | |
| ES Modules import | ❓ | | |

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
| getElementsByTagName | ⚠️ 仅首个 | |
| addEventListener | ✅ 存回调 | integration_js_features |
| removeEventListener | ✅ | integration_js_features |
| write | ❌ no-op | |
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
| classList | ⚠️ no-op | |
| dataset | ⚠️ 部分 | |
| style | ⚠️ 部分 | |
| getClientRects | ⚠️ 返回 [] | |
| addEventListener | ✅ 存回调 | integration_js_features |

### 网络
| API | 状态 | 测试 |
|------|------|------|
| fetch（Promise） | ✅ | integration_fetch |
| XMLHttpRequest | ✅ | integration_xhr |
| WebSocket | ✅ | integration_ws |
| setRequestHeader(XHR) | ❌ no-op | |

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
| TextEncoder | ❌ | |
| TextDecoder | ❌ | |
| crypto.getRandomValues | ⚠️ | |
| crypto.randomUUID | ⚠️ | |

### 其他 Web API
| API | 状态 | 备注 |
|------|------|------|
| URL/URLSearchParams | ✅ | compat_shim |
| structuredClone | ⚠️ | |
| performance.now | ✅ | |
| requestAnimationFrame | ⚠️ | |
| queueMicrotask | ✅ | integration_js_features | |
| MutationObserver | ❌ | |
| AbortController | ⚠️ | |
| Headers/FormData/Blob | ❌ | fetch 配套缺 |
| console.* | ✅ | |

---

## 三、补齐计划（按 ROI 排序）

每补一项：加 fixture + 测试 + 更新本表状态 + 对比基线指标。

| 优先级 | 项目 | 原因 |
|--------|------|------|
| P0 | async/await 回归测试 | M60 验收没入库，CI 盲区 |
| P0 | setInterval 实装 | 文档假声明，轮询 SPA 需要 |
| P1 | ES6 语言特性批量测（let/const/箭头/模板/解构/class） | 框架基础语法 |
| P1 | Symbol/Map/Set/Proxy 实测 | 框架硬依赖 |
| P2 | atob/btoa 修真 Base64 | JWT 场景 |
| P2 | Event/EventTarget 基础事件 | 框架 hydration |
| P3 | Element.querySelectorAll | 真实现 |
| P3 | queueMicrotask/MutationObserver | 高级 hydration |
