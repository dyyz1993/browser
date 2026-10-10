//! Arena 树：`Vec<Node> + NodeId`（与 browser 项目同款模式）。
//!
//! 标签名在编译期 intern 成 [`Tag`] 枚举，MCU 上不存字符串；
//! 属性/样式的存储在 M1 随树构建器一起进入。

use alloc::vec;
use alloc::vec::Vec;

pub type NodeId = u32;
pub const NULL: NodeId = u32::MAX;

/// 标签名（intern）。未知标签归入 [`Tag::Unknown`]，渲染时按容器/叶子
/// 语义降级处理（不影响兄弟节点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Svg,
    G,
    Rect,
    Circle,
    Ellipse,
    Line,
    Polyline,
    Polygon,
    Path,
    Defs,
    Use,
    Symbol,
    LinearGradient,
    RadialGradient,
    ClipPath,
    Mask,
    Pattern,
    Marker,
    /// `<switch>` 条件组（S10-04：取首个条件匹配的直接子元素渲染）。
    Switch,
    /// `<image>` 位图引用（S10-05：识别元素；data URI 解码超出当前
    /// 支持集——渲染期整体跳过）。
    Image,
    /// `<text>` 文本（M8 Tier B：内嵌 Hershey 矢量字模排版渲染）。
    Text,
    /// `<tspan>` 行内文本跨度（M8 Tier B：x/y 绝对跳位、dx/dy 位移、
    /// font-size/fill/font-weight 局部覆盖；缺省沿父文本 advance 前进）。
    Tspan,
    /// `<a>` 链接容器（车道AK）：自身不渲染、子树照常遍历（与未知标签
    /// 同语义）。独立变体的唯一目的：文本收集期可与 textPath（同为
    /// href 容器但布局语义不同）区分——text 内 <a> 透传，textPath 不
    /// 线性降级（保持不渲染，登记 CONFORMANCE）。
    A,
    /// `<textPath>`（车道AL）：文本沿引用路径弧长排布。只作为 `<text>`
    /// 的子元素在 emit_text 内按路径布局处理；直接出现在树中时不渲染
    ///（与 tspan 同语义）。
    TextPath,
    /// `<filter>`（M8：定义元素，内容为 primitive 链，不直接渲染）。
    Filter,
    /// fe* 滤镜 primitive（feDropShadow/feGaussianBlur/feColorMatrix/
    /// feOffset/feFlood/feComposite/feMerge/feMergeNode，M8 支持集；
    /// 具体种类在 ShapeAttrs.fe）。
    FePrim,
    /// SVG1.2 流式文本家族 `<flowRoot>`/`<flowRegion>`/`<flowPara>`/
    /// `<flowLine>`（车道LB）：Chrome 未实现流式文本，预言机对整棵
    /// 子树零输出——整子树不渲染（与 Defs 同「占位不描绘」语义）。
    /// flowRegion 的框矩形误按普通 rect 描绘曾产出整角黑块
    /// （openclipart Rabbit-001 5.4% 假差异实证）。
    FlowRoot,
    Unknown,
}

pub struct Node {
    pub tag: Tag,
    pub parent: NodeId,
    pub first_child: NodeId,
    pub last_child: NodeId,
    pub prev_sibling: NodeId,
    pub next_sibling: NodeId,
}

/// 文档树。根节点固定为 [`Tag::Svg`]。
pub struct Tree {
    nodes: Vec<Node>,
    root: NodeId,
}

impl Tree {
    pub fn new() -> Self {
        let nodes = vec![Node {
            tag: Tag::Svg,
            parent: NULL,
            first_child: NULL,
            last_child: NULL,
            prev_sibling: NULL,
            next_sibling: NULL,
        }];
        Self { nodes, root: 0 }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn create(&mut self, tag: Tag) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node {
            tag,
            parent: NULL,
            first_child: NULL,
            last_child: NULL,
            prev_sibling: NULL,
            next_sibling: NULL,
        });
        id
    }

    /// 把 `child` 挂到 `parent` 子节点末尾（文档序）。
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        let last = self.get_mut(parent).last_child;
        self.get_mut(child).parent = parent;
        self.get_mut(child).prev_sibling = last;
        if last != NULL {
            self.get_mut(last).next_sibling = child;
        } else {
            self.get_mut(parent).first_child = child;
        }
        self.get_mut(parent).last_child = child;
    }

    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    fn get_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id as usize]
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// svg > (g > (rect, circle)) 的构建与文档序遍历。
    #[test]
    fn build_and_walk_document_order() {
        let mut t = Tree::new();
        let g = t.create(Tag::G);
        let rect = t.create(Tag::Rect);
        let circle = t.create(Tag::Circle);
        t.append_child(t.root(), g);
        t.append_child(g, rect);
        t.append_child(g, circle);

        assert_eq!(t.get(t.root()).tag, Tag::Svg);
        assert_eq!(t.get(t.root()).first_child, g);
        assert_eq!(t.get(g).parent, t.root());

        // g 的子节点按文档序：rect -> circle
        let mut cur = t.get(g).first_child;
        assert_eq!(t.get(cur).tag, Tag::Rect);
        cur = t.get(cur).next_sibling;
        assert_eq!(t.get(cur).tag, Tag::Circle);
        assert_eq!(t.get(cur).next_sibling, NULL);
        assert_eq!(t.get(g).last_child, cur);
        assert_eq!(t.get(cur).prev_sibling, rect);
    }

    #[test]
    fn empty_tree_has_svg_root() {
        let t = Tree::new();
        let root = t.root();
        assert_eq!(t.get(root).tag, Tag::Svg);
        assert_eq!(t.get(root).first_child, NULL);
    }
}
