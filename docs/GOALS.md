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
html5ever、hyper+rustls、tokio、clap、boa_engine、winit+softbuffer、url、fontdue、png、image。

### G5. 真实世界可用性（持续提升 🟡）

**定义**：真实网站（example.com、本地 SPA）能抓能渲染。强反爬站点（百度）
和需要异步 JS 的站点（React/Vue 复杂 bundle）是**已知局限**，逐步改进。

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
| **完整 ES6+ 异步语义** | boa 0.20 限制，defer 到切 deno_core |

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

## 决策原则（当 "要不要做 X" 有争议时）

1. **爬虫价值优先**：X 能让更多 SPA 被正确爬取吗？是 → 做；否 → 跳过。
2. **学习价值次之**：X 能加深对浏览器原理的理解吗？是 → 做；否 → 跳过。
3. **复杂度门槛**：X 的实现成本（行数/API 复杂度）超过收益吗？是 → defer。
4. **自研优先**：能用 < 1000 行手写实现吗？是 → 手写；否 → 评估白名单 crate。
5. **不破坏既有**：X 会让现有 260 tests 变红吗？是 → 不做或重构。

---

## 当前状态快照（2026-06-06）

| 指标 | 值 |
|------|-----|
| HEAD | `3afbcbb`（M14.3 navigation/location shim） |
| 总 commits | 97 |
| 测试 | 260 passed, 0 clippy warnings |
| Crates | 12（net/dom/html-parser/css-engine/layout/render/js-runtime/page/cli/gui/storage/navigation） |
| CLI 子命令 | 8（get/parse/render-file/render-script/render-url/open/image-ascii/help） |
| 核心目标 G1 | ✅ 已达成（M4） |
| 截图 G2 | ✅ 已达成（M12.1） |
| 跨平台 G3 | ✅ 已达成 |

---

## 何时更新本文档

- **目标变更**（加/删目标、改验收标准）→ 必须更新 + 写 ADR
- **新增白名单依赖** → 更新 G4 白名单
- **完成里程碑** → 更新"当前状态快照"
- **本文档与其他文档冲突** → 以本文档为准，改其他文档
