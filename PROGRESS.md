# 自研浏览器项目 — 进度日志（活跃）

> 本文档记录每次重要变更，每 commit 后更新。
> **目标/非目标/验收** 见 [docs/GOALS.md](./docs/GOALS.md)（单一事实来源）。
> **能力清单** 见 [docs/FEATURES.md](./docs/FEATURES.md)。
> **里程碑** 见 [docs/ROADMAP.md](./docs/ROADMAP.md)。

---

## 当前状态快照

| 指标 | 值 |
|------|-----|
| HEAD | `3afbcbb`（M14.3 history/location JS shim） |
| 总 commits | 97 |
| 测试 | 260 passed, 0 clippy warnings |
| Crates | 12 |
| CLI 子命令 | 8 |
| 核心目标 G1（SPA 爬虫）| ✅ 达成（M4） |
| 截图 G2 | ✅ 达成（M12.1） |
| 跨平台 G3 | ✅ 达成 |

---

## 最近变更（倒序）

### 2026-06-06 — 文档体系建立（wiki 重构）🟡 进行中
**变更**：建立结构化 wiki 文档体系，解决多文档冗余 + 过时问题。
- ⭐ `docs/GOALS.md`：NORTH STAR（目标/非目标/验收，单一事实来源）
- `docs/FEATURES.md`：能力清单（CLI/CSS/JS 桥/Web API 矩阵）
- `docs/ARCHITECTURE.md`：更新到 M14（含 storage/navigation）
- `docs/CONVENTIONS.md`：工程规范（提交/自研边界/依赖白名单）
- `docs/DIRECTORY.md`：目录结构 + crate 职责
- `docs/TESTING.md`：三级测试分层 + 验收命令
- `docs/ROADMAP.md`：M0-M14 + 前瞻
- `README.md`：重写（快速开始 + SPA 爬虫示例 + 文档导航）
- 删除根目录 `ROADMAP.md` / `ARCHITECTURE.md`（移入 docs/）
- `docs/PLAN.md`：加 superseded 标注（历史归档）

### M14 — Navigation（history / location）🟡
- M14.1 ✅ browser-navigation crate（HistoryStack + Location 解析，11 tests）
- M14.2 ✅ `__history*` / `__location*` bridges + install_navigation
- M14.3 ✅ history/location JS 对象 shim + 接入 run_scripts（8 tests）
- M14.4 ⚪ e2e fixture（SPA 路由 + 百度 location.replace 跟随）
- M14.5 ⚪ PROGRESS/memory 同步

### M13 — Web Storage（localStorage / sessionStorage）✅
- M13.1 ✅ browser-storage crate（`Rc<RefCell<HashMap>>`，6 API，8 tests）
- M13.2 ✅ `__storage*` bridges + install_storage
- M13.3 ✅ localStorage/sessionStorage JS 对象 shim（6 tests）
- M14.4 ✅ e2e fixture storage-spa.html（2 tests）

### M12 — 截图 + 图像 ASCII ✅
- M12.1 ✅ PNG screenshot（`--screenshot` flag，fontdue + png）
- M12.3 ✅ image-ascii 子命令（image crate + 10 级灰阶 ramp）

### M11 — Bug 修复 ✅
- vh/vw/vmin/vmax → Zero / rem → 16px / pt → 4/3 px

### M10 — 性能优化 ✅
- LayoutCache（get_or_compute）+ DirtyTracker（mark/mark_subtree/is_dirty/clear）

### M9 — 图片占位符 ✅
- `[IMG: src]` 占位符注入（construct.rs build_box）

### M8 — 表单交互 ✅
- `__getValue` / `__setValue` / `__click` / `__submit` 橋 + e2e

### M7 — 渲染质量 + 交互 ✅
- M7.1 CSS margin/padding（真实解析 + UA defaults + collapsing）
- M7.2 完整 DOM API（`__createEl`/`__appendChild`/`__qs`/...）
- M7.4 真实字体（fontdue + DejaVuSans 739KB + 中文）
- M7.5 URL 栏 + 键盘输入 + 滚动
- M7.3 异步 JS 🟡 defer（boa 0.20 JsObject::call 私有）

### M0-M6 — 核心管线 ✅
- M0 骨架 + CI / M1 HTTPS+DOM / M2 CSS+布局+ASCII 渲染
- M3 JS 执行（boa）/ M4 SPA 渲染（**项目目标达成**）
- M5 GUI 窗口 / M6 渲染质量修复 + 跨平台 CI

---

## 前瞻（按爬虫价值排序，见 GOALS.md 决策原则）

1. **切 deno_core** ⭐⭐⭐⭐⭐ — 解锁 setTimeout/Promise/async-await
2. **Cookie jar** ⭐⭐⭐⭐ — 跨请求会话，解决登录态反爬
3. **XMLHttpRequest** ⭐⭐⭐ — 老 SPA 依赖
4. **真实图像渲染进 GUI** ⭐⭐
5. **WebSocket** ⭐⭐
6. **networkidle 算法** ⭐⭐⭐
7. **资源拦截器** ⭐⭐⭐（省 60% 内存）

---

## 文档维护规则

- **每 commit 后**：更新本文件"最近变更"
- **目标变更**：改 docs/GOALS.md + 写 ADR
- **新增功能**：改 docs/FEATURES.md
- **架构变化**：改 docs/ARCHITECTURE.md
- **里程碑完成**：改 docs/ROADMAP.md + 写 docs/postmortems/M<n>.md
- **冲突优先级**：GOALS > FEATURES > ARCHITECTURE > 其他
