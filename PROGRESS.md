# 自研浏览器项目 — 全程进度大纲

> 执行状态：**M0-M13 全部完成 ✅**（含 bug 修复 + 截图 + 图像 ASCII + Storage）
> 当前 HEAD: 0bc15a9 (M13.4 e2e storage)
> Workspace: 241 tests, 0 clippy warnings ✅

---

## SPA 爬虫满足条件（M4 已达成 ✅，M13 增强）

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
- ✅ **localStorage / sessionStorage（M13）** ← 新增
- ✅ **PNG 截图输出（M12.1）** ← 新增
- ✅ **image-ascii 子命令（M12.3）** ← 新增

**结论**：M4 满足 SPA 爬虫核心需求；M12-13 为增强功能（截图 + 图像 + 持久化）。

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
| M11 | ✅ | 3 commits | 217 | Bug 修复（vh/vw/rem/pt） |
| M12 | ✅ | 3 commits | 225 | 截图 + 图像 ASCII |
| M13 | ✅ | 4 commits | 241 | localStorage/sessionStorage |

---

## M7 — 渲染质量 + 交互 ✅ (23 commits)

**M7.1** ✅ (8 commits): CSS margin/padding/collapsing/snapshots
**M7.2** ✅ (3 commits): DOM API + selector
**M7.5** ✅ (6 commits): URL 栏 + 滚动
**M7.4** ✅ (5 commits): 真实字体
**M7.3** 🟡 defer: 异步 JS（boa 0.20 JsObject::call 私有）

---

## M8 — 表单交互 ✅ (4 commits)

**M8.1**: __getValue/__setValue 橋（4010ff0）
**M8.5**: e2e 测试（6bd8a24）

---

## M9 — 图片渲染 ✅ (3 commits)

**M9.5**: e2e 验收（8dcde2b）

---

## M10 — 性能优化 ✅ (2 commits)

**M10.1** ✅ (05e3f9f): LayoutCache 实现
**M10.2** ✅ (9bcad04): DirtyTracker 实现

---

## M11 — Bug 修复 ✅ (3 commits)

**Bug 1**: example.com 前面 16 行空白 → 2 行（9b5dfb7 + e92454a）
- 根因：CSS `body{margin:15vh auto}` 的 `vh` 被解析为 `Length::Px(15.0)`
- 修复：vh/vw/vmin/vmax → `Length::Zero`（ASCII 模式无 viewport）
- rem → `Length::Px(value * 16.0)`
- pt → `Length::Px(value * 4/3)`

**Bug 2**（调查后不是 bug）：demo.html "Loading..." → 用 render-script 而非 render-file

---

## M12 — 截图 + 图像 ASCII ✅ (3 commits)

**M12.1** ✅ (566ae03): PNG screenshot 输出
- `--screenshot <path>` flag 加在 render-file / render-script
- 实现：fontdue 真实字体 + png crate
- 5 张 PNG 生成在 tests/output/screenshots/
- 3 个单元测试

**M7.3** 🟡 defer (3f2d278): 异步 JS 文档
- boa 0.20 的 JsObject::call 是私有 API
- 推迟到切到 deno_core / V8 后

**M12.3** ✅ (929d507): image-ascii 子命令
- `./browser image-ascii <png/jpg> --width 60 --height 20`
- 算法：ITU-R BT.601 灰度 + 10 级 ASCII ramp `' .:-=+*#%@'`
- image crate（~30k 行，<10w 阈值）
- 5 个单元测试

---

## M13 — Web Storage（localStorage / sessionStorage）✅ (4 commits)

**M13.1** ✅ (1d4cb8a): browser-storage crate
- Rc<RefCell<HashMap>> handle，6 个 API: get/set/remove/clear/len/key
- localStorage 和 sessionStorage 共享一个 store（MVP 爬虫够用）
- 8 个单元测试

**M13.2** ✅ (3c951b9): __storage* bridges + install_storage()
- thread-local CURRENT_STORAGE slot
- TreeGuard::drop 同时清理 storage slot
- 6 个 bridge: __storageGet/Set/Remove/Clear/Len/Key
- with_storage(|s| ...) helper（同 with_tree 模式）

**M13.3** ✅ (ad30e00): localStorage/sessionStorage JS 对象 shim
- JS 代码可直接用 Web 标准 API：
  ```js
  localStorage.setItem('k', 'v')
  localStorage.getItem('k')   // 'v' 或 null
  localStorage.removeItem('k')
  localStorage.clear()
  localStorage.length()       // 注意：MVP 用方法
  localStorage.key(0)
  ```
- 用 ObjectInitializer + 内部 eval __storage* 函数
- 6 个单元测试（set/get/null/remove/length/clear/session_share）

**M13.4** ✅ (0bc15a9): e2e fixture
- crates/cli/tests/fixtures/storage-spa.html: 模拟真实 SPA 启动逻辑
- 2 个集成测试：render-script 执行 / render-file 不执行

---

## 下一步可选

**M14** 🟡: History API（pushState/replaceState）
- 浏览器历史栈

**M15** 🟡: WebSocket
- 连接管理 + 消息发送/接收

**M16** 🟡: 真实图像渲染（PNG/JPG 解码进 GUI）
- 现在 GUI 用占位符，CLI 用 image-ascii

**切 deno_core** 🟡: 解锁异步 JS + 真实 SPA bundle 渲染
- setTimeout / Promise / async-await 一等公民

---

## 项目总结

**核心成就**：
- L1 级自研架构（arena DOM + 手写布局 + 自研渲染）
- SPA 爬虫功能满足（M4 达成）+ 增强（M13 storage）
- GUI 窗口 + 滚动（M5 + M7.5）
- 真实字体（M7.4, DejaVuSans 757KB）
- CSS margin/padding + collapsing（M7.1）
- 完整 DOM API（M7.2）
- 表单交互（M8）
- 图片占位符（M9）
- 性能优化框架（M10）
- **PNG 截图（M12.1）** + **图像 ASCII（M12.3）**
- **localStorage / sessionStorage（M13）**

**Workspace**：241 tests, 0 clippy warnings ✅

**实际 HEAD**：0bc15a9

---

## 大纲维护说明

- 每次 commit 后更新此文档
- 同步实际 git 状态（HEAD、commits、tests）
- 记录 SPA 爬虫满足条件
