# Vendored: svgine-core / svgine-simd

本目录（`crates/svgine-core`、`crates/svgine-simd`）是兄弟仓
`../svgine` 的 **vendored 快照**，不是本仓原创代码。

## 为什么 vendor

ADR-0019 阶段二让页面内 `<svg>` 交 svgine 引擎真渲染，`svgine-host`
依赖 `svgine-core`。最初用仓库外 path 依赖（`../../../svgine/...`），
但 svgine 仓**没有远端**，GitHub CI / Release 的 checkout 里不存在该
路径，workspace manifest 加载直接失败（2026-09-14 起 CI/Release 双红，
M97.25 修复）。svgine-core + svgine-simd 快照入仓后，构建零外部依赖、
可复现，svgine 独立仓保持自治不受影响。

## 快照溯源

| 项 | 值 |
|----|-----|
| 上游仓库 | `../svgine`（本地仓，无远端） |
| 上游 commit | `4cf3f30bfb78ae8446a3d95f1359216116ec5194` |
| 快照日期 | 2026-10-10 |
| 覆盖范围 | `crates/svgine-core`、`crates/svgine-simd` 全目录（含 tests/testdata/examples），**两类例外见下** |
| Manifest 改动 | 仅 `Cargo.toml`：`workspace = true` 继承字段按 svgine 根仓同值内联（本仓 workspace 无 `[workspace.package]` / `[workspace.lints]` 对应表）；lints 内联保持 svgine 语义（core: unsafe_code=forbid + clippy unwrap_used=deny；simd: unsafe_code=deny） |
| 不同步项 1 | `Cargo.toml`（保留本仓内联化适配，rsync 排除） |
| 不同步项 2 | `tests/*.rs` 中引用 `../../`（svgine 仓根语料 tests/oracle、tests/corpus）的契约测试——本仓布局下路径失效（M97.25 实测 panic），同步脚本自动剔除；语料验收由上游仓自治 |

## 如何同步上游更新

```bash
tools/vendor_svgine.sh          # 默认取 ../svgine
tools/vendor_svgine.sh /path/to/svgine   # 布局不同时显式指定
```

脚本行为：rsync 全目录（`--delete`，**排除 Cargo.toml**，保留本仓
内联化适配）→ 刷新两个 VENDOR.md 的溯源表。同步后必须过三门禁
（fmt / clippy -D warnings / test）再 commit。
