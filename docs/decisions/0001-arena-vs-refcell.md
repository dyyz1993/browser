# 0001. DOM 存储：arena vs `Rc<RefCell>`

- **状态：** accepted
- **日期：** 2026-06-06
- **触发里程碑：** M1.2

## 背景（Context）

DOM 树是浏览器的核心数据结构，需要被多个子系统共享访问：
- `js-runtime`：JS 代码通过 DOM API 读写节点
- `layout`：遍历 DOM 计算布局
- `render`：根据 DOM + 布局绘制
- 事件系统：分发事件到目标节点

DOM 又是**双向引用**的：每个节点既知道父，也知道所有子。这导致两种典型的 Rust 实现方案各有痛点，需要明确选型。

## 选项（Options）

### 选项 A：`Rc<RefCell<Node>>`
```rust
struct Node {
    parent: Option<Weak<RefCell<Node>>>,
    children: Vec<Rc<RefCell<Node>>>,
    data: NodeData,
}
```
**优点：**
- API 符合直觉，类似 JS 的 `node.appendChild(child)`
- 节点可以独立传递，不需要带 `&Tree` 参数

**缺点：**
- **借用 panic 风险**：`borrow_mut` 在嵌套调用时容易 panic
- **循环引用 leak**：parent ↔ child 即使逻辑上释放也会被 `Rc` 引用计数保活，需要小心用 `Weak`
- **内存碎片化**：每个节点独立堆分配，cache 不友好
- **JS 桥复杂**：JS 持有的句柄要映射到 `Rc<RefCell<Node>>`，生命周期管理混乱

### 选项 B：Arena（`Vec<Node>` + `NodeId(usize)`）
```rust
pub type NodeId = usize;
struct Node {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    data: NodeData,
}
struct Tree {
    nodes: Vec<Node>,
}
```
**优点：**
- **无借用冲突**：所有访问通过 `tree.get(id)` 显式借 `&` / `&mut`
- **无循环引用**：`NodeId` 是普通 `usize`，不会保活节点
- **内存连续**：`Vec<Node>` 对 CPU cache 友好，遍历快
- **JS 桥清晰**：JS 持有 `NodeId`（一个数字），到 Rust 端再查 arena
- **与 Servo / Ladybird 工业实践一致**

**缺点：**
- API 略 verbose：每次访问都要带 `&Tree`（或 `&mut Tree`）
- 节点删除后 `NodeId` 失效（dangling id）——需要释放策略
- 不容易做"节点的部分共享"

### 选项 C：`indextree` / `id-tree` 等现成 arena crate
**优点：** 现成、经过验证。
**缺点：** 锁定外部 API；学习本项目时绕开了"自己实现 arena"的练习；删除策略不可控。

## 决策（Decision）

**选 选项 B：自研 arena（`Vec<Node>` + `NodeId(usize)`）。**

理由优先级：
1. **避免 `Rc<RefCell>` 的借用 panic 和循环引用 leak**（最关键，M3 写 JS 桥时会爆发）
2. **对齐 Servo 工业实践**（学习目的，便于阅读 Servo 源码）
3. **JS 桥最简**（JS 持 `NodeId`，比持 `Rc` 更可预测）
4. **不依赖现成 crate**（学习目的）

## 后果（Consequences）

**好处：**
- 借用检查在编译期完成，运行时无 panic 风险
- 节点删除只需 `nodes[id] = placeholder` 或 free list，不会 leak
- 遍历 DOM 时 cache 友好（连续内存）
- JS 桥只需在 JS 侧存一个 `number`，跨边界简单

**代价：**
- API 形态变成 `tree.append_child(parent_id, child_data)`，比 `node.append(child)` 长一截
- 调用方需要持有 `&mut Tree`，链式调用稍麻烦
- 需要自己实现"删除 + 复用 slot"的策略（M1 阶段先不删，M3+ 再加 free list）

**后续要补的事：**
- M3：实现 NodeId 在 JS 引擎中的句柄映射
- 如果将来要并行渲染，arena + `NodeId` 也最容易切到 `parking_lot::RwLock<Tree>`
