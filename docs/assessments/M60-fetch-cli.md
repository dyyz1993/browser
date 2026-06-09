# M60: `browser fetch` 子命令设计文档

> 目的：为爬虫场景提供 CLI 工具，直接获取 JS 渲染后的完整 HTML

## 功能概述

```bash
# 基本用法
browser fetch https://example.com > rendered.html

# 指定等待策略
browser fetch --wait-strategy dom-ready https://async-spa.com > output.html
browser fetch --wait-strategy load https://spa-with-onload.com > output.html

# 指定超时
browser fetch --timeout 5000 https://slow-spa.com > output.html
```

## 等待策略详解

### 1. `dom-ready` (默认)
- **触发时机**: HTML 解析完成，DOMContentLoaded 事件触发
- **适用场景**: 静态内容渲染 + 简单 DOM 操作
- **实现**: 等待所有内联 `<script>` 执行完成
- **优势**: 快速返回，不等待外部资源
- **局限**: 不包含异步数据加载内容

### 2. `load`
- **触发时机**: 所有资源加载完成，window.onload 事件触发
- **适用场景**: 异步数据加载、图片懒加载
- **实现**: 需要添加 `onload` 事件监听器
- **优势**: 包含异步动态内容
- **局限**: 需要页面正确实现 `onload` 事件链

### 3. `timeout`
- **触发时机**: 指定毫秒数后强制返回
- **适用场景**: 页面未正确实现事件、需要固定等待时间
- **实现**: 简单的 sleep + 返回
- **优势**: 可控性强，兜底策略
- **局限**: 可能返回未完成渲染的内容

## 实现步骤

### Step 1: 添加生命周期事件支持
- `js-runtime`: 添加 `on_domcontentloaded` 和 `onload` 事件钩子
- `eventloop`: 支持事件队列和触发机制
- `page`: 暴露 `wait_for_readiness(strategy, timeout)` 方法

### Step 2: CLI 子命令
- 添加 `Cmd::Fetch { url, wait_strategy, timeout }` 枚举
- 解析命令行参数
- 调用 page 层的渲染和等待逻辑
- 输出 HTML 到 stdout

### Step 3: 渲染 HTML 提取
- 添加 `Tree::to_html()` 方法，将 DOM 序列化为 HTML
- 包含所有 JS 修改后的状态

### Step 4: 新增 SPA fixtures
1. `async-load-spa.html` - 异步数据加载
2. `route-switch-spa.html` - 路由切换
3. `lazy-load-spa.html` - 懒加载
4. `dynamic-form-spa.html` - 动态表单
5. `redirect-spa.html` - 重定向

### Step 5: 集成测试
- 每个等待策略的测试
- 每个 SPA fixture 的测试
- 验证输出包含预期内容

## 技术细节

### DOM 序列化
```rust
impl Tree {
    pub fn to_html(&self) -> String {
        // 深度优先遍历，序列化为 HTML
        // 包含所有动态修改后的内容
    }
}
```

### 等待策略实现
```rust
pub enum WaitStrategy {
    DomReady,  // 等待所有内联脚本执行完成
    Load,      // 等待 window.onload 触发
    Timeout(Duration),  // 等待指定时间
}
```

### 事件监听器
```rust
// 在 DOM shim 中添加
window.addEventListener('DOMContentLoaded', handler);
window.addEventListener('load', handler);
```

## 验收标准

- [ ] `browser fetch https://example.com` 输出完整 HTML
- [ ] `--wait-strategy dom-ready` 工作正常
- [ ] `--wait-strategy load` 工作正常
- [ ] `--timeout 5000` 工作正常
- [ ] 异步数据加载被正确捕获
- [ ] 所有 5 个 SPA fixtures 正确渲染
- [ ] 集成测试全部通过
- [ ] `cargo test` 零失败
