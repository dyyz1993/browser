# Roadmap

> 状态图例：⚪ 未开始 / 🟡 进行中 / ✅ 完成 / ⏸ 暂停

## 总览

| 里程碑 | 状态 | 目标 | 周期 |
|--------|------|------|------|
| M0 | ✅ | 项目骨架 + CI | 1~2 天 |
| M1 | ✅ | 能看到 HTML（curl + parse + 打印 DOM） | 2 周 |
| M2 | ⚪ | 文本流渲染（Hacker News 能看出标题列表） | 3 周 |
| M3 | ⚪ | JS 执行（动态创建的元素能进 DOM） | 4 周 |
| M4 | ⚪ | SPA 渲染（React/Vue 应用可爬） | 5 周 |
| M5 | ⚪ | GUI 窗口（跨平台开窗浏览） | 持续 |

---

## M0 — 项目初始化 ✅

**完成日期：** 2026-06-06
**单次 commit：** `chore: initial workspace skeleton with 9 crates`

**产出：**
- Cargo workspace（9 个 crate：net / dom / html-parser / css-engine / layout / render / js-runtime / page / cli）
- ROADMAP.md / ARCHITECTURE.md / README.md
- docs/decisions/ + docs/postmortems/ 模板
- .github/workflows/ci.yml（三平台 matrix）
- 每个 crate 含占位 `ping` test

**验收命令：**
```bash
cargo build --workspace && cargo fmt --check --all && cargo clippy --workspace -- -D warnings && cargo test --workspace
```

---

## M1 — 能看到 HTML ✅

**完成日期：** 2026-06-06

| Step | 状态 | Commit | 验收命令 |
|------|------|--------|---------|
| M1.1 | ✅ | `feat(net): add HTTPS client with rustls` | `cargo test -p browser-net` |
| M1.2 | ✅ | `feat(dom): arena-backed DOM tree (ADR-0001)` | `cargo test -p browser-dom` |
| M1.3 | ✅ | `feat(html-parser): wrap html5ever with 5 edge-case fixtures` | `cargo test -p browser-html-parser` |
| M1.4 | ✅ | `feat(dom): add tree pretty-printer` | `cargo test -p browser-dom -- print` |
| M1.5 | ✅ | `feat(cli): add get and parse subcommands` | `cargo run -p browser-cli -- parse <fixture>` |
| M1.6 | ✅ | `test(cli): add e2e tests for get/parse subcommands` | `cargo test -p browser-cli` |
| M1.7 | ✅ | `docs: M1 complete, postmortem added` | 三平台 CI 全绿 |

**最终验收：**
```
cargo test --workspace     → 48 passed, 0 failed
cargo clippy -- -D warnings → 0 warnings
cargo run -p browser-cli -- get https://example.com → DOM 树输出
```

复盘见 `docs/postmortems/M1.md`。

---

## M2 — 文本流渲染（待细化）

## M3 — JS 执行（待 M2 后细化）

## M4 — SPA 渲染（待 M3 后细化）

## M5 — GUI 窗口（持续）
