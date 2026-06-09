# M40: 真实 SPA 站点评估报告

> 日期：2026-06-08
> HEAD：`de4a56f`（M40 timeout 修复）
> 目的：连真实网站，用数据决定后续方向（CDP / SPA 兼容性 / 性能）

## 1. 可靠性（最高优先级）

### 发现：HTTP 客户端无 timeout（致命缺陷）

`crates/net/src/client.rs` 的 `reqwest::Client::builder()` 完全没配
`connect_timeout` / `timeout`。任何慢响应或挂起的服务器 = 爬虫**永久 hang**。
对 G1（SPA 爬虫）这是头号可靠性缺陷。

### 修复（`de4a56f`）

```rust
.connect_timeout(Duration::from_secs(10))
.timeout(Duration::from_secs(30))
```

### 受控实验验证

| 站点 | curl 基线 | 修复前 | 修复后 |
|------|-----------|--------|--------|
| example.com | 200 / 0.87s | ✅ 正常 | ✅ 正常（954 bytes，无回归） |
| news.ycombinator.com | **curl 也超时**（环境不可达） | **永久 hang** | **10.7s 明确报错退出** |

HN 经 curl 确认是环境网络不可达（防火墙/DNS），不是代码问题。
修复前 reqwest 无超时 → 静默永久挂起；修复后正确暴露为 `error sending request`。
**这正是爬虫需要的明确报错行为。**

## 2. SPA 兼容性（baidu 实测）

### 测试对象
`render-url https://www.baidu.com`（curl 确认可达，200 / 0.11s）

### 结果
- **渲染成功**：402 行输出，导航栏 14 条真实链接全部出来
  （新闻/地图/贴吧/视频/图片/网盘/文库/搭子DuMate…）
- **JS 执行**：12 个 script，10 个报错

### JS 错误分类

| 错误 | 次数 | 根因 | 修复成本 |
|------|------|------|----------|
| `$ is not defined` | 3 | jQuery 未实现 | ❌ 高（完整 jQuery 是巨大工程） |
| `not a callable function` | 2 | 现代 API 缺失 | 中 |
| `SyntaxError: got ':'` | 2 | boa 不支持 ES6 shorthand `{ x }` | ⚠️ **boa 引擎限制**（非我们能修） |
| `require is not defined` | 1 | CommonJS（打包工具残留） | ❌ 高 |
| `Image is not defined` | 1 | 缺 Image 构造器 | ✅ 低（补 shim） |
| `F is not defined` | 1 | 压缩变量名 | 间接 |

### 关键判断
**真实网站 JS 兼容性的天花板在 boa 引擎，不在我们的 shim 层。**
- `$`（jQuery）/`require`（CommonJS）补了也跑不完整
- `SyntaxError: got ':'` 是 boa 解析器不支持 ES6 语法——引擎层限制
- 只有 `Image` 构造器是低成本可修的

**结论**：投入大量精力补 JS API 收益有限，真实 SPA 兼容性
受 boa 引擎限制封顶。

---

# M60: `browser fetch` 子命令 SPA 模式覆盖标准

> 更新日期：2026-06-09
> 目的：为 `browser fetch <url>` 子命令建立 SPA 模式分类和测试覆盖标准

## SPA 模式分类

### 核心模式（必覆盖）

1. **Async Data Loading** - 异步数据加载
   - DOMContentLoaded 时触发 `fetch`/`XHR` 加载内容
   - 等待策略：`dom-ready`（数据可能未完成）vs `load`（需要自定义 Promise chaining）
   - 验证：输出包含网络请求返回的内容

2. **Route Switching** - 单页路由切换
   - URL hash 变化或 History API 触发视图切换
   - 等待策略：`timeout`（等待路由解析）或手动触发
   - 验证：初始路由 + 切换后路由内容均在最终 HTML 中

3. **Lazy Loading** - 懒加载
   - IntersectionObserver 或 scroll 事件触发内容加载
   - 等待策略：`timeout`（等待观察器触发）或模拟 scroll
   - 验证：懒加载内容出现在最终 HTML 中

4. **Dynamic Form** - 动态表单
   - JS 动态修改 DOM（添加/删除/修改表单元素）
   - 等待策略：`dom-ready`（动态操作通常在 DCL 后）
   - 验证：初始 HTML 不存在的表单元素出现在最终 HTML 中

5. **Redirect** - JS 重定向
   - `window.location.href` 或 `window.location.replace()`
   - 等待策略：`timeout`（等待重定向触发）
   - 验证：最终 HTML 是重定向目标页面的内容

### 高级模式（可选，未来覆盖）

6. **WebSocket Data** - WebSocket 实时数据推送
7. **Polling** - 定时轮询更新内容
8. **MutationObserver** - DOM 变化监听器
9. **Shadow DOM** - Web Components 阴影 DOM
10. **Custom Elements** - 自定义元素定义

## 测试覆盖清单

| 模式 | Fixture 文件 | 等待策略 | 测试覆盖 | 状态 |
|------|-------------|----------|---------|------|
| Async Data Loading | `async-data-spa.html` | dom-ready, load, timeout | CLI tests | ⬜ 待实现 |
| Route Switching | `route-switch-spa.html` | dom-ready, load, timeout | CLI tests | ⬜ 待实现 |
| Lazy Loading | `lazy-load-spa.html` | dom-ready, load, timeout | CLI tests | ⬜ 待实现 |
| Dynamic Form | `dynamic-form-spa.html` | dom-ready, load, timeout | CLI tests | ⬜ 待实现 |
| Redirect | `js-redirect-spa.html` | dom-ready, load, timeout | CLI tests | ⬜ 待实现 |

## 等待策略实现要点

### dom-ready
- 触发时机：`DOMContentLoaded` 事件
- 适合：静态 DOM 操作完成后的内容提取
- 局限：异步数据可能未加载完成

### load
- 触发时机：`load` 事件（所有资源加载完成）
- 适合：确保所有同步资源加载完成
- 局限：不保证异步数据（fetch/XHR）完成，需要 Promise chaining

### timeout
- 触发时机：`--timeout <ms>` 后返回当前渲染结果
- 适合：不保证完全渲染，但避免永久挂起
- 局限：可能返回部分渲染结果

## 验收标准

### 基本要求
- ✅ `browser fetch <url>` 写入渲染后的 HTML 到 stdout
- ✅ 输出 HTML 包含 JS 生成的 DOM 内容（原始网络响应中不存在）
- ✅ `--wait-strategy` 支持 `dom-ready` / `load` / `timeout`（默认：`load`）
- ✅ `--timeout <ms>` 设置最大等待时间，到期返回当前渲染结果（exit code 0）
- ✅ 现有 fixtures (`async-spa.html`, `cn-spa.html`) 通过新命令完全渲染
- ✅ 5 个新 SPA 模式 fixtures 全部正确渲染

### 质量要求
- ✅ `cargo test` 0 failures（所有新 + 现有测试）
- ✅ `cargo clippy -D warnings` 0 warnings
- ✅ `cargo fmt --check` 格式正确
- ✅ CLI 集成测试覆盖每种等待策略和每个 SPA 模式

### 文档要求
- ✅ `docs/assessments/M40-real-world-spa.md` 更新（本文档）
- ✅ `PROGRESS.md` 更新里程碑状态
- ✅ `docs/ROADMAP.md` 更新 roadmap

## 3. 内存基线（达标）

| 场景 | RSS（peak） |
|------|-------------|
| baidu 首页（12 script + 402 行渲染 + 双字体回退 CJK 1.6MB） | **29.9 MB** |
| Chrome 同页面对比（参考） | ~300 MB+ |

**判断**：低内存目标已优秀达成（1/10 Chrome），**无需优化**。
最大开销是 CJK 字体子集 1.6MB（已在 M36 裁剪过），无法进一步压缩。

## 4. 数据驱动的方向建议

基于以上评估：

| 方向 | 评估 | 建议 |
|------|------|------|
| **CDP 服务端** | 学习价值最高；让 JS 引擎短板被工具生态弥补（Puppeteer 连进来后，即使 boa 跑不动也能用 `Page.captureScreenshot` / `DOM.querySelector` 等**不依赖 JS 执行**的 CDP 域爬数据） | ✅ 推荐（多 milestone 大工程） |
| Image 构造器 shim | 成本低（1 milestone），消除 1 类错误 | 可做（小） |
| 性能/内存优化 | 基线 29.9MB 已达标 | ❌ 不建议投入 |
| 补 $/require | 成本高且收益封顶（boa 限制） | ❌ 不建议 |

## 5. 后续里程碑建议

```
M41  Image 构造器 shim（低成本，清 1 类错误）
M42+ CDP 服务端（大工程，拆细）：
     M42  WebSocket server + JSON-RPC 框架
     M43  Target/Tab 管理 + discovery endpoint (/json/version, /json/list)
     M44  Page domain（navigate, captureScreenshot）
     M45  Runtime domain（evaluate, consoleLog）
     M46  DOM domain（querySelector, getOuterHTML）
     M47  Network domain（getResponseBody, enable/disable）
     M48  Puppeteer e2e 联调
```
