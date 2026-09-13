# ⭐ GOALS — North Star（单一事实来源）

> 本文档定义项目的**目标、非目标、验收标准**。所有功能取舍、范围决策、
> "要不要做 X"的争论，都以本文档为准。其他文档（FEATURES/ROADMAP/ARCHITECTURE）
> 必须与本文档一致，冲突时以本文档为准。

---

## 一句话定位

**一个用 Rust 手写的、跨平台的、能渲染 SPA 页面供爬虫使用的浏览器。**
兼具学习目的——通过造轮子深入理解浏览器内部原理。

---

## 目标（做什么）

### G1. SPA 渲染 + 爬虫数据提取（核心目标 ✅ 已达成 M4）

**定义**：给定一个依赖 JavaScript 动态生成内容的页面（SPA），本浏览器能：
1. 通过 HTTPS 获取页面 HTML
2. 解析 HTML 成 DOM
3. 执行页面 `<script>` 标签里的 JS
4. JS 通过 DOM API 修改 DOM（`__setBody`/`__createEl`/`__appendChild`/...）
5. JS 可发同步 fetch（`__fetchSetBody`/`__fetchAppendBody`）拿后端 API 数据
6. 渲染最终的 DOM（ASCII 文本 + PNG 截图）

**验收标准（机器可验证）：**
```bash
# 本地 SPA fixture：JS 动态生成产品列表 + localStorage token
./target/release/browser render-url http://localhost:8765/index.html --width 80
# stdout 必须包含：PRODUCTS: Rust Book:$39.99 | ... 和 TOKEN: tk-xxx
```

### G2. 截图输出（M12.1 ✅）

**定义**：把渲染结果导出为 PNG，供爬虫下游（OCR/对比/存档）使用。

**验收标准：**
```bash
./target/release/browser render-url <url> --width 80 --screenshot out.png
# out.png 存在、非空、可被 image crate 重新打开
```

### G3. 跨平台（macOS / Windows / Linux）

**定义**：同一份代码，三平台都能 `cargo build` + `cargo test` 全绿。
CI 在 GitHub Actions 三平台矩阵上每个 commit 全绿。

**验收标准：** `.github/workflows/ci.yml` 三平台矩阵通过。

### G4. 低依赖、自研优先（学习目的）

**定义**：浏览器架构（DOM/布局/渲染/JS 桥/事件/Page 生命周期）全部手写。
只有"底层解析/IO 库过于复杂（约 ≥10 万行）"时才允许引入外部 crate。

**当前允许的外部依赖（白名单，见 CONVENTIONS.md）：**
html5ever、hyper+rustls、tokio、clap、boa_engine、winit+softbuffer、url、fontdue、png、image。/`v8`（ADR-0006：V8 152 可选后端，默认不编译）

### G5. 真实世界可用性（持续提升 🟡）

**定义**：真实网站（example.com、本地 SPA）能抓能渲染。强反爬站点（百度）
和需要异步 JS 的站点（React/Vue 复杂 bundle）是**已知局限**，逐步改进。

### G6. CLI 爬虫命令（M59 ✅）

**定义**：提供 `browser fetch <url>` 命令行工具，自动完成 fetch → JS 执行 → HTML 序列化/内容提取 → stdout 输出。

**验收标准**：
```bash
browser fetch https://example.com/ --format html
# stdout 输出完整渲染后 HTML（包含 JS 动态插入内容），退出码 0
# 支持 --format {markdown|html|text|links}（默认 markdown）
# 支持 --wait-strategy {dom-ready|load|timeout}（默认 load）
# 支持 --timeout-ms <ms>（默认 30000）
```

**SPA 覆盖率标准（90% 目标）**：支持常见 SPA 模式：
- Async data loading（fetch + DOM 插入）
- Route switching（hash 路由 + 动态内容切换）
- Lazy loading（延迟加载组件）
- Dynamic form（表单动态生成）
- Redirect（location.href 跳转）
- XHR + WebSocket + Cookie
- localStorage/sessionStorage
- setTimeout/setInterval

**当前支持状态**：`browser fetch` 命令（M59）已支持全部模式。

---

## 非目标（明确不做什么）

> 这些能力对 SPA 爬取**无价值或价值极低**，明确放弃，避免 scope creep。

| 能力 | 原因 |
|------|------|
| **Service Worker** | 离线缓存，爬虫不需要 |
| **WebRTC** | P2P 视频，与爬虫无关 |
| **WebAudio** | 音频处理，与爬虫无关 |
| **WebGL / WebGPU** | 3D 渲染，与爬虫无关 |
| **视频 / 音频播放** | 媒体解码，与爬虫无关 |
| **CSS grid / table / float 布局** | 复杂度高，爬虫只关心文本 |
| **3D / 复杂动画** | 性能开销大，爬虫不需要 |
| **多进程架构** | 单进程足够，低内存优先 |
| **完整 ES6+ 异步语义** | M60 已升 boa 0.21，async/await 落地（18 项入库测试）。纯 CSR 无 SSR 站仍需 Chrome |

---

## 验收三级标准

每个功能/里程碑必须满足以下三级验收才算"完成"：

### Level 1 — 单元测试（`#[test]`）
- 与源文件同 crate，`cargo test -p <crate>`
- 测纯函数逻辑（解析、算法、数据结构）

### Level 2 — 集成测试（`tests/*.rs` + fixture）
- 用真实 fixture HTML 文件（纳入 git）
- `cargo test -p browser-cli --test <name>`
- 测端到端管线（fetch → parse → JS → render）

### Level 3 — 真实命令验证（verify-before-delivery）
- 用最终用户视角执行 CLI 命令
- 检查 stdout 每一行，零报错
- 见 `docs/TESTING.md` 的具体验收命令

---

## 基础原则（宪法级，优先于一切）

> 这是项目维护者确立的四条基础原则。所有架构决策、功能取舍、技术选型
> 都必须服从这四条。冲突时**基础原则 > 决策原则 > 个人偏好**。

### 原则一：SPA/CSR 尽可能全覆盖（项目存在的理由）

**尽可能解决所有 SPA、CSR 场景。** 这是本项目的 North Star。
- 遇到渲染不了的 SPA → 走 AGENTS.md 第六章「JS 报错自愈循环」补 API
- 纯 CSR 无 SSR 兜底的站（bark/vue-playground）是 boa 引擎天花板，标注需 Chrome，
  但不放弃——持续补 API 缩小失败面
- 覆盖率是核心 KPI：`csr_compare.sh` 评分 + `spa_compare.sh` 覆盖率持续追踪

### 原则二：自研优先（学习 + 可控）

**整个架构尽可能手写，不用别人的库。** 例外只有白名单（html5ever/hyper/
boa/clap/fontdue 等「≥10 万行底层库」）。
- JS Web API 桥、事件系统、Base64、Markdown 转换、内容提取器 → 全手写
- 补 API 用纯 JS polyfill（compat_shim），不引 Rust 依赖（保二进制不涨）
- 新增依赖必须：登记白名单 + 写 ADR + commit（禁止偷偷加）

### 原则三：低内存、快运行（核心卖点）

**保证低内存、运行快的特征。** 这是对标 Chrome 的核心优势（实测内存 1/11、
速度 6 倍）。
- 不启动 Chromium/V8（13MB 单文件，不引重型引擎）
- 任何改动后监控基线：二进制大小 + 峰值 RSS（补 API 不应让这俩涨）
- `--smart` 模式（先 SSR 后 JS）是有 SSR 站的杀手锏，持续优化

### 原则四：反爬不重点处理（明确边界）

**针对反爬不需要重点处理。** 本项目是「基本能用」的渲染引擎，不做指纹伪造/
验证码/行为模拟。
- 反爬太强的站（需签名/验证码）走 `--no-js` 兜底或标注为已知局限
- 不为绕过检测做 canvas 指纹/WebGL 伪造等（AGENTS.md 已写死）
- 但**不等于不处理任何防护**——brotli 解码、cookie 持久化、UA 伪装这些基础
  的「让请求能成功」的能力要做（M58 brotli、M15 cookie 都是）

---

## 决策原则（当 "要不要做 X" 有争议时）

1. **爬虫价值优先**：X 能让更多 SPA 被正确爬取吗？是 → 做；否 → 跳过。
2. **学习价值次之**：X 能加深对浏览器原理的理解吗？是 → 做；否 → 跳过。
3. **复杂度门槛**：X 的实现成本（行数/API 复杂度）超过收益吗？是 → defer。
4. **自研优先**：能用 < 1000 行手写实现吗？是 → 手写；否 → 评估白名单 crate。
5. **不破坏既有**：X 会让现有测试变红吗？是 → 不做或重构。
6. **测试全覆盖**：每个 JS 功能点都有单独测试用例。报错就继续补测试，
   直到零问题（详见 AGENTS.md 第六章自愈循环 + `docs/JS-COVERAGE.md`）。

---

## 当前状态快照（2026-06-18）

| 指标 | 值 |
|------|-----|
| HEAD | M62（boa 0.21 + JS 覆盖矩阵 + fetch 命令） |
| 总 commits | 200+ |
| 测试 | **776+ passed**（含 30 项 JS 特性入库测试），0 clippy warnings |
| Crates | 16（含 extractor 后置过滤器） |
| boa 版本 | 0.21（async/await 运行时落地） |
| 二进制 | 14MB（单文件，零运行时依赖） |
| SPA 覆盖率 | 有 SSR 站 80%，纯 CSR 站 39%（详见 assessment） |
| 核心目标 G1 | ✅ 已达成（M4），fetch 命令 M59 当 curl 用 |
| 截图 G2 | ✅ 已达成（M12.1） |
| 跨平台 G3 | ✅ 已达成 |

---

## 何时更新本文档

- **目标变更**（加/删目标、改验收标准）→ 必须更新 + 写 ADR
- **新增白名单依赖** → 更新 G4 白名单
- **完成里程碑** → 更新"当前状态快照"
- **本文档与其他文档冲突** → 以本文档为准，改其他文档
