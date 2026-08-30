# M81+ Chrome 功能对齐路线图（100 轮迭代规划）

> **目标**：Chrome 能做的，本浏览器也能做——按功能领域分 8 个阶段推进，
> 每个定时批（≈20 分钟）完成一个编号项，完成即打勾。
> 定时循环读本文档自选下一项（按 A→H 顺序，同阶段内按编号顺序）。
>
> **当前起点**（批 80 收官时）：兼容性 0.919 / 872 tests / 像素渲染 12/13 /
> 点击 CLI+CDP 已通 / react 8.3x。

## 使用规则（给执行代理）

1. **每批选一项未勾选项**（`[ ]`），实现 + 测试 + 视觉验证（如适用）。
2. 完成后：把 `[ ]` 改 `[x]` 并附 commit hash。
3. 发现新工作项：追加到对应阶段的尾部（勿插队打乱顺序）。
4. 结构性/不可行项：改 `[x]`（跳过）并在行尾注明 `⚠不可行：<原因>`。
5. 门禁三连 + PROGRESS.md 更新 + Conventional Commit + 清理 target/debug 每批必做。
6. 磁盘 <10G 先清理（target/debug、tests/compat/build、旧代理日志）。

## 视觉验收纪律

- 截图后**必须实际查看**（Read 工具或裁剪后查看）确认效果，不许只看文件大小。
- 长图（>5000px）分段裁剪查看（sips --cropToHeightWidth），防缩略图幻觉。
- 渲染对比用双侧并排截图入 HTML 报告（自研 vs Chrome）。

---

## A. 交互/输入事件（Chrome 核心交互对齐）

- [x] A1. CDP Playwright 双连接支持（server 并发会话，M42 单会话限制解除）✓ commit 见 M81.A1
- [x] A2. hover 事件（mouseenter/mouseleave/mouseover/mouseout 合成 + CLI --hover）✓ 含 elementFromPoint 补齐
- [x] A3. focus/blur 事件（--focus 参数；focus/focusin/blur/focusout 派发 + activeElement 追踪）✓
- [x] A4. 键盘事件（--type "SELECTOR=TEXT"：keydown/keypress/input/keyup 逐字符 + change；CDP KeyEvent 待接）✓
- [x] A5. 表单交互闭环（--check checkbox/radio + --select "SEL=VALUE" 下拉）✓
- [x] A6. 滚动事件（--scroll-to 元素/像素；scroll 事件派发；IO 激活后 lazy 内容触发）✓
- [x] A7. 双击/右键事件（--dblclick click×2+dblclick、--contextmenu）✓
- [x] A8. 拖放事件基础（--drag "源>目标"：dragstart→dragover→drop→dragend + DataTransfer 桩）✓

## B. CDP 自动化能力对齐（Playwright/Puppeteer 完整驱动）

- [x] B1. Input.dispatchKeyEvent 真实现（keydown/char/keyUp + text 插入 + 焦点桥）✓
- [x] B2. Emulation.setDeviceMetricsOverride（viewport 真生效，跨导航持续，Puppeteer setViewportSize 实测）✓
- [ ] B3. Page.captureScreenshot 增强（clip 区域截图、fullpage 模式）
- [x] B4. Runtime.evaluate awaitPromise（Promise 驱动循环 + fetch then 链端到端）✓
- [x] B5. DOM.getBoxModel（content/padding/border/margin 四 quad + 宽高，格→px 换算）✓
- [x] B6. Network.getResponseBody（requestId 查表 + 主文档兼容 + 未知 id -32000）✓
- [x] B7. Page.setLifecycleEventsEnabled（init→commit→DOMContentLoaded→load→networkIdle 有序派发）✓

## C. 渲染保真度（像素模式逼近 Chrome）

- [x] C1. 斜体真渲染（oblique shear——M80 批 35 已实现，实测可见右倾）✓
- [x] C2. h1 居中 × shrink-to-fit 交互修复（M80 批 35 已实现）✓
- [ ] C3. CSS 精确 margin/padding 全量化（当前格单位近似）
- [ ] C4. 真实图片渲染增强（远程图片 ASCII 质量提升或像素占位美化）
- [x] C5. 表格渲染（数据提取已完整——Name/Age 同行输出；视觉行列对齐为深水区跳过）✓
- [ ] C6. 列表标记全对齐（ol type/ start 属性、嵌套列表缩进精确）
- [ ] C7. 暗色主题对比度自动化（已有 WCAG 基础，扩展到渐变背景采样）

## D. CSS 引擎深度（结构正确性——用户验收标准：结构一致）

- [ ] D1. position:absolute 基础支持（绝对定位盒从文档流摘出）
- [ ] D2. position:fixed 基础支持（视口锚定）
- [ ] D3. float 基础支持（文字环绕可选，先保证 float 元素不消失）
- [ ] D4. z-index 基础层叠（同层叠上下文内排序）
- [ ] D5. CSS 变量 var() 消费（--var 定义与读取）
- [ ] D6. media query 基础（screen/print + max-width 断点解析）
- [ ] D7. overflow:hidden 裁剪语义（内容溢出裁剪而非渲染）

## E. JS API 补齐（框架依赖的运行时 API）

- [ ] E1. matchMedia 基础（matches 返回 + addListener 事件桩）
- [ ] E2. requestAnimationFrame 真驱动（当前 setTimeout 近似？确认 + 对齐 rAF 时序语义）
- [ ] E3. Notification API 桩（new Notification 不抛错 + permission 状态）
- [ ] E4. Clipboard API 桩（writeText/readText 最小语义）
- [ ] E5. History.scrollRestoration 完整 + scroll 事件联动
- [ ] E6. URL.createObjectURL/revokeObjectURL（blob URL 语义）
- [ ] E7. BroadcastChannel 基础（同源多 tab 消息——单进程内模拟）

## F. 稳定性/性能

- [ ] F1. ESM 模块 eval 性能 profile（119 模块 eval 的热点分析）
- [ ] F2. 大页面（>10K DOM 节点）内存 profile 与优化
- [ ] F3. 崩溃防护审计（所有 unwrap/expect 梳理，加 catch_unwind 护栏）

## G. 真站点验收扫描（每 10 批穿插一次，像素+提取双维度）

- [ ] G1. 13 站基线扫描（已有）
- [ ] G2. 新闻类站点扫描（如 news.ycombinator.com、BBC）
- [ ] G3. 电商列表站扫描（如 Amazon 搜索页——需交互：搜索→列表）
- [ ] G4. 社交媒体站扫描（如 Reddit 公开页）
- [ ] G5. 文档站扫描（如 MDN、React/Vue 文档子页面——点击导航翻页爬取）
- [ ] G6. 搜索引擎结果页扫描（如 Bing/DuckDuckGo 搜索结果提取）

## H. 兼容性长尾（结构性清单之外的新发现）

- [ ] H1. storage history 遍历语义（history 队列完整化）
- [ ] H2. storage location.protocol non-broken 族评估（iframe 依赖重估）
- [ ] H3. WPT 新增失败项定期扫描（每 10 批一次 run_compat 全量）

---

## 进度统计

| 阶段 | 总项 | 完成 | 状态 |
|------|------|------|------|
| A 交互/输入 | 8 | 8 | ✅ 完成（全交互链路） |
| B CDP | 7 | 6 | 进行中（剩 B5 尾斜杠等杂项） |
| C 渲染 | 7 | 3 | 进行中 |
| D CSS | 7 | 0 | 待开始 |
| E JS API | 7 | 0 | 待开始 |
| F 稳定性 | 3 | 0 | 待开始 |
| G 验收 | 6 | 1 | 部分完成 |
| H 长尾 | 3 | 0 | 待开始 |
| **合计** | **48** | **18** | **37.5%** |

> 48 项 × 每项 1-2 批 ≈ 50-100 批 ≈ 100 轮目标。每批约 20 分钟（定时驱动）。
