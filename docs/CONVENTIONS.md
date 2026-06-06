# CONVENTIONS — 工程规范

> 本文档规定代码风格、提交规范、自研边界、依赖白名单。所有 PR/commit 必须遵守。
> 冲突时以 [GOALS.md](./GOALS.md) 为准。

---

## 1. 提交规范（Conventional Commits）

**格式：** `<type>(<scope>): <subject>`

| type | 用途 |
|------|------|
| `feat` | 新功能 |
| `fix` | bug 修复 |
| `test` | 新增/修改测试 |
| `docs` | 文档变更 |
| `refactor` | 重构（无功能变化） |
| `chore` | 杂项（依赖、CI、配置） |
| `perf` | 性能优化 |

**scope**：crate 名（`net`/`dom`/`cli`/...）或 `progress`/`memory`/`ci`。

**subject**：祈使句、小写、≤72 字符、含里程碑号。

**示例：**
```
feat(storage): M13.1 browser-storage crate（localStorage / sessionStorage 后端）
fix(net): use realistic Chrome User-Agent（修复真实站点被反爬）
test(cli): M13.4 e2e fixture for storage-spa.html
docs(progress): M13 全部完成（4 commits，241 tests）
```

**body**（可选）：说明 **What/Why**，不啰嗦 How（代码自己说）。

---

## 2. 自研边界（G4 原则）

**自研（手写）：** 浏览器架构、DOM、布局引擎、JS 桥、事件系统、Page 生命周期、
渲染管线、navigation/storage 后端。

**复用（白名单 crate）：** 只允许"底层解析/IO 库过于复杂（约 ≥10 万行）"时引入。

### 当前依赖白名单

| crate | 用途 | 行数级别 | 引入里程碑 |
|-------|------|---------|-----------|
| `html5ever` | HTML5 解析 | ~15 万 | M1 |
| `hyper` + `hyper-rustls` | HTTP/HTTPS | ~5 万 | M1 |
| `tokio` | 异步运行时 | ~5 万 | M1 |
| `clap` | CLI | ~2 万 | M1 |
| `boa_engine` | JS 引擎 | ~10 万 | M3 |
| `winit` + `softbuffer` | 窗口/缓冲 | ~5 万 | M5 |
| `url` | URL 解析 | ~1 万 | M4 |
| `fontdue` | 字体光栅化 | ~1 万 | M7.4 |
| `png` | PNG 编码 | ~5 千 | M12.1 |
| `image` | 图像解码 | ~3 万 | M12.3 |

### 引入新依赖的流程
1. 确认无法用 < 1000 行手写实现
2. 评估行数（`cargo blox` 或人工估算）
3. 在 [GOALS.md](./GOALS.md) G4 白名单 + 本表登记
4. 写 ADR 记录决策（`docs/decisions/`）
5. commit 引入

**禁止**：偷偷加依赖不登记。

---

## 3. 代码风格

### Rust 通用
- `#![forbid(unsafe_code)]` 全 workspace 强制（无 unsafe）
- 公共函数必须有显式返回类型（`fn foo() -> Result<Bar>` 而非 `-> _`）
- 错误必须处理：禁止 `.unwrap()` / `.expect()` 在非启动代码（main 除外）
- 文件超 100 行考虑拆分（layout/construct.rs 等大文件例外，按职责拆）

### 错误处理
```rust
// ✅ 好：用 ? 传播，main 统一打印
fn parse(html: &str) -> Result<Tree> { ... }

// ❌ 坏：吞错误
fn parse(html: &str) -> Tree {
    do_something().unwrap_or_default()  // 错误被吞
}

// ✅ 启动期可 expect（fontdue FontdingError 非 std::error::Error）
let font = Font::from_bytes(...).expect("font embedded");
```

### 测试文件位置（三层）
```
crates/foo/
├── src/
│   ├── lib.rs
│   └── bar.rs           # #[cfg(test)] mod tests（单元测试）
└── tests/
    ├── integration_foo.rs   # 集成测试（assert_cmd / wiremock）
    └── fixtures/
        └── foo.html         # fixture 纳入 git
```

> **注意**：本项目用 Rust，不是 TypeScript。规则中的 `xxx.test.ts` 命名
> 对应 Rust 的 `tests/integration_xxx.rs` + `src/*.rs` 内 `mod tests`。

---

## 4. 依赖管理

- **workspace 统一**：公共依赖（tokio/anyhow）在根 `Cargo.toml [workspace.dependencies]`
- **版本锁定**：`rust-toolchain.toml` 锁定 stable channel
- **最小 feature**：`default-features = false` + 按需开 feature（如 `image` 只开 png/jpeg）

---

## 5. 工程纪律（强制）

每个 commit 前必须通过：
```bash
cargo fmt --all -- --check      # 格式
cargo clippy --workspace --all-targets -- -D warnings   # 0 warning
cargo test --workspace          # 全绿
```

**禁止行为：**
- 测试失败时 commit
- `git commit --amend`（失败用 `git revert`）
- 提交密钥/token/.env
- 用 `any` 类型（Rust 对应：禁止 `Box<dyn Any>` 滥用，用具体 enum）

---

## 6. 文档维护

| 文档 | 更新时机 |
|------|---------|
| [GOALS.md](./GOALS.md) | 目标/非目标/验收变更时 |
| [FEATURES.md](./FEATURES.md) | 新增功能/API 时 |
| [ARCHITECTURE.md](./ARCHITECTURE.md) | crate 结构/数据流变化时 |
| [PROGRESS.md](../PROGRESS.md) | 每个 commit 后（活跃日志） |
| [ROADMAP.md](./ROADMAP.md) | 里程碑完成时 |
| `docs/decisions/` | 每个"为什么这么选"的决策 |
| `docs/postmortems/` | 每个里程碑完成时 |

**冲突优先级：** GOALS.md > FEATURES.md > ARCHITECTURE.md > 其他。

---

## 7. 命名约定

- **crate 名**：`browser-<name>`（lib name `browser_<name>`）
- **JS 桥**：`__camelCase`（双下划线前缀，避免和 Web API 冲突）
- **fixture**：`<场景>.html`（kebab-case，如 `storage-spa.html`）
- **集成测试**：`integration_<被测能力>.rs`
- **milestone commit**：含 `M<号>`（如 `M13.1`）

---

## 8. 跨平台

- 支持 macOS / Windows / Linux
- CI 三平台矩阵（`.github/workflows/ci.yml`）
- 禁用平台特定 API（用 winit 等跨平台抽象）
- GUI 测试用 `--check` flag 支持 headless
