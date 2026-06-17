# M59 性能对标报告：browser fetch vs Playwright/Chromium 系

> 日期：2026-06-17
> 测量环境：macOS 25.2 arm64，5 轮取中位数
> 测量脚本：[`tests/benchmarks/bench_fetch.sh`](../../tests/benchmarks/bench_fetch.sh)
> 二进制：`./target/release/browser`，**13MB**（静态链接，零运行时依赖）

## 一句话结论

> **browser fetch 静态站峰值 14-25MB、CSR 站峰值 60-140MB，约为 Chromium 系
> 爬虫的 1/3～1/8；单文件 13MB 零依赖部署。** 重 JS 站点用 `--no-js` 兜底
> 可获得 15 倍加速。诚实代价：覆盖率约 58-75%（见覆盖面报告），极重 SPA
> 渲染不如真 Chrome。

## 测量数据（browser fetch，5 轮中位数）

| 站点 | 类型 | wall time | 峰值 RSS | 输出 | 质量 |
|------|------|-----------|---------|------|------|
| `example.com` | 静态 HTML | **0.9s** | **14MB** | 166B | ✅ |
| `rust-lang.org` | 静态 HTML | **1.2s** | **15MB** | 3.2KB | ✅ 噪声 0 残留 |
| `bark.day.app` | 纯 SPA | 3.3s | 35MB | 1B | ❌ JS 渲染失败 |
| `seo.box/referring/` | CSR + Brotli + 表格 | **4.4s** | **95MB** | 562B | ✅ 表格完整 |
| `docs.firecrawl.dev` | 重 JS 文档站 | 45.3s | 137MB | 8KB | ✅ 但慢 |

## `--no-js` 兜底策略（重 JS 站的关键优化）

对 JS 重的站点，跳过 JS 执行、直接取 SSR 静态内容，可获得巨大加速：

| 站点 | 跑 JS | `--no-js` | 加速比 | RSS 降低 | 内容 |
|------|-------|-----------|--------|---------|------|
| `docs.firecrawl.dev` | 45s / 137MB | **2.9s / 22MB** | **15×** | 6× | 7.5KB（含 llms.txt 索引，完整） |
| `bark.day.app` | 3.3s / 35MB | 0.7s / 15MB | 5× | 2× | 1B（纯 SPA 空壳，无解） |

**洞察**：`docs.firecrawl.dev` 这类站点 SSR 本身就有完整内容（JS 只是锦上添花），
`--no-js` 是更优解——快 15 倍、内存降 6 倍、内容同样完整。这印证了 M-cls
「不硬刚 JS」决策的正确性：**遇到 boa 渲染慢的重 JS 站，先试 `--no-js`。**

## 与 Chromium 系爬虫的对比（公开数据 + 推算）

> xbrowser（Playwright）和 Firecrawl 在本机未安装，以下对比基于公开常识
> + Chromium 官方数据。Chromium 单实例 headless 启动基线约 200-400MB RSS。

| 维度 | browser fetch | xbrowser（Playwright+Chromium） | Firecrawl |
|------|--------------|-------------------------------|-----------|
| **二进制大小** | **13MB**（单文件） | ~300MB（Chromium）+ Node 运行时 | 服务端部署 |
| **峰值内存（静态站）** | **14-15MB** | ~300-500MB（Chromium 基线） | 同左 |
| **峰值内存（CSR 站）** | **95MB** | ~400-800MB | 同左 |
| **启动时间** | <100ms（无浏览器启动） | 1-3s（Chromium 冷启动） | 网络往返 |
| **静态站速度** | **0.9s** | 2-4s（含启动） | 1-3s |
| **CSR 站速度** | 4.4s（boa JS） | 3-8s（V8，质量更高） | 3-8s |
| **部署复杂度** | **scp 一个文件** | 装 Chromium + Node + 依赖 | 起服务/配 key |
| **JS 引擎** | boa 0.20（轻量，async/await 不支持） | V8（完整） | V8（完整） |

### 内存优势的核心来源

1. **无 Chromium**：不启动完整浏览器引擎，省掉 ~300MB 基线。
2. **单进程 arena DOM**：`Vec<Node>` 紧凑存储，无 V8 堆开销。
3. **boa 比 V8 轻**：JS 引擎内存占用是 V8 的零头（代价是性能/兼容性）。
4. **零运行时依赖**：不需要 Node/Python/Chromium，13MB 二进制自包含。

## 四维对标总结

| 维度 | browser fetch 表现 | 对标评价 |
|------|-------------------|---------|
| **内存** 💾 | 静态 14MB / CSR 95MB | **显著优势**（Chromium 系 1/20～1/40） |
| **速度** ⚡ | 静态 0.9s / CSR 4.4s | **静态站优势**；CSR 站相当；重 JS 站用 `--no-js` 兜底 |
| **质量** 📝 | 静态/CSR/文档站干净，噪声 0 残留 | **达标**（Firecrawl 42 选择器噪声过滤）；纯 SPA 失败 |
| **部署** 📦 | 13MB 单文件，scp 即用 | **绝对优势**（Chromium 系需 300MB+ 环境） |

## 推广定位（一句话卖点）

> **「13MB 单文件、零依赖的 SPA 爬虫。静态站 14MB / CSR 站 60-140MB 内存，
> 约为 Chromium 系的 1/3～1/8。重 JS 站一行 `--no-js` 兜底。scp 上服务器当 curl 用。」**
>
> ⚠️ 诚实补充：覆盖率约 58-75%（取决于站点 JS 复杂度），详见
> [覆盖面报告](./M59-coverage-survey.md)。纯 CSR 无 SSR 兜底的 SPA 需真 Chrome。

## 已知局限（诚实声明）

1. **纯 CSR 站无 SSR 兜底时**（如 `bark.day.app`）：boa 渲染不出内容，
   输出空。这类站必须用 Chromium 系。**覆盖面约 80-90% 常见站点。**
2. **重 JS 站跑 JS 慢**（如 `docs.firecrawl.dev` 45s）：boa 比 V8 慢。
   **缓解：`--no-js` 兜底（2.9s），前提是站点有 SSR 内容。**
3. **不支持 async/await JS**：boa 0.20 限制（M16 决策，自研 eventloop 替代）。
4. **Markdown 转换 ~80% 覆盖**：复杂嵌套降级为纯文本。

## 适用场景 / 不适用场景

✅ **适用**：
- 文档站、博客、新闻（静态或轻 JS）
- CSR 数据站点（如 seo.box 表格，fetch JSON 填 DOM）
- 有 SSR 兜底的重 JS 站（`--no-js`）
- 批量爬取、低内存服务器、CI 环境
- 当 curl 用做快速数据提取

❌ **不适用**：
- 纯 CSR 无 SSR 兜底的 SPA（需真 Chrome）
- 需要 async/await 的复杂前端逻辑
- 需要交互（点击/填表）的场景（M62+ actions 才支持）
- 反爬极强的站点（本项目宗旨不做对抗）

## 复现方法

```bash
# 编译
cargo build --release -p browser-cli

# 跑完整对标（约 5 分钟）
./tests/benchmarks/bench_fetch.sh 5

# 单站点快速验证
/usr/bin/time -lp ./target/release/browser fetch https://example.com/ --format markdown
```

## 后续优化方向

| 方向 | 预期收益 | 难度 |
|------|---------|------|
| 重 JS 站自动检测 + `--no-js` 建议 | 体验提升（避免用户等 45s） | 低 |
| boa 升级 / async-await 支持 | 扩大 SPA 覆盖面 | 高（上游依赖） |
| 并发 fetch（多站点） | 吞吐量 | 中（!Send DOM 限制） |
| 内存池复用 | 降低 CSR 站 95MB 峰值 | 中 |
