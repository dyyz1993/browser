## M49: CDP Emulation domain（2026-06-09）

### Goal
实现 CDP Emulation domain，用于模拟设备视口、用户代理、时区等，提升真实网站兼容性。

### Scope
- **Emulation.setDeviceMetricsOverride**: 设置视口宽度、高度、设备比例因子、移动模式
- **Emulation.setUserAgentOverride**: 设置 User-Agent（Puppeteer `page.setUserAgent()`）
- **Emulation.clearDeviceMetricsOverride**: 清除设备模拟设置

### Implementation

**新增文件**: `crates/cdp/src/emulation_domain.rs`
- `EmulationState`: 存储设备指标和 User-Agent
  ```rust
  pub struct EmulationState {
      pub width: Option<u32>,
      pub height: Option<u32>,
      pub device_scale_factor: Option<f64>,
      pub mobile: bool,
      pub user_agent: Option<String>,
  }
  ```
- `dispatch(id, method, params, state)`: 处理 3 个方法
  - `Emulation.setDeviceMetricsOverride`: 提取 `width`, `height`, `deviceScaleFactor`, `mobile`
  - `Emulation.setUserAgentOverride`: 提取 `userAgent`
  - `Emulation.clearDeviceMetricsOverride`: 重置为默认值

**修改文件**:
- `crates/cdp/src/lib.rs`: 添加 `pub mod emulation_domain`
- `crates/cdp/src/server.rs`: 添加 `emulation: Arc<Mutex<EmulationState>>` 到 `CdpSession`
- `crates/cdp/src/page.rs` (可选): 在 `PageState::render` 中使用 `EmulationState` 影响 viewport/UA

### Integration
- 6 个单元测试（3 个方法 × 2 场景）
- `EmulationState::default()`: None 值（不覆盖）

### Limitations
- 不影响实际 HTTP 请求的 User-Agent（需修改 `browser-net` 的 `HttpClient`）
- 不影响实际 viewport 计算（需修改 `browser-layout`）
- 仅存储值用于未来扩展

---

## M50: CDP Input domain（2026-06-09）

### Goal
实现 CDP Input domain，支持模拟用户输入（点击、键盘事件）。

### Scope
- **Input.dispatchMouseEvent**: 鼠标移动、点击
- **Input.dispatchKeyEvent**: 键盘输入
- **Input.dispatchTouchEvent**: 触摸事件

### Implementation

**新增文件**: `crates/cdp/src/input_domain.rs`
- `InputEvent`: 枚举 (MouseEvent, KeyEvent, TouchEvent)
- `dispatch(id, method, params, state)`: 解析事件类型并存储到队列

**修改文件**:
- `crates/cdp/src/lib.rs`: 添加 `pub mod input_domain`
- `crates/cdp/src/server.rs`: 添加 `input_events: Vec<InputEvent>` 到 `CdpSession`

### Integration
- 6 个单元测试（每种事件类型 × 2 场景）

### Limitations
- 不真正执行事件（需修改 `browser-dom`/`browser-gui`）
- 仅存储事件用于未来扩展

---

## M51: CDP Performance domain（2026-06-09）

### Goal
实现 CDP Performance domain，提供性能指标（内存、时间）。

### Scope
- **Performance.getMetrics**: 返回内存使用、加载时间等
- **Performance.enable/disable**: no-op ack

### Implementation

**新增文件**: `crates/cdp/src/performance_domain.rs`
- `Metrics`: 结构体，包含 `Timestamp`, `JSHeapUsedSize`, `Documents` 等
- `get_metrics()`: 返回当前指标（部分值可从 `PageState` 获取）

**修改文件**:
- `crates/cdp/src/lib.rs`: 添加 `pub mod performance_domain`
- `crates/cdp/src/server.rs`: 添加 `performance: Arc<Mutex<PerformanceState>>` 到 `CdpSession`

### Integration
- 4 个单元测试（getMetrics × 2 场景 + enable/disable 各 1）

### Limitations
- 大部分指标返回固定值（如 `JSHeapUsedSize` 从 boa 无法获取）
- 时间戳可用 `std::time::SystemTime`

---

## M52: CDP Target domain（2026-06-09）

### Goal
实现 CDP Target domain，支持多目标（页面）管理，这是 Puppeteer `browser.pages()` 所需。

### Scope
- **Target.getTargets**: 列出所有目标（页面）
- **Target.activateTarget**: 激活指定目标（切换焦点）
- **Target.createTarget**: 创建新目标（新标签页）
- **Target.closeTarget`: 关闭目标

### Implementation

**新增文件**: `crates/cdp/src/target_domain.rs`
- `TargetInfo`: 结构体，包含 `targetId`, `type` (page), `title`, `url`, `attached`
- `TargetManager`: 管理多个 `PageState`（Vec `Arc<Mutex<PageState>>`）
  - `targets: HashMap<String, Arc<Mutex<PageState>>>`
  - `next_target_id: AtomicUsize`
- `dispatch(id, method, params, manager)`: 处理 4 个方法

**修改文件**:
- `crates/cdp/src/lib.rs`: 添加 `pub mod target_domain`
- `crates/cdp/src/server.rs`: 将 `page` 改为 `target_manager: Arc<Mutex<TargetManager>>`

### Integration
- 8 个单元测试（4 个方法 × 2 场景）

### Limitations
- 仅支持 `type: "page"`，不支持 `worker`, `service_worker`, `browser`
- `createTarget` 可创建新 `PageState` 但复用同一个 DOM tree
- `closeTarget` 仅从 `HashMap` 中删除，不清理资源

---

## M53: CDP Fetch domain（2026-06-09）

### Goal
实现 CDP Fetch domain，用于拦截和修改网络请求（Puppeteer `page.setRequestInterception(true)`）。

### Scope
- **Fetch.enable**: 启用请求拦截
- **Fetch.disable**: 禁用拦截
- **Fetch.continueRequest**: 继续被拦截的请求
- **Fetch.fulfillRequest**: 返回模拟响应（用于 mock）

### Implementation

**新增文件**: `crates/cdp/src/fetch_domain.rs`
- `FetchState`: 存储拦截状态
  - `enabled: bool`
  - `intercepted_requests: HashMap<RequestId, InterceptedRequest>`
  - `next_request_id: AtomicUsize`
- `InterceptedRequest`: 存储请求详情（URL, method, headers, body）
- `dispatch(id, method, params, state)`: 处理 4 个方法

**修改文件**:
- `crates/cdp/src/lib.rs`: 添加 `pub mod fetch_domain`
- `crates/cdp/src/page.rs`: 在 `PageState::render` 中调用 `fetch_domain` 的拦截逻辑

### Integration
- 8 个单元测试（4 个方法 × 2 场景）

### Limitations
- 需要深度集成 `browser-net` 的拦截器（已存在 `RequestContext`/`ResponseContext`）
- `continueRequest` 实际发送原始请求
- `fulfillRequest` 返回模拟响应，不真正 fetch

---

## M54: CDP Runtime consoleLog（2026-06-09）

### Goal
增强 CDP Runtime domain，支持 `console.log()` 输出捕获（Puppeteer `page.on('console')`）。

### Scope
- **Runtime.consoleAPICalled event**: 当 JS 执行 `console.log()` 时触发
- **Runtime.enable**: 启用 console 事件流

### Implementation

**修改文件**: `crates/cdp/src/runtime_domain.rs`
- 添加 `console_log_queue: Vec<ConsoleMessage>` 到 `RuntimeState`
- `ConsoleMessage`: 结构体（`type`, `args`）
- 在 `evaluate` 中捕获 boa 的 `console.log` 输出（如果 boa 支持）
- `Runtime.enable`: 开始监听 console 事件
- `Runtime.disable`: 停止监听

**修改文件**: `crates/cdp/src/server.rs`
- 在 `handle_frame` 循环中检查 `console_log_queue` 并发送 `Runtime.consoleAPICalled` 事件

### Integration
- 4 个单元测试（consoleAPICalled × 2 + enable/disable 各 1）

### Limitations
- boa 0.20 的 console API 支持未知（可能需要自建）
- 事件发送需要异步 channel（或每帧轮询）

---

## M55: CDP Page events（2026-06-09）

### Goal
增强 CDP Page domain，支持页面生命周期事件（`load`, `DOMContentLoaded`, `frameNavigated`）。

### Scope
- **Page.frameNavigated event**: 导航完成时触发
- **Page.loadEventFired event**: `onload` 触发
- **Page.domContentEventFired event**: `DOMContentLoaded` 触发

### Implementation

**修改文件**: `crates/cdp/src/page.rs`
- 在 `PageState::render` 完成后发送 `Page.frameNavigated`
- 如果 JS 执行完成（`JsRuntime::drain` 完成），发送 `Page.loadEventFired`
- 在解析 HTML 后发送 `Page.domContentEventFired`

**修改文件**: `crates/cdp/src/server.rs`
- 在 `handle_frame` 循环中检查事件队列并发送

### Integration
- 4 个单元测试（每种事件 × 2 场景）

### Limitations
- 事件检测依赖 JS 执行状态（ boa 可能不支持 `document.readyState`）
- 需要异步 channel 或轮询队列

---

## M56: End-to-end with Real Puppeteer（2026-06-09）

### Goal
使用真实 Puppeteer 库运行端到端测试，验证 CDP 兼容性。

### Setup

```bash
cd /Users/xuyingzhou/Project/study-rust/browser
npm init -y
npm install puppeteer
```

### Test Cases

1. **Basic Navigation + Screenshot**
   - Puppeteer `page.goto('https://example.com')`
   - Puppeteer `page.screenshot()`
   - 验证 base64 PNG 有效

2. **DOM Querying**
   - Puppeteer `page.$('h1')`
   - Puppeteer `page.$$('a')`
   - 验证 `querySelector`/`querySelectorAll` 返回正确 CDP nodeId

3. **JavaScript Evaluation**
   - Puppeteer `page.evaluate(() => 2 + 2)`
   - 验证 `Runtime.evaluate` 返回 `4`

4. **User-Agent Override**
   - Puppeteer `page.setUserAgent('MyBot/1.0')`
   - 验证 `Emulation.setUserAgentOverride` 接收值

5. **Device Metrics Override**
   - Puppeteer `page.setViewport({ width: 375, height: 667, isMobile: true })`
   - 验证 `Emulation.setDeviceMetricsOverride` 接收值

### Files

**新建目录**: `tests/e2e/`
- `tests/e2e/puppeteer_basic.js`: 基本导航和截图
- `tests/e2e/puppeteer_dom.js`: DOM 查询
- `tests/e2e/puppeteer_evaluate.js`: JS 执行
- `tests/e2e/puppeteer_emulation.js`: UA 和视口模拟

**执行脚本**:
```bash
# Terminal 1: 启动 CDP server
./target/release/browser cdp --port 9222

# Terminal 2: 运行 e2e 测试
node tests/e2e/puppeteer_basic.js
node tests/e2e/puppeteer_dom.js
node tests/e2e/puppeteer_evaluate.js
node tests/e2e/puppeteer_emulation.js
```

### Success Criteria

- [ ] 所有测试无崩溃
- [ ] `page.goto()` 返回 frameId
- [ ] `page.screenshot()` 返回有效 PNG
- [ ] `page.$()` 和 `page.$$()` 找到正确元素
- [ ] `page.evaluate()` 返回正确结果
- [ ] `page.setUserAgent()` 和 `page.setViewport()` 无错误

### Limitations

- JS 兼容性限制（boa 不支持 ES6 shorthand 等）
- 部分 Puppeteer 方法不可用（`page.waitForSelector`, `page.waitForNavigation` 等）
- 不支持多标签页（除非 M52 实现）

---

## M57: Documentation Update（2026-06-09）

### Goal
更新文档，反映 CDP 完整功能和 e2e 测试结果。

### Files to Update

1. **README.md**
   - 添加 CDP 使用示例
   - 添加 Puppeteer 集成示例
   - 更新功能列表

2. **docs/ARCHITECTURE.md**
   - 添加 CDP 架构章节
   - 描述各 domain 职责

3. **docs/FEATURES.md**
   - 添加 CDP 功能清单（M42-M56）
   - 列出已知限制

4. **docs/TESTING.md**
   - 添加 CDP 测试章节
   - 添加 e2e 测试指南

5. **PLAN.md**
   - 标记 M42-M57 完成
   - 更新下一步方向

### Deliverables

- 所有文档更新并提交
- 代码注释完整（crates/cdp/src/*.rs）
- 端到端测试结果写入文档

---

## Summary of M49-M57

| Milestone | Domain | Tests | Status |
|-----------|--------|-------|--------|
| M49 | Emulation | 6 | TODO |
| M50 | Input | 6 | TODO |
| M51 | Performance | 4 | TODO |
| M52 | Target | 8 | TODO |
| M53 | Fetch | 8 | TODO |
| M54 | Runtime consoleLog | 4 | TODO |
| M55 | Page events | 4 | TODO |
| M56 | Puppeteer e2e | 4+ | TODO |
| M57 | Documentation | N/A | TODO |

**Total Tests**: ~44 unit tests + ~20 e2e tests

**Estimated Time**: 3-4 hours

**Prerequisites**: M42-M48 已完成（基础 CDP 框架）

---

*Continuing from M48, expanding CDP coverage to full Puppeteer compatibility.*