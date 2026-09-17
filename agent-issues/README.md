# agent-issues — Agent 使用反馈回路

外部 Agent（经 `rust-browser` skill）使用 browser/bdiag 时遇到的问题记录在这里。
**browser 项目的 AGENTS.md 已约定：每次对话开工前拉取 `INBOX.md`，处理新条目。**

## 约定

| 文件/目录 | 作用 |
|-----------|------|
| `INBOX.md` | **唯一要读的文件**。未处理问题，新条目追加在顶部（`## <日期> <症状>` 一节一个） |
| `resolved/` | 修复归档。条目修完后从 INBOX 移过来（文件名补上修复 commit） |

## 条目模板（写入 INBOX.md 顶部）

```markdown
## 2026-09-17 某站正文抓不全只有导航
- url: https://example.com/post/123
- cmd: browser fetch https://example.com/post/123
- engine: quickjs
- 期望: 文章正文 3000 字 / 实际: 只有 nav 链接，正文 200 字
- 线索: bdiag 显示 console.error "Minified React error #418"
```

## 生命周期

```
Agent 发现问题 → 追加到 INBOX.md 顶部（无需通知任何人）
                ↓
browser 项目 Agent 开工 → 读 INBOX.md → 有新条目则优先处理
                ↓
修复（走自愈循环：报错驱动补 API → 测试 → 门禁 → commit）
                ↓
把条目移入 resolved/<原日期-slug>.md（文末补 `resolved-by: <commit>`）
```

- 只记录**可复现**的问题，附完整命令；猜测性问题写清楚"待验证"
- 与浏览器非目标（WebGL/风控对抗/像素级渲染）相关的，标记 `wontfix` 并写明依据
- INBOX.md 为空时保持一行说明文字即可
