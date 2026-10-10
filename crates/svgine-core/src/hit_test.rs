//! 命中检测与事件派发（车道GE；车道HJ 精确化+车道HK 事件族扩展）
//! ——交互动画链路的「外部时刻注入」环。
//!
//! 宪法 2（纯函数渲染）不动摇：本模块不引入任何时钟/事件循环。宿主
//! （CLI `--inject-event` / FFI 宿主 / Android touch）在渲染第 t 帧之前
//! 调用派发 API，把「事件名 × 事件目标 × 事件时刻」注入文档时间线；
//! 此后的 `build_display_list_at(t)` 仍是纯函数（事件只改变
//! `Timing.begin_instances` / `AnimBinding.end` 的取值，不改变求值语义）。
//!
//! 两半链路：
//!
//! 1. **命中检测** [`hit_test`]：设备空间点 → 最顶绘制元素（文档序
//!    后者胜 = 绘制栈顶），祖先 transform 逐层逆映射回元素用户空间做
//!    几何包含。车道HJ 精确化（Chrome elementFromPoint 探针矩阵
//!    tests/oracle/repros/chrome/hj/ 实证）：
//!    - `pointer-events` 域表（XML/CSS 双解析 + 继承链；visible* 值
//!      要求 visibility:visible，其余值不看 visibility）；
//!    - clip 裁断：clipPath 环集（复用渲染烘焙口径）/ 嵌套 svg 与
//!      use→symbol/svg 视口 AABB（未知引用宽容不裁、空定义全裁——
//!      与渲染链同语义）；
//!    - `<use>` 影子树展开：几何按实例化解析，命中 retarget 到 use
//!      元素本身（Chrome elementFromPoint 返回 use——探针 hj2 实证）；
//!    - text 多段（显式 x/y 的 tspan 段开新近似盒）、rect 圆角
//!      （rx/ry 缺省互取 + 钳半轴，同渲染 round_rect_geom 口径）。
//! 2. **事件派发** [`dispatch_event`]（精确目标）/ [`dispatch_event_bubble`]
//!    （冒泡链）/ [`dispatch_event_at`]（点形式 = 命中 + 冒泡）：
//!    对事件基 begin（`begin="click"` / `begin="btn.click"`，含带偏移
//!    形态 `btn.click+2s`——车道HK）把「事件时刻+偏移」追加为 begin
//!    实例（此后 restart/dur/repeat/fill 全部复用既有 `Timing::state_at`
//!    语义）；对事件基 end（`end="mouseout"`）把当前 Active 区间裁断到
//!    「事件时刻+偏移」。**事件名全参数化**——SVG1.1 UI 事件族
//!    （click/mousedown/mouseup/mouseover/mousemove/mouseout/focusin/
//!    focusout 等）一律按原文精确匹配，引擎层无白名单。
//! 3. **指针序列宿主语义**（车道HK，Chrome 对齐）：[`pointer_down`] /
//!    [`pointer_move`] / [`pointer_up`] 三段式——DOWN = mouseover 到达
//!    转换 + mousedown（记录按压目标）；MOVE = mouseout/mouseover 目标
//!    转换 + mousemove；UP = mouseup + **click 派发到按压目标与抬起
//!    目标的最近公共包含祖先**（Chrome/Blink 真实输入语义；同元素时
//!    即自身）。
//!
//! 已登记近似（CONFORMANCE「车道GE/HJ/JD」）：
//! - `<text>` 命中仍为 advance 近似盒（0.6em/字符），返回 text 元素
//!   （Chrome 返回 tspan 粒度）；dx/dy 累计与无显式 x/y 的连续段忽略；
//! - `<image>` 元素矩形命中**与 Chrome 一致**（meet letterbox 区命中、
//!   slice 溢出区不命中——hj4 探针实证，非近似）；
//! - **mask/filter 不参与命中裁断 = Chrome 语义**（车道JD jd1/jd2 探针
//!   实证：mask 黑区/全隐元素/默认区域黑区均照常命中，feOffset 位移后
//!   的显示区（原几何外）不命中、原几何照常命中——命中按**源几何**，
//!   与 HJ 登记的「近似」不同，实为一致，核销不改）；
//! - **dash 图案感知命中**（车道JD jd3 探针实证：间隙中心 miss/实段
//!   中心 hit——Chrome 描边带命中读 dash 相位）：弧长定位 + 相位判定
//!   （[`dash_on_at`]，与渲染 stroke_dashed 同一推进不变量）；% 图案
//!   按实线（视口对角线基准的嵌套视口差异，登记）；点模式零长亮段按
//!   点心圆命中（Round/Square 帽）。
//! - **fill-rule 感知命中**（车道JD jd3 探针实证：五角星中心
//!   nonzero 命中/evenodd 不命中——Chrome 读规则）：Path 全部子路径
//!   跨环求值（[`point_in_rings`]），poly/polygon 单环同式；fill-rule
//!   链值随 PaintCtx 继承（渲染 walk 同语义）。
//! - 动画事件基（`beginEvent`/`endEvent`/`repeatEvent`）可解析、可
//!   `dispatch_event` 注入；Chrome 实时播放自动派发、**seek（钉扎）
//!   不补发**（车道JD jd5 探针：setCurrentTime(1.5) 后链式动画保持
//!   基态）——引擎按 seek 语义渲染帧，自动触发源不内建（宿主要接
//!   实时派发须自带时钟检测，登记不做）。
//! - **拖动中 :active**（车道JD jd4/jd6 探针）：按压后指针滑出按压
//!   元素子树 → 整链清 :active、**不转移到指针下元素、滑回不恢复**
//!   （单发清除，UP 前不再重建）；[`drag_active_step`] 给出宿主逐
//!   MOVE 的纯函数更新（清空锁存由宿主持有）。

use alloc::string::String;
use alloc::vec::Vec;

use crate::anim::ActiveState;
use crate::builder::{
    clip_child_rings, resolve_href, DevRings, Document, FillSpec, PointerEvents, ShapeAttrs,
    StrokeSpec, TextAnchor,
};
use crate::dl::FillRule;
use crate::fnum::parse_f32;
use crate::geom::{parse_transform, viewport_map, Mat2x3};
use crate::path::{self, Cap};
use crate::tree::{NodeId, Tag, NULL};

/// 递归深度上限（防御病态嵌套；与 mask 构建期深度守卫同数量级）。
const MAX_DEPTH: u32 = 64;

/// 命中裁切域（车道HJ）：设备空间约束链。点固定 → 逐层 push 时判一次，
/// 链上任一域不含点即整子树剪枝（渲染上点处无该子树内容）。
enum ClipZone {
    /// 可解析但无几何的 clipPath（车道CW 渲染语义：引用元素整体不
    /// 描绘 → 命中同样全裁）。
    Empty,
    /// 视口 AABB（嵌套 svg / use→symbol/svg 视口；四角映射取 AABB，
    /// 与渲染 viewport_clip 同式——斜变换下近似）。
    Rect([f32; 4]),
    /// clipPath 烘焙环集 + 规则（复用渲染 [`clip_child_rings`] 口径）。
    Rings(DevRings, FillRule),
}

// ---------------------------------------------------------------------------
// 1. 命中检测
// ---------------------------------------------------------------------------

/// 设备空间点 `(px, py)` → 最顶绘制元素（`None` = 无绘制元素覆盖）。
///
/// `(vw, vh)` 为建档视口（与 `build_display_list_at` 的语义一致：根 svg
/// 无 viewBox 时 1:1 映射）。多元素覆盖时返回**文档序最后**的命中者
/// （SVG 绘制顺序 = 文档序，后者在栈顶）。display:none / 外命名空间
/// 抑制元素不命中；visibility:hidden 元素按 `pointer-events` 域表判定
///（仅 visible* 值要求可见——车道HJ Chrome 实证）。
pub fn hit_test(doc: &Document, vw: f32, vh: f32, px: f32, py: f32) -> Option<NodeId> {
    if !px.is_finite() || !py.is_finite() {
        return None;
    }
    // 根映射（与 build_display_list_at 同构）：viewBox → 元素矩形，
    // 根 transform 再叠加。inverse() 为 None（如 scale(0)）→ 整树退化
    // 不可命中。
    let root = &doc.shapes[doc.tree.root() as usize];
    let ew = used_len(root.w_set, root.pct.w, root.w, vw);
    let eh = used_len(root.h_set, root.pct.h, root.h, vh);
    let base = match root.viewbox {
        Some(vb) => viewport_map(ew, eh, vb, &root.par),
        None => Mat2x3::IDENTITY,
    };
    let base = match &root.transform {
        Some(t) => base.mul(&parse_transform(t)),
        None => base,
    };
    let mut st = HitState {
        doc,
        px,
        py,
        vw,
        vh,
        clips: Vec::new(),
        best: None,
    };
    visit_children(
        &mut st,
        doc.tree.root(),
        base,
        PaintCtx {
            fill: true,    // 链尽缺省 fill = 黑（可绘）
            stroke: false, // 链尽缺省 stroke = none
            stroke_w: 1.0,
            vis_hidden: false,
            pe: PointerEvents::VisiblePainted,
            fill_rule: FillRule::NonZero,
            dash: None,
            dash_offset: 0.0,
            path_length: None,
        },
        0,
        None,
    );
    st.best
}

/// 可继承描绘 presence 的链上值（命中语义只关心「有/无」与描边宽度）。
/// 车道JD 扩展：fill-rule / dash 图案链值（Chrome 实证命中读两者——
/// jd3 探针；继承语义与渲染 walk 一致：自身 `*_set` 位置胜，否则沿链）。
#[derive(Clone, Copy)]
struct PaintCtx<'a> {
    fill: bool,
    stroke: bool,
    stroke_w: f32,
    /// visibility:hidden 链值（显式 visible 复活）。
    vis_hidden: bool,
    /// `pointer-events` 链值（车道HJ；缺省 visiblePainted）。
    pe: PointerEvents,
    /// fill-rule 链值（缺省 NonZero）。
    fill_rule: FillRule,
    /// stroke-dasharray 链值（None = 实线；显式 none 已在 `*_set` 位
    /// 挡继承）。**% 图案值未归一**——命中按实线处理（登记近似）。
    dash: Option<&'a [f32]>,
    /// stroke-dashoffset 链值（图案起始相位）。
    dash_offset: f32,
    /// pathLength（元素属性，不继承；dash 图案按 实际长/pl 缩放——
    /// 渲染 stroke 消费同式）。
    path_length: Option<f32>,
}

struct HitState<'a> {
    doc: &'a Document,
    px: f32,
    py: f32,
    vw: f32,
    vh: f32,
    /// 命中裁切链（设备空间；进入容器/带裁元素时 push，返回前 pop）。
    clips: Vec<ClipZone>,
    best: Option<NodeId>,
}

/// `pointer-events` 域表：内部（fill/内容）域在 pe 取某值时是否激活。
/// `fill_paintable` = 链上 fill 非 none（visiblePainted/painted 的 paint
/// 依赖）；visible* 值要求 visibility:visible（Chrome hj1 探针实证）。
fn pe_interior(pe: PointerEvents, fill_paintable: bool, vis_hidden: bool) -> bool {
    match pe {
        PointerEvents::None => false,
        PointerEvents::VisiblePainted => !vis_hidden && fill_paintable,
        PointerEvents::Painted => fill_paintable,
        PointerEvents::Fill => true,
        PointerEvents::VisibleFill => !vis_hidden,
        PointerEvents::All => true,
        PointerEvents::VisibleAll => !vis_hidden,
        PointerEvents::Stroke | PointerEvents::VisibleStroke => false,
    }
}

/// `pointer-events` 域表：描边带域（`stroke_paintable` = 链上 stroke 非 none）。
fn pe_edge(pe: PointerEvents, stroke_paintable: bool, vis_hidden: bool) -> bool {
    match pe {
        PointerEvents::None => false,
        PointerEvents::VisiblePainted => !vis_hidden && stroke_paintable,
        PointerEvents::Painted => stroke_paintable,
        PointerEvents::Stroke => true,
        PointerEvents::VisibleStroke => !vis_hidden,
        PointerEvents::All => true,
        PointerEvents::VisibleAll => !vis_hidden,
        PointerEvents::Fill | PointerEvents::VisibleFill => false,
    }
}

/// 根/视口元素的单轴使用尺寸（与 builder::root_used_len 同式）。
fn used_len(set: bool, pct: bool, v: f32, viewport: f32) -> f32 {
    if !set {
        return viewport;
    }
    let used = if pct { v * viewport / 100.0 } else { v };
    if used < 0.0 {
        viewport
    } else {
        used
    }
}

/// 文档序深度优先（= 绘制顺序）：命中「后者胜」即栈顶元素。
///
/// `owner`：use 影子 retarget（车道HJ）——影子树内命中的上报 id 换成
/// 最外层 use 元素（Chrome elementFromPoint 对 use 区域返回 use 本身，
/// hj2 探针实证）；非影子路径恒 `None`。
fn visit_children(
    st: &mut HitState,
    node: NodeId,
    m: Mat2x3,
    inh: PaintCtx,
    depth: u32,
    owner: Option<NodeId>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let doc = st.doc;
    let mut cur = doc.tree.get(node).first_child;
    while cur != NULL {
        let next = doc.tree.get(cur).next_sibling;
        visit_one(st, cur, m, inh, depth, owner);
        cur = next;
    }
}

/// 点是否过全部裁切域（链与语义 = 渲染 clip 链式求交）。
fn in_clips(st: &HitState, x: f32, y: f32) -> bool {
    st.clips.iter().all(|z| match z {
        ClipZone::Empty => false,
        ClipZone::Rect(r) => x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3],
        ClipZone::Rings(rings, rule) => point_in_rings(rings, *rule, x, y),
    })
}

fn visit_one(
    st: &mut HitState,
    id: NodeId,
    m: Mat2x3,
    inh: PaintCtx,
    depth: u32,
    owner: Option<NodeId>,
) {
    let doc = st.doc;
    let s = &doc.shapes[id as usize];
    if s.display_none || s.suppressed {
        return;
    }
    let tag = doc.tree.get(id).tag;
    // visibility 链：显式声明复活/隐藏，未声明沿链。
    let vis_hidden = if s.vis_set {
        s.vis_hidden
    } else {
        inh.vis_hidden
    };
    // 描绘 presence 链：none 截断（不绘），Unset 沿链，其余可绘。
    let fill = match s.fill {
        FillSpec::None => false,
        FillSpec::Unset => inh.fill,
        _ => true,
    };
    let stroke = match s.stroke {
        StrokeSpec::None => false,
        StrokeSpec::Unset => inh.stroke,
        _ => true,
    };
    let stroke_w = s.stroke_width.unwrap_or(inh.stroke_w);
    // 车道HJ：pointer-events 链（自身合法声明胜，否则沿链）。
    let pe = if s.pointer_events_set {
        s.pointer_events
    } else {
        inh.pe
    };
    // 车道JD：fill-rule / dash 图案链（渲染 walk 继承同语义——自身
    // `*_set` 位置胜；dash 显式 none 的 `*_set`+None 挡住链上图案）。
    let fill_rule = if s.fill_rule_set {
        s.fill_rule
    } else {
        inh.fill_rule
    };
    let dash = if s.dash_set {
        s.dash.as_deref()
    } else {
        inh.dash
    };
    let dash_offset = if s.dash_offset_set {
        s.dash_offset
    } else {
        inh.dash_offset
    };
    let ctx = PaintCtx {
        fill,
        stroke,
        stroke_w,
        vis_hidden,
        pe,
        fill_rule,
        dash,
        dash_offset,
        path_length: s.path_length,
    };
    // 自身变换叠加（后续 children 与自身几何同空间）。
    let m = match &s.transform {
        Some(t) => m.mul(&parse_transform(t)),
        None => m,
    };
    // 非绘制容器/定义：不命中，按容器语义处理子树。
    let is_container = matches!(
        tag,
        Tag::Svg | Tag::G | Tag::A | Tag::Switch | Tag::Unknown | Tag::Tspan | Tag::TextPath
    );
    // 定义族：整子树不命中（内容只经引用实例化——渲染同口径）。
    let is_def = matches!(
        tag,
        Tag::Defs
            | Tag::Symbol
            | Tag::LinearGradient
            | Tag::RadialGradient
            | Tag::ClipPath
            | Tag::Mask
            | Tag::Pattern
            | Tag::Marker
            | Tag::Filter
            | Tag::FePrim
    );
    if is_def {
        return;
    }
    // use：影子树展开（车道HJ）。几何按实例化解析（x/y 平移；目标
    // symbol/svg 建视口），命中 retarget 到 use 本身。use 自身子元素
    // 不命中（渲染同口径）。
    if tag == Tag::Use {
        visit_use(st, id, s, m, ctx, depth, owner);
        return;
    }
    // 裁切链 push（车道HJ）：容器 clip-path 作用整个子树；形状只作用
    // 自身（子元素仅 animate 等不命中节点）。push 后点已出域 → 整子树
    // 剪枝（渲染上点处无该子树内容）。
    let clip_pushed = push_clip_zone(st, id, s, m, is_container);
    if clip_pushed && !in_clips(st, st.px, st.py) {
        st.clips.pop();
        return;
    }
    if is_container {
        // 嵌套 svg：新视口（x/y 平移 + viewBox 映射）+ 视口 AABB 裁切
        //（overflow:hidden 缺省——渲染 viewport_clip 同式）。
        if tag == Tag::Svg && id != doc.tree.root() {
            // 车道AK2：viewBox 零宽/高 → 整体不渲染（命中同样全裁）。
            let visible = match s.viewbox {
                Some((_, _, vbw, vbh)) => vbw > 0.0 && vbh > 0.0,
                None => true,
            };
            if visible {
                let w = if s.w > 0.0 { s.w } else { st.vw };
                let h = if s.h > 0.0 { s.h } else { st.vh };
                let mapped = match s.viewbox {
                    Some(vb) => viewport_map(w, h, vb, &s.par),
                    None => Mat2x3::IDENTITY,
                };
                let m_pre = m.mul(&Mat2x3::translation(s.x, s.y));
                let mut vp_pushed = false;
                if !s.overflow_visible {
                    st.clips.push(ClipZone::Rect(mat_aabb(m_pre, w, h)));
                    if in_clips(st, st.px, st.py) {
                        vp_pushed = true;
                    } else {
                        st.clips.pop();
                    }
                }
                if vp_pushed || s.overflow_visible {
                    visit_children(st, id, m_pre.mul(&mapped), ctx, depth + 1, owner);
                }
                if vp_pushed {
                    st.clips.pop();
                }
            }
        } else {
            visit_children(st, id, m, ctx, depth + 1, owner);
        }
        if clip_pushed {
            st.clips.pop();
        }
        return;
    }
    if in_clips(st, st.px, st.py) && hits_shape(st, id, s, m, ctx) {
        st.best = Some(owner.unwrap_or(id)); // 文档序后者胜 = 栈顶
    }
    // 形状也可能带子元素（animate 等——Unknown 叶子天然跳过）。
    visit_children(st, id, m, ctx, depth + 1, owner);
    if clip_pushed {
        st.clips.pop();
    }
}

/// use 影子树命中展开（车道HJ）。目标隐藏口径与渲染一致（display:none /
/// 外命名空间抑制 / 目标自身 visibility:hidden 不实例化——克隆保留自身
/// 计算值）。symbol/svg 目标走视口实例（三级回退 w/h + viewBox 映射 +
/// AABB 裁切）；其余元素按实例身份递归命中，上报 id retarget 到最外层
/// use（`owner` 已 Some 时保持——嵌套 use 最外 host 胜）。
fn visit_use(
    st: &mut HitState,
    id: NodeId,
    s: &ShapeAttrs,
    m: Mat2x3,
    ctx: PaintCtx,
    depth: u32,
    owner: Option<NodeId>,
) {
    let doc = st.doc;
    let my_owner = Some(owner.unwrap_or(id));
    if doc.cyclic_uses.contains(&id) {
        return; // 实例化环：解析期已计告警，命中同样跳过
    }
    let Some(href) = &s.href else {
        return;
    };
    let Some(target) = resolve_href(&doc.ids, href) else {
        return; // 未知引用：渲染不实例化，命中同样不命中
    };
    let ts = &doc.shapes[target as usize];
    if ts.display_none || ts.suppressed || (ts.vis_set && ts.vis_hidden) {
        return;
    }
    // use 的 transform 属性已在 visit_one 并入 m；x/y 等效追加 translate
    let m_inst = m.mul(&Mat2x3::translation(s.x, s.y));
    let ttag = doc.tree.get(target).tag;
    if ttag == Tag::Symbol || ttag == Tag::Svg {
        // 视口实例：use w/h 覆盖 → 目标自身 w/h → 引用处视口 100%
        //（AK2 三级回退同口径；hit 层百分比基准用根视口——登记近似）。
        let w = if s.w > 0.0 {
            s.w
        } else if ts.w_set && ts.w > 0.0 {
            ts.w
        } else {
            st.vw
        };
        let h = if s.h > 0.0 {
            s.h
        } else if ts.h_set && ts.h > 0.0 {
            ts.h
        } else {
            st.vh
        };
        // 车道AK2：viewBox 零宽/高 → 实例不渲染。
        if let Some((_, _, vbw, vbh)) = ts.viewbox {
            if vbw <= 0.0 || vbh <= 0.0 {
                return;
            }
        }
        // 车道CS：目标自身 x/y 是实例视口原点，use 的 x/y 与之相加。
        let m_inst_pre = m_inst.mul(&Mat2x3::translation(ts.x, ts.y));
        let inner = match ts.viewbox {
            Some(vb) => viewport_map(w, h, vb, &ts.par),
            None => Mat2x3::IDENTITY,
        };
        let pushed = if ts.overflow_visible {
            false
        } else {
            st.clips.push(ClipZone::Rect(mat_aabb(m_inst_pre, w, h)));
            if !in_clips(st, st.px, st.py) {
                st.clips.pop();
                return;
            }
            true
        };
        visit_children(st, target, m_inst_pre.mul(&inner), ctx, depth + 1, my_owner);
        if pushed {
            st.clips.pop();
        }
    } else {
        visit_one(st, target, m_inst, ctx, depth + 1, my_owner);
    }
}

/// clip-path 引用 → 设备空间裁切域 push（车道HJ）。渲染链同语义：
/// 未知/不可解析引用宽容不裁；无几何定义压 Empty（全裁）；oBB 单位
/// 锚定盒——容器 None（退化 userSpaceOnUse，渲染同口径），形状用元素
/// 矩形四角映射（rect 精确；其余形状近似——登记）。
#[cfg(feature = "clip-path")]
fn push_clip_zone(
    st: &mut HitState,
    _id: NodeId,
    s: &ShapeAttrs,
    m: Mat2x3,
    is_container: bool,
) -> bool {
    let Some(cid) = &s.clip_path else {
        return false;
    };
    let Some(def) = st.doc.clips.get(cid) else {
        return false; // 未知引用：渲染宽容不裁，命中同口径
    };
    let bbox: Option<[f32; 4]> = if def.units_object && !is_container {
        Some(rect_aabb(m, s.x, s.y, s.w, s.h))
    } else {
        None
    };
    let mut base = match bbox {
        Some(bb) if bb[2] > bb[0] && bb[3] > bb[1] => {
            Mat2x3::translation(bb[0], bb[1]).mul(&Mat2x3::scaling(bb[2] - bb[0], bb[3] - bb[1]))
        }
        _ => m, // oBB 无有效 bbox：退化为 userSpaceOnUse（渲染同口径）
    };
    if let Some(t) = &st.doc.shapes[def.node as usize].transform {
        base = base.mul(&parse_transform(t));
    }
    let mut rings: DevRings = Vec::new();
    let mut rule: Option<FillRule> = None;
    let mut c = st.doc.tree.get(def.node).first_child;
    while c != NULL {
        if let Some((sub, r)) = clip_child_rings(st.doc, c, base) {
            if rule.is_none() {
                rule = Some(r);
            }
            rings.extend(sub);
        }
        c = st.doc.tree.get(c).next_sibling;
    }
    match rule {
        Some(rule) if !rings.is_empty() => {
            st.clips.push(ClipZone::Rings(rings, rule));
        }
        _ => st.clips.push(ClipZone::Empty), // 无几何：引用元素整体不描绘
    }
    true
}

/// 非 clip-path 构建下无 clipPath 域（能力位降级：命中按未裁几何——
/// 与渲染「门关不裁」同口径）。
#[cfg(not(feature = "clip-path"))]
fn push_clip_zone(
    _st: &mut HitState,
    _id: NodeId,
    _s: &ShapeAttrs,
    _m: Mat2x3,
    _is_container: bool,
) -> bool {
    false
}

/// 矩阵把 (0,0)-(w,h) 四角映射后的 AABB（渲染 viewport_clip 同式：
/// 旋转矩形的包围盒必须全部四角参与）。
fn mat_aabb(m: Mat2x3, w: f32, h: f32) -> [f32; 4] {
    rect_aabb(m, 0.0, 0.0, w, h)
}

/// 矩阵把 (x,y)-(x+w,y+h) 四角映射后的 AABB。
fn rect_aabb(m: Mat2x3, x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for (px, py) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
        let (qx, qy) = m.apply(px, py);
        x0 = x0.min(qx);
        y0 = y0.min(qy);
        x1 = x1.max(qx);
        y1 = y1.max(qy);
    }
    [x0, y0, x1, y1]
}

/// 单元素几何命中（元素用户空间）。车道HJ：paint 语义按 `pointer-events`
/// 域表（pe_interior/pe_edge）判定域激活——visiblePainted 缺省口径与旧
/// 实现等价（fill 可绘 → 内部域、stroke 可绘 → 描边域、visibility 纳入
/// visible* 值要求）。
fn hits_shape(
    st: &HitState,
    id: NodeId,
    s: &crate::builder::ShapeAttrs,
    m: Mat2x3,
    c: PaintCtx,
) -> bool {
    let Some(inv) = m.inverse() else {
        return false; // 奇异变换（scale(0) 等）：几何退化不可命中
    };
    let (x, y) = inv.apply(st.px, st.py);
    let hw = (c.stroke_w * 0.5).max(0.0);
    let stroke_hit = pe_edge(c.pe, c.stroke, c.vis_hidden) && hw > 0.0;
    match st.doc.tree.get(id).tag {
        Tag::Rect => {
            if !(s.w > 0.0 && s.h > 0.0) {
                return false; // 零尺寸禁渲染（渲染同口径）
            }
            // SVG2 圆角：rx/ry 各缺省取对方、钳半轴（渲染 round_rect_geom
            // 同式）；无圆角走直角盒 SDF（与旧实现逐位等价）。
            let erx = if s.rx > 0.0 { s.rx } else { s.ry };
            let ery = if s.ry > 0.0 { s.ry } else { s.rx };
            let cx = s.x + s.w * 0.5;
            let cy = s.y + s.h * 0.5;
            let interior = pe_interior(c.pe, c.fill, c.vis_hidden);
            if erx > 0.0 && ery > 0.0 {
                let rx = erx.min(s.w * 0.5);
                let ry = ery.min(s.h * 0.5);
                // 归一化「圆角椭圆」距离（描边带按短半轴折算——与 ellipse
                // 臂同口径的近似）。
                let qx = ((x - cx).abs() - (s.w * 0.5 - rx)) / rx;
                let qy = ((y - cy).abs() - (s.h * 0.5 - ry)) / ry;
                if qx <= 0.0 && qy <= 0.0 {
                    interior
                } else {
                    let nx = qx.max(0.0);
                    let ny = qy.max(0.0);
                    let q = libm::sqrtf(nx * nx + ny * ny);
                    (interior && q <= 1.0) || (stroke_hit && (q - 1.0).abs() * rx.min(ry) <= hw)
                }
            } else {
                let sd = f32::max((x - cx).abs() - s.w * 0.5, (y - cy).abs() - s.h * 0.5);
                (interior && sd <= 0.0) || (stroke_hit && sd.abs() <= hw)
            }
        }
        Tag::Circle => {
            let interior = pe_interior(c.pe, c.fill, c.vis_hidden);
            let d = dist(x, y, s.cx, s.cy);
            (interior && d <= s.r) || (stroke_hit && (d - s.r).abs() <= hw)
        }
        Tag::Ellipse => {
            let interior = pe_interior(c.pe, c.fill, c.vis_hidden);
            let rx = if s.rx > 0.0 { s.rx } else { s.r };
            let ry = if s.ry > 0.0 { s.ry } else { s.r };
            if rx <= 0.0 || ry <= 0.0 {
                return false;
            }
            // 归一化空间的「圆度」距离（描边带按短半轴折算——近似）。
            let q = dist(x / rx, y / ry, 0.0, 0.0);
            (interior && q <= 1.0) || (stroke_hit && (q - 1.0).abs() * rx.min(ry) <= hw)
        }
        Tag::Line => {
            // line 无内部填充语义（SVG1.1）：只按描边带命中（车道JD：
            // dash 图案感知——Chrome 间隙 miss）。
            let pts = [(s.x1, s.y1), (s.x2, s.y2)];
            stroke_hit && stroke_band_hit(&pts, x, y, hw, c, s.cap)
        }
        Tag::Polyline | Tag::Polygon => {
            let interior = pe_interior(c.pe, c.fill, c.vis_hidden);
            let Some(pts) = parse_points(s.points.as_deref().unwrap_or("")) else {
                return false;
            };
            // fill：隐式闭合环 + fill-rule 链值（车道JD：Chrome 读规则）。
            if interior && rings_contain(&[&pts], c.fill_rule, x, y) {
                return true;
            }
            // stroke：dash 感知描边带；polygon 补 Z 闭合边（渲染描边
            // 同式——闭合边可命中）。
            if !stroke_hit {
                return false;
            }
            let is_polygon = st.doc.tree.get(id).tag == Tag::Polygon;
            let mut walk: Vec<(f32, f32)> = Vec::new();
            walk.extend_from_slice(&pts);
            if is_polygon && pts.len() >= 2 && pts[0] != pts[pts.len() - 1] {
                walk.push(pts[0]);
            }
            stroke_band_hit(&walk, x, y, hw, c, s.cap)
        }
        Tag::Path => {
            let interior = pe_interior(c.pe, c.fill, c.vis_hidden);
            let Some(d) = s.d.as_deref() else {
                return false;
            };
            // 用户空间平化（与 animateMotion 轨迹同口径）；d_cmds 动画
            // 帧不参与（命中按基准几何——登记近似）。
            let geom = path::flatten_d(d, Mat2x3::IDENTITY);
            // fill：全部子路径**跨环**求值 + fill-rule 链值（车道JD：
            // nonzero 绕数与跨环奇偶——旧实现逐子环独立奇偶，洞语义缺失）。
            if interior {
                let rings: Vec<&[(f32, f32)]> =
                    geom.subs.iter().map(|sp| sp.pts.as_slice()).collect();
                if rings_contain(&rings, c.fill_rule, x, y) {
                    return true;
                }
            }
            // stroke：dash 感知描边带（闭合子路径补 Z 边——渲染同式）。
            if stroke_hit {
                for sub in &geom.subs {
                    let mut walk: Vec<(f32, f32)> = Vec::new();
                    walk.extend_from_slice(&sub.pts);
                    if sub.closed && sub.pts.len() >= 2 && sub.pts[0] != sub.pts[sub.pts.len() - 1]
                    {
                        walk.push(sub.pts[0]);
                    }
                    if stroke_band_hit(&walk, x, y, hw, c, s.cap) {
                        return true;
                    }
                }
            }
            false
        }
        Tag::Image => {
            // 元素矩形命中。**与 Chrome 一致**（hj4 探针：meet letterbox
            // 空白区命中、slice 溢出区不命中——非近似）。image 是替换
            // 元素：内部域不依赖 fill 属性（内容即描绘）。
            let interior = pe_interior(c.pe, true, c.vis_hidden);
            interior && x >= s.x && x <= s.x + s.w && y >= s.y && y <= s.y + s.h
        }
        Tag::Text => hits_text(st, id, s, x, y, c),
        _ => false,
    }
}

/// 文本命中：advance 近似盒（0.6em/字符，锚点平移）。车道HJ 多段：
/// text 首段 + 子树内**显式 x/y 的 tspan 段**各开独立近似盒（多行文本
/// 常见形态——每行一个 tspan 定位）；无显式 x/y 的连续段与 dx/dy 累计
/// 忽略（登记近似）。命中粒度恒 text 元素（Chrome 返回 tspan——登记）。
fn hits_text(st: &HitState, id: NodeId, s: &ShapeAttrs, x: f32, y: f32, c: PaintCtx) -> bool {
    // 内部域资格：text 的字符内容 = 描绘（fill 可绘时 visiblePainted 命中；
    // pe=fill/all 不看 fill）。
    if !pe_interior(c.pe, c.fill, c.vis_hidden) {
        return false;
    }
    let fs = s.font_size.unwrap_or(16.0);
    let anchor = s.text_anchor.unwrap_or(TextAnchor::Start);
    // 首段：text 直接文本（段 0；无子元素时即全部内容）。
    let first: String = s.text_segs.first().map_or(String::new(), |t| t.clone());
    if text_seg_hit(
        &first,
        s.text_x.unwrap_or(0.0),
        s.text_y.unwrap_or(0.0),
        fs,
        anchor,
        x,
        y,
    ) {
        return true;
    }
    // 显式 x/y 的 tspan 段（子树递归收集）。
    text_tspan_hits(st, id, fs, anchor, x, y)
}

/// 递归收集 text 子树内显式定位 tspan 的近似段盒。
fn text_tspan_hits(
    st: &HitState,
    node: NodeId,
    fs: f32,
    anchor: TextAnchor,
    x: f32,
    y: f32,
) -> bool {
    let doc = st.doc;
    let mut cur = doc.tree.get(node).first_child;
    while cur != NULL {
        let next = doc.tree.get(cur).next_sibling;
        let tag = doc.tree.get(cur).tag;
        let ts = &doc.shapes[cur as usize];
        if tag == Tag::Tspan {
            let tfs = ts.font_size.unwrap_or(fs);
            let tanchor = ts.text_anchor.unwrap_or(anchor);
            if let (Some(tx), Some(ty)) = (ts.text_x, ts.text_y) {
                let seg: String = ts.text_segs.first().map_or(String::new(), |t| t.clone());
                if text_seg_hit(&seg, tx, ty, tfs, tanchor, x, y) {
                    return true;
                }
            }
            if text_tspan_hits(st, cur, tfs, tanchor, x, y) {
                return true;
            }
        }
        cur = next;
    }
    false
}

/// 单段近似盒命中：advance 宽 0.6em/字符 × font-size，锚点平移；
/// 基线上方 ~0.75em、下方 ~0.25em。
fn text_seg_hit(text: &str, tx: f32, ty: f32, fs: f32, anchor: TextAnchor, x: f32, y: f32) -> bool {
    if text.is_empty() {
        return false;
    }
    let n = text.chars().count() as f32;
    let w = n * 0.6 * fs;
    let x0 = match anchor {
        TextAnchor::Start => tx,
        TextAnchor::Middle => tx - w * 0.5,
        TextAnchor::End => tx - w,
    };
    x >= x0 && x <= x0 + w && y >= ty - 0.75 * fs && y <= ty + 0.25 * fs
}

/// 描边带命中（车道JD）：几何距离带定位最近弧长，dash 链值存在时
/// 再判相位（Chrome jd3 实证：间隙中心 miss/实段中心 hit）。无 dash
/// = 逐段距离带（与旧实现等价）。`pts` 为描边行走点列（闭合形态由
/// 调用方补 Z 边）。
fn stroke_band_hit(pts: &[(f32, f32)], x: f32, y: f32, hw: f32, c: PaintCtx, cap: Cap) -> bool {
    if pts.len() < 2 || hw <= 0.0 {
        return false;
    }
    let mut best_d2 = f32::MAX;
    let mut best_s = 0.0f32;
    let mut total = 0.0f32;
    for w in pts.windows(2) {
        let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
        let len2 = dx * dx + dy * dy;
        let (t, seg_len) = if len2 > f32::EPSILON {
            (
                (((x - w[0].0) * dx + (y - w[0].1) * dy) / len2).clamp(0.0, 1.0),
                libm::sqrtf(len2),
            )
        } else {
            (0.0, 0.0)
        };
        let ddx = x - (w[0].0 + t * dx);
        let ddy = y - (w[0].1 + t * dy);
        let d2 = ddx * ddx + ddy * ddy;
        if d2 < best_d2 {
            best_d2 = d2;
            best_s = total + t * seg_len;
        }
        total += seg_len;
    }
    if libm::sqrtf(best_d2) > hw {
        return false;
    }
    match c.dash {
        Some(pat) if pat.len() >= 2 => dash_on_at(
            pat,
            c.dash_offset,
            path_length_k(c.path_length, total),
            best_s,
            total,
            hw,
            matches!(cap, Cap::Round | Cap::Square),
        ),
        _ => true, // 实线（含 % 图案——登记近似）
    }
}

/// pathLength 归一因子（渲染 dash 消费同式：k = 实际总长/pl；pl 无效
/// 或总长为零 = 1 不缩放）。
fn path_length_k(pl: Option<f32>, actual: f32) -> f32 {
    match pl {
        Some(pl) if pl.is_finite() && pl > 0.0 && actual > 0.0 => actual / pl,
        _ => 1.0,
    }
}

/// 弧长 `s` 处是否落在 dash 图案的亮段/点（车道JD）。相位推进与段
/// 游标**同回卷**（path.rs `stroke_dashed` 同一不变量）；图案与相位
/// 按 k（pathLength 归一）缩放。亮段端帽沿弧长延伸 ±hw（Round/Square
/// ——渲染同式；Butt 不延伸）；点模式（全部 on 段为零）零长亮段按
/// 点心圆命中；混合图案零长亮段不命中（渲染一致）。全零图案 = 什么
/// 都不画 = 恒 miss。
fn dash_on_at(
    pat: &[f32],
    offset: f32,
    k: f32,
    s: f32,
    total_len: f32,
    hw: f32,
    cap_ext: bool,
) -> bool {
    let period = pat.iter().sum::<f32>() * k;
    if period <= 0.0 || period.is_nan() {
        return false; // 全零/退化图案（渲染端 NaN 短路同效：无亮段）
    }
    let dot_mode = pat.iter().step_by(2).all(|&v| v == 0.0);
    let mut phase = (offset * k) % period;
    if phase < 0.0 {
        phase += period;
    }
    // 定位初始相位所在段（stroke_dashed 同一跳过口径：非零段「段尾
    // <= 相位」才跳、零长段在相位之前才跳——恰落相位的零亮段保留，
    // 图案首元素为 0 的点线在 s=0 也命中）。
    let mut i = 0usize;
    let mut seg_start = 0.0f32;
    while i + 1 < pat.len()
        && ((pat[i] * k > 0.0 && seg_start + pat[i] * k <= phase)
            || (pat[i] == 0.0 && seg_start < phase))
    {
        seg_start += pat[i] * k;
        i += 1;
    }
    let mut s_pos = 0.0f32;
    let mut iters = 0usize;
    while s_pos < total_len - 1e-3 && iters < 100_000 {
        iters += 1;
        let seg_end = seg_start + pat[i] * k;
        let remain = (seg_end - phase).max(0.0);
        let take = remain.min(total_len - s_pos);
        if i % 2 == 0 {
            if take > 1e-3 {
                let ext = if cap_ext { hw } else { 0.0 };
                if s >= s_pos - ext && s <= s_pos + take + ext {
                    return true;
                }
            } else if dot_mode && cap_ext && (s - s_pos).abs() <= hw {
                return true; // 点模式零长亮段：点心 s_pos、点半径 hw
            }
        }
        s_pos += take;
        phase += take;
        if phase >= period {
            // 吃满本周期剩余：相位与段游标一起回卷（防失配）
            phase -= period;
            i = 0;
            seg_start = 0.0;
        } else {
            seg_start = seg_end;
            i += 1;
            if i == pat.len() {
                i = 0;
                seg_start = 0.0;
            }
        }
    }
    false
}

/// 单环奇偶包含（隐式闭合；车道JD 后仅作 EvenOdd 规则臂——规则值
/// 由 fill-rule 链解析，不再恒奇偶）。
fn point_in_ring(pts: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let n = pts.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) {
            let t = (y - yi) / (yj - yi);
            let x_cross = xi + t * (xj - xi);
            if x < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// 环集 + 规则的点包含（切片形态——形状 fill 臂用；跨环求值：EvenOdd
/// = 全部环奇偶计数、NonZero = 绕数和。车道JD：Chrome 实证五角星
/// nonzero 中心命中/evenodd 孔不命中）。
fn rings_contain(rings: &[&[(f32, f32)]], rule: FillRule, x: f32, y: f32) -> bool {
    match rule {
        FillRule::EvenOdd => rings.iter().filter(|r| point_in_ring(r, x, y)).count() % 2 == 1,
        FillRule::NonZero => rings.iter().map(|r| ring_winding(r, x, y)).sum::<i32>() != 0,
    }
}

/// 环集 + 规则的点包含（车道HJ：clipPath 烘焙消费，owned Vec 形态）。
fn point_in_rings(rings: &[Vec<(f32, f32)>], rule: FillRule, x: f32, y: f32) -> bool {
    match rule {
        FillRule::EvenOdd => rings.iter().filter(|r| point_in_ring(r, x, y)).count() % 2 == 1,
        FillRule::NonZero => rings.iter().map(|r| ring_winding(r, x, y)).sum::<i32>() != 0,
    }
}

/// 单环环绕数（射线法：右向射线；上穿 +1 / 下穿 −1 的有向交点计数）。
fn ring_winding(pts: &[(f32, f32)], x: f32, y: f32) -> i32 {
    let mut wn = 0i32;
    let n = pts.len();
    let mut j = n - 1;
    for i in 0..n {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[j];
        if y1 <= y {
            if y2 > y {
                // 上穿边：点在边左侧（叉积 > 0）则环绕 +1
                if (x2 - x1) * (y - y1) - (x - x1) * (y2 - y1) > 0.0 {
                    wn += 1;
                }
            }
        } else if y2 <= y {
            // 下穿边：点在边右侧（叉积 < 0）则环绕 −1
            if (x2 - x1) * (y - y1) - (x - x1) * (y2 - y1) < 0.0 {
                wn -= 1;
            }
        }
        j = i;
    }
    wn
}

fn dist(x: f32, y: f32, px: f32, py: f32) -> f32 {
    let dx = x - px;
    let dy = y - py;
    libm::sqrtf(dx * dx + dy * dy)
}

/// `points` 属性解析（f32 白名单解析——AF2 纪律：新增数值解析一律
/// fnum::parse_f32，不进 dec2flt 链）。奇数尾数截断，成对不足丢弃。
fn parse_points(s: &str) -> Option<Vec<(f32, f32)>> {
    let mut vals = Vec::new();
    for tok in s.split(|c: char| c.is_ascii_whitespace() || c == ',') {
        if tok.is_empty() {
            continue;
        }
        vals.push(parse_f32(tok)?);
    }
    if vals.len() < 4 || vals.len() % 2 != 0 {
        return None;
    }
    Some(
        vals.chunks_exact(2)
            .map(|p| (p[0], p[1]))
            .collect::<Vec<_>>(),
    )
}

// ---------------------------------------------------------------------------
// 2. 事件派发（SMIL 事件基 begin/end 的宿主时刻注入）
// ---------------------------------------------------------------------------

/// 把一次事件注入文档时间线（**精确目标**，不冒泡）。
///
/// - `event`：事件名（SMIL eventbase 的事件部分，如 `click`/`mouseover`，
///   区分大小写——与 `anim::parse_begin_item` 收集的原文精确匹配；
///   SVG1.1 UI 事件族全量可用，引擎层无白名单）。
/// - `target`：事件目标元素（DOM 语义的 `event.target`）。
/// - `t_ms`：事件时刻（文档时间轴毫秒；非有限忽略）。
///
/// 返回激活数：发生 begin 实例追加的绑定数 + 发生有效 end 裁断的绑定数
/// （0 = 无绑定关心此事件——目标不匹配或文档无事件基动画）。
///
/// 语义（SMIL Timing §3.5 eventbase）：
/// - begin：`begin="click"`（目标 = 动画的目标元素自身）或
///   `begin="btn.click"`（目标 = #btn）匹配时，把「事件时刻+偏移」
///   （`begin="btn.click+2s"`，车道HK；偏移可为负）追加为 begin 实例
///   并复位 `end` 到静态值（新活动区间的 end 列表重新求值）；此后
///   restart/dur/repeat/repeatDur/fill 全部复用 `Timing::state_at`。
///   多个 begin 项匹配同一事件（`begin="click;click+2s"`）= 每项一个
///   实例（SMIL event-value 列表语义）。
/// - end：`end="mouseout"` 匹配且当前（事件时刻+偏移处）恰处 Active
///   （`state_at(t)`）时把 `end` 裁到该时刻（freeze 冻结切点值 /
///   remove 复原）；Inactive 或已 Frozen（fill 冻结态）的区间忽略。
///   多个 end 项匹配时取**最早**偏移（SMIL 区间 end = 最早 end 实例）。
pub fn dispatch_event(doc: &mut Document, event: &str, target: NodeId, t_ms: f64) -> u32 {
    if !t_ms.is_finite() || target == NULL {
        return 0;
    }
    let t = t_ms / 1000.0;
    let mut activated = 0u32;
    for i in 0..doc.anims.len() {
        // 匹配阶段（不可变借用：eventbase 目标 id 反查需要 doc.ids）。
        // begin = 全部匹配项的偏移列表；end = 最早匹配偏移。
        let mut begin_offs: Vec<f64> = Vec::new();
        let mut end_off: Option<f64> = None;
        {
            let b = &doc.anims[i];
            for (tid, ev, off) in &b.event_begins {
                if eventbase_matches(doc, b.target, tid.as_deref(), target, ev, event) {
                    begin_offs.push(*off);
                }
            }
            for (tid, ev, off) in &b.event_ends {
                if eventbase_matches(doc, b.target, tid.as_deref(), target, ev, event)
                    && end_off.map_or(true, |o| *off < o)
                {
                    end_off = Some(*off);
                }
            }
        }
        if begin_offs.is_empty() && end_off.is_none() {
            continue;
        }
        // 车道PJ13：to-only 动画激活瞬间捕获基值=同 target 其它 to-only
        // 动画的 to 末值（最近被动画驱动的目标值；只读扫描先行，避免借用
        // 冲突）。
        let mut to_only_capture: Option<f64> = None;
        {
            let b_info = &doc.anims[i];
            if b_info.to_only_num {
                for other in doc.anims.iter() {
                    if other.target != b_info.target || !other.to_only_num {
                        continue;
                    }
                    if let crate::builder::AnimPayload::Num(vv) = &other.payload {
                        if let Some(&v) = vv.last().and_then(|v| v.last()) {
                            to_only_capture = Some(v);
                            break;
                        }
                    }
                }
            }
        }
        let b = &mut doc.anims[i];
        if !begin_offs.is_empty() {
            if let Some(v) = to_only_capture {
                b.to_only_base_dyn.set(Some(v));
            }
            for off in begin_offs {
                b.timing.begin_instances.push(t + off);
            }
            b.timing
                .begin_instances
                .sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
            // Timing 不变量：首元素 == begin（升序回填）。
            b.timing.begin = b.timing.begin_instances[0];
            // 新活动区间：end 列表按静态值重新生效（此前 mouseout 裁断
            // 不再殃及新区间——SMIL 每区间独立 end 实例的近似）。
            b.end = b.end_base;
            activated += 1;
        }
        if let Some(off) = end_off {
            let te = t + off;
            if matches!(b.timing.state_at(te), ActiveState::Active { .. }) {
                b.end = Some(te);
                activated += 1;
            }
        }
    }
    // 车道NB：事件锚 syncbase 链清偿——被派发的动画若被
    // `id.begin±k`/`id.end`/`id.repeat(n)` 后继引用（构建期落
    // `Document::sync_pending`），派发后其区间时刻沿链传播
    // （Chrome 148 钉扎 n16/n17 实证语义）。
    if activated > 0 {
        crate::builder::propagate_event_sync(doc);
    }
    activated
}

/// 事件基匹配（SMIL eventbase）：`None` = 动画目标元素自身；`Some(id)`
/// = 指定 id 的元素。**不冒泡**——祖先链由 [`dispatch_event_bubble`]
/// 逐级派发（DOM 事件冒泡语义）。
fn eventbase_matches(
    doc: &Document,
    anim_target: NodeId,
    spec_target: Option<&str>,
    event_target: NodeId,
    ev: &str,
    event: &str,
) -> bool {
    if ev != event {
        return false;
    }
    match spec_target {
        None => anim_target == event_target,
        Some(id) => doc.ids.get(id).copied() == Some(event_target),
    }
}

/// 事件冒泡链派发：从 `target` 沿祖先链到根逐级 [`dispatch_event`]
/// （DOM `bubbles: true` 语义——事件基目标为命中元素**或其祖先**的
/// 绑定都激活；`begin="btn.click"` 在点击 btn 的子元素时同样生效）。
pub fn dispatch_event_bubble(doc: &mut Document, event: &str, target: NodeId, t_ms: f64) -> u32 {
    let mut chain = Vec::new();
    let mut cur = target;
    while cur != NULL {
        chain.push(cur);
        cur = doc.tree.get(cur).parent;
    }
    let mut total = 0u32;
    for node in chain {
        total += dispatch_event(doc, event, node, t_ms);
    }
    total
}

/// 点形式派发（CLI `--inject-event "click:120,80@500"` 通道）：
/// 命中检测取最顶元素后按冒泡链派发。未命中任何绘制元素时以根 svg
/// 为事件目标（Chrome 独立 SVG 文档的空白区点击落在根元素上）。
/// 返回 `(事件目标元素, 激活数)`。
pub fn dispatch_event_at(
    doc: &mut Document,
    vw: f32,
    vh: f32,
    event: &str,
    px: f32,
    py: f32,
    t_ms: f64,
) -> (NodeId, u32) {
    let leaf = hit_test(doc, vw, vh, px, py).unwrap_or(doc.tree.root());
    let n = dispatch_event_bubble(doc, event, leaf, t_ms);
    (leaf, n)
}

// ---------------------------------------------------------------------------
// 3. 指针序列宿主语义（车道HK：DOWN/MOVE/UP 三段式，Chrome 对齐）
// ---------------------------------------------------------------------------

/// 最近公共包含祖先（`common inclusive ancestor`）：`a`/`b` 各自到根的
/// 祖先链（含自身）的第一个交点。Chrome/Blink 真实输入语义：click 的
/// 目标 = pointerdown 目标与 pointerup 目标的最近公共包含祖先（同元素
/// 时即自身；指针移出后抬起时 click 落在共同祖先而非抬起元素——
/// [`pointer_up`] 的 click 派发语义）。任一为 NULL（非法）时返回另一者。
pub fn common_inclusive_ancestor(doc: &Document, a: NodeId, b: NodeId) -> NodeId {
    if a == NULL {
        return b;
    }
    if b == NULL {
        return a;
    }
    let mut chain: Vec<NodeId> = Vec::new();
    let mut cur = a;
    while cur != NULL {
        chain.push(cur);
        cur = doc.tree.get(cur).parent;
    }
    let mut cur = b;
    while cur != NULL {
        if chain.contains(&cur) {
            return cur;
        }
        cur = doc.tree.get(cur).parent;
    }
    doc.tree.root() // 不可达（树不变量），防御回根
}

/// 命中目标（设备点 → 最顶绘制元素；落空 = 根 svg，与
/// [`dispatch_event_at`] 同语义）。宿主 DOWN/UP 序列的取目标入口——
/// 与派发拆开，宿主可在「命中」与「派发」之间插入交互态重级联
/// （Android/CLI 的 `:active` 槽位全量重解析；NodeIds 跨同源重解析
/// 稳定，元素引用不失效）。
pub fn hit_target(doc: &Document, vw: f32, vh: f32, px: f32, py: f32) -> NodeId {
    hit_test(doc, vw, vh, px, py).unwrap_or(doc.tree.root())
}

/// 指针按下事件族派发（宿主 DOWN：`:active` 重级联**之后**调用——
/// 派发效果须落在重解析后的文档上）：mouseover 到达转换（上次指针
/// 目标 ≠ 本次命中时 mouseout 旧目标 + mouseover 新目标，Chrome 触点
/// 兼容序列 mouseover→mousedown）+ mousedown 冒泡。`mouse_target`
/// 就地维护（None = 序列起点/指针刚进入）。返回激活数合计。
pub fn dispatch_pointer_down_events(
    doc: &mut Document,
    leaf: NodeId,
    t_ms: f64,
    mouse_target: &mut Option<NodeId>,
) -> u32 {
    let mut n = 0u32;
    if *mouse_target != Some(leaf) {
        if let Some(old) = *mouse_target {
            n += dispatch_event_bubble(doc, "mouseout", old, t_ms);
        }
        n += dispatch_event_bubble(doc, "mouseover", leaf, t_ms);
        *mouse_target = Some(leaf);
    }
    n + dispatch_event_bubble(doc, "mousedown", leaf, t_ms)
}

/// 指针移动事件族派发（宿主 MOVE）：目标转换（mouseout 旧 + mouseover
/// 新）+ mousemove 冒泡（Chrome 语义：mousemove 恒派发到当前命中，
/// 与是否转换无关）。`mouse_target` 就地维护。返回激活数合计。
pub fn dispatch_pointer_move_events(
    doc: &mut Document,
    leaf: NodeId,
    t_ms: f64,
    mouse_target: &mut Option<NodeId>,
) -> u32 {
    let mut n = 0u32;
    if *mouse_target != Some(leaf) {
        if let Some(old) = *mouse_target {
            n += dispatch_event_bubble(doc, "mouseout", old, t_ms);
        }
        n += dispatch_event_bubble(doc, "mouseover", leaf, t_ms);
        *mouse_target = Some(leaf);
    }
    n + dispatch_event_bubble(doc, "mousemove", leaf, t_ms)
}

/// 指针抬起事件族派发（宿主 UP）：mouseup 冒泡到抬起目标 + click 派发
/// 到「按压目标 × 抬起目标」的最近公共包含祖先（Chrome 真实输入语义
/// ——同元素即自身；分裂 click 落共同祖先，[`common_inclusive_ancestor`]）
/// 并清除按压记录。返回激活数合计。
pub fn dispatch_pointer_up_events(
    doc: &mut Document,
    leaf: NodeId,
    t_ms: f64,
    pressed: &mut Option<NodeId>,
) -> u32 {
    let mut n = dispatch_event_bubble(doc, "mouseup", leaf, t_ms);
    if let Some(down) = pressed.take() {
        let target = common_inclusive_ancestor(doc, down, leaf);
        n += dispatch_event_bubble(doc, "click", target, t_ms);
    }
    n
}

// ---------------------------------------------------------------------------
// 4. 拖动中 :active 的 Chrome 语义（车道JD）
// ---------------------------------------------------------------------------

/// 拖动（按压保持）中一次 MOVE 的 `:active` 槽位更新判定。Chrome
/// jd4/jd6 探针实证（chrome-headless-shell 148，CDP buttons=1 拖动）：
/// 按压后指针滑出按压元素子树 → **整链清 :active**——不转移到指针下
/// 元素（drag@rb：rb 不激活）、滑回按压元素**不恢复**（drag-back@ra）、
/// 共同父组也不保持（jd6 兄弟位移全清）。清除是**单发**的：锁存
/// `cleared` 置位后到 UP 前恒 [`DragActive::Cleared`]（锁存位由宿主
/// 持有，DOWN 时置回 false）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragActive {
    /// 指针仍在按压元素子树（含自身）内：active 槽位保持，无需重级联。
    Keep,
    /// 已离开按压元素子树：清 active 槽位（首次返回时宿主重级联一次）。
    Cleared,
}

/// 拖动中 :active 语义更新（纯函数；`cleared` = 宿主持有的单发清除
/// 锁存）。命中落空（空白区）同「离开」处理。按压元素的后代边界
/// （按到父组、滑到子形状）探针未覆盖——按子树内保持处理（登记）。
pub fn drag_active_step(
    doc: &Document,
    vw: f32,
    vh: f32,
    px: f32,
    py: f32,
    pressed: NodeId,
    cleared: &mut bool,
) -> DragActive {
    if *cleared {
        return DragActive::Cleared;
    }
    let inside = match hit_test(doc, vw, vh, px, py) {
        Some(h) => subtree_contains(doc, h, pressed),
        // 命中落空（根盒内空白区）= 根（gg 语义：空白区 hover/点击
        // 落根）——按压目标为根时保持；按压形状元素时即「离开」。
        None => pressed == doc.tree.root(),
    };
    if inside {
        DragActive::Keep
    } else {
        *cleared = true;
        DragActive::Cleared
    }
}

/// `a` 是否为 `b` 自身或其后代（沿父链上行）。
fn subtree_contains(doc: &Document, a: NodeId, b: NodeId) -> bool {
    let mut cur = a;
    while cur != NULL {
        if cur == b {
            return true;
        }
        cur = doc.tree.get(cur).parent;
    }
    false
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::parse_document;
    use alloc::vec;

    fn parse(svg: &str) -> Document {
        parse_document(svg).unwrap_or_else(|e| panic!("parse 失败：{e:?}"))
    }

    const CLICK_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="80">
      <circle id="c1" cx="30" cy="30" r="20" fill="#2E86C1">
        <animate attributeName="fill" to="#E94560" begin="click" dur="0.5s" fill="freeze"/>
      </circle>
      <rect id="r1" x="60" y="10" width="30" height="30" fill="#16BFA6"/>
      <g transform="translate(70,60)">
        <circle id="c2" cx="0" cy="0" r="10" fill="#8E44AD">
          <animate attributeName="r" values="10;16;10" dur="1s" begin="c1.click" restart="always"/>
        </circle>
      </g>
    </svg>"##;

    // ---- 1. 事件基建档：+∞ 哨兵（静态语义 = 永不激活） ----

    #[test]
    fn event_only_begin_builds_binding_with_infinity_sentinel() {
        let doc = parse(CLICK_SVG);
        assert_eq!(doc.anims.len(), 2, "两条事件基动画都建绑定");
        for b in &doc.anims {
            assert_eq!(b.event_begins.len(), 1);
            assert!(b.timing.begin.is_infinite(), "哨兵 begin = +∞");
            assert!(b.timing.begin_instances.is_empty());
        }
        // 事件目标自身（None）与跨元素（Some("c1")）两种形态
        assert_eq!(
            doc.anims[0].event_begins[0],
            (None, String::from("click"), 0.0)
        );
        assert_eq!(
            doc.anims[1].event_begins[0],
            (Some(String::from("c1")), String::from("click"), 0.0)
        );
        // 未注入：任意 t 均 Inactive（静态渲染不受影响）
        assert_eq!(doc.anims[0].timing.state_at(0.0), ActiveState::Inactive);
        assert_eq!(doc.anims[0].timing.state_at(1e9), ActiveState::Inactive);
    }

    // ---- 2. 派发求值：begin 翻转 → 时间线推进 ----

    #[test]
    fn dispatch_activates_begin_and_timeline_advances() {
        let mut doc = parse(CLICK_SVG);
        let c1 = doc.ids["c1"];
        // 点击 #c1（直接 id 目标）：c1 自身 "click" + c2 的 "c1.click"
        // 都以 c1 为事件基（精确目标匹配，无冒泡）→ 双激活
        let n = dispatch_event(&mut doc, "click", c1, 500.0);
        assert_eq!(n, 2, "c1 自身 begin + c2 的 c1.begin 同时刻激活");
        let b = &doc.anims[0];
        // begin=0.5s、dur=0.5s、freeze：t=0.5 Active 起点、t=0.75 中点、t≥1.0 冻结
        assert_eq!(
            b.timing.state_at(0.5),
            ActiveState::Active {
                local_t: 0.0,
                iter: 0
            }
        );
        assert_eq!(
            b.timing.state_at(0.75),
            ActiveState::Active {
                local_t: 0.25,
                iter: 0
            }
        );
        assert_eq!(b.timing.state_at(1.0), ActiveState::Frozen { final_t: 0.5 });
        // c2 的 begin 也在 0.5s 入列（restart=always 双实例时取最新）
        assert_eq!(doc.anims[1].timing.begin_instances, vec![0.5]);
    }

    #[test]
    fn eventbase_by_id_and_bubble_chain() {
        let mut doc = parse(CLICK_SVG);
        let c1 = doc.ids["c1"];
        let root = doc.tree.root();
        // 事件目标 = #c1：精确匹配激活 c2 的 "c1.click"
        let n = dispatch_event(&mut doc, "click", c1, 0.0);
        assert_eq!(n, 2, "c1 自身 click + c2 的 c1.click 同时激活");
        // 冒泡：向根派发时父链上的精确匹配不再重复（root 上无事件基）
        let mut doc2 = parse(CLICK_SVG);
        let n2 = dispatch_event_bubble(&mut doc2, "click", c1, 0.0);
        assert_eq!(n2, 2);
        // 点击根（空白区）：无绑定以根为事件基
        let n3 = dispatch_event_bubble(&mut doc2, "click", root, 0.0);
        assert_eq!(n3, 0);
        // 事件名区分大小写
        let mut doc3 = parse(CLICK_SVG);
        assert_eq!(dispatch_event(&mut doc3, "Click", c1, 0.0), 0);
    }

    #[test]
    fn restart_always_reclick_restarts_timeline() {
        let mut doc = parse(CLICK_SVG);
        let c1 = doc.ids["c1"];
        // 第二条动画 restart="always"：两次点击 → 两个实例，取最新
        dispatch_event(&mut doc, "click", c1, 0.0);
        dispatch_event(&mut doc, "click", c1, 2000.0);
        let b = &doc.anims[1];
        assert_eq!(b.timing.begin_instances, vec![0.0, 2.0]);
        // t=2.5：最新实例（2.0）的 local 0.5（第一实例早被忽略）
        assert_eq!(
            b.timing.state_at(2.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            }
        );
        // t=0.5（两次点击之间）：第一实例 active
        assert_eq!(
            b.timing.state_at(0.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            }
        );
    }

    #[test]
    fn event_end_cuts_active_interval_only() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="rc" x="10" y="10" width="40" height="40" fill="#BDC3C7">
            <animate attributeName="fill" to="#2ECC71" dur="10s" begin="mouseover" end="mouseout"/>
          </rect>
        </svg>"##;
        let mut doc = parse(svg);
        let rc = doc.ids["rc"];
        // mouseout 先于 mouseover：无活动区间 → 忽略
        assert_eq!(dispatch_event(&mut doc, "mouseout", rc, 100.0), 0);
        // mouseover @0.5s：begin 实例入列
        assert_eq!(dispatch_event(&mut doc, "mouseover", rc, 500.0), 1);
        assert_eq!(doc.anims[0].end, None, "未 mouseout：静态 end（None）");
        // 活动期内 mouseout @1.2s：end 裁断（渲染期由 state_with_end
        // 在 t≥1.2 切断——freeze 冻切点 / remove 复原，既有语义）
        assert_eq!(dispatch_event(&mut doc, "mouseout", rc, 1200.0), 1);
        let b = &doc.anims[0];
        assert_eq!(b.end, Some(1.2), "end 裁到 mouseout 时刻");
        // 重新 mouseover：end 复位到静态 None，新区间不再被旧裁断殃及
        dispatch_event(&mut doc, "mouseover", rc, 3000.0);
        assert_eq!(doc.anims[0].end, None);
        assert_eq!(
            doc.anims[0].timing.state_at(3.5),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            }
        );
        // 已冻结区间（fill=freeze 用尽）上的 mouseout：不复活不复原
        let svg2 = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="rf" x="10" y="10" width="40" height="40" fill="#BDC3C7">
            <animate attributeName="fill" to="#2ECC71" dur="1s" begin="click" fill="freeze"/>
          </rect>
        </svg>"##;
        let mut doc2 = parse(svg2);
        let rf = doc2.ids["rf"];
        dispatch_event(&mut doc2, "click", rf, 0.0);
        dispatch_event(&mut doc2, "mouseout", rf, 5000.0);
        assert_eq!(
            doc2.anims[0].end, None,
            "Frozen 上的事件 end 忽略（fill 冻结不被复原）"
        );
    }

    // ---- 3. 命中检测 ----

    #[test]
    fn hit_test_basic_shapes_and_topmost() {
        let doc = parse(CLICK_SVG);
        // circle #c1 内部
        assert_eq!(hit_test(&doc, 100.0, 80.0, 30.0, 30.0), Some(doc.ids["c1"]));
        // rect #r1 内部（与 c1 不重叠）
        assert_eq!(hit_test(&doc, 100.0, 80.0, 75.0, 20.0), Some(doc.ids["r1"]));
        // 变换后 #c2（g translate(70,60) + 圆心 (0,0)）→ 设备 (70,60)
        assert_eq!(hit_test(&doc, 100.0, 80.0, 70.0, 60.0), Some(doc.ids["c2"]));
        // 空白区
        assert_eq!(hit_test(&doc, 100.0, 80.0, 95.0, 75.0), None);
        // fill="none" 的内部不可命中，描边带可命中
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
          <rect id="rn" x="5" y="5" width="30" height="30" fill="none" stroke="black" stroke-width="4"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 40.0, 40.0, 20.0, 20.0), None, "无填充内部");
        assert_eq!(
            hit_test(&doc, 40.0, 40.0, 7.0, 20.0),
            Some(doc.ids["rn"]),
            "描边带内"
        );
        assert_eq!(hit_test(&doc, 40.0, 40.0, 1.0, 20.0), None, "描边带外");
    }

    #[test]
    fn hit_test_display_none_skipped_and_document_order_topmost() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
          <rect id="r1" x="5" y="5" width="30" height="30" fill="red"/>
          <rect id="r2" x="10" y="10" width="20" height="20" fill="blue"/>
          <rect id="r3" x="12" y="12" width="16" height="16" fill="green" display="none"/>
        </svg>"##;
        let doc = parse(svg);
        // 文档序后者胜：r2 盖在 r1 上
        assert_eq!(hit_test(&doc, 40.0, 40.0, 15.0, 15.0), Some(doc.ids["r2"]));
        // r1 独占区
        assert_eq!(hit_test(&doc, 40.0, 40.0, 6.0, 6.0), Some(doc.ids["r1"]));
        // display:none 不命中：r2 范围内 r3 不可见
        assert_eq!(hit_test(&doc, 40.0, 40.0, 25.0, 25.0), Some(doc.ids["r2"]));
    }

    #[test]
    fn dispatch_event_at_hits_and_bubbles() {
        let mut doc = parse(CLICK_SVG);
        let (leaf, n) = dispatch_event_at(&mut doc, 100.0, 80.0, "click", 70.0, 60.0, 1000.0);
        assert_eq!(leaf, doc.ids["c2"], "点落在变换后的 c2 上");
        assert_eq!(n, 0, "c2 自身无 begin，事件基是 c1（不在冒泡链上）");
        // 点击 c1：自身 begin + 冒泡链无额外（c1.click 的目标必须恰为 c1——
        // 冒泡语义下点击 c1 本身 target=c1，精确匹配）
        let (_, n2) = dispatch_event_at(&mut doc, 100.0, 80.0, "click", 30.0, 30.0, 1000.0);
        assert_eq!(n2, 2);
        // 空白区 → 根 svg 为目标
        let (leaf3, n3) = dispatch_event_at(&mut doc, 100.0, 80.0, "click", 95.0, 75.0, 1000.0);
        assert_eq!(leaf3, doc.tree.root());
        assert_eq!(n3, 0);
    }

    #[test]
    fn hit_test_rejects_degenerate_input() {
        let doc = parse(CLICK_SVG);
        assert_eq!(hit_test(&doc, 100.0, 80.0, f32::NAN, 30.0), None);
        assert_eq!(hit_test(&doc, 100.0, 80.0, 30.0, f32::INFINITY), None);
        // 奇异变换子树不可命中（inverse → None）
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
          <g transform="scale(0)"><rect id="rz" x="0" y="0" width="20" height="20" fill="red"/></g>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 40.0, 40.0, 5.0, 5.0), None);
    }

    // ---- 4. 车道HK：事件基偏移 / 指针序列 / 事件族 ----

    /// `begin="btn.click+1.5s"`：点击 @0.5s → begin 实例 = 0.5+1.5 = 2.0s
    /// （时刻全部取二进制精确值，断言逐位）。
    #[test]
    fn eventbase_offset_begin_delays_instance() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="btn" x="10" y="10" width="40" height="40" fill="#888">
            <animate attributeName="fill" to="#E94560" begin="btn.click+1.5s" dur="0.5s" fill="freeze"/>
          </rect>
        </svg>"##;
        let mut doc = parse(svg);
        let btn = doc.ids["btn"];
        assert_eq!(
            doc.anims[0].event_begins[0],
            (Some(String::from("btn")), String::from("click"), 1.5)
        );
        assert_eq!(dispatch_event(&mut doc, "click", btn, 500.0), 1);
        assert_eq!(doc.anims[0].timing.begin_instances, vec![2.0]);
        // 偏移窗口内（t < 2.0）：Inactive（基态）
        assert_eq!(doc.anims[0].timing.state_at(1.75), ActiveState::Inactive);
        // t = 2.25：新区间 local 0.25
        assert_eq!(
            doc.anims[0].timing.state_at(2.25),
            ActiveState::Active {
                local_t: 0.25,
                iter: 0
            }
        );
        // t ≥ 2.5：freeze 冻结
        assert_eq!(
            doc.anims[0].timing.state_at(3.0),
            // final_t = 冻结的本地相位 = dur（非 begin 时刻）
            ActiveState::Frozen { final_t: 0.5 }
        );
    }

    /// 负偏移：`begin="click-0.5s"` → 实例 = 事件时刻 − 0.5s。
    #[test]
    fn eventbase_negative_offset_begin() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="rc" x="10" y="10" width="40" height="40" fill="#888">
            <animate attributeName="fill" to="#E94560" begin="click-0.5s" dur="4s" fill="freeze"/>
          </rect>
        </svg>"##;
        let mut doc = parse(svg);
        let rc = doc.ids["rc"];
        assert_eq!(dispatch_event(&mut doc, "click", rc, 1000.0), 1);
        assert_eq!(doc.anims[0].timing.begin_instances, vec![0.5]);
        assert_eq!(
            doc.anims[0].timing.state_at(1.0),
            ActiveState::Active {
                local_t: 0.5,
                iter: 0
            }
        );
    }

    /// 事件基 end 带偏移：`end="mouseout+0.5s"` → 裁断时刻 = 事件+0.5s，
    /// 且 Active 判定也在该时刻：候选落活动期内才裁、落活动期外忽略
    /// （dur=1s fill=remove → mouseover@0.5 活动区间 [0.5, 1.5)）。
    #[test]
    fn eventbase_offset_end_cuts_later() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="rc" x="10" y="10" width="40" height="40" fill="#888">
            <animate attributeName="fill" to="#2ECC71" dur="1s" begin="mouseover" end="mouseout+0.5s"/>
          </rect>
        </svg>"##;
        let mut doc = parse(svg);
        let rc = doc.ids["rc"];
        dispatch_event(&mut doc, "mouseover", rc, 500.0);
        // mouseout @1.2s → 候选 = 1.7s：活动期已过（remove）→ 忽略
        assert_eq!(dispatch_event(&mut doc, "mouseout", rc, 1200.0), 0);
        assert_eq!(doc.anims[0].end, None);
        // mouseout @0.8s → 候选 = 1.3s：区间内 → 裁到 1.3
        assert_eq!(dispatch_event(&mut doc, "mouseout", rc, 800.0), 1);
        assert_eq!(doc.anims[0].end, Some(1.3));
    }

    /// 同一绑定多 begin 项匹配同一事件：`begin="click;click+2s"` →
    /// 一次派发产出两个实例（SMIL event-value 列表语义）。
    #[test]
    fn multi_matching_begin_items_each_get_instance() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="rc" x="10" y="10" width="40" height="40" fill="#888">
            <animate attributeName="fill" to="#E94560" begin="click;click+2s" dur="4s" fill="freeze"/>
          </rect>
        </svg>"##;
        let mut doc = parse(svg);
        let rc = doc.ids["rc"];
        assert_eq!(doc.anims[0].event_begins.len(), 2);
        assert_eq!(
            dispatch_event(&mut doc, "click", rc, 1000.0),
            1,
            "按绑定计 1"
        );
        assert_eq!(doc.anims[0].timing.begin_instances, vec![1.0, 3.0]);
    }

    /// 最近公共包含祖先：同元素 = 自身；兄弟 = 父；NULL 防御。
    #[test]
    fn common_inclusive_ancestor_cases() {
        let doc = parse(CLICK_SVG); // 根 → g(transform) → c2；根 → c1/r1
        let root = doc.tree.root();
        let c1 = doc.ids["c1"];
        let c2 = doc.ids["c2"];
        assert_eq!(common_inclusive_ancestor(&doc, c1, c1), c1);
        assert_eq!(common_inclusive_ancestor(&doc, c1, doc.ids["r1"]), root);
        assert_eq!(common_inclusive_ancestor(&doc, c2, root), root);
        assert_eq!(common_inclusive_ancestor(&doc, NULL, c1), c1);
    }

    /// 指针序列 DOWN/MOVE/UP：mouseover 到达 + mousedown；move 转换
    /// （mouseout 旧 + mouseover 新 + mousemove）；up = mouseup + click
    /// 落公共包含祖先（分裂 click 不丢——g 上 begin="click" 激活，
    /// A 上 begin="click" 不激活）。
    #[test]
    fn pointer_sequence_down_move_up_semantics() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60">
          <g id="wrap">
            <rect id="ra" x="5" y="5" width="50" height="50" fill="#888">
              <animate attributeName="fill" to="#E94560" begin="click" dur="4s" fill="freeze"/>
              <animate attributeName="x" to="200" begin="mouseover" dur="4s" fill="freeze"/>
            </rect>
            <rect id="rb" x="65" y="5" width="50" height="50" fill="#888"/>
            <animate attributeName="opacity" to="0.2" begin="click" dur="4s" fill="freeze"/>
          </g>
        </svg>"##;
        let doc = parse(svg);
        let ra = doc.ids["ra"];
        let rb = doc.ids["rb"];
        // begin="click" 的 g 内 animate：target = wrap（g）；绑定顺序 =
        // 文档序（ra.fill, ra.x, wrap.opacity）
        assert_eq!(doc.anims.len(), 3);
        let mut pressed: Option<NodeId>;
        let mut mouse_target: Option<NodeId> = None;
        let mut d = doc;
        // DOWN 在 ra：mouseover(ra) + mousedown(ra) → ra.x 动画激活
        let leaf = hit_target(&d, 120.0, 60.0, 30.0, 30.0);
        assert_eq!(leaf, ra);
        let n_down = dispatch_pointer_down_events(&mut d, leaf, 100.0, &mut mouse_target);
        assert_eq!(n_down, 1, "mouseover 激活 ra.x；mousedown 无绑定");
        assert_eq!(mouse_target, Some(ra));
        pressed = Some(leaf);
        // MOVE 到 rb：mouseout(ra) + mouseover(rb) + mousemove(rb)
        let leaf2 = hit_target(&d, 120.0, 60.0, 90.0, 30.0);
        assert_eq!(leaf2, rb);
        let n_move = dispatch_pointer_move_events(&mut d, leaf2, 200.0, &mut mouse_target);
        assert_eq!(n_move, 0, "转换事件均无绑定");
        assert_eq!(mouse_target, Some(rb));
        // UP 在 rb（分裂）：mouseup(rb) + click 落 wrap（公共祖先）
        // → wrap.opacity 激活、ra.click 不激活（Chrome 分裂 click 语义）
        let n_up = dispatch_pointer_up_events(&mut d, leaf2, 300.0, &mut pressed);
        assert_eq!(n_up, 1, "click 落 wrap 祖先绑定");
        assert_eq!(pressed, None, "按压记录消费清空");
        assert_eq!(
            d.anims[0].timing.begin_instances.len(),
            0,
            "ra.click 不激活"
        );
        assert_eq!(
            d.anims[2].timing.begin_instances,
            vec![0.3],
            "wrap.click 激活"
        );
        // 同元素 UP：click 落自身（并沿冒泡链再次命中 wrap 绑定——
        // Chrome 冒泡语义：ra 上的 click 一路冒到 g/root）
        let mut pressed2 = Some(ra);
        let n_up2 = dispatch_pointer_up_events(&mut d, ra, 400.0, &mut pressed2);
        assert_eq!(n_up2, 2, "ra.click 激活 + 冒泡再次命中 wrap.click");
        assert_eq!(
            d.anims[0].timing.begin_instances,
            vec![0.4],
            "同元素 click 落 ra"
        );
    }

    /// 落空点 = 根目标：DOWN 序列 mouseover/mousedown 以根为目标，
    /// 根上无绑定 → 零激活（帧零扰动）。
    #[test]
    fn pointer_sequence_on_blank_targets_root() {
        let mut doc = parse(CLICK_SVG);
        let root = doc.tree.root();
        let mut mouse_target: Option<NodeId> = None;
        let leaf = hit_target(&doc, 100.0, 80.0, 95.0, 75.0);
        assert_eq!(leaf, root);
        assert_eq!(
            dispatch_pointer_down_events(&mut doc, leaf, 0.0, &mut mouse_target),
            0
        );
        assert_eq!(mouse_target, Some(root));
        let mut pressed = Some(leaf);
        assert_eq!(
            dispatch_pointer_up_events(&mut doc, leaf, 100.0, &mut pressed),
            0
        );
    }

    /// 事件族直派（SVG1.1 UI 事件全族按名精确匹配，无白名单）：
    /// mousedown/mouseup/mousemove/focusin/focusout 各自激活对应绑定。
    #[test]
    fn ui_event_family_dispatch_by_name() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
          <rect id="rc" x="5" y="5" width="50" height="50" fill="#888">
            <animate attributeName="x" to="1" begin="mousedown" dur="1s" fill="freeze"/>
            <animate attributeName="y" to="1" begin="mouseup" dur="1s" fill="freeze"/>
            <animate attributeName="width" to="1" begin="mousemove" dur="1s" fill="freeze"/>
            <animate attributeName="height" to="1" begin="focusin" dur="1s" fill="freeze"/>
            <animate attributeName="rx" to="1" begin="focusout" dur="1s" fill="freeze"/>
            <animate attributeName="ry" to="1" begin="mouseout" dur="1s" fill="freeze"/>
          </rect>
        </svg>"##;
        let mut doc = parse(svg);
        let rc = doc.ids["rc"];
        assert_eq!(doc.anims.len(), 6);
        for (i, ev) in [
            "mousedown",
            "mouseup",
            "mousemove",
            "focusin",
            "focusout",
            "mouseout",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(
                dispatch_event(&mut doc, ev, rc, 100.0),
                1,
                "{ev} 应激活第 {i} 条绑定"
            );
            assert_eq!(doc.anims[i].timing.begin_instances, vec![0.1]);
        }
        // 未知事件名 / 大小写不匹配：零激活
        assert_eq!(dispatch_event(&mut doc, "mousehover", rc, 100.0), 0);
        assert_eq!(dispatch_event(&mut doc, "MouseDown", rc, 100.0), 0);
    }

    // ---- 4. 车道HJ：pointer-events 域表（Chrome hj1 探针口径） ----

    #[test]
    fn pointer_events_none_and_inheritance() {
        // pe=none：穿透（miss）；底层 bg 接住
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60">
          <rect id="bg" x="0" y="0" width="120" height="60" fill="yellow"/>
          <rect id="pn" x="10" y="10" width="40" height="40" fill="blue" pointer-events="none"/>
          <g id="gnone" pointer-events="none">
            <rect id="child" x="60" y="10" width="40" height="40" fill="green"/>
          </g>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 120.0, 60.0, 30.0, 30.0), Some(doc.ids["bg"]));
        // 继承：g 声明 none，子未声明 → 沿链穿透
        assert_eq!(hit_test(&doc, 120.0, 60.0, 80.0, 30.0), Some(doc.ids["bg"]));
    }

    #[test]
    fn pointer_events_fill_value_ignores_paint() {
        // pe=fill × fill=none：几何域命中（Chrome (225,30) → hit）
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="60">
          <rect id="bg" x="0" y="0" width="80" height="60" fill="yellow"/>
          <rect id="pf" x="10" y="10" width="40" height="40" fill="none" pointer-events="fill"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 80.0, 60.0, 30.0, 30.0), Some(doc.ids["pf"]));
    }

    #[test]
    fn pointer_events_stroke_value_skips_interior() {
        // pe=stroke × stroke=none：内部 miss（Chrome (417,30) → bg）；
        // 描边可绘时描边带命中。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="60">
          <rect id="bg" x="0" y="0" width="80" height="60" fill="yellow"/>
          <rect id="ps" x="10" y="10" width="40" height="40" fill="red" pointer-events="stroke"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 80.0, 60.0, 30.0, 30.0), Some(doc.ids["bg"]));
        let svg2 = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="60">
          <rect id="bg" x="0" y="0" width="80" height="60" fill="yellow"/>
          <rect id="ps2" x="10" y="10" width="40" height="40" fill="red"
            stroke="black" stroke-width="4" pointer-events="stroke"/>
        </svg>"##;
        let doc = parse(svg2);
        assert_eq!(hit_test(&doc, 80.0, 60.0, 12.0, 30.0), Some(doc.ids["ps2"]));
    }

    #[test]
    fn pointer_events_all_covers_both_domains() {
        // pe=all × fill/stroke 全 none：内部 + 描边带都命中（Chrome (352,*)）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="60">
          <rect id="bg" x="0" y="0" width="80" height="60" fill="yellow"/>
          <rect id="pa" x="10" y="10" width="40" height="40" fill="none" stroke="none" pointer-events="all"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 80.0, 60.0, 30.0, 30.0), Some(doc.ids["pa"]));
        assert_eq!(hit_test(&doc, 80.0, 60.0, 30.0, 11.0), Some(doc.ids["pa"]));
    }

    #[test]
    fn pointer_events_visibility_interaction() {
        // Chrome hj1 实证：visibility:hidden × pe=fill → 命中（非 visible*
        // 值不要求可见）；× pe=visibleFill → miss；× 缺省 visiblePainted →
        // miss（旧口径保持）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="180" height="60">
          <rect id="bg" x="0" y="0" width="180" height="60" fill="yellow"/>
          <rect id="vf" x="10" y="10" width="40" height="40" fill="red"
            visibility="hidden" pointer-events="fill"/>
          <rect id="vvis" x="60" y="10" width="40" height="40" fill="red"
            visibility="hidden" pointer-events="visibleFill"/>
          <rect id="vd" x="110" y="10" width="40" height="40" fill="red"
            visibility="hidden"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 180.0, 60.0, 30.0, 30.0), Some(doc.ids["vf"]));
        assert_eq!(
            hit_test(&doc, 180.0, 60.0, 80.0, 30.0),
            Some(doc.ids["bg"]),
            "visibleFill 要求 visibility:visible"
        );
        assert_eq!(
            hit_test(&doc, 180.0, 60.0, 130.0, 30.0),
            Some(doc.ids["bg"])
        );
    }

    #[test]
    fn pointer_events_css_decl_beats_xml_attr() {
        // CSS 声明位生效（style pe:none 穿透）；级联胜 XML 属性位
        //（attr none + css all → 命中，Chrome (676,30)）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60">
          <rect id="bg" x="0" y="0" width="120" height="60" fill="yellow"/>
          <rect id="css-none" x="10" y="10" width="40" height="40" fill="orange"
            style="pointer-events:none"/>
          <rect id="attr-css" x="60" y="10" width="40" height="40" fill="pink"
            pointer-events="none" style="pointer-events:all"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 120.0, 60.0, 30.0, 30.0), Some(doc.ids["bg"]));
        assert_eq!(
            hit_test(&doc, 120.0, 60.0, 80.0, 30.0),
            Some(doc.ids["attr-css"])
        );
    }

    // ---- 5. 车道HJ：clip 裁断（Chrome hj3 探针口径） ----

    #[test]
    fn clip_path_cuts_hit_region() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="100">
          <rect id="bg" x="0" y="0" width="120" height="100" fill="yellow"/>
          <clipPath id="half"><rect x="15" y="15" width="20" height="40"/></clipPath>
          <rect id="clipped" x="15" y="15" width="40" height="40" fill="red" clip-path="url(#half)"/>
          <rect id="unk" x="70" y="15" width="40" height="40" fill="orange" clip-path="url(#nosuch)"/>
        </svg>"##;
        let doc = parse(svg);
        // 保留半区命中；裁掉半区穿透 bg
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 25.0, 35.0),
            Some(doc.ids["clipped"])
        );
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 45.0, 35.0),
            Some(doc.ids["bg"])
        );
        // 未知引用：宽容不裁 → 命中（渲染同口径）
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 90.0, 35.0),
            Some(doc.ids["unk"])
        );
    }

    #[test]
    fn clip_path_on_group_cuts_subtree_and_empty_cuts_all() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="180" height="100">
          <rect id="bg" x="0" y="0" width="180" height="100" fill="yellow"/>
          <clipPath id="half2"><rect x="15" y="15" width="20" height="40"/></clipPath>
          <clipPath id="empty"/>
          <g clip-path="url(#half2)">
            <rect id="child-c" x="15" y="15" width="40" height="40" fill="blue"/>
          </g>
          <rect id="ec" x="70" y="15" width="40" height="40" fill="purple" clip-path="url(#empty)"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(
            hit_test(&doc, 180.0, 100.0, 25.0, 35.0),
            Some(doc.ids["child-c"])
        );
        assert_eq!(
            hit_test(&doc, 180.0, 100.0, 45.0, 35.0),
            Some(doc.ids["bg"])
        );
        // 空 clipPath：可解析但无几何 → 引用元素整体不描绘（CW 语义）
        assert_eq!(
            hit_test(&doc, 180.0, 100.0, 90.0, 35.0),
            Some(doc.ids["bg"])
        );
    }

    #[test]
    fn nested_svg_viewport_cuts_hit() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
          <rect id="bg" x="0" y="0" width="100" height="100" fill="yellow"/>
          <svg id="inner" x="10" y="10" width="40" height="30">
            <rect id="in-child" x="0" y="0" width="60" height="80" fill="green"/>
          </svg>
        </svg>"##;
        let doc = parse(svg);
        // 视口内命中溢出子元素
        assert_eq!(
            hit_test(&doc, 100.0, 100.0, 30.0, 20.0),
            Some(doc.ids["in-child"])
        );
        // 视口外（y > 40）裁断
        assert_eq!(
            hit_test(&doc, 100.0, 100.0, 30.0, 55.0),
            Some(doc.ids["bg"])
        );
    }

    #[test]
    fn clip_path_obb_units_anchors_element_box() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="100">
          <rect id="bg" x="0" y="0" width="120" height="100" fill="yellow"/>
          <clipPath id="obbc" clipPathUnits="objectBoundingBox">
            <rect x="0" y="0" width="0.5" height="1"/>
          </clipPath>
          <rect id="obbed" x="20" y="15" width="40" height="40" fill="brown" clip-path="url(#obbc)"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 30.0, 35.0),
            Some(doc.ids["obbed"])
        );
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 50.0, 35.0),
            Some(doc.ids["bg"])
        );
    }

    // ---- 6. 车道HJ：use 影子树展开（Chrome hj2 探针口径） ----

    #[test]
    fn use_instance_hits_retarget_to_use_element() {
        // elementFromPoint 返回 use 本身（Chrome hj2：(35,35) → u1）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="100">
          <rect id="bg" x="0" y="0" width="120" height="100" fill="yellow"/>
          <defs><rect id="orig" x="0" y="0" width="30" height="30" fill="red"/></defs>
          <use id="u1" href="#orig" x="20" y="20"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 35.0, 35.0),
            Some(doc.ids["u1"])
        );
        // 实例外（x/y 平移区外）miss；defs 原始区不渲染不命中
        assert_eq!(hit_test(&doc, 120.0, 100.0, 35.0, 5.0), Some(doc.ids["bg"]));
        assert_eq!(
            hit_test(&doc, 120.0, 100.0, 10.0, 10.0),
            Some(doc.ids["bg"])
        );
    }

    #[test]
    fn use_shadow_content_and_nested_use_owner() {
        // 影子内容命中（g 内形状）→ 上报 use；链式 use（use 引 use）→
        // 最外层 use 胜（retarget 逐层到 host）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="140">
          <rect id="bg" x="0" y="0" width="160" height="140" fill="yellow"/>
          <defs>
            <g id="gg"><rect id="g1" x="0" y="0" width="30" height="20" fill="cyan"/></g>
            <rect id="inner-t" x="0" y="0" width="20" height="20" fill="blue"/>
          </defs>
          <use id="u2" href="#gg" x="10" y="10"/>
          <use id="u3" href="#u2" x="60" y="60"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(
            hit_test(&doc, 160.0, 140.0, 25.0, 25.0),
            Some(doc.ids["u2"])
        );
        // 链式：u3 实例化 u2，u2 实例化 gg——命中上报最外层 u3
        assert_eq!(
            hit_test(&doc, 160.0, 140.0, 85.0, 75.0),
            Some(doc.ids["u3"])
        );
    }

    #[test]
    fn use_symbol_viewport_and_hidden_target() {
        // symbol 视口实例：w/h 建视口 + viewBox 映射 + 溢出裁断；
        // display:none 的目标不实例化（渲染同口径）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="140">
          <rect id="bg" x="0" y="0" width="200" height="140" fill="yellow"/>
          <defs>
            <symbol id="sym" viewBox="0 0 40 40">
              <circle id="symc" cx="20" cy="20" r="20" fill="teal"/>
              <rect id="over" x="30" y="30" width="30" height="30" fill="black"/>
            </symbol>
            <rect id="hidden-t" x="0" y="0" width="30" height="30" fill="red" display="none"/>
          </defs>
          <use id="u3" href="#sym" x="140" y="10" width="60" height="60"/>
          <use id="u4" href="#hidden-t" x="10" y="80"/>
        </svg>"##;
        let doc = parse(svg);
        // circle 圆心（viewBox 20,20 × 1.5 + 原点 140,10 → 设备 (170,40)）
        assert_eq!(
            hit_test(&doc, 200.0, 140.0, 170.0, 40.0),
            Some(doc.ids["u3"])
        );
        // 溢出内容（over → 设备 x∈[235..]）被视口（到 200）裁掉——取
        // 视口外点 miss
        assert_eq!(
            hit_test(&doc, 200.0, 140.0, 170.0, 100.0),
            Some(doc.ids["bg"])
        );
        // display:none 目标不实例化
        assert_eq!(
            hit_test(&doc, 200.0, 140.0, 25.0, 95.0),
            Some(doc.ids["bg"])
        );
    }

    // ---- 7. 车道HJ：text 多段 / rect 圆角 ----

    #[test]
    fn text_hits_explicit_tspan_segments() {
        // 显式 x/y 的 tspan 段开新近似盒（多行文本形态）；粒度恒 text。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="80">
          <rect id="bg" x="0" y="0" width="200" height="80" fill="yellow"/>
          <text id="t1" x="20" y="40" font-size="20" fill="black">ABCD<tspan id="ts2" x="110" y="20">EFGH</tspan></text>
        </svg>"##;
        let doc = parse(svg);
        // 首段（x=20 起，ABCD 宽 4×0.6×20=48 → 20..68）
        assert_eq!(hit_test(&doc, 200.0, 80.0, 40.0, 35.0), Some(doc.ids["t1"]));
        // 第二段（x=110 起，EFGH 宽 48 → 110..158；旧实现只按首段 = miss）
        assert_eq!(
            hit_test(&doc, 200.0, 80.0, 130.0, 15.0),
            Some(doc.ids["t1"])
        );
        // 段间空隙 miss
        assert_eq!(hit_test(&doc, 200.0, 80.0, 90.0, 30.0), Some(doc.ids["bg"]));
    }

    #[test]
    fn rect_rounded_corners_cut_hit() {
        // rx=40 > w/2=30 → 钳 30：角点 (5,5) 距圆角圆心 (30,30) 35 > 30 →
        // miss；内部命中（Chrome hj4 (305,25) → bg、(320,60) → hit）。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="80">
          <rect id="bg" x="0" y="0" width="80" height="80" fill="yellow"/>
          <rect id="rounded" x="0" y="0" width="60" height="60" rx="40" fill="steelblue"/>
        </svg>"##;
        let doc = parse(svg);
        assert_eq!(hit_test(&doc, 80.0, 80.0, 5.0, 5.0), Some(doc.ids["bg"]));
        assert_eq!(
            hit_test(&doc, 80.0, 80.0, 20.0, 50.0),
            Some(doc.ids["rounded"])
        );
        // 无圆角时直角盒（回归锚：旧语义）
        let svg2 = r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="80">
          <rect id="sq" x="0" y="0" width="60" height="60" fill="steelblue"/>
        </svg>"##;
        let doc = parse(svg2);
        assert_eq!(hit_test(&doc, 80.0, 80.0, 1.0, 1.0), Some(doc.ids["sq"]));
    }

    #[test]
    fn clip_rings_nonzero_and_evenodd_rules() {
        // 双子环 clipPath：NonZero 同向双环 → 并集（双环叠区 wn=2 仍内）；
        // EvenOdd 双环 → 叠区为外（奇偶翻转）。规则取首个贡献子元素。
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="80">
          <rect id="bg" x="0" y="0" width="200" height="80" fill="yellow"/>
          <clipPath id="cnz">
            <rect x="10" y="10" width="40" height="40"/>
            <rect x="30" y="10" width="40" height="40"/>
          </clipPath>
          <rect id="nz" x="10" y="10" width="60" height="40" fill="red" clip-path="url(#cnz)"/>
          <clipPath id="ceo">
            <rect x="110" y="10" width="40" height="40" clip-rule="evenodd"/>
            <rect x="130" y="10" width="40" height="40"/>
          </clipPath>
          <rect id="eo" x="110" y="10" width="60" height="40" fill="red" clip-path="url(#ceo)"/>
        </svg>"##;
        let doc = parse(svg);
        // NonZero：两环皆顺时针同向 → 叠区 (30..50) wn=2 → 内
        assert_eq!(hit_test(&doc, 200.0, 80.0, 40.0, 30.0), Some(doc.ids["nz"]));
        // EvenOdd：叠区奇偶 = 外 → miss
        assert_eq!(
            hit_test(&doc, 200.0, 80.0, 140.0, 30.0),
            Some(doc.ids["bg"])
        );
    }

    /// 车道HJ 双引擎对照镜像：Chrome elementFromPoint 语义表
    ///（tests/oracle/repros/chrome/hj/hj_semantics.json，2026-09-12
    /// chrome-headless-shell 148 实测）的引擎侧逐点复刻。表值 = 探针
    /// 观察；改动域判定/裁断/影子展开时先跑本测试。
    #[test]
    fn hj_chrome_semantics_mirror() {
        // -- hj1 pointer-events（采样点 → 期望元素/穿透） --
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="900" height="80">
          <rect id="bg" x="0" y="0" width="900" height="80" fill="yellow"/>
          <rect id="pe-default" x="10" y="10" width="40" height="40" fill="red"/>
          <rect id="pe-none-rect" x="75" y="10" width="40" height="40" fill="blue" pointer-events="none"/>
          <rect id="pe-fill-nonfill" x="205" y="10" width="40" height="40" fill="none" pointer-events="fill"/>
          <rect id="pe-def-stroke" x="268" y="10" width="40" height="40" fill="none" stroke="black" stroke-width="4"/>
          <rect id="pe-all-none" x="332" y="10" width="40" height="40" fill="none" stroke="none" pointer-events="all"/>
          <rect id="pe-stroke-nonstroke" x="397" y="10" width="40" height="40" fill="red" pointer-events="stroke"/>
          <rect id="pe-painted-stroke" x="461" y="10" width="40" height="40" fill="none" stroke="black" stroke-width="4" pointer-events="painted"/>
          <g id="g-pe-none" pointer-events="none">
            <rect id="child-in-g" x="526" y="10" width="40" height="40" fill="green"/>
          </g>
          <rect id="vis-fill" x="721" y="10" width="40" height="40" fill="red" visibility="hidden" pointer-events="fill"/>
          <rect id="vis-vfill" x="786" y="10" width="40" height="40" fill="red" visibility="hidden" pointer-events="visibleFill"/>
          <rect id="vfill-norm" x="851" y="10" width="40" height="40" fill="red" pointer-events="visibleFill"/>
        </svg>"##,
        );
        let ids = |n: &str| Some(svg.ids[n]);
        let hit = |x, y| hit_test(&svg, 900.0, 80.0, x, y);
        assert_eq!(hit(30.0, 30.0), ids("pe-default"));
        assert_eq!(hit(95.0, 30.0), ids("bg"), "pe=none 穿透");
        assert_eq!(
            hit(225.0, 30.0),
            ids("pe-fill-nonfill"),
            "pe=fill × fill=none"
        );
        assert_eq!(
            hit(288.0, 30.0),
            ids("bg"),
            "visiblePainted × fill=none 内部"
        );
        assert_eq!(hit(288.0, 12.0), ids("pe-def-stroke"), "顶边描边带命中");
        assert_eq!(hit(352.0, 30.0), ids("pe-all-none"), "pe=all 全 none 内部");
        assert_eq!(hit(352.0, 11.0), ids("pe-all-none"), "pe=all 边缘");
        assert_eq!(hit(417.0, 30.0), ids("bg"), "pe=stroke × stroke=none 内部");
        assert_eq!(hit(481.0, 30.0), ids("bg"), "painted × fill=none 内部");
        assert_eq!(hit(481.0, 10.0), ids("pe-painted-stroke"), "painted 描边带");
        assert_eq!(hit(546.0, 30.0), ids("bg"), "g pe=none 子继承穿透");
        assert_eq!(
            hit(741.0, 30.0),
            ids("vis-fill"),
            "vis:hidden × pe=fill 命中"
        );
        assert_eq!(hit(806.0, 30.0), ids("bg"), "vis:hidden × visibleFill miss");
        assert_eq!(hit(871.0, 30.0), ids("vfill-norm"), "visibleFill 正常");
        // -- hj4 image 元素矩形（meet letterbox 命中 = Chrome 一致） --
        let svg4 = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="420" height="110">
          <rect id="bg" x="0" y="0" width="420" height="110" fill="yellow"/>
          <image id="img" x="200" y="40" width="40" height="20" href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="/>
        </svg>"##,
        );
        // letterbox 区（内容矩形 x∈[210,230] 外、元素矩形内）Chrome 命中
        // → 引擎同判（元素矩形模型）；元素矩形外 miss。
        assert_eq!(
            hit_test(&svg4, 420.0, 110.0, 205.0, 50.0),
            Some(svg4.ids["img"])
        );
        assert_eq!(
            hit_test(&svg4, 420.0, 110.0, 250.0, 50.0),
            Some(svg4.ids["bg"])
        );
    }

    // ---- 8. 车道JD：dash 图案感知命中（jd3 探针镜像） ----

    /// Chrome jd3 实证：dash 间隙中心 (35,40) miss、实段中心 (45,40)
    /// hit（butt 帽缺省——亮段 [20,30)/[40,50)/…，无帽延伸）。
    #[test]
    fn dash_gap_misses_and_dash_segment_hits() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <path id="dl" d="M 20 40 H 220" stroke="black" stroke-width="8" stroke-dasharray="10 10" fill="none"/>
        </svg>"##,
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 45.0, 40.0),
            Some(svg.ids["dl"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 35.0, 40.0),
            Some(svg.ids["bg"]),
            "dash 间隙不命中 → 穿透 bg（Chrome elementFromPoint 同判）"
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 26.0, 40.0),
            Some(svg.ids["dl"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 34.0, 40.0),
            Some(svg.ids["bg"])
        );
    }

    /// dashoffset 平移相位；pathLength 归一（k = 实际/pl，图案/相位同
    /// 缩——渲染 dash 消费同式）。
    #[test]
    fn dash_offset_shifts_and_path_length_scales() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <path id="dof" d="M 20 40 H 220" stroke="black" stroke-width="8" stroke-dasharray="10 10" stroke-dashoffset="5" fill="none"/>
          <path id="plp" d="M 0 100 H 100" pathLength="50" stroke="black" stroke-width="8" stroke-dasharray="10 10" fill="none"/>
        </svg>"##,
        );
        // offset=5：亮段 [15,25)/[35,45)/… → x=40 (s=20→相位5) 命中，
        // x=50 (s=30→相位15) 间隙
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 40.0, 40.0),
            Some(svg.ids["dof"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 50.0, 40.0),
            Some(svg.ids["bg"])
        );
        // pathLength=50、实际 100 → k=2 → 图案 20/20：x=10 (s=10) 亮段
        // [0,20) 命中；x=30 (s=30) 间隙 [20,40) miss
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 10.0, 100.0),
            Some(svg.ids["plp"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 30.0, 100.0),
            Some(svg.ids["bg"])
        );
    }

    /// 点模式（全部 on 段为零）× round 帽：点心圆命中；butt 帽不画点
    /// → 恒 miss（渲染两态语义同源）。
    #[test]
    fn dash_dot_mode_round_cap_hits_butt_misses() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <path id="dotr" d="M 10 40 H 110" stroke="black" stroke-width="8" stroke-linecap="round" stroke-dasharray="0 10" fill="none"/>
          <path id="dotb" d="M 10 100 H 110" stroke="black" stroke-width="8" stroke-dasharray="0 10" fill="none"/>
        </svg>"##,
        );
        // 点心 s=0,10,20… 半径 hw=4：s=0 命中、s=5 两点间隙 miss、
        // s=20 命中
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 10.0, 40.0),
            Some(svg.ids["dotr"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 15.0, 40.0),
            Some(svg.ids["bg"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 30.0, 40.0),
            Some(svg.ids["dotr"])
        );
        // butt 帽：零长亮段不画 → 穿透 bg
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 10.0, 100.0),
            Some(svg.ids["bg"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 30.0, 100.0),
            Some(svg.ids["bg"])
        );
    }

    /// dash 图案链继承（g 声明、子未声明吃链值）；显式 none 挡继承
    /// 回实线。
    #[test]
    fn dash_inheritance_chain_and_explicit_none() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <g stroke="black" stroke-width="8" stroke-dasharray="6 6">
            <line id="li" x1="10" y1="20" x2="110" y2="20"/>
            <line id="ln" x1="10" y1="60" x2="110" y2="60" stroke-dasharray="none"/>
          </g>
        </svg>"##,
        );
        // li 继承 6/6：亮段 [10,16)/[22,28)/… → (19,20) s=9 间隙 miss
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 13.0, 20.0),
            Some(svg.ids["li"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 19.0, 20.0),
            Some(svg.ids["bg"])
        );
        // ln 显式 none：实线（旧近似把继承也当实线——显式 none 挡链）
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 19.0, 60.0),
            Some(svg.ids["ln"])
        );
    }

    // ---- 9. 车道JD：fill-rule 感知命中（jd3 探针镜像） ----

    /// 五角星：nonzero（缺省）中心 winding=2 命中（Chrome jd3 同判，
    /// 旧恒奇偶实现 miss）；evenodd 中心孔 miss、尖角（winding=1）双
    /// 规则都命中。
    #[test]
    fn fill_rule_pentagram_center_and_hole() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <path id="pnz" d="M 60 75 L 39.43 138.32 L 93.29 99.18 L 26.71 99.18 L 80.57 138.32 Z" fill="red"/>
          <polygon id="peo" points="190,75 169.43,138.32 223.29,99.18 156.71,99.18 210.57,138.32" fill-rule="evenodd" fill="blue"/>
        </svg>"##,
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 60.0, 110.0),
            Some(svg.ids["pnz"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 60.0, 88.0),
            Some(svg.ids["pnz"])
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 190.0, 110.0),
            Some(svg.ids["bg"]),
            "evenodd 中心孔 → 穿透 bg（Chrome elementFromPoint 同判）"
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 190.0, 88.0),
            Some(svg.ids["peo"])
        );
    }

    /// 多子路径跨环求值：反向双环（洞）nonzero miss（旧逐环奇偶并集
    /// 误命中）；同向双环中心 winding=2 nonzero 命中（Chrome 同判）。
    #[test]
    fn path_multi_subpath_nonzero_winding() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <path id="hole" d="M 10 10 H 110 V 110 H 10 Z M 90 50 H 50 V 90 H 90 Z" fill="red"/>
          <path id="fill" d="M 210 10 H 310 V 110 H 210 Z M 250 50 H 290 V 90 H 250 Z" fill="red"/>
        </svg>"##,
        );
        // 反向内环：中心 (60,70) 绕数和 0 → 洞（Chrome nonzero 语义）
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 60.0, 70.0),
            Some(svg.ids["bg"]),
            "反向内环 = 洞 → 穿透 bg"
        );
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 30.0, 30.0),
            Some(svg.ids["hole"])
        );
        // 同向内环：中心 (270,70) 绕数 2 → 命中
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 270.0, 70.0),
            Some(svg.ids["fill"])
        );
    }

    /// polygon 描边闭合边可命中（车道JD 顺带对齐渲染——旧实现 Z 边
    /// 不可命中）。
    #[test]
    fn polygon_closing_edge_stroke_hit() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="700" height="160">
          <rect id="bg" x="0" y="0" width="700" height="160" fill="yellow"/>
          <polygon id="pg" points="10,10 60,10 60,60" fill="none" stroke="black" stroke-width="8"/>
        </svg>"##,
        );
        // 闭合边 (60,60)→(10,10) 中点 (35,35)
        assert_eq!(
            hit_test(&svg, 700.0, 160.0, 35.0, 35.0),
            Some(svg.ids["pg"])
        );
    }

    // ---- 10. 车道JD：拖动中 :active（jd4/jd6 探针镜像） ----

    /// Chrome jd4 实证：按压后指针在按压元素内 → Keep；滑出 → Cleared
    /// 且**不转移**（指针下元素不激活）；滑回**不恢复**（单发清除锁存）；
    /// 命中落空（空白区）同 Cleared。
    #[test]
    fn drag_active_step_keep_clear_and_latch() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="200">
          <rect id="ra" x="20" y="20" width="120" height="120" fill="red"/>
          <rect id="rb" x="260" y="20" width="120" height="120" fill="green"/>
        </svg>"##,
        );
        let ra = svg.ids["ra"];
        let mut cleared = false;
        // 指针在按压元素内：保持
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 80.0, 80.0, ra, &mut cleared),
            DragActive::Keep
        );
        assert!(!cleared);
        // 滑出到 rb：清（不转移）
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 320.0, 80.0, ra, &mut cleared),
            DragActive::Cleared
        );
        assert!(cleared);
        // 滑回 ra：锁存 Cleared（Chrome 不恢复）
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 80.0, 80.0, ra, &mut cleared),
            DragActive::Cleared
        );
        // 命中落空（无 bg 空白区）：同 Cleared
        let mut cleared2 = false;
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 300.0, 190.0, ra, &mut cleared2),
            DragActive::Cleared
        );
    }

    /// 拖动 :active 的按压目标自身域：按到 rb 后指针在 rb 内移动恒
    /// Keep（不重级联）；根链（空白区点击 = 根目标）内移动同 Keep。
    #[test]
    fn drag_active_pressed_self_and_root_target_keep() {
        let svg = parse(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="200">
          <rect id="ra" x="20" y="20" width="120" height="120" fill="red"/>
          <rect id="rb" x="260" y="20" width="120" height="120" fill="green"/>
        </svg>"##,
        );
        let rb = svg.ids["rb"];
        let root = svg.tree.root();
        let mut cleared = false;
        // 按压 rb、指针在 rb 内两处移动：保持
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 300.0, 50.0, rb, &mut cleared),
            DragActive::Keep
        );
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 360.0, 110.0, rb, &mut cleared),
            DragActive::Keep
        );
        assert!(!cleared);
        // 空白区点击（落空以根为目标——dispatch_event_at 同构）：根盒内
        // 移动 = 根子树内 → 保持
        let mut cleared_root = false;
        assert_eq!(
            drag_active_step(&svg, 480.0, 200.0, 200.0, 150.0, root, &mut cleared_root),
            DragActive::Keep
        );
    }
}
