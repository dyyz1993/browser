# Roadmap

> 状态图例：⚪ 未开始 / 🟡 进行中 / ✅ 完成 / ⏸ 暂停

## 总览

| 里程碑 | 状态 | 目标 | 周期 |
|--------|------|------|------|
| M0 | ✅ | 项目骨架 + CI | 1~2 天 |
| M1 | ✅ | 能看到 HTML（curl + parse + 打印 DOM） | 2 周 |
| M2 | ✅ | 文本流渲染（HN fixture 能看出标题列表） | 3 周 |
| M3 | ✅ | JS 执行（动态创建的元素能进 DOM） | 4 周 |
| M4 | ⚪ | SPA 渲染（React/Vue 应用可爬） | 5 周 |
| M5 | ⚪ | GUI 窗口（跨平台开窗浏览） | 持续 |

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

## M4 — SPA 渲染（待细化）

## M5 — GUI 窗口（持续）
