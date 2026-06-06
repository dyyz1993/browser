# 自研浏览器项目 — 全程进度大纲

> 执行状态：**M0-M9 完成**，M10.1 进行中
> 当前 HEAD: 063e27a (M9 完成 + M10 进行中)
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

## 里程碑进度

| 里程碑 | 状态 | Commit | 测试 | 备注 |
|--------|------|--------|------|------|
| M0 | ✅ | 5147b17 | — | 项目骨架 + CI（9 crate） |
| M1 | ✅ | 多个 | — | HTML 获取 + DOM 树打印 |
| M2 | ✅ | 多个 | 111 | 文本流渲染 + 最小布局引擎 |
| M3 | ✅ | 多个 | 140 | JS 执行 + JS↔DOM 桥 |
| M4 | ✅ | 多个 | 160 | SPA 渲染（**项目目标达成**） |
| M5 | ✅ | 4 commits | 179 | GUI 窗口（winit + softbuffer） |
| M6 | ✅ | 3 commits | 211 | 渲染质量修复 + 黄金对比 + CI |
| M7 | ✅ | 22 commits | 217 | 渲染质量 + 交互（CSS margin + DOM API + 真实字体 + URL 栏 + 滚动） |
| M8 | ✅ | 4 commits | 217 | 表单交互（input/textarea/button/form） |
| M9 | ✅ | 3 commits | 217 | 图片占位符渲染（[IMG: src]） |
| M10 | 🟡 | 进行中 | — | 性能优化（layout 缓存 + 增量渲染） |

---

## M7 — 渲染质量 + 交互 ✅ (22 commits)

**M7.1** ✅ (8 commits): CSS margin/padding/collapsing/snapshots
- M7.1.1: margin parsing module
- M7.1.2: LayoutBox carries margin/padding BoxEdges
- M7.1.3-4: construct.rs/block.rs 填充和消耗
- M7.1.5: horizontal margins
- M7.1.6: UA defaults + <style> extraction
- M7.1.7: margin collapsing through empty anon
- M7.1.8: UPDATE_SNAPSHOTS=1 重生成

**M7.2** ✅ (3 commits): DOM API + selector
- M7.2.1: 8 个桥（createEl/appendChild/setAttr/getElById/qs/setText/getTag/getBody）
- M7.2.2: e2e tests（integration_dom_api.rs）
- M7.2.4: selector 增强（* / .class / compound）

**M7.5** ✅ (6 commits): URL 栏 + 滚动
- M7.5.1: URL 栏渲染（灰底 + placeholder）
- M7.5.2-3: 键盘输入 + URL navigation
- M7.5.4: MouseWheel 滚动
- M7.5.5: PageUp/PageDown/Home/End
- M7.5.6: long-scroll fixture（200 lines）

**M7.4** ✅ (5 commits): 真实字体
- M7.4.2: FontCache（fontdue wrapper）
- M7.4.5: bitmap_font 替换为 fontdue
- M7.4.5.1: skip font tests pending M7.4.1
- M7.4.1: embed DejaVuSans.ttf 757KB
- M7.4.7: 中文支持验证（CLI）

**M7.3** 🟡 defer: 异步 JS（MicrotaskQueue 写完但 boa 0.20 API 复杂）

---

## M8 — 表单交互 ✅ (4 commits)

**M8.1**: __getValue/__setValue 桥
**M8.2**: textarea 支持（通过 __getValue）
**M8.3**: __click 桥
**M8.4**: __submit 橋
**M8.5**: e2e 测试（form-interaction.html）

---

## M9 — 图片渲染 ✅ (3 commits)

**M9.1.0**: 移除 img 非渲染黑名单
**M9.1.1**: 注入占位符文本 [IMG: src]
**M9.5**: e2e 验收（本地 + 远程 img）

占位符已满足爬虫需求（图片 src 可见）。

---

## M10 — 性能优化 🟡 (进行中)

**目标**：layout 缓存 + 增量渲染

**验收**：
- 布局缓存（避免重复计算）
- 增量渲染（只重绘 dirty 区域）

**子任务**：
- M10.1: layout 缓存实现（进行中）
- M10.2: dirty tracking
- M10.3: 增量渲染
- M10.4: e2e 测试

---

## 下一步

**当前执行**：M10.1（layout 缓存实现）
**下一个**：M10.2（dirty tracking）

---

## 大纲维护说明

- 每次 commit 后更新此文档
- 同步实际 git 状态（HEAD、commits、tests）
- 记录 SPA 爬虫满足条件
