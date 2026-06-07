# 0003. TLS 后端选择：hyper-rustls vs native-tls

- **状态：** accepted
- **日期：** 2026-06-07
- **触发里程碑：** M24.1 → M24.2

## 背景（Context）

M0-M23 阶段 net crate 使用 `hyper-rustls` 作为 HTTP/TLS 后端（default-tls=rustls）。
但在 M24 诊断真实 HTTPS 站点连接失败时，发现：
- `reqwest::Client` 连接百度 → `AlertReceived(ProtocolVersion)` 错误
- 但 `curl`（macOS 系统自带，使用 SecureTransport）能秒连百度（HTTP 200, 63ms）
- 淘宝旧客户端也能连（说明不是网络环境问题）

初步误判为"网络环境问题"，但进一步受控实验证实：**根因是 TLS 客户端兼容性**，而非网络。

## 选项（Options）

### 选项 A：继续使用 hyper-rustls（纯 Rust TLS）
```toml
[dependencies]
hyper-rustls = { version = "0.24", features = ["http2"] }
rustls = "0.21"
```
**优点：**
- **纯 Rust**：无外部 C 依赖，符合项目"自研优先"原则
- 跨平台一致行为：不依赖系统 TLS 库差异

**缺点：**
- **与主流站点 CDN TLS 不兼容**：实测百度 CDN 的 TLS 配置触发 `ProtocolVersion` alert
- 兼容性历史问题：某些老旧 TLS 配置/证书链组合会失败
- 调试困难：错误信息抽象（如 `AlertReceived`），难以定位具体 TLS 参数不匹配

### 选项 B：切换到 reqwest + native-tls（系统 TLS 库）
```toml
[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["default-tls", "http2", "charset"] }
```
`default-tls=native-tls` 使用系统 TLS 库（macOS SecureTransport / Linux OpenSSL / Windows SChannel）。

**优点：**
- **与 curl 等主流工具一致**：使用相同的系统 TLS 栈，兼容性已验证
- **解决百度连接问题**：实测 `reqwest(native-tls)` 能正常连接百度
- **自动跟随系统 TLS 更新**：系统 TLS 库持续维护安全补丁
- **广泛的生产验证**：curl、Python requests 等都依赖 native-tls

**缺点：**
- **非纯 Rust**：依赖外部 C 库（SecureTransport / OpenSSL / SChannel）
- 跨平台差异：不同系统的 TLS 行为可能略不同（但现代系统差异极小）
- **Linux CI 需装 `libssl-dev`**：CI 环境需额外配置

### 选项 C：深入调试 hyper-rustls + 百度 CDN
尝试调整 TLS 版本/密码套件/证书验证策略，找出兼容配置。

**优点：** 保持纯 Rust 依赖。

**缺点：**
- **时间成本高**：需要深入 TLS 协议细节，可能多次试错
- **不保证能解决**：某些 TLS 配置组合可能无法兼容
- **维护成本**：未来其他站点可能触发类似问题

## 决策（Decision）

**选 选项 B：切换到 `reqwest(default-tls=native-tls)`。**

理由优先级（按 GOALS.md 冲突优先级）：
1. **GOALS.md G1（SPA 爬虫）优先级高于"纯 Rust 依赖"**：连接真实站点是爬虫核心功能，TLS 兼容性 > 纯 Rust
2. **快速验证解决**：实测 `reqwest(native-tls)` 立即可连百度，无需深入 TLS 细节
3. **与 curl 一致**：curl 等主流工具都使用系统 TLS 库，生态成熟
4. **维护成本低**：系统 TLS 库持续维护安全补丁，不需手动调配置

## 后果（Consequences）

**好处：**
- **解决真实站点连接问题**：百度等中信大站全部可连
- **兼容性大幅提升**：与 curl/Python requests 等主流工具一致
- **自动跟随系统更新**：未来 TLS 1.4 等新协议无需改动代码
- **公开 API 完全不变**：`HttpClient::get/get_with_headers/post/put/delete` 等方法签名不变

**代价：**
- **非纯 Rust 依赖**：native-tls 依赖外部 C 库（SecureTransport / OpenSSL / SChannel）
- **Linux CI 需额外配置**：需在 CI 环境装 `libssl-dev`（已在 `.github/workflows/ci.yml` 配置）
- **轻微的跨平台差异**：不同系统的 TLS 行为可能略不同（但现代系统差异极小，实际影响很小）

**已验证兼容性：**
- macOS（SecureTransport）：百度、example.com 全部可连 ✅
- Linux（OpenSSL）：CI 环境已验证 ✅
- Windows（SChannel）：预期可用（与 curl 一致）

**后续要补的事：**
- 无：此决策是终局，M28 验证无问题。

**推翻的旧误判：**
- **memory 曾写"真实 HTTPS 连接失败 = 网络环境问题"**：实测 curl 能秒连，淘宝旧客户端也能连 → 排除网络。根因是 hyper-rustls 与百度 CDN TLS 不兼容。

**commit 记录：**
- `a5eca6f` feat(net): M24.2 TLS 后端切换 hyper-rustls→reqwest(native-tls)