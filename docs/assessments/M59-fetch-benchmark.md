# M59 fetch 命令对标验收

> 日期：2026-06-17
> 对标脚本：[`tests/benchmarks/compare_xbrowser.sh`](../../tests/benchmarks/compare_xbrowser.sh)
> 设计文档：[`docs/plans/M59-fetch-command-design.md`](../plans/M59-fetch-command-design.md)

## 验收结论：✅ 通过

`browser fetch` 命令已实装，4 种格式（markdown/html/text/links）+ 噪声过滤 + CSS 选择器 + JSON 输出全部可用。对标脚本 2/2 真实站点通过关键词命中检查。

## 对标结果（browser fetch vs xbrowser scrape）

> xbrowser 当前未安装，本次为 browser-only 验证 + 关键词命中检查。
> 装上 xbrowser 后脚本会自动跑 side-by-side 行数对比。

| 站点 | 类型 | browser fetch 结果 | 关键词命中 |
|------|------|-------------------|-----------|
| `example.com` | 静态 HTML 基线 | 3 行 / 166 字节，markdown 干净 | ✅ Example Domain, documentation examples |
| `seo.box/referring/` | CSR + Brotli 压缩 + JS 填表格 | 7 行 / 562 字节，GFM 表格 | ✅ Domain, Share |

### 端到端验证记录

```bash
# example.com → markdown
$ browser fetch https://example.com/ --format markdown
# Example Domain
This domain is for use in documentation examples without needing permission. Avoid use in operations.
[Learn more](https://iana.org/domains/example)

# seo.box（M58 修复 brotli 后的 CSR 站）→ markdown 表格
$ browser fetch https://seo.box/referring/ --format markdown --selector "table"
| No. | Domain | Global Rank | Share (%) | Visits (K) | Change (%) | Comparison |
| --- | --- | --- | --- | --- | --- | --- |
...

# links 格式（相对 URL 绝对化）
$ browser fetch https://example.com/ --format links
Learn more → https://iana.org/domains/example

# --json 结构化输出
$ browser fetch https://example.com/ --format text --json
{"url":"https://example.com/","title":"Example Domain","content":"Example Domain\n..."}
```

## 测试覆盖

| 层级 | 数量 | 内容 |
|------|------|------|
| L1 单元（extractor） | 48 | selector/html/text/links/markdown 各 format + 噪声过滤 + force-include + 空兜底 |
| L2 集成（cli） | 9 | 4 种 format + only_main_content + selector + json + 非法格式 + GFM 表格 |
| L3 真实站点 | 2 | example.com（静态）+ seo.box（CSR+brotli） |
| **总计** | **59** | workspace 全量 739 passed / 0 failed |

## 与 xbrowser / Firecrawl 的能力对比

| 能力 | browser fetch | xbrowser scrape | Firecrawl |
|------|--------------|-----------------|-----------|
| 单页 markdown | ✅ | ✅ | ✅ |
| html / text 格式 | ✅ | ✅ | ✅ |
| links 格式 | ✅ | ❌（需 map 命令） | ✅ |
| 噪声过滤（onlyMainContent） | ✅ Firecrawl 42 选择器 | ✅ | ✅ 原版 |
| CSS 选择器提取 | ✅ `--selector` | ✅ `--selector` | ✅ |
| JSON 结构化输出 | ✅ `--json` | ✅ `--json` | ✅ |
| networkidle 等待 | ✅ eventloop pump | ✅ Playwright | ✅ |
| 多页 crawl | ❌（M60+） | ✅ | ✅ |
| 自动化 actions | ❌（M60+） | ✅ | ✅ |
| SPA 渲染引擎 | boa（轻量） | Playwright/Chrome | Playwright/Chrome |

## 已知局限（首版）

1. **Markdown 转换覆盖 ~80%**：复杂嵌套（如表格里的列表）、罕见 HTML5 标签降级为纯文本。记为已知局限，不硬刚。
2. **css-engine 不支持属性选择器**：`a[href]` 这类无法用 `--selector`。links 格式内部直接遍历 `<a>` 绕开此限制。
3. **JS 引擎差距**：boa 0.20 渲染不了的复杂 SPA（如需 async/await、复杂 bundle），对标 xbrowser（真 Chrome）可能拿不到数据。符合 M-cls 决策——记为已知局限，走 SSR 兜底而非硬刚 JS。
4. **无 LLM 清洗**：Firecrawl 的 onlyCleanContent（GPT-4o 去噪）非本项目宗旨（无 AI 依赖）。
5. **无图片/OG 元数据提取**：defer 到后续 format 扩展。

## 后续路线（M60+）

| 里程碑 | 能力 | 触发条件 |
|--------|------|---------|
| M60 | `browser map` 发现全站 URL | fetch 单页稳定后 |
| M61 | `browser crawl` 递归多页 | map 之后 |
| M62 | `browser actions` 自动化（点击/填表/截图） | 需 CDP 交互层 |
| - | 图片/OG 元数据 format 扩展 | 按需 |
| - | HTTP 服务模式（curl/网页可调） | 单机 CLI 稳定后 |
