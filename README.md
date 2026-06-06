# Browser

> 一个用 Rust 手写的、跨平台的、能渲染 SPA 页面供爬虫使用的浏览器。
> 兼具学习目的——通过造轮子深入理解浏览器内部原理。

[![CI](https://img.shields.io/badge/CI-macOS%20%7C%20Linux%20%7C%20Windows-green)](.github/workflows/ci.yml)
[![Tests](https://img.shields.io/badge/tests-260%20passed-brightgreen)](#测试)
[![License](https://img.shields.io/badge/license-MIT-blue)](#license)

---

## 这是什么

一个 **L1 级自研浏览器**（自研架构 + 复用底层解析/IO crate）。核心用途：
**渲染并爬取 SPA（单页应用）页面**。不是 Chrome/Firefox 的替代品，而是
一个**可控的、低依赖的、能执行 JS 的爬虫渲染引擎**。

**项目目标已达成**（M4）：能 fetch → 执行 `<script>` → 渲染 → 截图。
后续 M5-M14 是增强（GUI / 字体 / Storage / Navigation）。

---

## 功能特性

- ✅ **SPA 渲染**：执行页面 `<script>`，JS 通过 DOM API 修改 DOM（M4）
- ✅ **同步 fetch 桥**：JS 可发 `__fetchSetBody(url)` 拿后端 API 数据（M4）
- ✅ **localStorage / sessionStorage**：Web 标准 API，SPA 状态持久化（M13）
- ✅ **history / location**：SPA 路由基础（pushState/replaceState）（M14）
- ✅ **PNG 截图**：`--screenshot` 输出渲染结果为图片（M12.1）
- ✅ **图像 ASCII art**：`image-ascii` 子命令转图片为 ASCII（M12.3）
- ✅ **真实字体**：fontdue + DejaVuSans，支持中文（M7.4）
- ✅ **GUI 窗口**：winit + softbuffer，URL 栏 + 滚动（M5/M7.5）
- ✅ **跨平台**：macOS / Linux / Windows，CI 三平台全绿

完整能力清单见 [docs/FEATURES.md](./docs/FEATURES.md)。

---

## 快速开始

### 安装

```bash
git clone <repo>
cd browser
cargo build --release -p browser-cli
# binary: ./target/release/browser
```

### 使用

```bash
# 1. 抓取静态页面，打印 DOM
./target/release/browser get https://example.com/

# 2. 渲染本地 HTML 为 ASCII（不执行 JS）
./target/release/browser render-file page.html --width 80

# 3. 渲染本地 SPA（执行 JS + localStorage + history）
./target/release/browser render-script spa.html --width 80

# 4. 端到端：fetch URL + 执行 JS + 渲染 + 截图
./target/release/browser render-url https://example.com/ \
    --width 80 --screenshot out.png

# 5. 图像转 ASCII
./target/release/browser image-ascii photo.png --width 60 --height 20

# 6. GUI 窗口浏览（需要显示器）
./target/release/browser open https://example.com/
```

### SPA 爬虫示例

```bash
# 启动本地 SPA（JS 动态生成产品列表 + localStorage token）
mkdir -p /tmp/spa-demo && cat > /tmp/spa-demo/index.html << 'EOF'
<!DOCTYPE html><html><body>
<p>Loading...</p>
<script>
var products = [{n:'Book',p:39}, {n:'Keyboard',p:129}];
localStorage.setItem('token', 'tk-123');
__setBody(products.map(p => p.n + ':$' + p.p).join(' | '));
__appendBody('TOKEN: ' + localStorage.getItem('token'));
</script>
</body></html>
EOF
cd /tmp/spa-demo && python3 -m http.server 8765 &

# 爬取：JS 执行后能拿到动态内容（curl 拿不到）
./target/release/browser render-url http://localhost:8765/index.html --width 80
# stdout: Book:$39 | Keyboard:$129
#         TOKEN: tk-123
```

---

## 测试

```bash
# 三级门禁（每个 commit 前必跑）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
# → 260 passed, 0 failed, 0 clippy warnings
```

测试策略详见 [docs/TESTING.md](./docs/TESTING.md)。

---

## 文档导航

| 文档 | 内容 |
|------|------|
| ⭐ [docs/GOALS.md](./docs/GOALS.md) | **NORTH STAR**：目标 / 非目标 / 验收标准（单一事实来源） |
| [docs/FEATURES.md](./docs/FEATURES.md) | 能力清单（CLI / CSS / JS 桥 / Web API 矩阵） |
| [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) | 架构与数据流（crate 依赖图 + 管线） |
| [docs/CONVENTIONS.md](./docs/CONVENTIONS.md) | 工程规范（提交 / 自研边界 / 依赖白名单） |
| [docs/DIRECTORY.md](./docs/DIRECTORY.md) | 目录结构 + crate 职责 |
| [docs/TESTING.md](./docs/TESTING.md) | 测试分层 + 验收命令 |
| [docs/ROADMAP.md](./docs/ROADMAP.md) | 里程碑路线图（M0-M14 + 前瞻） |
| [PROGRESS.md](./PROGRESS.md) | 活跃日志（每 commit 后更新） |
| [docs/decisions/](./docs/decisions/) | 架构决策记录（ADR） |
| [docs/postmortems/](./docs/postmortems/) | 里程碑复盘 |

**冲突优先级**：GOALS.md > FEATURES.md > ARCHITECTURE.md > 其他。

---

## 项目结构

```
browser/
├── crates/        # 12 个 crate（net/dom/html-parser/css-engine/layout/
│                  #            render/js-runtime/page/cli/gui/storage/navigation）
├── docs/          # 结构化 wiki（GOALS/FEATURES/ARCHITECTURE/...）
├── PROGRESS.md    # 活跃日志
└── README.md      # 本文件
```

详见 [docs/DIRECTORY.md](./docs/DIRECTORY.md)。

## 开发指南

- **Rust stable**（`rust-toolchain.toml` 锁定）
- **无 unsafe**（`#![forbid(unsafe_code)]` 全 workspace）
- **提交规范**：Conventional Commits（见 [CONVENTIONS.md](./docs/CONVENTIONS.md)）
- **每个 commit**：fmt + clippy(-D warnings) + test 全绿

## 已知局限

1. **异步 JS**：`setTimeout`/`Promise` 会抛 ReferenceError（boa 0.20 限制），defer 到切 deno_core
2. **强反爬站点**：百度等返回 `location.replace` 反爬页（location 已支持 M14，Cookie jar 未实现）
3. **GUI 需显示器**：`open` 在 headless 失败，用 `render-url` 或 `--check` 替代
4. **真实图像渲染**：GUI 只显示 `[IMG: src]` 占位符（CLI 用 image-ascii）

详见 [docs/FEATURES.md](./docs/FEATURES.md) §已知局限。

## 贡献

见 [docs/CONVENTIONS.md](./docs/CONVENTIONS.md)。核心原则：
每个功能 = 一个 commit + 可验证的验收测试。

## License

MIT
