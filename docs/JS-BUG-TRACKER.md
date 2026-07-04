# JS Bug Tracker

> 单一事实来源：所有 JS 相关问题的跟踪、状态、修复记录。
> 新增 JS bug 必须在此登记 + 关联 CI fixture（`tests/bug-hunt/categories/`）。
>
> 更新规则：每次修复 JS bug → 更新状态 + 关联 commit + 移动 fixture 到通过列表。

---

## 当前状态

| 指标 | 值 |
|:-----|:---:|
| **总 fixturess** | 35 |
| **通过** | 33 |
| **待修** | 2 |
| **CI 阻断** | 否（已知 bug 可接受） |
| **最后更新** | 2026-07-04 |

---

## 一、Fixturess 清单

### ✅ 已通过（33）

| Fixture | 类别 | 说明 |
|:--------|:-----|:-----|
| A01-promise-allSettled | JS Core | Promise.allSettled |
| A05-optional-chaining | JS Core | `?.` 语法 |
| A08-reflect | JS Core | Reflect API |
| A09-proxy-basic | JS Core | Proxy 基础 |
| A10-proxy-revocable | JS Core | Proxy.revocable |
| A11-symbol | JS Core | Symbol API |
| A12-weakref | JS Core | WeakRef |
| A13-finalization-registry | JS Core | FinalizationRegistry |
| A14-string-matchAll | JS Core | String.matchAll |
| A15-from-entries | JS Core | Object.fromEntries |
| A16-flat | JS Core | Array.flat |
| A17-flatMap | JS Core | Array.flatMap |
| A18-trimStart-End | JS Core | String.trimStart/End |
| A19-intl | JS Core | Intl API |
| B01-createElement | DOM API | 元素创建 |
| B02-querySelector | DOM API | 选择器 |
| B03-classList | DOM API | CSS 类操作 |
| B04-innerHTML | DOM API | innerHTML |
| B05-appendChild | DOM API | 子节点追加 |
| B06-removeChild | DOM API | 子节点移除 |
| B07-textContent | DOM API | 文本内容 |
| B08-dataset | DOM API | data-* 属性 |
| B09-style | DOM API | style 属性 |
| B10-event-basic | DOM API | 事件绑定 |
| C01-history | SPA 路由 | history.pushState |
| C02-localStorage | SPA 存储 | localStorage |
| C03-setTimeout | SPA 异步 | setTimeout |
| C04-cookie-safe | SPA 存储 | Cookie 操作 |
| C05-promise-all | SPA 异步 | Promise.all |
| C06-json-parse | SPA 数据 | JSON.parse |
| E01-log-basic | Console | console.log |
| E02-console-warn | Console | console.warn |
| E03-console-error | Console | console.error |
| E04-console-assert | Console | console.assert |
| F01-blob | Edge | Blob |
| F02-file-reader | Edge | FileReader |
| F03-formdata | Edge | FormData |
| F04-url | Edge | URL API |

### 🐛 待修（2）

| ID | Fixture | 问题 | 优先级 | 根因 | 备注 |
|:---|:--------|:-----|:-----:|:-----|:-----|
| JS-001 | D02-XHR-basic | `xhr.send()` 返回空，`responseText` 未更新 | P2 | QuickJS XHR shim 在同步模式下 `send()` 后没有及时填充 `responseText` | 爬虫场景 XHR 不常用，优先级低 |
| JS-002 | D06-fetch-redirect | `response.redirected` 为 `false`（应为 `true`） | P3 | `__fetchSync` 透明跟随重定向，JS 层无法感知是否发生过重定向 | 仅影响 `redirected` 属性检测；实际内容已正常返回 |

---

## 二、真实站点问题

| 站点 | 问题 | 状态 | 优先级 | 根因 |
|:-----|:-----|:----:|:-----:|:-----|
| solidjs.com | 641KB minified bundle `export{V_ as $,...} → runtime error` | ⚠️ 语法已修复，运行时 `cloneNode` 兼容性 | P2 | SolidJS DOM 渲染引擎的 `cloneNode` 操作遇到 null 引用 |
| astro.build | 站点不可达（curl 也超时） | ❌ 外部 | — | CDN/服务器问题，非引擎 bug |
| nextjs.org | 16 个 chunk 加载错误 | ✅ M74 修复 | — | rspack chunk 预取 + module eval 改造 |
| github.com (PR35) | 11 个 stderr JS 错误 | ✅ M74 清零 | — | JSX chunk 过滤 + rspack module eval |
| react.dev | smart fallback 正确恢复 | ✅ 正常 | — | SSR fallback 正确工作 |
| nuxt.com | 超时（30s→60s 后正常） | ✅ 修复 | — | 默认超时从 30s→60s |
| angular.io | 超时（30s→60s 后正常） | ✅ 修复 | — | 默认超时从 30s→60s |

---

## 三、已修复历史

| ID | 问题 | 修复 commit | 日期 | 修复方式 |
|:---|:-----|:-----------|:----|:---------|
| GAP-A | iframe contentDocument/contentWindow | `27ba199` | M71.3 | 纯 JS stub |
| GAP-B | DocumentFragment nodeType/childNodes | `27ba199` | M71.3 | createDocumentFragment 修复 |
| GAP-C | matches/closest/getComputedStyle | `27ba199` + `a8c5b73` | M71.3 | bridge 函数 + shim |
| GAP-D | template.content / importNode | `27ba199` | M71.3 | JS stub |
| GAP-E | HTMLCanvasElement getContext stub | `27ba199` | M71.3 | 纯 JS stub |
| GAP-F | WebGL getContext stub | `27ba199` | M71.3 | 纯 JS stub |
| GAP-G | console.dir/debug/table | `27ba199` | M71.3 | JS stub |
| GAP-H | performance.navigation | `27ba199` | M71.3 | 纯 JS stub |
| GAP-I | querySelectorAll 后代选择器对动态 DOM 失效 | `636ea58` | M71.4 | find_by_selector/find_all_by_selector 修复 |
| GAP-J | cloneNode 多复制子节点 | deferred | — | 记录待修 |
| GAP-K | fragment 插入父节点后自动清空 | deferred | — | Web 标准行为差异 |
| — | PR35 GitHub stderr 错误清零 | `bbc9316` | M76 | JSX 过滤 + rspack module eval |
| — | Vite CSR Module::declare Exception | `b6943df` | M76 | has_static 检测改用 `has_static_esm_syntax` |
| — | Vite import.meta.env 不可赋 | `e7679b0` | M76fin | 替换为 `__vite_env__` 模块级变量 |
| — | JSX/generator 语法错误（非 ES 规范） | `bbc9316` | M76 | 静默过滤 |
| — | inline export{...} 遗漏 | `070f6f0` | M76fin4 | 全量移除 solidjs/nuxt 的 `export{}` |
| — | 默认 HTTP 超时 30s→60s | `c50768e` | M76fin4 | 修 nuxt/nextjs/angular 超时 |
| — | Loader namespace registry | `9405ef8` | M76fin2 | CJS 模块立即 eval 捕获 exports |
| — | promise.finish 非致命 | `9405ef8` | M76fin2 | 失败仍返回 Ok（React 模块已评估） |

---

## 四、新增 Bug 流程

```
1. 发现 → 写 fixture HTML（含 PASS/FAIL 断言）→ 放入 tests/bug-hunt/categories/
2. 登记 → 在此 tracker 添加条目（ID/描述/优先级）
3. 修复 → 改代码 → fixture 通过 → 更新状态 + 关联 commit
4. CI 验证 → python3 tests/run_js_tests.py 应显示新增 fixture 通过
```

### CI 集成

```
cargo test --workspace         # Rust 测试
python3 tests/run_js_tests.py  # JS fixture 测试
```

两者都必须通过才能提交。已知 bug（JS-001, JS-002）设为例外不阻断。
