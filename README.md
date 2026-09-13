# Browser

> 一个用 Rust 手写的、跨平台的、能渲染 SPA 页面供爬虫使用的浏览器。
> 兼具学习目的——通过造轮子深入理解浏览器内部原理。

[![CI](https://img.shields.io/badge/CI-macOS%20%7C%20Linux%20%7C%20Windows-green)](.github/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/dyyz1993/browser?color=blue)](https://github.com/dyyz1993/browser/releases)
[![License](https://img.shields.io/badge/license-MIT-blue)](#license)

---

## 一键安装

```bash
curl -fsSL https://raw.githubusercontent.com/dyyz1993/browser/main/install.sh | sh
```

自动检测操作系统（macOS/Linux）、CPU 架构（x86_64/arm64）、glibc 版本，下载对应的预编译二进制。老 Linux（glibc < 2.29）会自动附带 portable-glibc 兼容包。

## 这是什么

一个 **L1 级自研浏览器**（自研架构 + 复用底层解析/IO crate）。核心用途：
**渲染并爬取 SPA（单页应用）页面**，包括需要通过反爬验证的站点。

不是 Chrome/Firefox 的替代品，而是一个**可控的、低依赖的、能执行 JS 的爬虫渲染引擎**。

---

## 功能特性

- ✅ **SPA 渲染**：执行页面 `<script>`，JS 通过 DOM API 修改 DOM
- ✅ **双 JS 引擎**：QuickJS（默认，轻量 10MB）+ V8（Chrome 同源，过反爬）
- ✅ **Chrome 同形 TLS**：BoringSSL 传输（`--features chrome-tls`）
- ✅ **反爬通过**：自动通过 xcancel 等站的浏览器验证（PoW + 指纹 + 加密）
- ✅ **资源下载**：`--save-assets` 按域名/后缀/glob 匹配批量下载
- ✅ **CDP server**：Puppeteer/Playwright 可连
- ✅ **多格式输出**：text / markdown / html / links / JSON
- ✅ **PNG 截图**：`--screenshot` 输出渲染结果为图片
- ✅ **跨平台**：macOS (ARM64/Intel) / Linux (x86_64)

完整能力清单见 [docs/FEATURES.md](./docs/FEATURES.md)。

---

## 快速开始

### 方式一：一键安装（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/dyyz1993/browser/main/install.sh | sh
browser --version
```

### 方式二：从 Release 手动下载

到 [Releases 页面](https://github.com/dyyz1993/browser/releases) 下载对应平台的 tar.gz：

| 文件 | 平台 |
|------|------|
| `browser-darwin-arm64.tar.gz` | macOS (M1/M2/M3) |
| `browser-linux-x86_64.tar.gz` | Linux x86_64 |
| `portable-glibc-x86_64.tar.gz` | 老 Linux glibc 兼容包（可选） |

```bash
tar xzf browser-linux-x86_64.tar.gz
chmod +x browser
./browser --version
```

### 方式三：从源码编译

```bash
git clone https://github.com/dyyz1993/browser.git
cd browser

# 轻量版（默认，QuickJS，10MB）
cargo build --release -p browser-cli
# → ./target/release/browser

# 全功能版（V8 + Chrome TLS，60MB，需要 cmake）
cargo build --release -p browser-cli --features "quickjs,v8,chrome-tls"
```

---

## 使用

### 基本用法

```bash
# 拉取页面（文本）
browser fetch https://example.com/ --format text

# 拉取 SPA 页面（等 JS 执行完再提取）
browser fetch https://react.dev/ --format markdown

### 需要通过反爬验证的站点（如 xcancel）

# 全功能版（需 --features "v8,chrome-tls" 构建）
# 需要能访问目标站点的代理
browser fetch https://xcancel.com/elonmusk \
  --js-engine v8 \
  --proxy http://127.0.0.1:7890 \
  --format markdown

### 批量下载资源

# 按域名下载全部图片
browser fetch https://xcancel.com/elonmusk \
  --js-engine v8 --proxy http://127.0.0.1:7890 \
  --format markdown \
  --save-assets "pbs.twimg.com" \
  --save-dir ./images

# 按 glob 匹配
  --save-assets "**/profile_images/**_400x400.jpg"

# 按后缀
  --save-assets ".jpg" --save-assets ".woff2"

### 截图

browser render-url https://example.com/ --screenshot out.png

### 启动 CDP server（Puppeteer 可连）

browser cdp --port 9222
```

---

## 反爬说明

本项目能通过以下类型的反爬验证：

| 验证类型 | 原理 | 状态 |
|----------|------|------|
| **PoW 工作量证明** | JS 解题（SHA-256 前缀匹配） | ✅ 自动通过 |
| **浏览器指纹** | V8 引擎 + 100+ 项 API 对齐 Chrome | ✅ 自动通过 |
| **TLS 指纹** | BoringSSL（Chromium 同源库） | ✅ 需要 `chrome-tls` |
| **行为序列** | favicon / Sec-Fetch / priority 等请求头 | ✅ 自动通过 |
| **验证码** | 图片验证码 | ❌ 不处理 |

**注意事项**：
- 全功能版（过反爬）需要 `--features "quickjs,v8,chrome-tls"` 编译
- 国内服务器需要 `--proxy` 指定一个能访问目标站点的代理
- xcancel.com 的验证时好时坏（有暂停窗口），失败时重试即可

---

## 架构

```
                    ┌─────────────┐
                    │  cli (bin)  │  ← 唯一可执行入口
                    └──┬──────────┘
        ┌──────────┬───┼────┬─────────┬────────┐
        ▼          ▼   ▼    ▼         ▼        ▼
     render    gui  cdp  page    js-runtime  storage
        │       │    │    |    (QuickJS/V8) (后端)
        │       │    │                │
     layout    font  ws         ┌───┴───┐
        │      (共享)            │       │
     css-engine                dom     net
        │                    (arena)     ↑
     html-parser                       cookie / eventloop
```

- **16 个 Rust crate**，`#![forbid(unsafe_code)]`（例外仅限 cli 沙箱）
- **双 JS 引擎**：QuickJS（rquickjs 0.12）+ V8（Chrome 152 同源），`trait JsEngine` 切换
- **Chrome 同形 TLS**：BoringSSL + H2（`--features chrome-tls`，默认关）
- 详细文档见 [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md)

---

## 测试

```bash
cargo test --workspace
```

---

## License

MIT
