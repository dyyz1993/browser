# Architecture Decision Records (ADR)

每个"为什么这么选"的决策记一份 ADR。

## 命名规则
`NNNN-short-kebab-title.md`，NNNN 为四位编号，零填充。

例：
- `0001-arena-vs-refcell.md`
- `0002-javascript-engine-selection.md`

## 模板

```markdown
# NNNN. 标题

- **状态：** proposed / accepted / deprecated / superseded by ADR-XXXX
- **日期：** YYYY-MM-DD

## 背景（Context）
为什么需要做这个决策？触发条件是什么？

## 选项（Options）
列出至少 2 个候选项，各自的优缺点。

## 决策（Decision）
选了哪个？为什么？

## 后果（Consequences）
这个决策带来的好处和代价。后续可能要补的坑。
```

## 索引

| # | 标题 | 状态 |
|---|------|------|
| 0001 | DOM 存储：arena vs Rc<RefCell> | 待写（M1.2 触发） |
