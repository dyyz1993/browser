# TESTING — 测试策略与验收命令

> 本文档定义测试分层、验收命令、fixture 管理。冲突时以 [GOALS.md](./GOALS.md) 为准。
> 配合 [CONVENTIONS.md](./CONVENTIONS.md) 的工程纪律使用。

---

## 三级测试分层

每个功能必须满足三级验收才算"完成"（见 [GOALS.md](./GOALS.md) 验收标准）。

### Level 1 — 单元测试（`#[test]` in src）

- **位置**：与源文件同 crate，`src/*.rs` 内 `#[cfg(test)] mod tests`
- **范围**：纯函数逻辑（解析、算法、数据结构）
- **不依赖**：外部网络、文件 IO、其他 crate
- **运行**：`cargo test -p <crate>` 或 `cargo test -p <crate> -- <module>`

**示例**（`crates/storage/src/lib.rs`）：
```rust
#[cfg(test)]
mod tests {
    #[test]
    fn set_and_get() {
        let s = new_storage();
        storage_set(&s, "foo", "bar");
        assert_eq!(storage_get(&s, "foo"), Some("bar".to_string()));
    }
}
```

### Level 2 — 集成测试（`tests/*.rs` + fixture）

- **位置**：crate 根的 `tests/integration_<能力>.rs`
- **范围**：跨模块/跨 crate 管线（fetch → parse → JS → render）
- **依赖**：真实 fixture HTML 文件（**纳入 git**）
- **运行**：`cargo test -p browser-cli --test integration_<name>`
- **工具**：`assert_cmd`（CLI 调用）+ `wiremock`（mock HTTP server）

**示例**（`crates/cli/tests/integration_storage.rs`）：
```rust
#[test]
fn storage_spa_first_visit_renders_stored_token() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/storage-spa.html");
    let output = Command::cargo_bin("browser")?
        .args(["render-script", fixture.to_str().unwrap(), "--width", "80"])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stored token=abc-123"));
}
```

### Level 3 — 真实命令验证（verify-before-delivery）

- **方式**：用最终用户视角执行 CLI 命令
- **检查**：stdout 每一行，零报错（不能容忍 "Failed to load" 等）
- **时机**：交付前、对比测试、demo 时
- **命令**：见下方"常用验收命令"

---

## 三级门禁命令（每个 commit 前必跑）

```bash
# 1. 格式
cargo fmt --all -- --check

# 2. Clippy（零 warning，-D warnings 当 error）
cargo clippy --workspace --all-targets -- -D warnings

# 3. 全量测试
cargo test --workspace --no-fail-fast
```

**全绿才能 commit。** 任何一项失败禁止 commit（见 [CONVENTIONS.md](./CONVENTIONS.md) §5）。

---

## 常用验收命令（按功能）

### SPA 爬虫核心（G1）
```bash
# 启动本地 SPA fixture
cd /tmp/spa-demo && python3 -m http.server 8765 &

# Level 3 验证：JS 执行后能拿到动态内容
./target/release/browser render-url http://localhost:8765/index.html --width 80
# ✅ stdout 必须含：PRODUCTS: Rust Book:$39.99 | ... 和 TOKEN: tk-xxx
```

### 截图（G2）
```bash
./target/release/browser render-url <url> --width 80 --screenshot out.png
# ✅ out.png 存在、非空、file out.png 显示 PNG image data
```

### 图像 ASCII（M12.3）
```bash
./target/release/browser image-ascii <png> --width 60 --height 20
# ✅ stdout 输出 10 级灰阶 ASCII art
```

### Web Storage（M13）
```bash
./target/release/browser render-script crates/cli/tests/fixtures/storage-spa.html --width 80
# ✅ stdout 含：first visit, stored token=abc-123, theme=dark
```

### Navigation（M14）
```bash
# history.pushState / location.href
# （通过 JS 单元测试验证，见 js-runtime navigation_shim tests）
cargo test -p browser-js-runtime navigation_shim
```

---

## Fixture 管理

### 位置
- `crates/html-parser/tests/fixtures/*.html` — HTML 解析 edge case
- `crates/cli/tests/fixtures/*.html` — 端到端渲染 fixture

### 现有 fixture（10 个）
| 文件 | 用途 |
|------|------|
| `simple.html` / `nested.html` / `unclosed.html` / `with-doctype.html` / `attrs.html` | HTML 解析 edge case |
| `example.com.html` | 真实站点快照（静态） |
| `news.ycombinator.com.html` | HN 快照（SSR） |
| `spa-blog.html` | SPA 动态生成（`__setBody`） |
| `dom-api.html` | JS DOM API 测试 |
| `storage-spa.html` | localStorage SPA 测试（M13） |

### 规则
- **纳入 git**：fixture 是快照，不重新生成
- **小而真实**：能复现真实场景，又不过大（< 50KB）
- **命名**：`<场景>.html`（kebab-case）
- **注释**：fixture 顶部注释说明用途 + 预期行为

### 黄金快照对比
```bash
UPDATE_SNAPSHOTS=1 cargo test -p browser-cli --test integration_snapshot
# 重生成黄金对比（M6.0a/b/c 修复 pin 住）
python3 tests/snapshots/compare.py
# 弹出 Safari vs 本浏览器对比
```

---

## CI（三平台矩阵）

`.github/workflows/ci.yml` 在每个 push/PR 上跑：
- **平台**：ubuntu-latest / macos-latest / windows-latest
- **步骤**：fmt check → clippy -D warnings → cargo test --workspace
- **Linux 额外**：安装 GUI 依赖（`libxkbcommon-dev` 等，供 gui crate 编译）

**要求**：每个 commit 三平台全绿才能合并。

---

## 测试统计（当前）

| 层 | 数量 |
|----|------|
| 单元测试（`#[test]` in src） | ~200 |
| 集成测试文件（`tests/*.rs`） | 11 |
| Fixture HTML | 10 |
| **workspace 总测试** | **260 passed, 0 failed** |

---

## 何时不写测试

- **POC / 实验代码**：先跑通再补测试
- **纯胶水代码**：如 main.rs 的子命令分发（由集成测试覆盖）
- **平台特定 UI**：GUI 窗口显示（用 `--check` flag 替代）

但 **90% 的代码必须有测试**（核心目标 G1/G2 相关代码 100% 有测试）。
