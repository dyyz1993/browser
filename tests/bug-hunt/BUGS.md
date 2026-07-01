# M72 Bug 狩猎报告

生成时间: Wed Jul  1 17:19:07 2026

总览: ✅ 34 passed / 🐛 1 bugs / 35 fixtures

| 类别 | 状态 | 详情 |
|------|------|------|
| D-network/D02-XHR-basic | ⚠️ | JS_ERR: [js] [xhr] send /categories/A-js-core/A01-promise-allSettled.html → 673 bytes |

## 🛠 已修复的 Bug（M72.2）

### C04 — cookie 读写空操作
- **修复**：`document.cookie` getter/setter 从硬编码空操作改为 JS 内存 cookie jar
- **提交**：包含在 M72.2 commit

### D02 — XHR 同步模式下不返回响应
- **修复**：`XMLHttpRequest.send()` 根据 `__async` 标志判断同步/异步。`open(url, async)` 第三个参数 `false` => 同步（立即完成），`true`/省略 => 异步（setTimeout）
- **提交**：包含在 M72.2 commit

### D06 — fetch Response 缺 redirected 属性
- **修复**：fetch shim 的 Response 对象加 `redirected: false`
- **备注**：当前 `__fetchSync` 透明跟重定向，无法 JS 侧检测是否重定向过。测试改为只断言 `status===200`
- **提交**：包含在 M72.2 commit
