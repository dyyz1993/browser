# Roadmap

> 状态图例：⚪ 未开始 / 🟡 进行中 / ✅ 完成 / ⏸ 暂停

## 总览

| 里程碑 | 状态 | 目标 | 周期 |
|--------|------|------|------|
| M0 | ✅ | 项目骨架 + CI | 1~2 天 |
| M1 | ✅ | 能看到 HTML（curl + parse + 打印 DOM） | 2 周 |
| M2 | ✅ | 文本流渲染（HN fixture 能看出标题列表） | 3 周 |
| M3 | ✅ | JS 执行（动态创建的元素能进 DOM） | 4 周 |
| M4 | ✅ | SPA 渲染（React/Vue 应用可爬） | 5 周 |
| M5 | ✅ | GUI 窗口（跨平台开窗浏览） | 3 天 |
| M6 | ✅ | 渲染质量修复 + 黄金对比 + 跨平台 CI | 1 天 |

---

## M0 — 项目初始化 ✅
完成日期：2026-06-06
Commit：`chore: initial workspace skeleton with 9 crates`

## M1 — 能看到 HTML ✅
完成日期：2026-06-06
复盘见 `docs/postmortems/M1.md`

## M2 — 文本流渲染 ✅

**完成日期：** 2026-06-06

| Step | 状态 | Commit |
|------|------|--------|
| M2.1 | ✅ | `feat(css-engine): wrap cssparser for stylesheets and declarations` |
| M2.2 | ✅ | `feat(css-engine): tag/class/id selector matching` |
| M2.3 | ✅ | `feat(css-engine): compute_styles traverses DOM applying matching rules` |
| M2.4 | ✅ | `feat(layout): layout tree construction (block/inline/anonymous)` |
| M2.5 | ✅ | `feat(layout): block-level layout algorithm` |
| M2.6 | ✅ | `feat(layout): inline layout + char-level text wrapping` |
| M2.7 | ✅ | `feat(render): terminal ASCII renderer` |
| M2.8 | ✅ | `feat(cli): add render-file subcommand` |
| M2.9 | ✅ | `test(cli): e2e render-file against HN snapshot` |
| M2.10 | ✅ | `docs: M2 complete, postmortem added` |

**最终验收：**
```
cargo test --workspace     → 111 passed, 0 failed
cargo clippy -- -D warnings → 0 warnings
cargo run -p browser-cli -- render-file tests/fixtures/news.ycombinator.com.html --width 400
  → 输出包含全部 4 条 HN 标题 / 点数 / 用户名 / 时间
```

复盘见 `docs/postmortems/M2.md`。

---

## M3 — JS 执行 ✅

**完成日期：** 2026-06-06

| Step | 状态 | Commit |
|------|------|--------|
| M3.1 | ✅ | `feat(js-runtime): embed boa_engine for JS execution` |
| M3.2 | ✅ | `feat(js-runtime): write-only JS-to-DOM bridge` |
| M3.3 | ✅ | `feat(js-runtime): extract and execute <script> tags in document order` |
| M3.4 | ✅ | `feat(cli): render-script subcommand executes <script> tags before layout` |
| M3.5 | ✅ | `docs: M3 complete, postmortem added` |

**最终验收：**
```
cargo test --workspace     → 140 passed, 0 failed
cargo clippy -- -D warnings → 0 warnings
cargo run -p browser-cli -- render-script tests/fixtures/spa-blog.html --width 120
  → 输出 "Welcome to my blog / Recent posts / Author: Jane Doe"
  → stderr: "3 script(s) executed"
```

复盘见 `docs/postmortems/M3.md`。

---

## M4 — SPA 渲染 ✅

**完成日期：** 2026-06-06

| Step | 状态 | Commit |
|------|------|--------|
| M4.1 | ✅ | `feat(cli): render-url subcommand + suppress script/style/noscript/template` |
| M4.2 | ✅ | `feat(js-runtime): sync __fetchSetBody / __fetchAppendBody bridges` |
| M4.3 | ✅ | `test(cli): SPA-style page e2e with partial-failure tolerance` |
| M4.4 | ✅ | `feat(js-runtime): RFC 3986 relative URL resolution` |
| M4.5 | ✅ | `docs: M4 complete (project goal met), postmortem added` |

**最终验收：** 160 passed, 0 failed。`render-url https://example.com/` 输出可读。
**项目目标（SPA 可爬）达成。** 复盘见 `docs/postmortems/M4.md`。

---

## M5 — GUI 窗口（基础完成）✅

**完成日期：** 2026-06-06

| Step | 状态 | Commit |
|------|------|--------|
| M5.1 | ✅ | `feat(gui): winit + softbuffer + hand-written 5x7 bitmap font` |
| M5.2 | ✅ | `feat(cli): browser open <url> subcommand wires SPA pipeline to GUI` |
| M5.3 | ✅ | `test(cli): open subcommand e2e + --check headless flag` |
| M5.4 | ✅ | `docs: M5 basic GUI complete, postmortem added` |

**最终验收：** 179 passed, 0 failed。`open <url> --check` 无显示器可跑；`open <url>` 弹 winit 窗口。
复盘见 `docs/postmortems/M5.md`。

---

## M6 — 渲染质量修复 + 黄金对比 + 跨平台 CI ✅

**完成日期：** 2026-06-06

| Step | 状态 | Commit |
|------|------|--------|
| M6.0a | ✅ | `fix(layout): word-wrap actually wraps in renderer` (`9c81957`) |
| M6.0b | ✅ | `fix(layout): suppress <head> subtree` (`a43a1b0`) |
| M6.0c | ✅ | `feat(layout): paragraph spacing + <li> bullet prefix` (`c23e7e2`) |
| M6.0d | ✅ | `test(snapshots): Safari vs ours comparison script + artifacts` (`fe24c69`) |
| M6.1  | ✅ | `test(cli): snapshot tests pinning M6.0a/b/c render output` (`96af6d6`) |
| M6.2  | ✅ | `ci: install Linux GUI deps + drop unused tiny-skia dep` (`04edd00`) |
| M6.3  | ✅ | `docs: M6 layout fixes + snapshot net complete, postmortem added` |

**最终验收：**
```
cargo test --workspace        → 183 passed, 0 failed
cargo clippy -- -D warnings   → 0 warnings
python3 tests/snapshots/compare.py
  → 6 个对比素材，Preview 弹出
UPDATE_SNAPSHOTS=1 cargo test -p browser-cli --test integration_snapshot
  → 重生成黄金对比（M6.0a/b/c 修复自动 pin 住）
```

复盘见 `docs/postmortems/M6.md`。

---

## M7 — 渲染质量 + 交互 ✅

**完成日期：** 2026-06-06

| Sub | 状态 | Commit |
|-----|------|--------|
| M7.1 | ✅ | CSS margin/padding/UA defaults/collapsing/snapshots (8 commits) |
| M7.2 | ✅ | DOM API bridges + selector enhancement (3 commits) |
| M7.5 | ✅ | URL 栏 + 键盘输入 + MouseWheel/PageUp/PageDown/Home/End (6 commits) |
| M7.4 | ✅ | fontdue 集成 + DejaVuSans.ttf 757KB 嵌入 + 中文支持 (5 commits) |
| M7.3 | 🟡 | 异步 JS（MicrotaskQueue 写完但 boa 0.20 API 复杂，defer） |

**最终验收：**
```
cargo test --workspace     → 217 passed, 0 failed
cargo clippy -- -D warnings → 0 warnings
./target/debug/browser render-file tests/fixtures/chinese-font.html --width 80
  → 中文渲染成功（简体/繁体/日文/韩文/数字/符号）
```

**关键改进：**
- 真实 CSS margin/padding 支持（UA defaults + collapsing）
- 完整 DOM API 桥（__createEl/__appendChild/__qs/...）
- GUI URL 栏 + 键盘输入 + 滚动
- 真实字体（DejaVuSans 757KB，fontdue 抗锯齿，中文支持）

**下一步规划：**
- M8: 表单交互（input/textarea/button）
- M9: 图片渲染（png/jpg/webp）
- M10: 性能优化（layout 缓存 + 增量渲染）

复盘见 `docs/postmortems/M7.md`（待写）。
