# M72 — Bug 狩猎战役：CSR/Network/Console/JS 对标 Chrome

> **目标**：大规模系统性对标 Chrome，找到并修复 ≥100 个 bug。
>
> **用户需求**（2026-07-01 确认）：
> 1. **CSR 全覆盖** — 能执行所有 SPA 并提取页面
> 2. **Network 拦截** — 统计网络请求数据
> 3. **Console 拦截** — 采集页面报错/log
> 4. **JS 层面对齐 V8** — 对标 Chrome 的 JS 引擎

---

## 一、现状摸底（已有弹药）

| 能力 | 状态 | 说明 |
|------|:---:|------|
| CSR 提取 | ✅ M71 已到 89% | `browser fetch` 8 档对比矩阵 |
| Network 拦截 | ⚠️ 有 `CapturedNetworkEvent`/`n_events()` | 但**只 CDP 用**，CLI fetch 没透出 |
| Console 拦截 | ⚠️ 有 `__log` stderr 输出 | 但**无结构化采集**（无 level/无 JSON 输出）|
| JS API 覆盖 | ✅ 103 项 / ❌ 7 项缺口 | JS-COVERAGE.md 矩阵 |
| 对比工具 | ✅ `tests/render-matrix/compare_vs_chrome.py` | 可复用 |

**关键差距**：network/console 拦截**已有底层能力**（CDP 在用），但 **CLI 用户拿不到**（fetch 命令没暴露）。这是最该补的。

---

## 二、Bug 分类与预期产出（≥100 个）

### 类别 A：JS Core / ECMAScript（~25 个 bug）
对标 V8/Chrome 的 ES2020+ 语法与内置对象。

```
A1  Promise 系：allSettled/any/race/finally, async iteration
A2  BigInt 运算/字面量/转换
A3  Optional chaining (?.) / Nullish coalescing (??) 边界
A4  globalThis / Reflect / Proxy 边界（revocable/handler traps）
A5  Symbol（iterator/asyncIterator/toPrimitive/hasInstance）
A6  WeakRef / FinalizationRegistry（GC 相关）
A7  Array 系：flat/flatMap/from/entries/keys/values/at
A8  Object 系：fromEntries/entries/is/assign/getOwnPropertyDescriptors
A9  String 系：padStart/padEnd/matchAll/replaceAll/normalize
A10 数值/数学：Number.isInteger/parseInt/Float精确性
A11 正则：named groups/lookbehind/dotAll/sticky
A12 解构：嵌套/默认值/rest/交换
A13 生成器/迭代器协议
A14 模块：import 动态 import() / import.meta
A15 类语法：private #field / static block / extends
A16 try/catch 无参数绑定
A17 模板字符串：嵌套/标签函数/raw
A18 Map/Set 迭代与构造（数组/可迭代入参）
A19 类型转换：ToPrimitive/Symbol.toPrimitive
A20 Spread/Rest 混合使用
```

### 类别 B：DOM API（~25 个 bug）
对标 Chrome DOM 树操作。

```
B1  Node：appendChild/insertBefore/removeChild/replaceChild 顺序
B2  Node：cloneNode 浅/深、contains、isSameNode
B3  Element：classList add/remove/toggle/contains/replace
B4  Element：dataset get/set/delete/枚举
B5  Element：attributes getNamedItem/setNamedItem/item
B6  Element：scrollLeft/scrollTop/offsetWidth（布局尺寸 stub）
B7  Element：getBoundingClientRect（布局 stub）
B8  querySelector/All：伪类(:nth-child/:first-child/:not/:disabled)、属性选择器
B9  querySelector/All：组合选择器 a>b / a+b / a~b
B10 querySelector/All：转义字符、*:scope
B11 innerHTML/outerHTML 解析（含 <script>、嵌套 table）
B12 insertAdjacentHTML/Element/Text
B13 DocumentFragment/ShadowRoot/attachShadow
B14 textContent vs innerText 差异
B15 createDocumentFragment/createComment/createTextNode
B16 createElementNS（SVG/MathML namespace）
B17 TreeWalker/NodeIterator/createNodeIterator
B18 节点关系：nextElementSibling/previousElementSibling/children
B19 MutationObserver（observe/disconnect/takeRecords）
B20 表单：value/checked/disabled/form.elements
B21 表单：FormData/elements namedItem
B22 事件：addEventListener capture/passive/once/removeEventListener
B23 事件：Event.bubbles/cancelable/composedPath/defaultPrevented
B24 事件：CustomEvent detail/target/currentTarget
B25 Range/Selection（getSelection/createRange）
```

### 类别 C：SPA / 框架模式（~20 个 bug）
对标真实框架 CSR。

```
C1  React：hydration、useState、effect、条件渲染
C2  Vue：reactive/ref/computed、template 编译产物
C3  SvelteKit：sveltekit hydration、裸全局赋值
C4  Angular：zone.js、DI、change detection
C5  Next.js：getServerSideProps 数据注入、router
C6  Nuxt：asyncData、useFetch payload
C7  动态 import() chunk loader（webpack/vite）
C8  hash router（#/path）vs history router（pushState）
C9  base href 与相对路径解析
C10 fetch SSR 注水 → CSR hydrate
C11 Web Component（customElements.define + shadow DOM）
C12 IntersectionObserver lazy load
C13 requestAnimationFrame 驱动动画
C14 事件委托（document 级 listener + 冒泡）
C15 contenteditable 输入
C16 svg/canvas/icon 占位
C17 preload/prefetch link 标签
C18 cross-origin script/module
C19 CSP meta 头处理
C20 meta refresh 跳转
```

### 类别 D：Network 拦截（~15 个 bug）
**前置**：先给 CLI fetch 加 `--capture-network` 和 `--capture-console` 输出。

```
D1  fetch GET 请求记录（url/method/status/headers）
D2  fetch POST/PUT/DELETE 请求记录
D3  XHR open/send/setRequestHeader 记录
D4  fetch 失败/超时/abort 记录
D5  请求 body 采集（JSON/formdata）
D6  响应 status code 透出
D7  响应 Content-Type 透出
D8  重复请求/并发请求
D9  redirect 链（301/302）
D10 CORS 预检 OPTIONS
D11 cookie 设置（Set-Cookie → Cookie）
D12 request header 覆盖顺序
D13 preload link 触发的 fetch
D14 WebSocket 连接记录
D15 EventSource(SSE) 连接记录
```

### 类别 E：Console 拦截（~15 个 bug）
**前置**：先给 CLI 加结构化 console 输出。

```
E1  console.log 多参数/对象展开
E2  console.error/warn/info/debug level 采集
E3  console.dir 对象树
E4  console.table 表格数据
E5  console.group/groupEnd/groupCollapsed
E6  console.time/timeEnd/timeLog
E7  console.count/countReset
E8  console.trace 堆栈
E9  console.assert
E10 console.trace/%s/%d/%o 格式化占位符
E11 同步 vs 异步 console（Promise.then 内）
E12 throw 未捕获错误采集
E13 window.onerror/window.onunhandledrejection
E14 console.clear
E15 循环引用对象 console
```

### 类别 F：错误/边界（~10 个 bug）

```
F1  HTML 注释解析
F2  CDATA/条件注释（IE）
F3  void 元素自闭合
F4  嵌套 table 修正
F5  foreign content（SVG 内 HTML）
F6  entity 解码（&amp; &#x; 等全部）
F7  duplicate id 处理
F8  超大文档（10000+ 节点）
F9  超深嵌套（100+ 层）
F10 meta charset / BOM
```

**预期总 bug 数**：A(25) + B(25) + C(20) + D(15) + E(15) + F(10) = **110 个**

---

## 三、执行方法论（每类一致）

### Step 1：写 fixture（每类 1 批 HTML）
- 每个测试点写一个最小 HTML，JS 执行后把**断言结果**写入 `#out`
- 格式：`<div id="out">TESTNAME:PASS</div>` 或 `:FAIL（详情）`

### Step 2：Chrome 跑 baseline
```bash
# Chrome dump-dom → 提取 #out（作为正确答案）
chrome --headless=new --dump-dom file://fixture.html | extract #out
```

### Step 3：我们跑 + 逐行对比
```bash
# 用我们的浏览器 fetch（走真实 JS 管线）
browser fetch http://localhost:PORT/fixture.html --format text
# 逐行对比：PASS 对齐 Chrome 的 = OK；不一致 = BUG
```

### Step 4：bug 归档 + 修复
- 每个 bug 记录：fixture / Chrome 结果 / 我们结果 / 根因 / 修复 commit
- 按影响面分 P0（阻断 CSR）/ P1（影响体验）/ P2（边界）
- 同类 bug 批量修（一个 commit 修一组同类）

### Step 5：回归测试固化
- 每个修好的 bug 加一个入库测试（`integration_js_features.rs`）
- 更新 JS-COVERAGE.md 矩阵状态

---

## 四、基础设施改动（先做，使后续可验证）

### 4.1 CLI 透出 network/console（前置依赖）
```bash
# 目标命令
browser fetch <url> --capture-network    # 输出 JSON 含所有请求
browser fetch <url> --capture-console    # 输出 JSON 含所有 console
browser fetch <url> --format json        # 整合 content+network+console
```

**改动量**：~80 行（main.rs fetch 分支，复用已有的 `n_events()`）。
**为什么先做**：D/E 类 bug 必须能"看到 network/console"才能测。

### 4.2 bug 狩猎 harness
```bash
tests/bug-hunt/
├── harness.py          # 跑 Chrome + 我们，逐行对比 #out，输出 bug 报告
├── categories/
│   ├── A-js-core/      # 25 个 fixture
│   ├── B-dom-api/      # 25 个
│   ├── C-spa/          # 20 个
│   ├── D-network/      # 15 个
│   ├── E-console/      # 15 个
│   └── F-edge/         # 10 个
├── chrome-baseline/    # Chrome 产出的正确答案（可重新生成）
└── BUGS.md             # 归档：每个 bug 一个条目
```

---

## 五、里程碑拆分（每个独立可交付）

| 阶段 | 内容 | 产出 |
|------|------|------|
| **M72.0** | 基础设施：CLI network/console 透出 + harness | 可测 D/E 类 |
| **M72.A** | JS Core（A1-A20）：20 fixture → 找 bug → 修 | ~25 bug |
| **M72.B** | DOM API（B1-B25）：25 fixture → 找 bug → 修 | ~25 bug |
| **M72.C** | SPA（C1-C20）：20 fixture → 找 bug → 修 | ~20 bug |
| **M72.D** | Network（D1-D15）：依赖 M72.0，15 fixture → 找 bug → 修 | ~15 bug |
| **M72.E** | Console（E1-E15）：依赖 M72.0，15 fixture → 找 bug → 修 | ~15 bug |
| **M72.F** | Edge（F1-F10）：10 fixture → 找 bug → 修 | ~10 bug |

**每个阶段 = 一个 commit**（fixture + 修复 + 回归测试 + BUGS.md 更新）。

---

## 六、验收标准

- [ ] CLI 支持 `--capture-network` / `--capture-console` / `--format json`
- [ ] ≥100 个 fixture，覆盖 A-F 六类
- [ ] `tests/bug-hunt/BUGS.md` 归档 ≥100 个 bug（含根因+修复）
- [ ] 所有修复都有回归测试
- [ ] JS-COVERAGE.md 缺口项从 7 降到 ≤2
- [ ] 真实站点（react.dev/vuejs.org/nuxt.com/svelte.dev）CSR 覆盖率 ≥90%
