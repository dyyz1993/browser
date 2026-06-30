# M71 二进制体积分析与减重计划

> 日期：2026-06-30
> 触发：用户问"打包出来多大体积"，引发一次系统性体检。
> 本文是**分析报告 + 可执行减重清单**，不是已落地的功能。
> 优先级：**P2（锦上添花）**——17MB 当前可用，不阻塞任何核心能力。

---

## 一、当前体积（实测，HEAD = M70.18）

| 构建模式 | 体积 | 备注 |
|---------|------|------|
| Debug | 59 MB | 未优化 + 调试符号 |
| **Release（已 strip）** | **17 MB** | ✅ 当前产物 |
| Release gzip 压缩后 | **7.7 MB** | 实际传输/分发大小 |

**构建配置**（`Cargo.toml [profile.release]`，已属较优）：

```toml
opt-level = 3       # 全速优化
lto = "thin"        # 链上链接时优化（平衡体积和编译时间）
codegen-units = 1   # 单代码生成单元（最大化优化机会）
strip = true        # 自动剥离符号表
```

**结论：17MB 对一个 Rust + 双 JS 引擎的浏览器来说是健康水平。** 对标：
- Chrome.app ~600MB+ / Chromium headless ~150MB
- `curl` ~300KB（纯 SSR）
- 纯 SSR 爬虫 ~5MB（无 JS 引擎）

---

## 二、体积构成（实测数据）

### 2.1 依赖占比（编译后 rlib 大小）

| 依赖 | rlib 大小 | 性质 | 可选？ |
|------|----------|------|--------|
| **boa_engine**（全套 boa_*） | **49 MB**（最大单依赖，编译期） | JS 引擎 #2 | ❌ **强制依赖** |
| **render crate**（含 image/png/fontdue） | 11 MB | 截图+渲染 | 部分（image feature） |
| **rquickjs-core** | 4.3 MB | JS 引擎 #1（默认） | ✅ optional+default |
| **js_runtime crate** | 5.3 MB | 5400 行 JS shim + bridge | 核心 |
| **gui crate**（winit+softbuffer） | 1.1 MB | GUI 窗口 | ❌ **强制依赖** |
| cdp / ws / html_parser / net 等 | 各 0.3~0.8 MB | 核心 | 核心 |

### 2.2 binary 内符号证据（`strings` 命中数）

```
boa             335 命中  ← 双引擎里 boa 实打实链接进 binary
tokio            70 命中
hyper            37 命中
png              31 命中
winit            22 命中  ← GUI 确实编进 release
reqwest          14 命中
quickjs/rquickjs 10/34 命中
softbuffer        5 命中
fontdue/html5ever 各 3/10 命中
```

### 2.3 关键架构事实（来自 Cargo.toml）

```toml
# crates/js-runtime/Cargo.toml
boa_engine = { git = "...", branch = "main" }          # ← 非 optional！
rquickjs    = { ..., optional = true }                  # ← optional

# crates/cli/Cargo.toml
[features]
default = ["quickjs"]                                   # ← QuickJS 默认引擎
# 没有 no-boa / gui-optional feature
```

**核心矛盾**：
- M66 起 **QuickJS 是默认引擎**，CLI/CDP 全部走 QuickJS。
- 但 **boa 仍是强制依赖**——即使用户从不传 `--js-engine boa`，boa 也被编进 binary。
- `boa_engine`（git main, 1.0.0-dev）是单一最大依赖，编译期吃 ~49MB rlib。

---

## 三、减重清单（按 ROI 排序）

> 原则：不破坏现有测试，不改默认行为。所有改动走 feature flag，用户按需编译。

### 🥇 Tier 1：让 boa 成为可选依赖（预估 -3~5MB）

**改动**：
- `crates/js-runtime/Cargo.toml`：把 `boa_engine` 改成 `optional = true`，新增 `boa` feature。
- `crates/cli/Cargo.toml`：`default = ["quickjs"]`（已是），保留 `--features boa` 编译 boa 后端。
- `crates/js-runtime/src/engine_boa.rs`：用 `#[cfg(feature = "boa")]` 门控。
- `crates/js-runtime/src/engine.rs`：`EngineKind::parse_str("boa")` 在未启用 feature 时返回错误。
- runtime 调用点全部 `#[cfg(feature = "boa")]` 包裹。

**收益**：纯爬虫用户（不传 `--js-engine boa`）**省掉 boa 全套**。
**代价**：想用 boa 调试需 `--features boa` 编译。
**风险**：中——需全量 `#[cfg]` 包裹，CI 要加双 feature matrix。

**验收**：
```bash
cargo build --release -p browser-cli                       # 无 boa，预期 ~12-14MB
cargo build --release -p browser-cli --features boa         # 有 boa，预期 ~17MB
./target/release/browser fetch https://nuxt.com/ --format text   # QuickJS 路径不变
cargo test --workspace                                        # 全绿
```

### 🥈 Tier 2：让 GUI 成为可选依赖（预估 -1~1.5MB）

**现状**：`Cmd::Open` 引用 `browser_gui::WindowConfig`，所以 gui crate 强制编译。
**改动**：
- `crates/cli/Cargo.toml`：`browser-gui` 改 optional，新增 `gui` feature。
- `Cmd::Open`：`#[cfg(feature = "gui")]` 门控，未启用时打印"需 `--features gui` 重编译"。
- CI 默认不带 gui feature（headless 爬虫用不到）。

**收益**：headless 爬虫场景（用户实际主力用法）省掉 winit+softbuffer。
**代价**：`browser open` 需 `--features gui` 才可用。
**风险**：低——改动面小。

**验收**：
```bash
cargo build --release -p browser-cli                    # 无 gui
./target/release/browser open https://example.com/      # 应报错提示需重编译
cargo build --release -p browser-cli --features gui     # 有 gui
./target/release/browser open --check https://example.com/   # 可用
```

### 🥉 Tier 3：开关式微优化（预估 -0.5~2MB，边际递减）

| 优化 | 收益 | 代价 |
|------|------|------|
| `lto = "fat"`（替 thin） | -1~2MB | 编译时间 +50~100% |
| `opt-level = "z"`（替 3） | -2~4MB | 性能降 20-30%（**不推荐**，违背"快运行"原则 G3） |
| image crate 只留 png（砍 jpeg） | -~0.5MB | 失去 jpeg 截图 |
| UPX 压缩 | 降到 ~5MB | 启动解压损耗（**不推荐**，反低内存卖点） |

**结论**：Tier 3 单独看不值得做，只在 Tier 1/2 已做且仍想压时考虑。

---

## 四、建议的执行路径

```
现状（17MB，全功能）
    │
    ├─[可选] Tier 1: boa → feature flag
    │        产出两个构建档位：
    │        · slim（默认）: ~12-14MB，仅 QuickJS
    │        · full（--features boa）: ~17MB，双引擎
    │
    └─[可选] Tier 2: gui → feature flag
             进一步给 slim 档位：
             · slim-headless: ~11-13MB，无 GUI（爬虫主力）
             · slim-gui: slim + GUI
```

**不立即执行的项**（记录在案，避免重复评估）：
- ❌ 砍掉 JS 引擎 → 违背 G1（SPA 爬虫存在理由）
- ❌ UPX 压缩 → 违背 G3（低内存、快运行）
- ❌ `opt-level = "z"` → 违背 G3
- ❌ 追求 1:1 渲染相关的大体积依赖 → 违背"爬虫够用"原则

---

## 五、行动项

- [ ] **决策点**：是否接受"boa 成为 optional feature"？若接受，开 Tier 1 task。
- [ ] **决策点**：是否接受"gui 成为 optional feature"？若接受，开 Tier 2 task。
- [ ] 若两者都不做：本 issue 关闭，17MB 定型（完全合理，非问题）。

---

## 六、附：本次分析用的命令（可复现）

```bash
# 1. binary 体积
ls -lh target/release/browser
gzip -c target/release/browser | wc -c   # gzip 后

# 2. 各 crate 编译大小
ls -lh target/release/*.rlib | sort -k5 -rh

# 3. 依赖 footprint
du -sh target/release/deps/*.rlib | sort -rh | head
ls -lh target/release/deps/ | grep -iE 'boa|quickjs|winit|reqwest|tokio'

# 4. binary 内符号证据（验证依赖真的链接进来）
for kw in boa quickjs winit softbuffer reqwest tokio; do
  printf "%-12s %s\n" "$kw" "$(strings target/release/browser | grep -ci "$kw")"
done

# 5. feature 配置
grep -n 'optional\|default\|features' crates/js-runtime/Cargo.toml crates/cli/Cargo.toml
```
