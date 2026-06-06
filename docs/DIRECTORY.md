# DIRECTORY — 目录结构

> 本文档定义仓库目录约定。冲突时以 [GOALS.md](./GOALS.md) 为准。

---

## 仓库根

```
browser/
├── Cargo.toml                  # workspace 根（[workspace.dependencies] 统一版本）
├── Cargo.lock                  # 锁定
├── rust-toolchain.toml         # 锁定 stable toolchain
├── README.md                   # 入口：快速开始 + 文档导航
├── PROGRESS.md                 # 活跃日志（每 commit 后更新）
├── .github/workflows/ci.yml    # 三平台 CI 矩阵
├── .gitignore
│
├── crates/                     # 12 个 crate（见下表）
│   ├── net/
│   ├── dom/
│   ├── html-parser/
│   ├── css-engine/
│   ├── layout/
│   ├── render/
│   ├── js-runtime/
│   ├── page/                   # stub
│   ├── gui/
│   ├── storage/
│   ├── navigation/
│   └── cli/                    # 唯一 bin
│
└── docs/                       # 文档体系
    ├── GOALS.md                # ⭐ NORTH STAR（目标/非目标/验收）
    ├── FEATURES.md             # 能力清单
    ├── ARCHITECTURE.md         # 架构与数据流
    ├── CONVENTIONS.md          # 工程规范
    ├── DIRECTORY.md            # 本文档
    ├── TESTING.md              # 测试策略
    ├── ROADMAP.md              # 里程碑路线图
    ├── PLAN.md                 # 历史计划（superseded，保留）
    ├── decisions/              # ADR（架构决策记录）
    │   ├── README.md
    │   └── 0001-arena-vs-refcell.md
    └── postmortems/            # 里程碑复盘
        ├── README.md
        └── M1.md ~ M6.md
```

---

## 单个 crate 标准结构

```
crates/<name>/
├── Cargo.toml                  # [package] + [dependencies] + [dev-dependencies]
├── assets/                     # （可选）嵌入资源（如 cli/assets/font.ttf）
└── src/
    ├── lib.rs                  # crate 入口（pub mod + pub use）
    ├── <module>.rs             # 按职责拆分（>100 行考虑拆）
    └── tests/                  # ❌ 不放这（见下）
        └── integration_<x>.rs  # 集成测试（tests/ 在 crate 根，不在 src/）
    └── tests/
        ├── integration_<x>.rs  # 集成测试
        └── fixtures/           # 测试用 HTML（纳入 git）
            └── <scene>.html
```

### 文件命名
- **源文件**：`<职责>.rs`（snake_case，如 `construct.rs` / `storage_shim.rs`）
- **集成测试**：`integration_<被测能力>.rs`（如 `integration_storage.rs`）
- **fixture**：`<场景>.html`（kebab-case，如 `storage-spa.html`）
- **ADR**：`<编号>-<决策>.md`（如 `0001-arena-vs-refcell.md`）
- **复盘**：`M<号>.md`（如 `M6.md`）

---

## 12 Crate 职责速查

| Crate | LOC | 状态 | 职责 |
|-------|-----|------|------|
| `net` | 262 | ✅ 稳定 | HTTP/HTTPS GET + User-Agent |
| `dom` | 747 | ✅ 稳定 | arena DOM（NodeId/Tree/Node/Document/print） |
| `html-parser` | 429 | ✅ 稳定 | html5ever → dom::Tree |
| `css-engine` | 1418 | ✅ 稳定 | parser/selector/computed/properties |
| `layout` | 1415 | ✅ 稳定 | construct/block/inline/box/boxes/cache/dirty |
| `render` | 246 | ✅ 稳定 | ASCII 渲染器 |
| `js-runtime` | 2295 | ✅ 稳定 | runtime/bridge/scripts/storage_shim/navigation_shim |
| `page` | 17 | 🟡 stub | Page/Frame 编排（待完善） |
| `gui` | 528 | ✅ 可用 | window/font/bitmap_font（URL 栏 + 滚动 + 中文字体） |
| `storage` | 138 | ✅ 稳定 | localStorage/sessionStorage 后端 |
| `navigation` | 297 | ✅ 稳定 | history/location 后端 |
| `cli` | 1686 | ✅ 稳定 | 8 子命令 + screenshot + img_ascii |

---

## crate 依赖方向（强约束）

**依赖只能"向下"指向更基础的 crate，禁止循环：**

```
cli  →  page, render, gui, js-runtime
gui  →  render, js-runtime
render →  layout
layout  →  css-engine
css-engine →  dom, html-parser
js-runtime →  dom, net, storage, navigation
storage / navigation  →  （仅 url / 无）
net  →  （仅 hyper/rustls）
dom  →  （无）
```

**禁止**：dom 依赖任何上层 crate；下层依赖上层（如 net 依赖 cli）。

---

## 输出产物

```
target/
├── debug/
│   └── browser                 # 开发用 binary
├── release/
│   └── browser                 # 发布用 binary（爬虫部署用这个）
└── ...
```

**测试输出**（纳入 git，供对比）：
```
tests/output/
├── ALL.txt                     # 所有 fixture 真实渲染输出
└── screenshots/                # --screenshot 生成的 PNG
    ├── 01-example.com.png
    ├── 02-hn.png
    └── ...
```
