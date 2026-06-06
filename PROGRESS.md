# 自研浏览器项目 — 全程进度大纲

> 执行状态：**M0-M10 全部完成 ✅**（SPA 爬虫功能满足）
> 当前 HEAD: fa580fa (M10 完成)
> Workspace: 217 tests, 0 clippy warnings

---

## SPA 爬虫满足条件（M4 已达成 ✅）

**核心能力（已具备）**：
- ✅ HTTP 获取（hyper + rustls）
- ✅ HTML 解析（html5ever wrapper）
- ✅ DOM 树构建（arena pattern）
- ✅ JS 执行（boa_engine, <script> 标签）
- ✅ 布局引擎（block + inline + 折行 + margin/padding collapsing）
- ✅ 文本渲染（ASCII + GUI + 真实字体）
- ✅ CSS 选择器（tag/class/id/compound）
- ✅ DOM API 橋（createEl/appendChild/setAttribute/innerHTML/getValue/setValue/click/submit）
- ✅ 异步 fetch 橋（__fetchSetBody/__fetchAppendBody）
- ✅ 表单交互（input/textarea/button/form）
- ✅ 图片占位符渲染（[IMG: src]）

**结论**：M4 已满足 SPA 爬虫核心需求，M7-10 为增强功能。

---

## 里程碑进度（实际 git 状态）

| 里程碑 | 状态 | Commit | 测试 | 备注 |
|--------|------|--------|------|------|
| M0 | ✅ | 5147b17 | — | 项目骨架 + CI（9 crate） |
| M1 | ✅ | 多个 | — | HTML 获取 + DOM 树打印 |
| M2 | ✅ | 多个 | 111 | 文本流渲染 + 最小布局引擎 |
| M3 | ✅ | 多个 | 140 | JS 执行 + JS↔DOM 橋 |
| M4 | ✅ | 多个 | 160 | SPA 渲染（**项目目标达成**） |
| M5 | ✅ | 4 commits | 179 | GUI 窗口（winit + softbuffer） |
| M6 | ✅ | 3 commits | 211 | 渲染质量修复 + 黄金对比 + CI |
| M7 | ✅ | 23 commits | 217 | 渲染质量 + 交互 |
| M8 | ✅ | 4 commits | 217 | 表单交互 |
| M9 | ✅ | 3 commits | 217 | 图片占位符渲染 |
| M10 | ✅ | 2 commits | — | 性能优化（layout 缓存 + dirty tracking） |

---

## M7 — 渲染质量 + 交互 ✅ (23 commits)

**M7.1** ✅ (8 commits): CSS margin/padding/collapsing/snapshots
**M7.2** ✅ (3 commits): DOM API + selector
**M7.5** ✅ (6 commits): URL 栏 + 滚动
**M7.4** ✅ (5 commits): 真实字体
**M7.3** 🟡 defer: 异步 JS

---

## M8 — 表单交互 ✅ (4 commits)

**M8.1**: __getValue/__setValue 橋
**M8.2**: textarea 支持
**M8.3**: __click 橋
**M8.4**: __submit 橋
**M8.5**: e2e 测试

---

## M9 — 图片渲染 ✅ (3 commits)

**M9.1.0**: 移除 img 黑名单
**M9.1.1**: 注入占位符 [IMG: src]
**M9.5**: e2e 验收

---

## M10 — 性能优化 ✅ (2 commits)

**M10.1** ✅ (05e3f9f): LayoutCache 实现
- `get_or_compute(tree, styles)` → &LayoutTree（缓存）
- 3 个单元测试

**M10.2** ✅ (9bcad04): DirtyTracker 实现
- `mark(id)` / `mark_subtree(tree, root_id)` / `is_dirty(id)` / `clear()`
- 3 个单元测试

---

## 下一步可选

**M11** 🟡: WebSocket 支持
- 连接管理 + 消息发送/接收

**M12** 🟡: Storage（localStorage/sessionStorage）

**M13** 🟡: History API（pushState/replaceState）

**M14** 🟡: Worker 线程

---

## 项目总结

**核心成就**：
- L1 级自研架构（arena DOM + 手写布局 + 自研渲染）
- SPA 爬虫功能满足（M4 达成）
- GUI 窗口 + 滚动（M5 + M7.5）
- 真实字体（M7.4, DejaVuSans 757KB）
- CSS margin/padding + collapsing（M7.1）
- 完整 DOM API（M7.2）
- 表单交互（M8）
- 图片占位符（M9）
- 性能优化框架（M10）

**Workspace**：217 tests, 0 clippy warnings

**实际 HEAD**：fa580fa（M10 完成）

---

## 大纲维护说明

- 每次 commit 后更新此文档
- 同步实际 git 状态（HEAD、commits、tests）
- 记录 SPA 爬虫满足条件
