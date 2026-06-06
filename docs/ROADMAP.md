# Roadmap — 里程碑路线图

> 状态图例：⚪ 未开始 / 🟡 进行中 / ✅ 完成 / ⏸ 暂停（defer）
> 详细功能见 [FEATURES.md](./FEATURES.md)，目标/非目标见 [GOALS.md](./GOALS.md)。

---

## 总览

| 里程碑 | 状态 | 目标 | 测试数 |
|--------|------|------|--------|
| M0 | ✅ | 项目骨架 + CI | — |
| M1 | ✅ | 能看到 HTML（curl + parse + 打印 DOM） | — |
| M2 | ✅ | 文本流渲染（HN fixture 看出标题列表） | 111 |
| M3 | ✅ | JS 执行（动态创建元素进 DOM） | 140 |
| M4 | ✅ | **SPA 渲染（项目目标达成）** | 160 |
| M5 | ✅ | GUI 窗口（跨平台开窗浏览） | 179 |
| M6 | ✅ | 渲染质量修复 + 黄金对比 + 跨平台 CI | 211 |
| M7 | ✅ | 渲染质量 + 交互（margin/font/URL 栏/滚动） | 217 |
| M8 | ✅ | 表单交互（input/textarea/button/submit） | 217 |
| M9 | ✅ | 图片占位符渲染（[IMG: src]） | 217 |
| M10 | ✅ | 性能优化（LayoutCache + DirtyTracker） | 217 |
| M11 | ✅ | Bug 修复（vh/vw/rem/pt） | 217 |
| M12 | ✅ | 截图 + 图像 ASCII（--screenshot / image-ascii） | 225 |
| M13 | ✅ | Web Storage（localStorage/sessionStorage） | 241 |
| M14 | 🟡 | Navigation（history/location） | 260（M14.1-3 完成） |
| M15+ | ⚪ | 前瞻（见下） | — |

---

## 已完成里程碑详情

### M0-M6（核心管线）
- **M0** ✅ workspace 骨架 + CI
- **M1** ✅ HTTPS GET + arena DOM + html5ever + CLI + e2e
- **M2** ✅ 手写 CSS parser/selector/computed + 布局（block/inline/折行）+ ASCII 渲染器
- **M3** ✅ boa_engine JS 执行 + JS↔DOM 桥（`__setBody` 等）
- **M4** ✅ SPA 渲染（同步 fetch + 相对 URL resolve）— **项目目标达成**
- **M5** ✅ winit + softbuffer GUI + 5x7 bitmap font
- **M6** ✅ 长段落折行 / `<head>` 不渲染 / `<li>` 前缀 / 黄金对比 / 跨平台 CI

复盘见 `docs/postmortems/M1.md` ~ `M6.md`。

### M7（渲染质量 + 交互）✅
| Sub | 状态 | 内容 |
|-----|------|------|
| M7.1 | ✅ | CSS margin/padding（真实解析 + UA defaults + collapsing） |
| M7.2 | ✅ | 完整 DOM API（`__createEl`/`__appendChild`/`__qs`/...） |
| M7.4 | ✅ | 真实字体（fontdue + DejaVuSans 739KB + 中文支持） |
| M7.5 | ✅ | URL 栏 + 键盘输入 + 滚动（MouseWheel/PageUp/Down/Home/End） |
| M7.3 | 🟡 defer | 异步 JS（boa 0.20 `JsObject::call` 私有） |

### M8-M11（增强 + bug 修复）✅
- **M8** ✅ 表单交互（`__getValue`/`__setValue`/`__click`/`__submit`）
- **M9** ✅ 图片占位符渲染（`[IMG: src]`）
- **M10** ✅ 性能优化（LayoutCache `get_or_compute` + DirtyTracker）
- **M11** ✅ Bug 修复（vh/vw/vmin/vmax → Zero / rem → 16px / pt → 4/3 px）

### M12（截图 + 图像 ASCII）✅
- **M12.1** ✅ PNG screenshot（`--screenshot` flag，fontdue + png）
- **M12.3** ✅ image-ascii 子命令（image crate + 10 级灰阶 ramp）

### M13（Web Storage）✅
- **M13.1** ✅ browser-storage crate（`Rc<RefCell<HashMap>>`，6 API）
- **M13.2** ✅ `__storage*` bridges + `install_storage`
- **M13.3** ✅ localStorage/sessionStorage JS 对象 shim（Web 标准 API）
- **M13.4** ✅ e2e fixture（storage-spa.html）

---

## M14（进行中 🟡）— Navigation

| Sub | 状态 | 内容 |
|-----|------|------|
| M14.1 | ✅ | browser-navigation crate（HistoryStack + Location 解析） |
| M14.2 | ✅ | `__history*` / `__location*` bridges + `install_navigation` |
| M14.3 | ✅ | history/location JS 对象 shim + 接入 run_scripts |
| M14.4 | ⚪ | e2e fixture（SPA 路由 + 百度 `location.replace` 跟随） |
| M14.5 | ⚪ | PROGRESS.md + memory 同步 |

---

## 前瞻（M15+，待排序）

> 按 **爬虫价值**（见 GOALS.md 决策原则）排序，非按号。

| 候选 | 价值 | 工作量 | 备注 |
|------|------|--------|------|
| **切 deno_core** | ⭐⭐⭐⭐⭐ | 大 | 一举解锁 setTimeout/Promise/async-await/真实 SPA bundle |
| **Cookie jar** | ⭐⭐⭐⭐ | 中 | 跨请求保持会话，解决百度等登录态反爬 |
| **XMLHttpRequest** | ⭐⭐⭐ | 中 | 老 SPA 依赖，fetch 的补充 |
| **真实图像渲染进 GUI** | ⭐⭐ | 中 | PNG/JPG 解码显示（非占位符） |
| **WebSocket** | ⭐⭐ | 大 | 实时 SPA（聊天/推送） |
| **networkidle 算法** | ⭐⭐⭐ | 中 | 自动判断 SPA 何时渲染完，爬虫体验提升 |
| **资源拦截器** | ⭐⭐⭐ | 小 | block image/font/media，省 60% 内存 |

**决策原则**：见 [GOALS.md](./GOALS.md) §决策原则。每次只推进一个模块，
"先实现后完善"（MVP → 测试 → e2e → 文档）。
