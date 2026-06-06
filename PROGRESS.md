# 自研浏览器项目 — 全程进度大纲

> 执行状态：**M0-M9 完成**，M10.1 进行中
> 当前 HEAD: 5992855 (同步实际 git 状态)
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
- ✅ DOM API 桥（createEl/appendChild/setAttribute/innerHTML/getValue/setValue/click/submit）
- ✅ 异步 fetch 桥（__fetchSetBody/__fetchAppendBody）
- ✅ 表单交互（input/textarea/button/form）
- ✅ 图片占位符渲染（[IMG: src]）

**结论**：M4 已满足 SPA 爬虫核心需求，M7-9 为增强功能。

---

## 里程碑进度（实际 git 状态）

| 里程碑 | 状态 | Commit | 测试 | 备注 |
|--------|------|--------|------|------|
| M0 | ✅ | 5147b17 | — | 项目骨架 + CI（9 crate） |
| M1 | ✅ | 多个 | — | HTML 获取 + DOM 树打印 |
| M2 | ✅ | 多个 | 111 | 文本流渲染 + 最小布局引擎 |
| M3 | ✅ | 多个 | 140 | JS 执行 + JS↔DOM 桥 |
| M4 | ✅ | 多个 | 160 | SPA 渲染（**项目目标达成**） |
| M5 | ✅ | 4 commits | 179 | GUI 窗口（winit + softbuffer） |
| M6 | ✅ | 3 commits | 211 | 渲染质量修复 + 黄金对比 + CI |
| M7 | ✅ | 23 commits | 217 | 渲染质量 + 交互 |
| M8 | ✅ | 4 commits | 217 | 表单交互 |
| M9 | ✅ | 3 commits | 217 | 图片占位符渲染 |
| M10 | 🟡 | 进行中 | — | 性能优化（layout 缓存 + 增量渲染） |

---

## M7 — 渲染质量 + 交互 ✅ (23 commits)

**M7.1** ✅ (8 commits): CSS margin/padding/collapsing/snapshots
**M7.2** ✅ (3 commits): DOM API + selector（8 个桥 + * / .class / compound）
**M7.5** ✅ (6 commits): URL 栏 + 滚动
**M7.4** ✅ (5 commits): 真实字体（FontCache + fontdue + DejaVuSans 757KB + 中文支持）
**M7.3** 🟡 defer: 异步 JS

---

## M8 — 表单交互 ✅ (4 commits)

**M8.1**: __getValue/__setValue 桥
**M8.2**: textarea 支持
**M8.3**: __click 桥
**M8.4**: __submit 桥
**M8.5**: e2e 测试

---

## M9 — 图片渲染 ✅ (3 commits)

**M9.1.0**: 移除 img 黑名单
**M9.1.1**: 注入占位符 [IMG: src]
**M9.5**: e2e 验收

---

## M10 — 性能优化 🟡 (进行中)

**目标**：layout 缓存 + 增量渲染

**子任务**：
- M10.1: layout 缓存实现（进行中）
- M10.2: dirty tracking
- M10.3: 增量渲染
- M10.4: e2e 测试

---

## 下一步

**当前执行**：M10.1（layout 缓存）
**下一个**：M10.2（dirty tracking）

---

## 大纲维护说明

- 每次 commit 后更新此文档
- 同步实际 git 状态（HEAD、commits、tests）
- 记录 SPA 爬虫满足条件
