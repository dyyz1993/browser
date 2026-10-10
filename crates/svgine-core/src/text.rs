//! M8 Tier B 文本渲染：内嵌矢量字模 + `<text>`/`<tspan>` 排版度量。
//!
//! **字形方案（车道决策）**：核心不引字体库/字体文件——内嵌 **Hershey
//! Roman Simplex** 单线矢量字模（公版数据，Dr. A. V. Hershey 于美国
//! 国家标准局工作时数字化，James Hurt 打包的 JHF 发行版；许可允许任何
//! 目的使用，仅要求随数据分发下述致谢）：
//!
//! - The Hershey Fonts were originally created by Dr. A. V. Hershey
//!   while working at the U. S. National Bureau of Standards.
//! - The format of the Font data in this distribution was originally
//!   created by James Hurt, Cognition, Inc., 900 Technology Park Drive,
//!   Billerica, MA 01821 (mit-eddie!ci-dandelion!hurt).
//!
//! 覆盖 ASCII 32..=126（95 字形，1113 个顶点 ≈ 2.2KB flash）；**中文等
//! 非 ASCII 字符降级为 □ 方框**（advance 1em，逐字符计告警，见 builder
//! 解析期统计）。
//!
//! **度量对齐 Chrome**：advance 表用 Helvetica AFM 宽度（千分 em，
//! 与 Chrome 的 sans-serif 实测差 <1px@13px，见 oracle 校准）；大写高
//! 0.715em ↔ Hershey 21 单位做缩放；笔画以展开四边形（宽度
//! [`STROKE_EM`]·em，平端 butt cap）近似 Chrome 的实心字形——单线
//! 骨架的已知度量边界：与 Chrome 实心字形的像素差分恒为正贡献
//!（AA 边带错配），Tier B 取"可见性优先、度量尽量贴近"的平衡。
//!
//! 坐标约定：字模点 y 向上、基线 y=0、字形 ink 左缘 x=0；排版时经
//! `T(pen)·S(gs,-gs)·C(ink 居中)` 映射到用户空间（y 翻转），再走元素
//! 矩阵烘焙进显示列表点池（复用描边即填充管线，raster 零改动）。

// Tier C 字体注入（feature `text-full`；Hershey 路径不依赖 font 模块）
#[cfg(feature = "text-full")]
use crate::font::Font;
use crate::geom::Mat2x3;
use crate::path::{stroke_outline, Cap, LineJoin, PathGeom, Subpath};
use alloc::vec::Vec;

/// 字模顶点（Hershey 整数单位；y 向上、基线 0、ink 左缘 0）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphPt {
    pub x: i8,
    pub y: i8,
}

/// 一个字形：ink 宽（Hershey 单位）+ 笔画折线集。
pub struct Glyph {
    pub ink_w: i8,
    pub strokes: &'static [&'static [GlyphPt]],
}

// 大写高：Hershey 21 单位 = Helvetica 0.715em（oracle 实测 10px@fs14）。
pub const CAP_UNITS: f32 = 21.0;
pub const CAP_EM: f32 = 0.715;

/// Helvetica AFM advance 宽度表（千分 em），ASCII 32..=126。
/// 与 Chrome sans-serif 的 oracle 实测对齐（"middle"@13px 差 <0.5px）。
#[rustfmt::skip]
pub static ADV1000: [u16; 95] = [
    //   sp     !      "      #      $      %      &      '      (      )
        278,   278,   355,   556,   556,   889,   667,   191,   333,   333,
    //   *      +      ,      -      .      /      0      1      2      3
        389,   584,   278,   333,   278,   278,   556,   556,   556,   556,
    //   4      5      6      7      8      9      :      ;      <      =
        556,   556,   556,   556,   556,   556,   278,   278,   584,   584,
    //   >      ?      @      A      B      C      D      E      F      G
        584,   556,  1015,   667,   667,   722,   722,   667,   611,   778,
    //   H      I      J      K      L      M      N      O      P      Q
        722,   278,   500,   667,   556,   833,   722,   778,   667,   778,
    //   R      S      T      U      V      W      X      Y      Z      [
        722,   667,   611,   722,   667,   944,   667,   667,   611,   278,
    //   \      ]      ^      _      `      a      b      c      d      e
        278,   278,   469,   556,   333,   556,   556,   500,   556,   556,
    //   f      g      h      i      j      k      l      m      n      o
        278,   556,   556,   222,   222,   500,   222,   833,   556,   556,
    //   p      q      r      s      t      u      v      w      x      y
        556,   556,   333,   500,   278,   556,   500,   722,   500,   500,
    //   z      {      |      }      ~
        500,   334,   260,   334,   584,
];

// ---------------------------------------------------------------------------
// oracle 校准常数（s7-01 差分调优的单一入口）
// ---------------------------------------------------------------------------

/// advance 全局倍率（Chrome 逐字符舍入累计 ~1%，微调对齐 oracle；
/// s7-01 扫描 0.97/1.0/1.03，1.0 最优）。
pub const ADV_SCALE: f32 = 1.0;
/// 字间额外间距（em；Chrome 无额外字距，0 对齐 oracle）。
pub const LETTER_SPACING_EM: f32 = 0.0;
/// 常规笔画宽（em）。车道BK 全文本语料联动扫描（2026-09-11，十档
/// 0.03–0.12 步长 0.01，每档 WPT 全集 --force 重评 + 主语料 diff_stage）：
/// 谷底 0.06–0.07，0.06 三口径同时最优（WPT 全集 mean tol32 1.514→1.505%、
/// 文本子集 72 枚 2.847→2.816%、主语料 s7-01 11.67→11.63%），非文本域
/// （WPT 非文本 167 枚 / 主语料 12 阶段）逐位零扰动。0.07 的 +1 pass
/// （pathLength-zero 2.13→1.94%）是 2% 达标线阈值摆动（文本子集均值回退
/// 到基线），按「头条 ±1 枚勿归因」纪律不取。旧 s7-01「加宽单调变差、
/// 0.04 更优」的地形已失效（预言机重冻+AA/滤镜演进后倒转）；BE 车道
/// 「Chrome 干道 ~0.1em、加粗可收敛」假设被像素差分否证——单线骨架与
/// Chrome 实心字形的差分主项是骨架几何错配，加粗只扩大错配带（≥0.08
/// 全指标单调劣化）。详见 tests/oracle/reports/text-weight-bk.md。
pub const STROKE_EM: f32 = 0.06;
/// 笔画端头延伸系数（×hw）：盖住转角接头；0 = 平端（butt cap）。
/// s7-01 实测 0.5 端延伸比 0 差 0.09pp（延伸段落在字形墨区外）。
pub const STROKE_EXT: f32 = 0.0;
/// font-weight bold 额外加粗（em；近似双描加宽）。
pub const BOLD_EXTRA_EM: f32 = 0.03;
/// 非 ASCII（CJK 等）降级 □ 的 advance（em；Chrome CJK = 1em 实测）。
pub const FALLBACK_ADV_EM: f32 = 1.0;

// ---------------------------------------------------------------------------
// 文本样式语义常数（车道AK：dominant-baseline / text-decoration /
// font-size-adjust / 仿斜的单一入口；对齐 Helvetica AFM 度量——与
// ADV1000 advance 表同源）
// ---------------------------------------------------------------------------

/// x-height（em；Helvetica AFM xHeight 523/1000）。font-size-adjust 的
/// aspect 分母：adjusted = size × (adjust / 本值)。
pub const X_HEIGHT_EM: f32 = 0.523;
/// em 盒上升（CSS em-box 约定；dominant-baseline top/text-before-edge 用）。
pub const ASCENT_EM: f32 = 0.8;
/// em 盒下降（同上；bottom 用）。
pub const DESCENT_EM: f32 = 0.2;
/// 车道BG：hanging 基线偏移（em）。Chrome 预言机逐像素实测（bgp1/bgp2
/// 探针，2026-09-11）：fallback serif（Times New Roman）下 hanging 落位 =
/// 0.7129em ≈ 0.8 × hhea ascent(0.891)——hanging 用独立的 0.8×ascent 式，
/// 不与 em 盒 top（0.8）同值。100/200px 双字号线性验证 142/89px。
pub const HANGING_EM: f32 = 0.713;
/// 车道BG：central 基线偏移（em）。Chrome 实测 = (hhea ascent − descent)/2
/// = (0.891−0.216)/2 ≈ 0.3375（TNR fallback）；bgp4 探针 baseline+34px@fs100
/// 钉死。旧值 0.30（引擎 em 盒自洽）与预言机差 0.0375em（central-large
/// fixture 13.55% 差分的落位根因之一）。
pub const CENTRAL_EM: f32 = 0.3375;
/// underline 偏移（em，基线下方）。车道BG 预言机实测（bgp4/bgp5/bgp7）：
/// Chrome SVG 文本 underline = 基线下 0.05em、Helvetica/Times 一致
/// （200px 下条顶 = baseline+10px）。旧值 0.10（AFM UnderlinePosition）与
/// 预言机差一倍。
pub const UNDERLINE_OFFSET_EM: f32 = 0.05;
/// 装饰条厚（em）。Chrome 实测 0.10em（200px 下 20px，两族字体一致）。
pub const DECO_THICK_EM: f32 = 0.10;
/// 车道BG：dominant-baseline: central 时 underline 的额外下移（em）。
/// Chrome 实测（bgp4/bg5 对照）：其余基线值（hanging/middle/edge）下
/// underline 恒 = 移位后基线 +0.05em，唯 central 落 +0.16em（= 常规
/// +0.11em）——Blink 对 central 移位的装饰定位异常（crbug 389845192
/// 关联面），按预言机逐像素建模。
pub const UNDERLINE_CENTRAL_EXTRA_EM: f32 = 0.11;
/// line-through 中心偏移（em，基线上方；AFM/OS2 strikeout ≈0.26 近似）。
pub const STRIKE_OFFSET_EM: f32 = 0.26;
/// overline 偏移（em，基线上方；取 cap 高 0.715）。
pub const OVERLINE_OFFSET_EM: f32 = 0.715;
/// 仿斜剪切系数（≈ tan 14°；无斜体字形时的合成斜体，登记近似）。
pub const OBLIQUE_SKEW: f32 = 0.25;
/// 车道AO：纵排（writing-mode 纵排值）central baseline 偏移（em）——
/// 字形 alphabetic baseline 相对列轴（device x 向）的偏移 = (asc−desc)/2，
/// 与 dominant-baseline Central 同式（0.30，引擎 em 盒自洽）。Chrome
/// 预言机侧随实际回退字体浮动（DejaVu ≈0.346、其他 ≈0.26）：ao-vertical
/// 探针（DejaVu）互验 ink bbox 1-2px；fixture 系扫描 0.25/0.30/0.35，
/// 0.30 综合最优（登记 CONFORMANCE 近似）。注意：车道BG 把线性文本的
/// dominant-baseline central 改为 CENTRAL_EM(0.3375，预言机实测)，
/// 纵排仍用本常数（预言机系扫描取优，两处语义分立）。
pub const VERT_CENTRAL_EM: f32 = (ASCENT_EM - DESCENT_EM) * 0.5;

/// text-decoration 位标志（[`decoration_bar`] 的 kind 参数）。
pub struct Deco;

impl Deco {
    pub const UNDERLINE: u8 = 1;
    pub const LINE_THROUGH: u8 = 2;
    pub const OVERLINE: u8 = 4;
}

/// Unicode bidi / 格式控制符（Cf 族 + ZW 族 + 软连字符）：零 advance、
/// 不渲染。Chrome 对 SVG text 中这些字符不占宽（wpt
/// text-bidi-controls-anchors-1/2 根因——此前按非 ASCII 降级 □ + 1em）。
pub fn is_zero_width_format(ch: char) -> bool {
    matches!(ch as u32,
        0x061C                     // ARABIC LETTER MARK
        | 0x00AD                   // SOFT HYPHEN（SVG 不换行 → 不可见）
        | 0x180B..=0x180E          // MONGOLIAN 变体选择符/元音分隔
        | 0x200B..=0x200F          // ZWSP/ZWNJ/ZWJ/LRM/RLM
        | 0x202A..=0x202E          // LRE/RLE/PDF/LRO/RLO
        | 0x2060..=0x2064          // WORD JOINER 族
        | 0x2066..=0x2069          // LRI/RLI/FSI/PDI
        | 0xFEFF                   // ZWNBSP/BOM
    )
}

/// text-anchor（继承属性）。定义在 builder（解析期字段无条件存在，
/// ADR-0009），此处 re-export 保持 `svgine_core::text::TextAnchor` 路径。
pub use crate::builder::TextAnchor;

// ---------------------------------------------------------------------------
// 车道BG：bidi 显示重排（最小 UAX#9 子集）——text-lite 门（消费点
// emit_text 在门内；ADR-0009 文本模块门纪律，T1 无文本构建零体积）。
// ---------------------------------------------------------------------------
#[cfg(feature = "text-lite")]
/// bidi 字符强类型（L/R）；其余可视字符按中性（N）参与解析。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BidiType {
    L,
    R,
    N,
}

#[cfg(feature = "text-lite")]
fn bidi_char_type(ch: char) -> BidiType {
    let c = ch as u32;
    if matches!(c,
        0x0590..=0x08FF   // Hebrew/Arabic 等右向文字区
        | 0xFB1D..=0xFDFF // presentation forms（RTL 主导）
        | 0xFE70..=0xFEFF // Arabic presentation forms
    ) {
        BidiType::R
    } else if ch.is_alphabetic() {
        BidiType::L
    } else {
        BidiType::N
    }
}

/// 显式嵌入 scope：id → (父 id, 是否 RTL 向, 嵌入 level)。
#[cfg(feature = "text-lite")]
struct BidiScope {
    parent: Option<usize>,
    rtl: bool,
    level: i32,
}

/// 一段文本的逻辑序解析产物：可视字符 + 嵌入 level（隐式调整后）+ 强类
/// 型（供中性解析回溯）+ scope 归属。
#[cfg(feature = "text-lite")]
struct BidiItem {
    ch: char,
    level: i32,
    ty: BidiType,
    scope: usize,
}

/// 把逻辑序文本解析为 [`BidiItem`] 序列（控制符不入列；scope 栈随
/// LRE/RLE/LRO/RLO 压、PDF 弹；override 字符类型 = 覆写方向）。
/// 同时返回 scope 表（供中性解析的跨 scope 祖先链）。
#[cfg(feature = "text-lite")]
fn bidi_parse(
    text: &str,
    base_level: i32,
    base_rtl: bool,
) -> (alloc::vec::Vec<BidiItem>, alloc::vec::Vec<BidiScope>) {
    let mut scopes: alloc::vec::Vec<BidiScope> = alloc::vec![BidiScope {
        parent: None,
        rtl: base_rtl,
        level: base_level,
    }];
    let mut out = alloc::vec::Vec::new();
    let mut cur_scope = 0usize;
    let mut cur_level = base_level;
    let mut cur_ov: Option<BidiType> = None;
    for ch in text.chars() {
        match ch {
            '\u{202C}' => {
                // PDF：弹 scope（回到父 scope 的 level；override 清除）
                if cur_scope != 0 {
                    if let Some(p) = scopes[cur_scope].parent {
                        cur_scope = p;
                        cur_level = scopes[p].level;
                    }
                }
                cur_ov = None;
            }
            '\u{202A}' | '\u{202B}' | '\u{202D}' | '\u{202E}' => {
                let rtl = matches!(ch, '\u{202B}' | '\u{202E}');
                let nl = cur_level + 1;
                let nl = if rtl {
                    if nl % 2 == 1 {
                        nl
                    } else {
                        nl + 1
                    }
                } else {
                    if nl % 2 == 0 {
                        nl
                    } else {
                        nl + 1
                    }
                };
                scopes.push(BidiScope {
                    parent: Some(cur_scope),
                    rtl,
                    level: nl,
                });
                cur_scope = scopes.len() - 1;
                cur_level = nl;
                cur_ov = match ch {
                    '\u{202D}' => Some(BidiType::L),
                    '\u{202E}' => Some(BidiType::R),
                    _ => None,
                };
            }
            '\u{200E}' => out.push(BidiItem {
                ch,
                level: cur_level,
                ty: BidiType::L,
                scope: cur_scope,
            }),
            '\u{200F}' => out.push(BidiItem {
                ch,
                level: cur_level,
                ty: BidiType::R,
                scope: cur_scope,
            }),
            _ => {
                if is_zero_width_format(ch) {
                    continue; // 其余零宽控制符：不入重排、不出字形
                }
                let ty = cur_ov.unwrap_or_else(|| bidi_char_type(ch));
                out.push(BidiItem {
                    ch,
                    level: cur_level,
                    ty,
                    scope: cur_scope,
                });
            }
        }
    }
    (out, scopes)
}

/// 中性解析（N1/N2 近似 + isolate 边界）：从 i 向两侧找最近强类型；
/// 若强类型在别的 scope 里，沿 scope 祖先链取「公共祖先下该侧子 scope」
/// 的方向作为该侧贡献（RLE/LRE 作用域对越界中性呈 isolate 语义——
/// Chrome/Firefox 预言机实证：wpt text-bidi-controls-anchors-2 row3/4
/// 的词交换只有此规则能复现，直通式 N1 给出恒等序与预言机矛盾）。
#[cfg(feature = "text-lite")]
fn bidi_neutral_type(
    items: &[BidiItem],
    scopes: &[BidiScope],
    i: usize,
    base_rtl: bool,
) -> BidiType {
    let e = if base_rtl { BidiType::R } else { BidiType::L };
    // 无分配扫描（start/end/step 三参；分配版会拖累 T1 预算线）
    let look = |mut j: isize, end: isize, step: isize| -> Option<BidiType> {
        while j != end {
            let it = &items[j as usize];
            if it.ty != BidiType::N {
                if it.scope == items[i].scope {
                    return Some(it.ty);
                }
                // sj 与 si 分叉：从 sj 沿父链上行，首个祖先 == si 的节点
                // 的方向即为该侧贡献
                let mut anc = it.scope;
                while let Some(p) = scopes[anc].parent {
                    if p == items[i].scope {
                        return Some(if scopes[anc].rtl {
                            BidiType::R
                        } else {
                            BidiType::L
                        });
                    }
                    anc = p;
                }
                return Some(it.ty);
            }
            j += step;
        }
        None
    };
    let left = look(i as isize - 1, -1, -1);
    let right = look(i as isize + 1, items.len() as isize, 1);
    match (left, right) {
        (Some(l), Some(r)) if l == r => l,
        _ => e,
    }
}

/// bidi 显式控制/方向符（参与重排判定）：LRE/RLE/LRO/RLO/PDF + LRM/RLM
/// （isolate 族 2066-2069 不参与重排但算控制符，触发保守重排路径）。
#[cfg(feature = "text-lite")]
pub fn is_bidi_control(ch: char) -> bool {
    matches!(ch as u32,
        0x061C | 0x200E | 0x200F
        | 0x202A..=0x202E
        | 0x2066..=0x2069
    )
}

/// 强 RTL 字符（Hebrew/Arabic 区）。wpt text-inline-size-003 实证：
/// 无控制符的纯阿拉伯语文本在 direction:rtl 下同样需要 L2 反转显示，
/// 触发条件必须含强 R 字符（纯 LTR 内容进重排也是恒等，行为安全）。
#[cfg(feature = "text-lite")]
pub fn is_rtl_char(ch: char) -> bool {
    matches!(ch as u32,
        0x0590..=0x08FF
        | 0xFB1D..=0xFDFF
        | 0xFE70..=0xFEFF
    )
}

/// 车道BG：bidi 显示重排（最小 UAX#9 子集）。
///
/// 输入逻辑序 `text` 与段落方向 `base_rtl`（`direction` 属性），输出
/// **视觉序字符串**：显式控制符（LRE/RLE/LRO/RLO/PDF、ZW 族等零宽符）
/// 全部剔除，其余字符按重排后的视觉顺序输出（Latin 字形不镜像——
/// 镜像对近似登记 CONFORMANCE）。
///
/// 规则面（Python 参考实现对 wpt text-bidi-controls-anchors-1/2 的
/// 42 行 ref 字符串 14/14 全匹配后移植）：
/// 1. X1-X8 简化：LRE/RLE/LRO/RLO 压 scope（level 取下一偶/奇），PDF
///    弹栈；override 字符类型 = 覆写方向、无隐式调整。
/// 2. 中性（标点/空白/数字）：N1 同向取同向、异向/缺失取嵌入方向
///    （N2），跨 scope 呈 isolate 语义（见 [`bidi_neutral_type`]）。
/// 3. X4/X5 隐式：L@奇 +1、R@偶 +1。
/// 4. L2 重排：level 从最高降到最低奇，逐层反转 ≥level 的连续段。
///
/// 不做（登记 CONFORMANCE 近似）：EN/AN 数字细分、镜像对、isolate
/// 控制符（LRI/RLI/FSI/PDI 仅按零宽剔除不参与重排）、逐字符 x 定位
/// 与 bidi 组合。
#[cfg(feature = "text-lite")]
pub fn bidi_reorder(text: &str, base_rtl: bool) -> alloc::string::String {
    let base_level = if base_rtl { 1 } else { 0 };
    let (items, scopes) = bidi_parse(text, base_level, base_rtl);
    if items.is_empty() {
        return alloc::string::String::new();
    }
    let mut levels = alloc::vec::Vec::with_capacity(items.len());
    for i in 0..items.len() {
        let lv = if items[i].ty == BidiType::N {
            let e = bidi_neutral_type(&items, &scopes, i, base_rtl);
            if e == BidiType::L {
                if items[i].level % 2 == 1 {
                    items[i].level + 1
                } else {
                    items[i].level
                }
            } else {
                if items[i].level % 2 == 0 {
                    items[i].level + 1
                } else {
                    items[i].level
                }
            }
        } else if items[i].ty == BidiType::L {
            if items[i].level % 2 == 1 {
                items[i].level + 1
            } else {
                items[i].level
            }
        } else {
            if items[i].level % 2 == 0 {
                items[i].level + 1
            } else {
                items[i].level
            }
        };
        levels.push(lv);
    }
    // L2：从最高 level 降到最低奇 level，逐层反转连续 ≥level 段
    let mut vis: alloc::vec::Vec<char> = items.iter().map(|it| it.ch).collect();
    let max_l = levels.iter().copied().max().unwrap_or(base_level);
    let min_l = levels.iter().copied().min().unwrap_or(base_level);
    let min_odd = if min_l % 2 == 1 { min_l } else { min_l + 1 };
    let mut lv = max_l;
    while lv >= min_odd {
        let mut i = vis.len();
        while i > 0 {
            i -= 1;
            if levels[i] < lv {
                continue;
            }
            let mut j = i;
            while j > 0 && levels[j - 1] >= lv {
                j -= 1;
            }
            vis[j..=i].reverse();
            i = j;
        }
        lv -= 1;
    }
    vis.into_iter().collect()
}

/// 车道NA（stress-corpus）：缺字字符的 ASCII 别名（**仅缺字语境**消费）。
/// 真实世界文本/工具链输出高频、而 Hershey（ASCII 32..=127）必缺的形态，
/// 与其出 tofu 框（大面积假差异）不如降级到最近 ASCII 形态：
/// - U+2212 minus（matplotlib 负刻度 tick/clabel 全用）与 U+2010..2014
///   dash 族 → '-'；
/// - 弯引号 U+2018/2019/201C/201D → 直引号；
/// - U+00A0 nbsp → 空格（CSS 里同为 word-separator，word-spacing 语义
///   一致）。
///
/// 1:1 字符映射——advance_count / textLength 分配分母不变；bidi 类不变
/// （ON/中性同 '-'）。
pub fn ascii_alias(ch: char) -> char {
    match ch as u32 {
        0x2212 | 0x2010 | 0x2011 | 0x2012 | 0x2013 | 0x2014 => '-',
        0x2018 | 0x2019 => '\'',
        0x201C | 0x201D => '"',
        0x00A0 => ' ',
        _ => ch,
    }
}

/// 字符 advance（em 单位）：ASCII 查表；非 ASCII 先过 [`ascii_alias`]
/// （缺字降级，车道NA——'−' 的 advance 必须与发射臂渲染的 '-' 字形
/// 同源）；仍无别名的（CJK 等）1em；控制字符 / bidi 格式符 0。
pub fn advance_em(ch: char) -> f32 {
    let c = ch as u32;
    if c < 0x20 {
        0.0
    } else if c < 0x7f {
        ADV1000[(c - 0x20) as usize] as f32 / 1000.0
    } else if is_zero_width_format(ch) {
        0.0
    } else {
        let a = ascii_alias(ch);
        if a != ch {
            ADV1000[(a as u32 - 0x20) as usize] as f32 / 1000.0
        } else {
            FALLBACK_ADV_EM
        }
    }
}

/// 一段文本在字号 `fs`（用户单位）下的总 advance（用户单位）。
pub fn run_advance(text: &str, fs: f32) -> f32 {
    run_advance_ls(text, fs, 0.0, 0.0)
}

/// 同 [`run_advance`] + 逐字符额外间距 `ls`（用户单位；letter-spacing
/// 属性——含尾字符，CSS 语义）与空格额外间距 `ws`（用户单位；
/// word-spacing 属性——仅 U+0020 的 advance 追加，车道EJ）。
pub fn run_advance_ls(text: &str, fs: f32, ls: f32, ws: f32) -> f32 {
    let mut w = 0.0;
    for ch in text.chars() {
        w += fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM)
            + ls
            + if ch == ' ' { ws } else { 0.0 };
    }
    w
}

/// 车道CV：注入字体语境的 run 宽度求和——与 [`emit_run_font_adj`] 的
/// 逐字形 advance **同源同序**（hmtx 度量、缺字回落分支逐项一致：
/// gid 缺失/0 → ASCII 回落 Hershey advance、其余 0.6em；advance 缺表 →
/// 0.6em）。text-anchor / textLength 的锚定宽度必须走本函数而非
/// [`run_advance_ls`]——后者是 Helvetica AFM 表，与发射期 hmtx 劈叉
/// （G1：middle/end 锚定整体右偏 deficit/2、deficit；G2：textLength
/// natural 低估 → 调整量过冲）。零宽格式符跳过（同发射期：不占
/// advance 也不加 ls，注意 [`run_advance_ls`] 的 Hershey 口径是加 ls
/// ——两者各自与自己的发射路径一致，勿互相"统一"）。
#[cfg(feature = "text-full")]
pub fn run_advance_ls_font(text: &str, fs: f32, ls: f32, ws: f32, font: &crate::font::Font) -> f32 {
    let mut w = 0.0;
    for ch in text.chars() {
        if is_zero_width_format(ch) {
            continue;
        }
        let sp = if ch == ' ' { ws } else { 0.0 };
        w += match font.glyph_id(ch as u32) {
            Some(g) if g != 0 => match font.advance_em(g) {
                Some(a) => fs * a + ls + sp,
                None => fs * 0.6 + ls + sp,
            },
            _ => {
                // 车道NA：缺字别名同发射臂（emit_run_font_adj）——'−' 等
                // 非 ASCII 缺字按 ASCII 臂口径计 advance，两臂同源
                let ch = if (ch as u32) >= 0x7f {
                    ascii_alias(ch)
                } else {
                    ch
                };
                if (ch as u32) < 0x7f {
                    fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM) + ls + sp
                } else {
                    fs * 0.6 + ls + sp
                }
            }
        };
    }
    w
}

/// 车道AO：一段文本里**消耗 advance 的字符数**（零宽格式符除外，空格
/// 与控制符计入——与 [`emit_run`] 的推进口径一致）。textLength spacing
/// 模式的逐 advance 分配分母。
pub fn advance_count(text: &str) -> usize {
    text.chars().filter(|&ch| !is_zero_width_format(ch)).count()
}

/// 一条笔画折线展开为四边形环集（用户空间：法向偏移 ±hw、两端延伸 hw
/// 盖住转角接头），经矩阵 `m` 烘焙进 `out`。
fn stroke_quads(pts: &[(f32, f32)], hw: f32, m: Mat2x3, out: &mut PathGeom) {
    for i in 0..pts.len().saturating_sub(1) {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[i + 1];
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = libm::sqrtf(dx * dx + dy * dy);
        if len < 1e-6 {
            continue;
        }
        let (ux, uy) = (dx / len, dy / len);
        let (nx, ny) = (-uy * hw, ux * hw);
        let (ex, ey) = (ux * hw * STROKE_EXT, uy * hw * STROKE_EXT);
        let (ax, ay) = (x0 - ex, y0 - ey);
        let (bx, by) = (x1 + ex, y1 + ey);
        let ring = [
            m.apply(ax + nx, ay + ny),
            m.apply(bx + nx, by + ny),
            m.apply(bx - nx, by - ny),
            m.apply(ax - nx, ay - ny),
        ];
        out.subs.push(Subpath {
            pts: Vec::from(ring),
            closed: true,
        });
    }
}

/// 非 ASCII 降级 □：1em advance 格内 [0.20, 0.80]em × [-0.62em, +0.02em]
/// （基线上下）的方框环。0.6em 尺寸为 oracle 差分调优：落在 Chrome CJK
/// 字形的高密度墨区，比满格方框少 ~1pp 差分（可见降级标记保留）。
fn fallback_box(pen_x: f32, baseline: f32, fs: f32, hw: f32, m: Mat2x3, out: &mut PathGeom) {
    let (x0, x1) = (pen_x + 0.20 * fs, pen_x + 0.80 * fs);
    let (y0, y1) = (baseline - 0.62 * fs, baseline + 0.02 * fs);
    let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)];
    stroke_quads(&ring, hw, m, out);
}

/// 装饰条（text-decoration）：`x0..x1`（run advance 带）× 基线相对
/// 偏移带的矩形环，经矩阵 `m` 烘焙进 `out`。kind 取 [`Deco`] 位。
/// 带（y 向下）：underline = 基线下 [off, off+thick]；line-through =
/// 基线上 STRIKE 中心 ±thick/2；overline = 基线上 [cap, cap+thick]。
/// `under_extra_em`：underline 偏移的附加 em（车道BG：dominant-baseline
/// central 的 Chrome 装饰定位异常，+0.11em；其余基线值传 0.0）。
#[allow(clippy::too_many_arguments)]
pub fn decoration_bar(
    kind: u8,
    x0: f32,
    x1: f32,
    baseline: f32,
    fs: f32,
    m: Mat2x3,
    out: &mut PathGeom,
    under_extra_em: f32,
) {
    let t = fs * DECO_THICK_EM;
    let (top, bot) = if kind & Deco::UNDERLINE != 0 {
        let off = UNDERLINE_OFFSET_EM + under_extra_em;
        (baseline + off * fs, baseline + (off + DECO_THICK_EM) * fs)
    } else if kind & Deco::LINE_THROUGH != 0 {
        let c = STRIKE_OFFSET_EM * fs;
        (baseline - c - t * 0.5, baseline - c + t * 0.5)
    } else if kind & Deco::OVERLINE != 0 {
        (
            baseline - (OVERLINE_OFFSET_EM + DECO_THICK_EM) * fs,
            baseline - OVERLINE_OFFSET_EM * fs,
        )
    } else {
        return;
    };
    // stroke_quads 会向两侧各展 hw——环先内缩 hw，外缘恰落在带上
    let hw = t * 0.5;
    let (y0, y1) = (top + hw, bot - hw);
    let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)];
    stroke_quads(&ring, hw, m, out);
}

/// 发射一段同风格文本运行：从 `(pen_x, baseline)`（用户空间，baseline
/// 为 alphabetic 基线）起逐字符排版，几何（笔画四边形环）按矩阵 `m`
/// 烘焙进 `out`。返回消耗的 advance（用户单位）。
///
/// 每字形 ink 在自己的 advance 格内居中（Helvetica 光学边距的近似）；
/// `bold` 加宽笔画（advance 不变——Chrome 实测粗体 CJK/拉丁 advance
/// 不变）；`ls` = letter-spacing 逐字符加排（用户单位）。
/// Tier C（ADR-0008）：注入字体的 run 发射。真字形填充（y 翻转 +
/// m 变换），advance 用字体度量；未映射码点回落 Hershey 单字
///（ASCII）或 tofu 框（>0x7F）。`bold` = 伪粗体合成（[`fake_bold_em`]，
/// 车道CR；纯 hmtx 管线无变体概念，注入恒按需合成）。
/// 返回 run 宽度（与 [`emit_run`] 同语义）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "text-full")]
pub fn emit_run_font(
    text: &str,
    pen_x: f32,
    baseline: f32,
    fs: f32,
    ls: f32,
    m: Mat2x3,
    out: &mut PathGeom,
    font: &Font,
    bold: bool,
) -> f32 {
    emit_run_font_adj(
        text, pen_x, baseline, fs, ls, 0.0, 0.0, 0.0, m, out, font, bold,
    )
}

/// 车道CR：伪粗体合成线宽（em 分数）。Chrome/Skia fake bold 语义逐值
/// 移植（SkTextFormatParams.h `kStdFakeBoldInterpKeys/Values`）：
/// 字号 ≤9px → size/24，≥36px → size/32，之间对**分数**线性插值
/// （探针 cr1/cr2/cr4 实证：500 不合成、600/700/900 全同 = 二值触发；
/// 加宽对称 ±w/2、advance 不变）。应用形态 = 轮廓 stroke-and-fill
/// 膨胀（useStrokeForFakeBold：kFrameAndFill + SkPaint 默认 Miter
/// join/miterlimit 4；字形轮廓恒闭合，cap 无效）。
fn fake_bold_em(fs: f32) -> f32 {
    if fs <= 9.0 {
        1.0 / 24.0
    } else if fs >= 36.0 {
        1.0 / 32.0
    } else {
        1.0 / 24.0 + (fs - 9.0) * (1.0 / 32.0 - 1.0 / 24.0) / 27.0
    }
}

/// 车道NP：Tier C 字重配平微加粗（em 分数；0 = 关闭，逐字节等价 CR 基线）。
/// KD 实测注入字体口径下我方系统性欠墨（墨量比 0.78-0.88，边缘:核心
/// ≈9:1——Skia 文本 mask gamma 使字形边缘更重）；本常数以对称膨胀
/// （同 [`fake_bold_em`] 的 stroke-and-fill 机制，宽度叠加）做
/// stem-darkening 式补偿。**九档扫描终判：全局常数档销案**（0.008
/// 最优公共档也只有 5/8 枚改善 < 6/8 判据；paint-order ×2 /
/// text-inline-size-201 全档位结构性恶化；全表面 +2 枚净收益的代价
/// 是 pathlength-003 翻红）——证据曲线见
/// tests/oracle/reports/weight-probe-np.md。常数保留作未来
/// 「fill-only + 非 bold」语义分支配平的现成杠杆，勿以全局档启用。
#[cfg(feature = "text-full")]
const WEIGHT_TRIM_EM: f32 = 0.0;

/// 有向面积（鞋带公式；>0 = 逆时针）。仅用于伪粗体展开环的绕向对齐。
#[cfg(feature = "text-full")]
fn poly_sarea(pts: &[(f32, f32)]) -> f32 {
    let n = pts.len();
    let mut s = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        s += pts[i].0 * pts[j].1 - pts[j].0 * pts[i].1;
    }
    s * 0.5
}

/// 车道AO：textLength `spacing` 模式的注入字体发射（同
/// [`emit_run_adj`] 语义：每个 advance 追加 `extra`；`extra_final` =
/// **最后一个消耗 advance 的字符**的追加值——车道CV：Chrome 均摊在
/// n−1 个字间隙上，末字形 advance 终点恰落在 x+textLength，末位补 0）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "text-full")]
pub fn emit_run_font_adj(
    text: &str,
    pen_x: f32,
    baseline: f32,
    fs: f32,
    ls: f32,
    ws: f32,
    extra: f32,
    extra_final: f32,
    m: Mat2x3,
    out: &mut PathGeom,
    font: &Font,
    bold: bool,
) -> f32 {
    // 车道CP：outline() 契约 = **em 归一化**用户空间坐标（font.rs
    // simple_outline：xs/upem、ys/upem，y 向上）→ 直接乘 fs 得用户单位。
    // 旧 `scale = fs / units_per_em` 把 em 坐标再除 upem——字形被压成
    // 亚像素点（'A' 0.668em @16px → 0.005px 宽，FFI 车道 DL 级实证），
    // 自 540ffa3d 接线起即坏（CH 报告的 43/43 与本实证矛盾，归因存疑）。
    // advance 侧 `fs * advance_em` 本就是 em×fs 语义，与此对齐。
    let mut x = pen_x;
    // 车道CV：剩余 advance 数（含当前）——最后一个是末字形
    let mut remaining = advance_count(text);
    for ch in text.chars() {
        if is_zero_width_format(ch) {
            continue;
        }
        let ex = if remaining <= 1 { extra_final } else { extra };
        remaining -= 1;
        // 车道EJ：word-spacing 仅空格（U+0020）追加
        let sp = if ch == ' ' { ws } else { 0.0 };
        let gid = match font.glyph_id(ch as u32) {
            Some(g) if g != 0 => g,
            _ => {
                // 缺字：先过缺字别名（车道NA——'−' 等按 '-' 走 ASCII 臂，
                // 不再出 tofu 框）；ASCII 回落 Hershey 单字推进；其余 tofu 框
                let ch = if (ch as u32) >= 0x7f {
                    ascii_alias(ch)
                } else {
                    ch
                };
                if (ch as u32) < 0x7f {
                    x += fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM) + ls + sp + ex;
                } else {
                    let adv = fs * 0.6;
                    fallback_box(x, baseline, fs, fs * 0.05, m, out);
                    x += adv + ls + sp + ex;
                }
                continue;
            }
        };
        if let Some(o) = font.outline(gid) {
            // 车道CR：伪粗体在正确缩放（em×fs）之后、矩阵烘焙之前施加
            // ——轮廓 stroke-and-fill 膨胀 = 原轮廓 + stroke 展开环的
            // nonzero 并集（对齐 Skia kFrameAndFill）。展开环绕向必须
            // 对齐源轮廓：stroke_outline 的带片/楔形方向跟输入相反且
            // 相互混向（独立描边 nonzero 无感，叠加原填充时把带内原墨
            // 抵成反墨洞——ours-bold 变淡指纹，cr-bold 探针实证）。
            // tofu/ASCII 缺字回落臂不合成（登记近似：Chrome 侧缺字
            // 走系统回退真字形，本就不可逐位对齐）。
            for sp in &o.subs {
                // 用户空间（未乘 m）：膨胀与绕向判定都在此空间做
                let local: Vec<(f32, f32)> = sp
                    .pts
                    .iter()
                    .map(|&(gx, gy)| (x + gx * fs, baseline - gy * fs))
                    .collect();
                if local.len() < 3 {
                    continue;
                }
                // 车道NP：字重配平与伪粗体共用同一展开机制，宽度叠加
                //（bold = CR 语义原值；trim = 配平增量）。trim=0 且非
                // bold 时跳过 = 与既有输出逐位同。
                let widen = fs * (WEIGHT_TRIM_EM + if bold { fake_bold_em(fs) } else { 0.0 });
                if widen > 0.0 {
                    let mut g = PathGeom::default();
                    g.subs.push(Subpath {
                        pts: local.clone(),
                        closed: true,
                    });
                    let src_pos = poly_sarea(&local) >= 0.0;
                    for ring in stroke_outline(&g, widen, Cap::Butt, &[], 0.0, LineJoin::Miter, 4.0)
                    {
                        let mut ring = ring;
                        if (poly_sarea(&ring) >= 0.0) != src_pos {
                            ring.reverse();
                        }
                        let pts: Vec<(f32, f32)> =
                            ring.iter().map(|&(ux, uy)| m.apply(ux, uy)).collect();
                        if pts.len() >= 3 {
                            out.subs.push(Subpath { pts, closed: true });
                        }
                    }
                }
                let pts: Vec<(f32, f32)> = local.iter().map(|&(ux, uy)| m.apply(ux, uy)).collect();
                out.subs.push(Subpath { pts, closed: true });
            }
            x += fs * o.advance_em + ls + sp + ex;
        } else {
            x += fs * 0.6 + ls + sp + ex;
        }
    }
    x - pen_x
}

#[allow(clippy::too_many_arguments)]
pub fn emit_run(
    text: &str,
    pen_x: f32,
    baseline: f32,
    fs: f32,
    bold: bool,
    ls: f32,
    m: Mat2x3,
    out: &mut PathGeom,
) -> f32 {
    emit_run_adj(text, pen_x, baseline, fs, bold, ls, 0.0, 0.0, 0.0, m, out)
}

/// 车道AO：textLength `spacing` 模式的发射——每个 advance 追加
/// `extra`（（textLength − 自然宽）/（advance 数 − 1），可为负；
/// 车道CV 修正：Chrome 均摊在 **n−1 个字间隙**上——末字形 advance
/// 终点恰落在 x+textLength（cv3 探针 Ha/Ham/HH/H_i 逐位实证），故
/// `extra_final`（末字形追加值）由调用方传 0；n=1 时整段不调整
///（间隙数 0，Chrome H300 探针实证）。字形形状不变。返回 run 总
/// advance（= 自然宽 + (N−1)×extra + extra_final）。
#[allow(clippy::too_many_arguments)]
pub fn emit_run_adj(
    text: &str,
    pen_x: f32,
    baseline: f32,
    fs: f32,
    bold: bool,
    ls: f32,
    ws: f32,
    extra: f32,
    extra_final: f32,
    m: Mat2x3,
    out: &mut PathGeom,
) -> f32 {
    let gs = fs * CAP_EM / CAP_UNITS; // Hershey 单位 → 用户单位
    let hw = fs * (STROKE_EM + if bold { BOLD_EXTRA_EM } else { 0.0 }) * 0.5;
    let mut x = pen_x;
    // 车道CV：剩余 advance 数（含当前）——最后一个是末字形
    let mut remaining = advance_count(text);
    for ch in text.chars() {
        if is_zero_width_format(ch) {
            continue; // 零宽不可见：不占 advance、不出字形
        }
        // 车道NA：非 ASCII 先过缺字别名（'−'→'-' 等）——advance_em 与
        // GLYPHS 消费同一别名，advance/墨迹同源；真无别名（CJK）落原臂。
        let ch = if (ch as u32) >= 0x7f {
            ascii_alias(ch)
        } else {
            ch
        };
        let ex = if remaining <= 1 { extra_final } else { extra };
        remaining -= 1;
        // 字形 advance（不含 letter-spacing）——ink 在自身格内居中；
        // ls 在格后追加（Chrome 语义：字距不挪字形墨位）；
        // ws 仅空格追加（车道EJ）
        let adv0 = fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM);
        let adv = adv0 + ls + if ch == ' ' { ws } else { 0.0 } + ex;
        let c = ch as u32;
        if c < 0x20 || ch == ' ' {
            x += adv;
            continue;
        }
        if c >= 0x7f {
            fallback_box(x, baseline, fs, hw, m, out);
            x += adv;
            continue;
        }
        let g = &GLYPHS[(c - 0x20) as usize];
        // ink 居中于自身 advance 格
        let ink_w_user = g.ink_w as f32 * gs;
        let x_off = x + (adv0 - ink_w_user) * 0.5;
        for stroke in g.strokes {
            let pts: Vec<(f32, f32)> = stroke
                .iter()
                .map(|p| (x_off + p.x as f32 * gs, baseline - p.y as f32 * gs))
                .collect();
            stroke_quads(&pts, hw, m, out);
        }
        x += adv;
    }
    x - pen_x
}

// ---------------------------------------------------------------------------
// 车道AL：textPath 沿路径排版（弧长定位 + 切线角旋转）
// ---------------------------------------------------------------------------

/// 一条可做弧长定位的折线轨道（textPath 引用路径的平化折线，**用户
/// 空间**——字形逐个以用户空间切线角旋转后再经元素矩阵进设备空间，
/// 与 SVG 语义一致；非均匀变换下的字符变形随元素矩阵自然生效）。
///
/// 多子路径拼接：子路径按序首尾相连，跨子路径的跳跃不计弧长（与
/// [`polyline_length`] / getTotalLength 的逐子路径求和口径一致）——
/// 边界处是零长段，采样时跳过（方向取下一非零段）。闭合子路径补
/// 隐式闭合边（与 polyline_length 同约定）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathTrack {
    /// 拼接后的折线顶点（≥2 才有效）。
    pub pts: Vec<(f32, f32)>,
    /// 累积弧长（cum[i] = pts[0..=i] 的折线长，cum[0] = 0）。
    pub cum: Vec<f32>,
    /// 总弧长。
    pub total: f32,
}

impl PathTrack {
    /// 从平化几何构建轨道。空几何 / 单点 → 空轨道（total 0，采样恒
    /// None——textPath 无路径可排即不渲染，同 Chrome）。
    pub fn build(g: &PathGeom) -> Self {
        let mut pts: Vec<(f32, f32)> = Vec::new();
        for sub in &g.subs {
            for &p in &sub.pts {
                // 重复点（闭合折线末点==首点约定 / 退化平化段）跳过
                if pts.last() == Some(&p) {
                    continue;
                }
                pts.push(p);
            }
            // Z 隐式闭合边：末点≠首点时补首点形成闭合段
            if sub.closed && sub.pts.len() >= 2 {
                let first = sub.pts[0];
                if pts.last() != Some(&first) {
                    pts.push(first);
                }
            }
        }
        if pts.len() < 2 {
            return Self::default();
        }
        let mut cum = Vec::with_capacity(pts.len());
        cum.push(0.0f32);
        for k in 1..pts.len() {
            let (dx, dy) = (pts[k].0 - pts[k - 1].0, pts[k].1 - pts[k - 1].1);
            cum.push(cum[k - 1] + libm::sqrtf(dx * dx + dy * dy));
        }
        let total = cum[cum.len() - 1];
        Self { pts, cum, total }
    }

    /// 弧长 `s` 处的点与切线角（弧度，用户空间 y 向下坐标系——角度
    /// 顺时针为正，直接进 Mat2x3::rotation 同语义）。`s` 越出
    /// [0, total] 或空轨道 → None（字形不渲染：SVG 规范——路径端点
    /// 之外的字符不描绘）。s == total 落在末个非零段的 t=1 端点。
    pub fn sample(&self, s: f32) -> Option<((f32, f32), f32)> {
        if self.total <= 0.0 || !(0.0..=self.total).contains(&s) {
            return None;
        }
        // 线性扫描定位段（轨道短——字形级别调用，二分无谓复杂度）；
        // 零长段（子路径边界跳跃）方向未定义，跳过不作为候选
        let n = self.pts.len();
        let mut cand: Option<usize> = None;
        for k in 0..n - 1 {
            let b = self.cum[k + 1];
            if b - self.cum[k] <= 1e-6 {
                continue;
            }
            cand = Some(k);
            if s <= b {
                break;
            }
        }
        let k = cand?;
        let (a, b) = (self.cum[k], self.cum[k + 1]);
        let t = ((s - a) / (b - a)).clamp(0.0, 1.0);
        let (p0, p1) = (self.pts[k], self.pts[k + 1]);
        let pt = (p0.0 + (p1.0 - p0.0) * t, p0.1 + (p1.1 - p0.1) * t);
        let ang = libm::atan2f(p1.1 - p0.1, p1.0 - p0.0);
        Some((pt, ang))
    }
}

/// 沿 [`PathTrack`] 发射一段同风格文本：逐字符 advance 累计走弧长，
/// 字形 advance 格的**中点**落在轨道采样点上、基线沿切线方向旋转
///（SVG textPath 语义——字符中点在路径上）。`dy` 为基线垂向偏移
///（tspan dy 在路径语境的语义：沿字形局部 y = 垂直路径方向平移）。
/// 越出轨道 [0,total] 的字符不描绘（但照常消耗 advance——与 Chrome
/// 一致，路径末端截断后无可见差异）。
///
/// 字形几何在**轨道局部空间**产生（轨道由调用方按 text 元素用户空间
/// 平化，车道AP：最终元素累积矩阵由 emit_text_geom 统一施加——此前
/// 字形矩阵自带 m 且 emit 再乘 m，变换语境下双重施放）。`font` =
/// Tier C 注入字体（None = Hershey）。返回结束弧长。
#[allow(clippy::too_many_arguments)]
pub fn emit_run_on_path(
    text: &str,
    start_s: f32,
    track: &PathTrack,
    fs: f32,
    bold: bool,
    ls: f32,
    ws: f32,
    dy: f32,
    out: &mut PathGeom,
    font: Option<&crate::font::Font>,
) -> f32 {
    let mut cur = start_s;
    for ch in text.chars() {
        if is_zero_width_format(ch) {
            continue;
        }
        // 车道DC：on-path 逐字符 advance 与发射相位同源（车道CV 线性口
        // 的路径版）——注入字体走 hmtx（缺字回落分支与 emit_run_font_adj
        // 逐项一致）；无字体 = Hershey AFM（原口径，逐位不变）。此前
        // 游标恒 AFM、字形发射走 hmtx——Tier C 钉字体下整行欠宽
        //（opacity.svg 探针 "textPath" 行 182 vs chrome 207 实证）。
        #[cfg(feature = "text-full")]
        let adv0 = match font {
            Some(f) => match f.glyph_id(ch as u32) {
                Some(g) if g != 0 => fs * f.advance_em(g).unwrap_or(0.6),
                _ => {
                    // 车道NA：缺字别名同 emit_run_font_adj（'−'→'-' 走
                    // ASCII 臂），两臂同源
                    let ch = if (ch as u32) >= 0x7f {
                        ascii_alias(ch)
                    } else {
                        ch
                    };
                    if (ch as u32) < 0x7f {
                        fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM)
                    } else {
                        fs * 0.6
                    }
                }
            },
            None => fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM),
        };
        #[cfg(not(feature = "text-full"))]
        let adv0 = fs * (advance_em(ch) * ADV_SCALE + LETTER_SPACING_EM);
        // 车道EJ：ws 仅空格追加（word-spacing on path，同线性口径）
        let adv = adv0 + ls + if ch == ' ' { ws } else { 0.0 };
        let c = ch as u32;
        if c >= 0x20 && ch != ' ' {
            let mid = cur + adv0 * 0.5;
            if let Some((p, ang)) = track.sample(mid) {
                // 局部（本次 emit_run 调用的笔位 0 基线坐标——字形格中心
                // 在局部 adv0/2 处）→ 轨道局部空间：T(p)·R(ang)·T(−adv0/2, dy)。
                // 平移量是**半格**不是运行绝对 mid：逐字符 emit_run 的局部
                // 原点都在各自格起点。元素累积矩阵由 emit_text_geom 施加。
                let (sa, ca) = (libm::sinf(ang), libm::cosf(ang));
                let m_g = Mat2x3::translation(p.0, p.1)
                    .mul(&Mat2x3 {
                        a: ca,
                        b: sa,
                        c: -sa,
                        d: ca,
                        e: 0.0,
                        f: 0.0,
                    })
                    .mul(&Mat2x3::translation(-adv0 * 0.5, dy));
                let mut buf = [0u8; 4];
                let s1 = ch.encode_utf8(&mut buf);
                #[cfg(feature = "text-full")]
                match font {
                    Some(f) => {
                        emit_run_font(s1, 0.0, 0.0, fs, 0.0, m_g, out, f, bold);
                    }
                    None => {
                        emit_run(s1, 0.0, 0.0, fs, bold, 0.0, m_g, out);
                    }
                }
                #[cfg(not(feature = "text-full"))]
                {
                    let _ = font;
                    emit_run(s1, 0.0, 0.0, fs, bold, 0.0, m_g, out);
                }
            }
        }
        cur += adv;
    }
    cur
}

// ---------------------------------------------------------------------------
// 字形表（由 Hershey Roman Simplex rowmans.jhf 生成；ASCII 32..=127 顺序，
// 末项 DEL 不参与渲染）
// ---------------------------------------------------------------------------

pub static GLYPHS: [Glyph; 96] = [
    Glyph {
        ink_w: 16,
        strokes: &[],
    }, // 0x20
    Glyph {
        ink_w: 10,
        strokes: &[
            &[GlyphPt { x: 5, y: 21 }, GlyphPt { x: 5, y: 7 }],
            &[
                GlyphPt { x: 5, y: 2 },
                GlyphPt { x: 4, y: 1 },
                GlyphPt { x: 5, y: 0 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 5, y: 2 },
            ],
        ],
    }, // 0x21
    Glyph {
        ink_w: 16,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 14 }],
            &[GlyphPt { x: 12, y: 21 }, GlyphPt { x: 12, y: 14 }],
        ],
    }, // 0x22
    Glyph {
        ink_w: 21,
        strokes: &[
            &[GlyphPt { x: 11, y: 25 }, GlyphPt { x: 4, y: -7 }],
            &[GlyphPt { x: 17, y: 25 }, GlyphPt { x: 10, y: -7 }],
            &[GlyphPt { x: 4, y: 12 }, GlyphPt { x: 18, y: 12 }],
            &[GlyphPt { x: 3, y: 6 }, GlyphPt { x: 17, y: 6 }],
        ],
    }, // 0x23
    Glyph {
        ink_w: 20,
        strokes: &[
            &[GlyphPt { x: 8, y: 25 }, GlyphPt { x: 8, y: -4 }],
            &[GlyphPt { x: 12, y: 25 }, GlyphPt { x: 12, y: -4 }],
            &[
                GlyphPt { x: 17, y: 18 },
                GlyphPt { x: 15, y: 20 },
                GlyphPt { x: 12, y: 21 },
                GlyphPt { x: 8, y: 21 },
                GlyphPt { x: 5, y: 20 },
                GlyphPt { x: 3, y: 18 },
                GlyphPt { x: 3, y: 16 },
                GlyphPt { x: 4, y: 14 },
                GlyphPt { x: 5, y: 13 },
                GlyphPt { x: 7, y: 12 },
                GlyphPt { x: 13, y: 10 },
                GlyphPt { x: 15, y: 9 },
                GlyphPt { x: 16, y: 8 },
                GlyphPt { x: 17, y: 6 },
                GlyphPt { x: 17, y: 3 },
                GlyphPt { x: 15, y: 1 },
                GlyphPt { x: 12, y: 0 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 5, y: 1 },
                GlyphPt { x: 3, y: 3 },
            ],
        ],
    }, // 0x24
    Glyph {
        ink_w: 24,
        strokes: &[
            &[GlyphPt { x: 21, y: 21 }, GlyphPt { x: 3, y: 0 }],
            &[
                GlyphPt { x: 8, y: 21 },
                GlyphPt { x: 10, y: 19 },
                GlyphPt { x: 10, y: 17 },
                GlyphPt { x: 9, y: 15 },
                GlyphPt { x: 7, y: 14 },
                GlyphPt { x: 5, y: 14 },
                GlyphPt { x: 3, y: 16 },
                GlyphPt { x: 3, y: 18 },
                GlyphPt { x: 4, y: 20 },
                GlyphPt { x: 6, y: 21 },
                GlyphPt { x: 8, y: 21 },
                GlyphPt { x: 10, y: 20 },
                GlyphPt { x: 13, y: 19 },
                GlyphPt { x: 16, y: 19 },
                GlyphPt { x: 19, y: 20 },
                GlyphPt { x: 21, y: 21 },
            ],
            &[
                GlyphPt { x: 17, y: 7 },
                GlyphPt { x: 15, y: 6 },
                GlyphPt { x: 14, y: 4 },
                GlyphPt { x: 14, y: 2 },
                GlyphPt { x: 16, y: 0 },
                GlyphPt { x: 18, y: 0 },
                GlyphPt { x: 20, y: 1 },
                GlyphPt { x: 21, y: 3 },
                GlyphPt { x: 21, y: 5 },
                GlyphPt { x: 19, y: 7 },
                GlyphPt { x: 17, y: 7 },
            ],
        ],
    }, // 0x25
    Glyph {
        ink_w: 26,
        strokes: &[&[
            GlyphPt { x: 23, y: 12 },
            GlyphPt { x: 23, y: 13 },
            GlyphPt { x: 22, y: 14 },
            GlyphPt { x: 21, y: 14 },
            GlyphPt { x: 20, y: 13 },
            GlyphPt { x: 19, y: 11 },
            GlyphPt { x: 17, y: 6 },
            GlyphPt { x: 15, y: 3 },
            GlyphPt { x: 13, y: 1 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 7, y: 0 },
            GlyphPt { x: 5, y: 1 },
            GlyphPt { x: 4, y: 2 },
            GlyphPt { x: 3, y: 4 },
            GlyphPt { x: 3, y: 6 },
            GlyphPt { x: 4, y: 8 },
            GlyphPt { x: 5, y: 9 },
            GlyphPt { x: 12, y: 13 },
            GlyphPt { x: 13, y: 14 },
            GlyphPt { x: 14, y: 16 },
            GlyphPt { x: 14, y: 18 },
            GlyphPt { x: 13, y: 20 },
            GlyphPt { x: 11, y: 21 },
            GlyphPt { x: 9, y: 20 },
            GlyphPt { x: 8, y: 18 },
            GlyphPt { x: 8, y: 16 },
            GlyphPt { x: 9, y: 13 },
            GlyphPt { x: 11, y: 10 },
            GlyphPt { x: 16, y: 3 },
            GlyphPt { x: 18, y: 1 },
            GlyphPt { x: 20, y: 0 },
            GlyphPt { x: 22, y: 0 },
            GlyphPt { x: 23, y: 1 },
            GlyphPt { x: 23, y: 2 },
        ]],
    }, // 0x26
    Glyph {
        ink_w: 10,
        strokes: &[&[
            GlyphPt { x: 5, y: 19 },
            GlyphPt { x: 4, y: 20 },
            GlyphPt { x: 5, y: 21 },
            GlyphPt { x: 6, y: 20 },
            GlyphPt { x: 6, y: 18 },
            GlyphPt { x: 5, y: 16 },
            GlyphPt { x: 4, y: 15 },
        ]],
    }, // 0x27
    Glyph {
        ink_w: 14,
        strokes: &[&[
            GlyphPt { x: 11, y: 25 },
            GlyphPt { x: 9, y: 23 },
            GlyphPt { x: 7, y: 20 },
            GlyphPt { x: 5, y: 16 },
            GlyphPt { x: 4, y: 11 },
            GlyphPt { x: 4, y: 7 },
            GlyphPt { x: 5, y: 2 },
            GlyphPt { x: 7, y: -2 },
            GlyphPt { x: 9, y: -5 },
            GlyphPt { x: 11, y: -7 },
        ]],
    }, // 0x28
    Glyph {
        ink_w: 14,
        strokes: &[&[
            GlyphPt { x: 3, y: 25 },
            GlyphPt { x: 5, y: 23 },
            GlyphPt { x: 7, y: 20 },
            GlyphPt { x: 9, y: 16 },
            GlyphPt { x: 10, y: 11 },
            GlyphPt { x: 10, y: 7 },
            GlyphPt { x: 9, y: 2 },
            GlyphPt { x: 7, y: -2 },
            GlyphPt { x: 5, y: -5 },
            GlyphPt { x: 3, y: -7 },
        ]],
    }, // 0x29
    Glyph {
        ink_w: 16,
        strokes: &[
            &[GlyphPt { x: 8, y: 21 }, GlyphPt { x: 8, y: 9 }],
            &[GlyphPt { x: 3, y: 18 }, GlyphPt { x: 13, y: 12 }],
            &[GlyphPt { x: 13, y: 18 }, GlyphPt { x: 3, y: 12 }],
        ],
    }, // 0x2a
    Glyph {
        ink_w: 26,
        strokes: &[
            &[GlyphPt { x: 13, y: 18 }, GlyphPt { x: 13, y: 0 }],
            &[GlyphPt { x: 4, y: 9 }, GlyphPt { x: 22, y: 9 }],
        ],
    }, // 0x2b
    Glyph {
        ink_w: 10,
        strokes: &[&[
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 5, y: 0 },
            GlyphPt { x: 4, y: 1 },
            GlyphPt { x: 5, y: 2 },
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 6, y: -1 },
            GlyphPt { x: 5, y: -3 },
            GlyphPt { x: 4, y: -4 },
        ]],
    }, // 0x2c
    Glyph {
        ink_w: 26,
        strokes: &[&[GlyphPt { x: 4, y: 9 }, GlyphPt { x: 22, y: 9 }]],
    }, // 0x2d
    Glyph {
        ink_w: 10,
        strokes: &[&[
            GlyphPt { x: 5, y: 2 },
            GlyphPt { x: 4, y: 1 },
            GlyphPt { x: 5, y: 0 },
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 5, y: 2 },
        ]],
    }, // 0x2e
    Glyph {
        ink_w: 22,
        strokes: &[&[GlyphPt { x: 20, y: 25 }, GlyphPt { x: 2, y: -7 }]],
    }, // 0x2f
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 9, y: 21 },
            GlyphPt { x: 6, y: 20 },
            GlyphPt { x: 4, y: 17 },
            GlyphPt { x: 3, y: 12 },
            GlyphPt { x: 3, y: 9 },
            GlyphPt { x: 4, y: 4 },
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 9, y: 0 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 14, y: 1 },
            GlyphPt { x: 16, y: 4 },
            GlyphPt { x: 17, y: 9 },
            GlyphPt { x: 17, y: 12 },
            GlyphPt { x: 16, y: 17 },
            GlyphPt { x: 14, y: 20 },
            GlyphPt { x: 11, y: 21 },
            GlyphPt { x: 9, y: 21 },
        ]],
    }, // 0x30
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 6, y: 17 },
            GlyphPt { x: 8, y: 18 },
            GlyphPt { x: 11, y: 21 },
            GlyphPt { x: 11, y: 0 },
        ]],
    }, // 0x31
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 4, y: 16 },
            GlyphPt { x: 4, y: 17 },
            GlyphPt { x: 5, y: 19 },
            GlyphPt { x: 6, y: 20 },
            GlyphPt { x: 8, y: 21 },
            GlyphPt { x: 12, y: 21 },
            GlyphPt { x: 14, y: 20 },
            GlyphPt { x: 15, y: 19 },
            GlyphPt { x: 16, y: 17 },
            GlyphPt { x: 16, y: 15 },
            GlyphPt { x: 15, y: 13 },
            GlyphPt { x: 13, y: 10 },
            GlyphPt { x: 3, y: 0 },
            GlyphPt { x: 17, y: 0 },
        ]],
    }, // 0x32
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 5, y: 21 },
            GlyphPt { x: 16, y: 21 },
            GlyphPt { x: 10, y: 13 },
            GlyphPt { x: 13, y: 13 },
            GlyphPt { x: 15, y: 12 },
            GlyphPt { x: 16, y: 11 },
            GlyphPt { x: 17, y: 8 },
            GlyphPt { x: 17, y: 6 },
            GlyphPt { x: 16, y: 3 },
            GlyphPt { x: 14, y: 1 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 5, y: 1 },
            GlyphPt { x: 4, y: 2 },
            GlyphPt { x: 3, y: 4 },
        ]],
    }, // 0x33
    Glyph {
        ink_w: 20,
        strokes: &[
            &[
                GlyphPt { x: 13, y: 21 },
                GlyphPt { x: 3, y: 7 },
                GlyphPt { x: 18, y: 7 },
            ],
            &[GlyphPt { x: 13, y: 21 }, GlyphPt { x: 13, y: 0 }],
        ],
    }, // 0x34
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 15, y: 21 },
            GlyphPt { x: 5, y: 21 },
            GlyphPt { x: 4, y: 12 },
            GlyphPt { x: 5, y: 13 },
            GlyphPt { x: 8, y: 14 },
            GlyphPt { x: 11, y: 14 },
            GlyphPt { x: 14, y: 13 },
            GlyphPt { x: 16, y: 11 },
            GlyphPt { x: 17, y: 8 },
            GlyphPt { x: 17, y: 6 },
            GlyphPt { x: 16, y: 3 },
            GlyphPt { x: 14, y: 1 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 5, y: 1 },
            GlyphPt { x: 4, y: 2 },
            GlyphPt { x: 3, y: 4 },
        ]],
    }, // 0x35
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 16, y: 18 },
            GlyphPt { x: 15, y: 20 },
            GlyphPt { x: 12, y: 21 },
            GlyphPt { x: 10, y: 21 },
            GlyphPt { x: 7, y: 20 },
            GlyphPt { x: 5, y: 17 },
            GlyphPt { x: 4, y: 12 },
            GlyphPt { x: 4, y: 7 },
            GlyphPt { x: 5, y: 3 },
            GlyphPt { x: 7, y: 1 },
            GlyphPt { x: 10, y: 0 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 14, y: 1 },
            GlyphPt { x: 16, y: 3 },
            GlyphPt { x: 17, y: 6 },
            GlyphPt { x: 17, y: 7 },
            GlyphPt { x: 16, y: 10 },
            GlyphPt { x: 14, y: 12 },
            GlyphPt { x: 11, y: 13 },
            GlyphPt { x: 10, y: 13 },
            GlyphPt { x: 7, y: 12 },
            GlyphPt { x: 5, y: 10 },
            GlyphPt { x: 4, y: 7 },
        ]],
    }, // 0x36
    Glyph {
        ink_w: 20,
        strokes: &[
            &[GlyphPt { x: 17, y: 21 }, GlyphPt { x: 7, y: 0 }],
            &[GlyphPt { x: 3, y: 21 }, GlyphPt { x: 17, y: 21 }],
        ],
    }, // 0x37
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 8, y: 21 },
            GlyphPt { x: 5, y: 20 },
            GlyphPt { x: 4, y: 18 },
            GlyphPt { x: 4, y: 16 },
            GlyphPt { x: 5, y: 14 },
            GlyphPt { x: 7, y: 13 },
            GlyphPt { x: 11, y: 12 },
            GlyphPt { x: 14, y: 11 },
            GlyphPt { x: 16, y: 9 },
            GlyphPt { x: 17, y: 7 },
            GlyphPt { x: 17, y: 4 },
            GlyphPt { x: 16, y: 2 },
            GlyphPt { x: 15, y: 1 },
            GlyphPt { x: 12, y: 0 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 5, y: 1 },
            GlyphPt { x: 4, y: 2 },
            GlyphPt { x: 3, y: 4 },
            GlyphPt { x: 3, y: 7 },
            GlyphPt { x: 4, y: 9 },
            GlyphPt { x: 6, y: 11 },
            GlyphPt { x: 9, y: 12 },
            GlyphPt { x: 13, y: 13 },
            GlyphPt { x: 15, y: 14 },
            GlyphPt { x: 16, y: 16 },
            GlyphPt { x: 16, y: 18 },
            GlyphPt { x: 15, y: 20 },
            GlyphPt { x: 12, y: 21 },
            GlyphPt { x: 8, y: 21 },
        ]],
    }, // 0x38
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 16, y: 14 },
            GlyphPt { x: 15, y: 11 },
            GlyphPt { x: 13, y: 9 },
            GlyphPt { x: 10, y: 8 },
            GlyphPt { x: 9, y: 8 },
            GlyphPt { x: 6, y: 9 },
            GlyphPt { x: 4, y: 11 },
            GlyphPt { x: 3, y: 14 },
            GlyphPt { x: 3, y: 15 },
            GlyphPt { x: 4, y: 18 },
            GlyphPt { x: 6, y: 20 },
            GlyphPt { x: 9, y: 21 },
            GlyphPt { x: 10, y: 21 },
            GlyphPt { x: 13, y: 20 },
            GlyphPt { x: 15, y: 18 },
            GlyphPt { x: 16, y: 14 },
            GlyphPt { x: 16, y: 9 },
            GlyphPt { x: 15, y: 4 },
            GlyphPt { x: 13, y: 1 },
            GlyphPt { x: 10, y: 0 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 5, y: 1 },
            GlyphPt { x: 4, y: 3 },
        ]],
    }, // 0x39
    Glyph {
        ink_w: 10,
        strokes: &[
            &[
                GlyphPt { x: 5, y: 14 },
                GlyphPt { x: 4, y: 13 },
                GlyphPt { x: 5, y: 12 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 5, y: 14 },
            ],
            &[
                GlyphPt { x: 5, y: 2 },
                GlyphPt { x: 4, y: 1 },
                GlyphPt { x: 5, y: 0 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 5, y: 2 },
            ],
        ],
    }, // 0x3a
    Glyph {
        ink_w: 10,
        strokes: &[
            &[
                GlyphPt { x: 5, y: 14 },
                GlyphPt { x: 4, y: 13 },
                GlyphPt { x: 5, y: 12 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 5, y: 14 },
            ],
            &[
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 5, y: 0 },
                GlyphPt { x: 4, y: 1 },
                GlyphPt { x: 5, y: 2 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 6, y: -1 },
                GlyphPt { x: 5, y: -3 },
                GlyphPt { x: 4, y: -4 },
            ],
        ],
    }, // 0x3b
    Glyph {
        ink_w: 24,
        strokes: &[&[
            GlyphPt { x: 20, y: 18 },
            GlyphPt { x: 4, y: 9 },
            GlyphPt { x: 20, y: 0 },
        ]],
    }, // 0x3c
    Glyph {
        ink_w: 26,
        strokes: &[
            &[GlyphPt { x: 4, y: 12 }, GlyphPt { x: 22, y: 12 }],
            &[GlyphPt { x: 4, y: 6 }, GlyphPt { x: 22, y: 6 }],
        ],
    }, // 0x3d
    Glyph {
        ink_w: 24,
        strokes: &[&[
            GlyphPt { x: 4, y: 18 },
            GlyphPt { x: 20, y: 9 },
            GlyphPt { x: 4, y: 0 },
        ]],
    }, // 0x3e
    Glyph {
        ink_w: 18,
        strokes: &[
            &[
                GlyphPt { x: 3, y: 16 },
                GlyphPt { x: 3, y: 17 },
                GlyphPt { x: 4, y: 19 },
                GlyphPt { x: 5, y: 20 },
                GlyphPt { x: 7, y: 21 },
                GlyphPt { x: 11, y: 21 },
                GlyphPt { x: 13, y: 20 },
                GlyphPt { x: 14, y: 19 },
                GlyphPt { x: 15, y: 17 },
                GlyphPt { x: 15, y: 15 },
                GlyphPt { x: 14, y: 13 },
                GlyphPt { x: 13, y: 12 },
                GlyphPt { x: 9, y: 10 },
                GlyphPt { x: 9, y: 7 },
            ],
            &[
                GlyphPt { x: 9, y: 2 },
                GlyphPt { x: 8, y: 1 },
                GlyphPt { x: 9, y: 0 },
                GlyphPt { x: 10, y: 1 },
                GlyphPt { x: 9, y: 2 },
            ],
        ],
    }, // 0x3f
    Glyph {
        ink_w: 27,
        strokes: &[
            &[
                GlyphPt { x: 18, y: 13 },
                GlyphPt { x: 17, y: 15 },
                GlyphPt { x: 15, y: 16 },
                GlyphPt { x: 12, y: 16 },
                GlyphPt { x: 10, y: 15 },
                GlyphPt { x: 9, y: 14 },
                GlyphPt { x: 8, y: 11 },
                GlyphPt { x: 8, y: 8 },
                GlyphPt { x: 9, y: 6 },
                GlyphPt { x: 11, y: 5 },
                GlyphPt { x: 14, y: 5 },
                GlyphPt { x: 16, y: 6 },
                GlyphPt { x: 17, y: 8 },
            ],
            &[
                GlyphPt { x: 12, y: 16 },
                GlyphPt { x: 10, y: 14 },
                GlyphPt { x: 9, y: 11 },
                GlyphPt { x: 9, y: 8 },
                GlyphPt { x: 10, y: 6 },
                GlyphPt { x: 11, y: 5 },
            ],
            &[
                GlyphPt { x: 18, y: 16 },
                GlyphPt { x: 17, y: 8 },
                GlyphPt { x: 17, y: 6 },
                GlyphPt { x: 19, y: 5 },
                GlyphPt { x: 21, y: 5 },
                GlyphPt { x: 23, y: 7 },
                GlyphPt { x: 24, y: 10 },
                GlyphPt { x: 24, y: 12 },
                GlyphPt { x: 23, y: 15 },
                GlyphPt { x: 22, y: 17 },
                GlyphPt { x: 20, y: 19 },
                GlyphPt { x: 18, y: 20 },
                GlyphPt { x: 15, y: 21 },
                GlyphPt { x: 12, y: 21 },
                GlyphPt { x: 9, y: 20 },
                GlyphPt { x: 7, y: 19 },
                GlyphPt { x: 5, y: 17 },
                GlyphPt { x: 4, y: 15 },
                GlyphPt { x: 3, y: 12 },
                GlyphPt { x: 3, y: 9 },
                GlyphPt { x: 4, y: 6 },
                GlyphPt { x: 5, y: 4 },
                GlyphPt { x: 7, y: 2 },
                GlyphPt { x: 9, y: 1 },
                GlyphPt { x: 12, y: 0 },
                GlyphPt { x: 15, y: 0 },
                GlyphPt { x: 18, y: 1 },
                GlyphPt { x: 20, y: 2 },
                GlyphPt { x: 21, y: 3 },
            ],
            &[
                GlyphPt { x: 19, y: 16 },
                GlyphPt { x: 18, y: 8 },
                GlyphPt { x: 18, y: 6 },
                GlyphPt { x: 19, y: 5 },
            ],
        ],
    }, // 0x40
    Glyph {
        ink_w: 18,
        strokes: &[
            &[GlyphPt { x: 9, y: 21 }, GlyphPt { x: 1, y: 0 }],
            &[GlyphPt { x: 9, y: 21 }, GlyphPt { x: 17, y: 0 }],
            &[GlyphPt { x: 4, y: 7 }, GlyphPt { x: 14, y: 7 }],
        ],
    }, // 0x41
    Glyph {
        ink_w: 21,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 21 },
                GlyphPt { x: 13, y: 21 },
                GlyphPt { x: 16, y: 20 },
                GlyphPt { x: 17, y: 19 },
                GlyphPt { x: 18, y: 17 },
                GlyphPt { x: 18, y: 15 },
                GlyphPt { x: 17, y: 13 },
                GlyphPt { x: 16, y: 12 },
                GlyphPt { x: 13, y: 11 },
            ],
            &[
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 13, y: 11 },
                GlyphPt { x: 16, y: 10 },
                GlyphPt { x: 17, y: 9 },
                GlyphPt { x: 18, y: 7 },
                GlyphPt { x: 18, y: 4 },
                GlyphPt { x: 17, y: 2 },
                GlyphPt { x: 16, y: 1 },
                GlyphPt { x: 13, y: 0 },
                GlyphPt { x: 4, y: 0 },
            ],
        ],
    }, // 0x42
    Glyph {
        ink_w: 21,
        strokes: &[&[
            GlyphPt { x: 18, y: 16 },
            GlyphPt { x: 17, y: 18 },
            GlyphPt { x: 15, y: 20 },
            GlyphPt { x: 13, y: 21 },
            GlyphPt { x: 9, y: 21 },
            GlyphPt { x: 7, y: 20 },
            GlyphPt { x: 5, y: 18 },
            GlyphPt { x: 4, y: 16 },
            GlyphPt { x: 3, y: 13 },
            GlyphPt { x: 3, y: 8 },
            GlyphPt { x: 4, y: 5 },
            GlyphPt { x: 5, y: 3 },
            GlyphPt { x: 7, y: 1 },
            GlyphPt { x: 9, y: 0 },
            GlyphPt { x: 13, y: 0 },
            GlyphPt { x: 15, y: 1 },
            GlyphPt { x: 17, y: 3 },
            GlyphPt { x: 18, y: 5 },
        ]],
    }, // 0x43
    Glyph {
        ink_w: 21,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 21 },
                GlyphPt { x: 11, y: 21 },
                GlyphPt { x: 14, y: 20 },
                GlyphPt { x: 16, y: 18 },
                GlyphPt { x: 17, y: 16 },
                GlyphPt { x: 18, y: 13 },
                GlyphPt { x: 18, y: 8 },
                GlyphPt { x: 17, y: 5 },
                GlyphPt { x: 16, y: 3 },
                GlyphPt { x: 14, y: 1 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 4, y: 0 },
            ],
        ],
    }, // 0x44
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 17, y: 21 }],
            &[GlyphPt { x: 4, y: 11 }, GlyphPt { x: 12, y: 11 }],
            &[GlyphPt { x: 4, y: 0 }, GlyphPt { x: 17, y: 0 }],
        ],
    }, // 0x45
    Glyph {
        ink_w: 18,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 17, y: 21 }],
            &[GlyphPt { x: 4, y: 11 }, GlyphPt { x: 12, y: 11 }],
        ],
    }, // 0x46
    Glyph {
        ink_w: 21,
        strokes: &[
            &[
                GlyphPt { x: 18, y: 16 },
                GlyphPt { x: 17, y: 18 },
                GlyphPt { x: 15, y: 20 },
                GlyphPt { x: 13, y: 21 },
                GlyphPt { x: 9, y: 21 },
                GlyphPt { x: 7, y: 20 },
                GlyphPt { x: 5, y: 18 },
                GlyphPt { x: 4, y: 16 },
                GlyphPt { x: 3, y: 13 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 4, y: 5 },
                GlyphPt { x: 5, y: 3 },
                GlyphPt { x: 7, y: 1 },
                GlyphPt { x: 9, y: 0 },
                GlyphPt { x: 13, y: 0 },
                GlyphPt { x: 15, y: 1 },
                GlyphPt { x: 17, y: 3 },
                GlyphPt { x: 18, y: 5 },
                GlyphPt { x: 18, y: 8 },
            ],
            &[GlyphPt { x: 13, y: 8 }, GlyphPt { x: 18, y: 8 }],
        ],
    }, // 0x47
    Glyph {
        ink_w: 22,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 18, y: 21 }, GlyphPt { x: 18, y: 0 }],
            &[GlyphPt { x: 4, y: 11 }, GlyphPt { x: 18, y: 11 }],
        ],
    }, // 0x48
    Glyph {
        ink_w: 8,
        strokes: &[&[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }]],
    }, // 0x49
    Glyph {
        ink_w: 16,
        strokes: &[&[
            GlyphPt { x: 12, y: 21 },
            GlyphPt { x: 12, y: 5 },
            GlyphPt { x: 11, y: 2 },
            GlyphPt { x: 10, y: 1 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 6, y: 0 },
            GlyphPt { x: 4, y: 1 },
            GlyphPt { x: 3, y: 2 },
            GlyphPt { x: 2, y: 5 },
            GlyphPt { x: 2, y: 7 },
        ]],
    }, // 0x4a
    Glyph {
        ink_w: 21,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 18, y: 21 }, GlyphPt { x: 4, y: 7 }],
            &[GlyphPt { x: 9, y: 12 }, GlyphPt { x: 18, y: 0 }],
        ],
    }, // 0x4b
    Glyph {
        ink_w: 17,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 4, y: 0 }, GlyphPt { x: 16, y: 0 }],
        ],
    }, // 0x4c
    Glyph {
        ink_w: 24,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 12, y: 0 }],
            &[GlyphPt { x: 20, y: 21 }, GlyphPt { x: 12, y: 0 }],
            &[GlyphPt { x: 20, y: 21 }, GlyphPt { x: 20, y: 0 }],
        ],
    }, // 0x4d
    Glyph {
        ink_w: 22,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 18, y: 0 }],
            &[GlyphPt { x: 18, y: 21 }, GlyphPt { x: 18, y: 0 }],
        ],
    }, // 0x4e
    Glyph {
        ink_w: 22,
        strokes: &[&[
            GlyphPt { x: 9, y: 21 },
            GlyphPt { x: 7, y: 20 },
            GlyphPt { x: 5, y: 18 },
            GlyphPt { x: 4, y: 16 },
            GlyphPt { x: 3, y: 13 },
            GlyphPt { x: 3, y: 8 },
            GlyphPt { x: 4, y: 5 },
            GlyphPt { x: 5, y: 3 },
            GlyphPt { x: 7, y: 1 },
            GlyphPt { x: 9, y: 0 },
            GlyphPt { x: 13, y: 0 },
            GlyphPt { x: 15, y: 1 },
            GlyphPt { x: 17, y: 3 },
            GlyphPt { x: 18, y: 5 },
            GlyphPt { x: 19, y: 8 },
            GlyphPt { x: 19, y: 13 },
            GlyphPt { x: 18, y: 16 },
            GlyphPt { x: 17, y: 18 },
            GlyphPt { x: 15, y: 20 },
            GlyphPt { x: 13, y: 21 },
            GlyphPt { x: 9, y: 21 },
        ]],
    }, // 0x4f
    Glyph {
        ink_w: 21,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 21 },
                GlyphPt { x: 13, y: 21 },
                GlyphPt { x: 16, y: 20 },
                GlyphPt { x: 17, y: 19 },
                GlyphPt { x: 18, y: 17 },
                GlyphPt { x: 18, y: 14 },
                GlyphPt { x: 17, y: 12 },
                GlyphPt { x: 16, y: 11 },
                GlyphPt { x: 13, y: 10 },
                GlyphPt { x: 4, y: 10 },
            ],
        ],
    }, // 0x50
    Glyph {
        ink_w: 22,
        strokes: &[
            &[
                GlyphPt { x: 9, y: 21 },
                GlyphPt { x: 7, y: 20 },
                GlyphPt { x: 5, y: 18 },
                GlyphPt { x: 4, y: 16 },
                GlyphPt { x: 3, y: 13 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 4, y: 5 },
                GlyphPt { x: 5, y: 3 },
                GlyphPt { x: 7, y: 1 },
                GlyphPt { x: 9, y: 0 },
                GlyphPt { x: 13, y: 0 },
                GlyphPt { x: 15, y: 1 },
                GlyphPt { x: 17, y: 3 },
                GlyphPt { x: 18, y: 5 },
                GlyphPt { x: 19, y: 8 },
                GlyphPt { x: 19, y: 13 },
                GlyphPt { x: 18, y: 16 },
                GlyphPt { x: 17, y: 18 },
                GlyphPt { x: 15, y: 20 },
                GlyphPt { x: 13, y: 21 },
                GlyphPt { x: 9, y: 21 },
            ],
            &[GlyphPt { x: 12, y: 4 }, GlyphPt { x: 18, y: -2 }],
        ],
    }, // 0x51
    Glyph {
        ink_w: 21,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 21 },
                GlyphPt { x: 13, y: 21 },
                GlyphPt { x: 16, y: 20 },
                GlyphPt { x: 17, y: 19 },
                GlyphPt { x: 18, y: 17 },
                GlyphPt { x: 18, y: 15 },
                GlyphPt { x: 17, y: 13 },
                GlyphPt { x: 16, y: 12 },
                GlyphPt { x: 13, y: 11 },
                GlyphPt { x: 4, y: 11 },
            ],
            &[GlyphPt { x: 11, y: 11 }, GlyphPt { x: 18, y: 0 }],
        ],
    }, // 0x52
    Glyph {
        ink_w: 20,
        strokes: &[&[
            GlyphPt { x: 17, y: 18 },
            GlyphPt { x: 15, y: 20 },
            GlyphPt { x: 12, y: 21 },
            GlyphPt { x: 8, y: 21 },
            GlyphPt { x: 5, y: 20 },
            GlyphPt { x: 3, y: 18 },
            GlyphPt { x: 3, y: 16 },
            GlyphPt { x: 4, y: 14 },
            GlyphPt { x: 5, y: 13 },
            GlyphPt { x: 7, y: 12 },
            GlyphPt { x: 13, y: 10 },
            GlyphPt { x: 15, y: 9 },
            GlyphPt { x: 16, y: 8 },
            GlyphPt { x: 17, y: 6 },
            GlyphPt { x: 17, y: 3 },
            GlyphPt { x: 15, y: 1 },
            GlyphPt { x: 12, y: 0 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 5, y: 1 },
            GlyphPt { x: 3, y: 3 },
        ]],
    }, // 0x53
    Glyph {
        ink_w: 16,
        strokes: &[
            &[GlyphPt { x: 8, y: 21 }, GlyphPt { x: 8, y: 0 }],
            &[GlyphPt { x: 1, y: 21 }, GlyphPt { x: 15, y: 21 }],
        ],
    }, // 0x54
    Glyph {
        ink_w: 22,
        strokes: &[&[
            GlyphPt { x: 4, y: 21 },
            GlyphPt { x: 4, y: 6 },
            GlyphPt { x: 5, y: 3 },
            GlyphPt { x: 7, y: 1 },
            GlyphPt { x: 10, y: 0 },
            GlyphPt { x: 12, y: 0 },
            GlyphPt { x: 15, y: 1 },
            GlyphPt { x: 17, y: 3 },
            GlyphPt { x: 18, y: 6 },
            GlyphPt { x: 18, y: 21 },
        ]],
    }, // 0x55
    Glyph {
        ink_w: 18,
        strokes: &[
            &[GlyphPt { x: 1, y: 21 }, GlyphPt { x: 9, y: 0 }],
            &[GlyphPt { x: 17, y: 21 }, GlyphPt { x: 9, y: 0 }],
        ],
    }, // 0x56
    Glyph {
        ink_w: 24,
        strokes: &[
            &[GlyphPt { x: 2, y: 21 }, GlyphPt { x: 7, y: 0 }],
            &[GlyphPt { x: 12, y: 21 }, GlyphPt { x: 7, y: 0 }],
            &[GlyphPt { x: 12, y: 21 }, GlyphPt { x: 17, y: 0 }],
            &[GlyphPt { x: 22, y: 21 }, GlyphPt { x: 17, y: 0 }],
        ],
    }, // 0x57
    Glyph {
        ink_w: 20,
        strokes: &[
            &[GlyphPt { x: 3, y: 21 }, GlyphPt { x: 17, y: 0 }],
            &[GlyphPt { x: 17, y: 21 }, GlyphPt { x: 3, y: 0 }],
        ],
    }, // 0x58
    Glyph {
        ink_w: 18,
        strokes: &[
            &[
                GlyphPt { x: 1, y: 21 },
                GlyphPt { x: 9, y: 11 },
                GlyphPt { x: 9, y: 0 },
            ],
            &[GlyphPt { x: 17, y: 21 }, GlyphPt { x: 9, y: 11 }],
        ],
    }, // 0x59
    Glyph {
        ink_w: 20,
        strokes: &[
            &[GlyphPt { x: 17, y: 21 }, GlyphPt { x: 3, y: 0 }],
            &[GlyphPt { x: 3, y: 21 }, GlyphPt { x: 17, y: 21 }],
            &[GlyphPt { x: 3, y: 0 }, GlyphPt { x: 17, y: 0 }],
        ],
    }, // 0x5a
    Glyph {
        ink_w: 14,
        strokes: &[
            &[GlyphPt { x: 4, y: 25 }, GlyphPt { x: 4, y: -7 }],
            &[GlyphPt { x: 5, y: 25 }, GlyphPt { x: 5, y: -7 }],
            &[GlyphPt { x: 4, y: 25 }, GlyphPt { x: 11, y: 25 }],
            &[GlyphPt { x: 4, y: -7 }, GlyphPt { x: 11, y: -7 }],
        ],
    }, // 0x5b
    Glyph {
        ink_w: 14,
        strokes: &[&[GlyphPt { x: 0, y: 21 }, GlyphPt { x: 14, y: -3 }]],
    }, // 0x5c
    Glyph {
        ink_w: 14,
        strokes: &[
            &[GlyphPt { x: 9, y: 25 }, GlyphPt { x: 9, y: -7 }],
            &[GlyphPt { x: 10, y: 25 }, GlyphPt { x: 10, y: -7 }],
            &[GlyphPt { x: 3, y: 25 }, GlyphPt { x: 10, y: 25 }],
            &[GlyphPt { x: 3, y: -7 }, GlyphPt { x: 10, y: -7 }],
        ],
    }, // 0x5d
    Glyph {
        ink_w: 16,
        strokes: &[
            &[
                GlyphPt { x: 6, y: 15 },
                GlyphPt { x: 8, y: 18 },
                GlyphPt { x: 10, y: 15 },
            ],
            &[
                GlyphPt { x: 3, y: 12 },
                GlyphPt { x: 8, y: 17 },
                GlyphPt { x: 13, y: 12 },
            ],
            &[GlyphPt { x: 8, y: 17 }, GlyphPt { x: 8, y: 0 }],
        ],
    }, // 0x5e
    Glyph {
        ink_w: 16,
        strokes: &[&[GlyphPt { x: 0, y: -2 }, GlyphPt { x: 16, y: -2 }]],
    }, // 0x5f
    Glyph {
        ink_w: 10,
        strokes: &[&[
            GlyphPt { x: 6, y: 21 },
            GlyphPt { x: 5, y: 20 },
            GlyphPt { x: 4, y: 18 },
            GlyphPt { x: 4, y: 16 },
            GlyphPt { x: 5, y: 15 },
            GlyphPt { x: 6, y: 16 },
            GlyphPt { x: 5, y: 17 },
        ]],
    }, // 0x60
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 15, y: 14 }, GlyphPt { x: 15, y: 0 }],
            &[
                GlyphPt { x: 15, y: 11 },
                GlyphPt { x: 13, y: 13 },
                GlyphPt { x: 11, y: 14 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 3, y: 6 },
                GlyphPt { x: 4, y: 3 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 13, y: 1 },
                GlyphPt { x: 15, y: 3 },
            ],
        ],
    }, // 0x61
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 11, y: 14 },
                GlyphPt { x: 13, y: 13 },
                GlyphPt { x: 15, y: 11 },
                GlyphPt { x: 16, y: 8 },
                GlyphPt { x: 16, y: 6 },
                GlyphPt { x: 15, y: 3 },
                GlyphPt { x: 13, y: 1 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 4, y: 3 },
            ],
        ],
    }, // 0x62
    Glyph {
        ink_w: 18,
        strokes: &[&[
            GlyphPt { x: 15, y: 11 },
            GlyphPt { x: 13, y: 13 },
            GlyphPt { x: 11, y: 14 },
            GlyphPt { x: 8, y: 14 },
            GlyphPt { x: 6, y: 13 },
            GlyphPt { x: 4, y: 11 },
            GlyphPt { x: 3, y: 8 },
            GlyphPt { x: 3, y: 6 },
            GlyphPt { x: 4, y: 3 },
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 13, y: 1 },
            GlyphPt { x: 15, y: 3 },
        ]],
    }, // 0x63
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 15, y: 21 }, GlyphPt { x: 15, y: 0 }],
            &[
                GlyphPt { x: 15, y: 11 },
                GlyphPt { x: 13, y: 13 },
                GlyphPt { x: 11, y: 14 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 3, y: 6 },
                GlyphPt { x: 4, y: 3 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 13, y: 1 },
                GlyphPt { x: 15, y: 3 },
            ],
        ],
    }, // 0x64
    Glyph {
        ink_w: 18,
        strokes: &[&[
            GlyphPt { x: 3, y: 8 },
            GlyphPt { x: 15, y: 8 },
            GlyphPt { x: 15, y: 10 },
            GlyphPt { x: 14, y: 12 },
            GlyphPt { x: 13, y: 13 },
            GlyphPt { x: 11, y: 14 },
            GlyphPt { x: 8, y: 14 },
            GlyphPt { x: 6, y: 13 },
            GlyphPt { x: 4, y: 11 },
            GlyphPt { x: 3, y: 8 },
            GlyphPt { x: 3, y: 6 },
            GlyphPt { x: 4, y: 3 },
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 13, y: 1 },
            GlyphPt { x: 15, y: 3 },
        ]],
    }, // 0x65
    Glyph {
        ink_w: 12,
        strokes: &[
            &[
                GlyphPt { x: 10, y: 21 },
                GlyphPt { x: 8, y: 21 },
                GlyphPt { x: 6, y: 20 },
                GlyphPt { x: 5, y: 17 },
                GlyphPt { x: 5, y: 0 },
            ],
            &[GlyphPt { x: 2, y: 14 }, GlyphPt { x: 9, y: 14 }],
        ],
    }, // 0x66
    Glyph {
        ink_w: 19,
        strokes: &[
            &[
                GlyphPt { x: 15, y: 14 },
                GlyphPt { x: 15, y: -2 },
                GlyphPt { x: 14, y: -5 },
                GlyphPt { x: 13, y: -6 },
                GlyphPt { x: 11, y: -7 },
                GlyphPt { x: 8, y: -7 },
                GlyphPt { x: 6, y: -6 },
            ],
            &[
                GlyphPt { x: 15, y: 11 },
                GlyphPt { x: 13, y: 13 },
                GlyphPt { x: 11, y: 14 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 3, y: 6 },
                GlyphPt { x: 4, y: 3 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 13, y: 1 },
                GlyphPt { x: 15, y: 3 },
            ],
        ],
    }, // 0x67
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 10 },
                GlyphPt { x: 7, y: 13 },
                GlyphPt { x: 9, y: 14 },
                GlyphPt { x: 12, y: 14 },
                GlyphPt { x: 14, y: 13 },
                GlyphPt { x: 15, y: 10 },
                GlyphPt { x: 15, y: 0 },
            ],
        ],
    }, // 0x68
    Glyph {
        ink_w: 8,
        strokes: &[
            &[
                GlyphPt { x: 3, y: 21 },
                GlyphPt { x: 4, y: 20 },
                GlyphPt { x: 5, y: 21 },
                GlyphPt { x: 4, y: 22 },
                GlyphPt { x: 3, y: 21 },
            ],
            &[GlyphPt { x: 4, y: 14 }, GlyphPt { x: 4, y: 0 }],
        ],
    }, // 0x69
    Glyph {
        ink_w: 10,
        strokes: &[
            &[
                GlyphPt { x: 5, y: 21 },
                GlyphPt { x: 6, y: 20 },
                GlyphPt { x: 7, y: 21 },
                GlyphPt { x: 6, y: 22 },
                GlyphPt { x: 5, y: 21 },
            ],
            &[
                GlyphPt { x: 6, y: 14 },
                GlyphPt { x: 6, y: -3 },
                GlyphPt { x: 5, y: -6 },
                GlyphPt { x: 3, y: -7 },
                GlyphPt { x: 1, y: -7 },
            ],
        ],
    }, // 0x6a
    Glyph {
        ink_w: 17,
        strokes: &[
            &[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }],
            &[GlyphPt { x: 14, y: 14 }, GlyphPt { x: 4, y: 4 }],
            &[GlyphPt { x: 8, y: 8 }, GlyphPt { x: 15, y: 0 }],
        ],
    }, // 0x6b
    Glyph {
        ink_w: 8,
        strokes: &[&[GlyphPt { x: 4, y: 21 }, GlyphPt { x: 4, y: 0 }]],
    }, // 0x6c
    Glyph {
        ink_w: 30,
        strokes: &[
            &[GlyphPt { x: 4, y: 14 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 10 },
                GlyphPt { x: 7, y: 13 },
                GlyphPt { x: 9, y: 14 },
                GlyphPt { x: 12, y: 14 },
                GlyphPt { x: 14, y: 13 },
                GlyphPt { x: 15, y: 10 },
                GlyphPt { x: 15, y: 0 },
            ],
            &[
                GlyphPt { x: 15, y: 10 },
                GlyphPt { x: 18, y: 13 },
                GlyphPt { x: 20, y: 14 },
                GlyphPt { x: 23, y: 14 },
                GlyphPt { x: 25, y: 13 },
                GlyphPt { x: 26, y: 10 },
                GlyphPt { x: 26, y: 0 },
            ],
        ],
    }, // 0x6d
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 4, y: 14 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 10 },
                GlyphPt { x: 7, y: 13 },
                GlyphPt { x: 9, y: 14 },
                GlyphPt { x: 12, y: 14 },
                GlyphPt { x: 14, y: 13 },
                GlyphPt { x: 15, y: 10 },
                GlyphPt { x: 15, y: 0 },
            ],
        ],
    }, // 0x6e
    Glyph {
        ink_w: 19,
        strokes: &[&[
            GlyphPt { x: 8, y: 14 },
            GlyphPt { x: 6, y: 13 },
            GlyphPt { x: 4, y: 11 },
            GlyphPt { x: 3, y: 8 },
            GlyphPt { x: 3, y: 6 },
            GlyphPt { x: 4, y: 3 },
            GlyphPt { x: 6, y: 1 },
            GlyphPt { x: 8, y: 0 },
            GlyphPt { x: 11, y: 0 },
            GlyphPt { x: 13, y: 1 },
            GlyphPt { x: 15, y: 3 },
            GlyphPt { x: 16, y: 6 },
            GlyphPt { x: 16, y: 8 },
            GlyphPt { x: 15, y: 11 },
            GlyphPt { x: 13, y: 13 },
            GlyphPt { x: 11, y: 14 },
            GlyphPt { x: 8, y: 14 },
        ]],
    }, // 0x6f
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 4, y: 14 }, GlyphPt { x: 4, y: -7 }],
            &[
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 11, y: 14 },
                GlyphPt { x: 13, y: 13 },
                GlyphPt { x: 15, y: 11 },
                GlyphPt { x: 16, y: 8 },
                GlyphPt { x: 16, y: 6 },
                GlyphPt { x: 15, y: 3 },
                GlyphPt { x: 13, y: 1 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 4, y: 3 },
            ],
        ],
    }, // 0x70
    Glyph {
        ink_w: 19,
        strokes: &[
            &[GlyphPt { x: 15, y: 14 }, GlyphPt { x: 15, y: -7 }],
            &[
                GlyphPt { x: 15, y: 11 },
                GlyphPt { x: 13, y: 13 },
                GlyphPt { x: 11, y: 14 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 6, y: 13 },
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 3, y: 6 },
                GlyphPt { x: 4, y: 3 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 11, y: 0 },
                GlyphPt { x: 13, y: 1 },
                GlyphPt { x: 15, y: 3 },
            ],
        ],
    }, // 0x71
    Glyph {
        ink_w: 13,
        strokes: &[
            &[GlyphPt { x: 4, y: 14 }, GlyphPt { x: 4, y: 0 }],
            &[
                GlyphPt { x: 4, y: 8 },
                GlyphPt { x: 5, y: 11 },
                GlyphPt { x: 7, y: 13 },
                GlyphPt { x: 9, y: 14 },
                GlyphPt { x: 12, y: 14 },
            ],
        ],
    }, // 0x72
    Glyph {
        ink_w: 17,
        strokes: &[&[
            GlyphPt { x: 14, y: 11 },
            GlyphPt { x: 13, y: 13 },
            GlyphPt { x: 10, y: 14 },
            GlyphPt { x: 7, y: 14 },
            GlyphPt { x: 4, y: 13 },
            GlyphPt { x: 3, y: 11 },
            GlyphPt { x: 4, y: 9 },
            GlyphPt { x: 6, y: 8 },
            GlyphPt { x: 11, y: 7 },
            GlyphPt { x: 13, y: 6 },
            GlyphPt { x: 14, y: 4 },
            GlyphPt { x: 14, y: 3 },
            GlyphPt { x: 13, y: 1 },
            GlyphPt { x: 10, y: 0 },
            GlyphPt { x: 7, y: 0 },
            GlyphPt { x: 4, y: 1 },
            GlyphPt { x: 3, y: 3 },
        ]],
    }, // 0x73
    Glyph {
        ink_w: 12,
        strokes: &[
            &[
                GlyphPt { x: 5, y: 21 },
                GlyphPt { x: 5, y: 4 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 10, y: 0 },
            ],
            &[GlyphPt { x: 2, y: 14 }, GlyphPt { x: 9, y: 14 }],
        ],
    }, // 0x74
    Glyph {
        ink_w: 19,
        strokes: &[
            &[
                GlyphPt { x: 4, y: 14 },
                GlyphPt { x: 4, y: 4 },
                GlyphPt { x: 5, y: 1 },
                GlyphPt { x: 7, y: 0 },
                GlyphPt { x: 10, y: 0 },
                GlyphPt { x: 12, y: 1 },
                GlyphPt { x: 15, y: 4 },
            ],
            &[GlyphPt { x: 15, y: 14 }, GlyphPt { x: 15, y: 0 }],
        ],
    }, // 0x75
    Glyph {
        ink_w: 16,
        strokes: &[
            &[GlyphPt { x: 2, y: 14 }, GlyphPt { x: 8, y: 0 }],
            &[GlyphPt { x: 14, y: 14 }, GlyphPt { x: 8, y: 0 }],
        ],
    }, // 0x76
    Glyph {
        ink_w: 22,
        strokes: &[
            &[GlyphPt { x: 3, y: 14 }, GlyphPt { x: 7, y: 0 }],
            &[GlyphPt { x: 11, y: 14 }, GlyphPt { x: 7, y: 0 }],
            &[GlyphPt { x: 11, y: 14 }, GlyphPt { x: 15, y: 0 }],
            &[GlyphPt { x: 19, y: 14 }, GlyphPt { x: 15, y: 0 }],
        ],
    }, // 0x77
    Glyph {
        ink_w: 17,
        strokes: &[
            &[GlyphPt { x: 3, y: 14 }, GlyphPt { x: 14, y: 0 }],
            &[GlyphPt { x: 14, y: 14 }, GlyphPt { x: 3, y: 0 }],
        ],
    }, // 0x78
    Glyph {
        ink_w: 16,
        strokes: &[
            &[GlyphPt { x: 2, y: 14 }, GlyphPt { x: 8, y: 0 }],
            &[
                GlyphPt { x: 14, y: 14 },
                GlyphPt { x: 8, y: 0 },
                GlyphPt { x: 6, y: -4 },
                GlyphPt { x: 4, y: -6 },
                GlyphPt { x: 2, y: -7 },
                GlyphPt { x: 1, y: -7 },
            ],
        ],
    }, // 0x79
    Glyph {
        ink_w: 17,
        strokes: &[
            &[GlyphPt { x: 14, y: 14 }, GlyphPt { x: 3, y: 0 }],
            &[GlyphPt { x: 3, y: 14 }, GlyphPt { x: 14, y: 14 }],
            &[GlyphPt { x: 3, y: 0 }, GlyphPt { x: 14, y: 0 }],
        ],
    }, // 0x7a
    Glyph {
        ink_w: 14,
        strokes: &[
            &[
                GlyphPt { x: 9, y: 25 },
                GlyphPt { x: 7, y: 24 },
                GlyphPt { x: 6, y: 23 },
                GlyphPt { x: 5, y: 21 },
                GlyphPt { x: 5, y: 19 },
                GlyphPt { x: 6, y: 17 },
                GlyphPt { x: 7, y: 16 },
                GlyphPt { x: 8, y: 14 },
                GlyphPt { x: 8, y: 12 },
                GlyphPt { x: 6, y: 10 },
            ],
            &[
                GlyphPt { x: 7, y: 24 },
                GlyphPt { x: 6, y: 22 },
                GlyphPt { x: 6, y: 20 },
                GlyphPt { x: 7, y: 18 },
                GlyphPt { x: 8, y: 17 },
                GlyphPt { x: 9, y: 15 },
                GlyphPt { x: 9, y: 13 },
                GlyphPt { x: 8, y: 11 },
                GlyphPt { x: 4, y: 9 },
                GlyphPt { x: 8, y: 7 },
                GlyphPt { x: 9, y: 5 },
                GlyphPt { x: 9, y: 3 },
                GlyphPt { x: 8, y: 1 },
                GlyphPt { x: 7, y: 0 },
                GlyphPt { x: 6, y: -2 },
                GlyphPt { x: 6, y: -4 },
                GlyphPt { x: 7, y: -6 },
            ],
            &[
                GlyphPt { x: 6, y: 8 },
                GlyphPt { x: 8, y: 6 },
                GlyphPt { x: 8, y: 4 },
                GlyphPt { x: 7, y: 2 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 5, y: -1 },
                GlyphPt { x: 5, y: -3 },
                GlyphPt { x: 6, y: -5 },
                GlyphPt { x: 7, y: -6 },
                GlyphPt { x: 9, y: -7 },
            ],
        ],
    }, // 0x7b
    Glyph {
        ink_w: 8,
        strokes: &[&[GlyphPt { x: 4, y: 25 }, GlyphPt { x: 4, y: -7 }]],
    }, // 0x7c
    Glyph {
        ink_w: 14,
        strokes: &[
            &[
                GlyphPt { x: 5, y: 25 },
                GlyphPt { x: 7, y: 24 },
                GlyphPt { x: 8, y: 23 },
                GlyphPt { x: 9, y: 21 },
                GlyphPt { x: 9, y: 19 },
                GlyphPt { x: 8, y: 17 },
                GlyphPt { x: 7, y: 16 },
                GlyphPt { x: 6, y: 14 },
                GlyphPt { x: 6, y: 12 },
                GlyphPt { x: 8, y: 10 },
            ],
            &[
                GlyphPt { x: 7, y: 24 },
                GlyphPt { x: 8, y: 22 },
                GlyphPt { x: 8, y: 20 },
                GlyphPt { x: 7, y: 18 },
                GlyphPt { x: 6, y: 17 },
                GlyphPt { x: 5, y: 15 },
                GlyphPt { x: 5, y: 13 },
                GlyphPt { x: 6, y: 11 },
                GlyphPt { x: 10, y: 9 },
                GlyphPt { x: 6, y: 7 },
                GlyphPt { x: 5, y: 5 },
                GlyphPt { x: 5, y: 3 },
                GlyphPt { x: 6, y: 1 },
                GlyphPt { x: 7, y: 0 },
                GlyphPt { x: 8, y: -2 },
                GlyphPt { x: 8, y: -4 },
                GlyphPt { x: 7, y: -6 },
            ],
            &[
                GlyphPt { x: 8, y: 8 },
                GlyphPt { x: 6, y: 6 },
                GlyphPt { x: 6, y: 4 },
                GlyphPt { x: 7, y: 2 },
                GlyphPt { x: 8, y: 1 },
                GlyphPt { x: 9, y: -1 },
                GlyphPt { x: 9, y: -3 },
                GlyphPt { x: 8, y: -5 },
                GlyphPt { x: 7, y: -6 },
                GlyphPt { x: 5, y: -7 },
            ],
        ],
    }, // 0x7d
    Glyph {
        ink_w: 24,
        strokes: &[
            &[
                GlyphPt { x: 3, y: 6 },
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 4, y: 11 },
                GlyphPt { x: 6, y: 12 },
                GlyphPt { x: 8, y: 12 },
                GlyphPt { x: 10, y: 11 },
                GlyphPt { x: 14, y: 8 },
                GlyphPt { x: 16, y: 7 },
                GlyphPt { x: 18, y: 7 },
                GlyphPt { x: 20, y: 8 },
                GlyphPt { x: 21, y: 10 },
            ],
            &[
                GlyphPt { x: 3, y: 8 },
                GlyphPt { x: 4, y: 10 },
                GlyphPt { x: 6, y: 11 },
                GlyphPt { x: 8, y: 11 },
                GlyphPt { x: 10, y: 10 },
                GlyphPt { x: 14, y: 7 },
                GlyphPt { x: 16, y: 6 },
                GlyphPt { x: 18, y: 6 },
                GlyphPt { x: 20, y: 7 },
                GlyphPt { x: 21, y: 10 },
                GlyphPt { x: 21, y: 12 },
            ],
        ],
    }, // 0x7e
    Glyph {
        ink_w: 14,
        strokes: &[&[
            GlyphPt { x: 6, y: 21 },
            GlyphPt { x: 4, y: 20 },
            GlyphPt { x: 3, y: 18 },
            GlyphPt { x: 3, y: 16 },
            GlyphPt { x: 4, y: 14 },
            GlyphPt { x: 6, y: 13 },
            GlyphPt { x: 8, y: 13 },
            GlyphPt { x: 10, y: 14 },
            GlyphPt { x: 11, y: 16 },
            GlyphPt { x: 11, y: 18 },
            GlyphPt { x: 10, y: 20 },
            GlyphPt { x: 8, y: 21 },
            GlyphPt { x: 6, y: 21 },
        ]],
    }, // 0x7f
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// advance 表抽查：Helvetica AFM 关键宽度。
    #[test]
    fn advance_table_values() {
        assert_eq!(advance_em(' '), 0.278);
        assert_eq!(advance_em('A'), 0.667);
        assert_eq!(advance_em('m'), 0.833);
        assert_eq!(advance_em('W'), 0.944);
        assert_eq!(advance_em('i'), 0.222);
        assert_eq!(advance_em('@'), 1.015);
        assert_eq!(advance_em('中'), 1.0); // 非 ASCII 降级
    }

    /// run_advance = Σ advance·fs。
    #[test]
    fn run_advance_sums() {
        let ab = 0.667 + 0.667;
        assert!((run_advance("AB", 10.0) - ab * 10.0).abs() < 1e-3);
        // 中文按 1em
        assert!((run_advance("中中", 20.0) - 40.0).abs() < 1e-3);
    }

    /// bidi / 格式控制字符：零 advance、不渲染（Chrome 语义——U+202A 族
    /// 等不可见控制符在 SVG text 中不占宽）。wpt text-bidi-controls-
    /// anchors-1/2：此前按非 ASCII 走 □ tofu + 1em advance，排版整体错位。
    #[test]
    fn bidi_format_controls_zero_width() {
        for ch in [
            '\u{061C}', '\u{200B}', '\u{200C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}',
            '\u{202C}', '\u{202D}', '\u{202E}', '\u{2060}', '\u{2066}', '\u{2069}', '\u{FEFF}',
            '\u{00AD}',
        ] {
            assert_eq!(advance_em(ch), 0.0, "U+{:04X} 应零宽", ch as u32);
        }
        // 与无控制符串等宽（advance 恒等）
        let base = run_advance("Start Anchor", 12.0);
        assert_eq!(run_advance("\u{202B}Start Anchor\u{202C}", 12.0), base);
        // 不产生任何字形几何（□ tofu 禁用）
        let mut g = PathGeom::default();
        emit_run(
            "\u{202B}\u{202C}",
            0.0,
            20.0,
            12.0,
            false,
            0.0,
            Mat2x3::IDENTITY,
            &mut g,
        );
        assert!(g.subs.is_empty(), "bidi 控制符不得产生 tofu 几何");
    }

    /// 车道BG：bidi 重排对 wpt text-bidi-controls-anchors-1（LTR 基向）
    /// 的 7 行 ref 字符串逐一匹配（ref 由 Chrome/Firefox 预言机承认）。
    /// Python 参考实现 14/14 后移植；RLE/PDF 内全 Latin = 恒等（RLE 只抬
    /// 嵌入层，L 字符隐式 +1 后双重反转为恒等），RLO 内 = 逐字符反转。
    #[cfg(feature = "text-lite")]
    #[test]
    fn bidi_reorder_ltr_rows() {
        let la = "Start Anchor";
        assert_eq!(bidi_reorder(la, false), la);
        assert_eq!(bidi_reorder("\u{202B}Start Anchor\u{202C}", false), la);
        assert_eq!(bidi_reorder("\u{202B}Start\u{202C} Anchor", false), la);
        assert_eq!(bidi_reorder("Start \u{202B}Anchor\u{202C}", false), la);
        assert_eq!(
            bidi_reorder("\u{202E}Start Anchor\u{202C}", false),
            "rohcnA tratS"
        );
        assert_eq!(
            bidi_reorder("\u{202E}Start\u{202C} Anchor", false),
            "tratS Anchor"
        );
        assert_eq!(
            bidi_reorder("Start \u{202E}Anchor\u{202C}", false),
            "Start rohcnA"
        );
    }

    /// 车道BG：bidi-2（RTL 基向）7 行。row3/4 的词交换是 isolate 边界
    /// 中性解析的关键判例（直通式 N1 给恒等序，与预言机矛盾）。
    #[cfg(feature = "text-lite")]
    #[test]
    fn bidi_reorder_rtl_rows() {
        let la = "Start Anchor";
        assert_eq!(bidi_reorder(la, true), la);
        assert_eq!(bidi_reorder("\u{202B}Start Anchor\u{202C}", true), la);
        assert_eq!(
            bidi_reorder("\u{202B}Start\u{202C} Anchor", true),
            "Anchor Start"
        );
        assert_eq!(
            bidi_reorder("Start \u{202B}Anchor\u{202C}", true),
            "Anchor Start"
        );
        assert_eq!(
            bidi_reorder("\u{202E}Start Anchor\u{202C}", true),
            "rohcnA tratS"
        );
        assert_eq!(
            bidi_reorder("\u{202E}Start\u{202C} Anchor", true),
            "Anchor tratS"
        );
        assert_eq!(
            bidi_reorder("Start \u{202E}Anchor\u{202C}", true),
            "rohcnA Start"
        );
    }

    /// bidi 重排不改变总 advance（零宽控制符剔除不占宽；字符集合不变）。
    #[cfg(feature = "text-lite")]
    #[test]
    fn bidi_reorder_preserves_advance() {
        let s = "\u{202B}Start Anchor\u{202C}";
        assert_eq!(
            run_advance(&bidi_reorder(s, false), 12.0),
            run_advance("Start Anchor", 12.0)
        );
    }

    /// dominant-baseline: central 的 underline 异常附加位（车道BG）：
    /// extra=0.11em 时条带 = 基线下 [0.16, 0.26]em（Chrome bgp4 实测）。
    #[test]
    fn decoration_bar_underline_central_extra() {
        let fs = 100.0_f32;
        let baseline = 74.0_f32;
        let mut g = PathGeom::default();
        decoration_bar(
            Deco::UNDERLINE,
            0.0,
            50.0,
            baseline,
            fs,
            Mat2x3::IDENTITY,
            &mut g,
            UNDERLINE_CENTRAL_EXTRA_EM,
        );
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for sub in &g.subs {
            for &(_, y) in &sub.pts {
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        assert!(
            y0 >= baseline + 0.16 * fs - 0.2 && y1 <= baseline + 0.26 * fs + 0.2,
            "central underline 带 [{y0},{y1}] 应在基线下 [0.16,0.26]em"
        );
    }

    /// letter-spacing：逐字符加排（含尾字符——CSS 语义）；run_advance_ls
    /// 与 emit_run 返回值一致。
    #[test]
    fn letter_spacing_advances() {
        let base = run_advance("AB", 10.0);
        let ls = 3.0;
        let spaced = run_advance_ls("AB", 10.0, ls, 0.0);
        assert!((spaced - (base + 2.0 * ls)).abs() < 1e-3);
        let mut g = PathGeom::default();
        let adv = emit_run("AB", 0.0, 20.0, 10.0, false, ls, Mat2x3::IDENTITY, &mut g);
        assert!(
            (adv - spaced).abs() < 1e-3,
            "emit_run 消耗 {adv} vs {spaced}"
        );
    }

    /// 车道EJ：word-spacing 原语——ws 只加在 U+0020 的 advance 上；
    /// AFM（run_advance_ls）与 hmtx（run_advance_ls_font）求和同源，
    /// 且与发射原语（emit_run_adj / emit_run_font_adj）返回宽一致。
    #[test]
    fn word_spacing_space_only_advances() {
        let fs = 20.0_f32;
        let ws = 7.0_f32;
        let base = run_advance_ls("A B", fs, 0.0, 0.0);
        let spaced = run_advance_ls("A B", fs, 0.0, ws);
        assert!(
            (spaced - (base + ws)).abs() < 1e-4,
            "{spaced} vs {base}+{ws}"
        );
        // 无空格串不受影响
        assert!(
            (run_advance_ls("AB", fs, 0.0, ws) - run_advance_ls("AB", fs, 0.0, 0.0)).abs() < 1e-6
        );
        // 发射原语同口径（AFM 路径）
        let mut g = PathGeom::default();
        let adv = emit_run_adj(
            "A B",
            0.0,
            20.0,
            fs,
            false,
            0.0,
            ws,
            0.0,
            0.0,
            Mat2x3::IDENTITY,
            &mut g,
        );
        assert!((adv - spaced).abs() < 1e-4, "emit {adv} vs 求和 {spaced}");
        // 注入字体路径：锚定宽 = 发射宽（含 ws）
        let font = Font::parse(include_bytes!("../testdata/PinSans.ttf"))
            .expect("testdata/PinSans.ttf 应可解析");
        let anchor = run_advance_ls_font("A B", fs, 0.0, ws, &font);
        let mut g2 = PathGeom::default();
        let emit = emit_run_font_adj(
            "A B",
            0.0,
            20.0,
            fs,
            0.0,
            ws,
            0.0,
            0.0,
            Mat2x3::IDENTITY,
            &mut g2,
            &font,
            false,
        );
        assert!(
            (anchor - emit).abs() < 1e-4,
            "hmtx 锚定 {anchor} vs 发射 {emit}"
        );
    }

    /// 车道AO：emit_run_adj 的 extra 逐 advance 追加（textLength spacing
    /// 模式发射原语）——extra_final = extra 时总 advance = 自然宽 +
    /// N×extra（N 含空格、不含零宽格式符）；extra=0 与 emit_run 逐位
    /// 同结果。车道CV：extra_final=0（Chrome n−1 间隙均摊）时总 advance
    /// = 自然宽 + (N−1)×extra。
    #[test]
    fn emit_run_adj_extra_totals() {
        let fs = 10.0_f32;
        let natural = run_advance("AB C", fs);
        assert_eq!(advance_count("AB C"), 4, "空格计入 advance 数");
        assert_eq!(advance_count("A\u{200B}B"), 2, "零宽格式符不计入");
        for extra in [-3.0f32, 0.0, 2.5] {
            let mut g = PathGeom::default();
            let adv = emit_run_adj(
                "AB C",
                0.0,
                20.0,
                fs,
                false,
                0.0,
                0.0,
                extra,
                extra,
                Mat2x3::IDENTITY,
                &mut g,
            );
            let expect = natural + 4.0 * extra;
            assert!(
                (adv - expect).abs() < 1e-3,
                "extra={extra}: {adv} vs {expect}"
            );
            // 车道CV：末位不追加（间隙均摊）——总 advance = 自然宽 + 3×extra
            let mut g2 = PathGeom::default();
            let adv2 = emit_run_adj(
                "AB C",
                0.0,
                20.0,
                fs,
                false,
                0.0,
                0.0,
                extra,
                0.0,
                Mat2x3::IDENTITY,
                &mut g2,
            );
            let expect2 = natural + 3.0 * extra;
            assert!(
                (adv2 - expect2).abs() < 1e-3,
                "gap extra={extra}: {adv2} vs {expect2}"
            );
        }
        let mut g0 = PathGeom::default();
        let a0 = emit_run("AB", 0.0, 20.0, fs, false, 0.0, Mat2x3::IDENTITY, &mut g0);
        let mut g1 = PathGeom::default();
        let a1 = emit_run_adj(
            "AB",
            0.0,
            20.0,
            fs,
            false,
            0.0,
            0.0,
            0.0,
            0.0,
            Mat2x3::IDENTITY,
            &mut g1,
        );
        assert!(
            (a0 - a1).abs() < 1e-6 && g0.subs == g1.subs,
            "extra=0 同 emit_run"
        );
    }

    /// 车道CV（G1）：注入字体语境的锚定宽度（`run_advance_ls_font`）
    /// 必须与发射期逐字形 advance（`emit_run_font_adj` 返回宽）同源——
    /// 旧实现锚定走 [`run_advance_ls`] 的 Helvetica AFM 表、发射走
    /// hmtx，middle/end 锚定整体右偏（CT 车道 pinfont-gaps 报告）。
    /// PinSans（DejaVu 子集）的 hmtx 与 AFM 表对 "Hamburgefonstiv"
    /// 差 ≈21px@fs30——劈叉必现，两口径相等即回归。
    #[test]
    fn anchor_width_matches_emit_font() {
        let font = Font::parse(include_bytes!("../testdata/PinSans.ttf"))
            .expect("testdata/PinSans.ttf 应可解析");
        for (s, fs) in [
            ("Hamburgefonstiv", 30.0_f32),
            ("abc", 12.0),
            ("A 中 v", 24.0),
        ] {
            let anchor_w = run_advance_ls_font(s, fs, 0.0, 0.0, &font);
            let mut g = PathGeom::default();
            let emit_w = emit_run_font(
                s,
                0.0,
                40.0,
                fs,
                0.0,
                Mat2x3::IDENTITY,
                &mut g,
                &font,
                false,
            );
            assert!(
                (anchor_w - emit_w).abs() < 1e-3,
                "{s:?}: 锚定 {anchor_w} vs 发射 {emit_w}"
            );
            // 劈叉钉子（长串，hmtx−AFM ≈21px@fs30）：hmtx 求和 ≠ AFM 求和
            //（否则本测试失去判别力）
            if s.len() > 10 {
                let hershey_w = run_advance_ls(s, fs, 0.0, 0.0);
                assert!(
                    (anchor_w - hershey_w).abs() > fs * 0.3,
                    "{s:?}: hmtx({anchor_w}) 与 AFM({hershey_w}) 意外重合"
                );
            }
        }
        // letter-spacing 同样同源（含零宽格式符：不加 ls，同发射期）
        let a = run_advance_ls_font("A\u{200B}B", 20.0, 3.0, 0.0, &font);
        let mut g = PathGeom::default();
        let e = emit_run_font(
            "A\u{200B}B",
            0.0,
            40.0,
            20.0,
            3.0,
            Mat2x3::IDENTITY,
            &mut g,
            &font,
            false,
        );
        assert!((a - e).abs() < 1e-3, "ls+零宽: 锚定 {a} vs 发射 {e}");
    }

    /// 车道CV（G2）：textLength spacing 的目标宽语义——delta 均摊在
    /// n−1 个字间隙、末字形 advance 终点 = x+target（Chrome cv3 探针
    /// Ha/Ham/HH/H_i 逐位实证）；n=1 整段不调整。注入字体路径。
    #[test]
    fn textlength_gap_model_font() {
        let font = Font::parse(include_bytes!("../testdata/PinSans.ttf"))
            .expect("testdata/PinSans.ttf 应可解析");
        let fs = 30.0_f32;
        let s = "Hamburgefonstiv";
        let natural = run_advance_ls_font(s, fs, 0.0, 0.0, &font);
        let target = 400.0_f32;
        let n = advance_count(s) as f32;
        let d = (target - natural) / (n - 1.0);
        let mut g = PathGeom::default();
        let total = emit_run_font_adj(
            s,
            0.0,
            40.0,
            fs,
            0.0,
            0.0,
            d,
            0.0,
            Mat2x3::IDENTITY,
            &mut g,
            &font,
            false,
        );
        assert!(
            (total - target).abs() < 1e-3,
            "末字形终点应落在 target：{total} vs {target}"
        );
        // n=1：无间隙 → 不调整（Chrome H300 探针）
        let nat1 = run_advance_ls_font("H", fs, 0.0, 0.0, &font);
        let mut g1 = PathGeom::default();
        let total1 = emit_run_font_adj(
            "H",
            0.0,
            40.0,
            fs,
            0.0,
            0.0,
            999.0,
            0.0,
            Mat2x3::IDENTITY,
            &mut g1,
            &font,
            false,
        );
        assert!(
            (total1 - nat1).abs() < 1e-3,
            "n=1 不调整：{total1} vs {nat1}"
        );
    }

    /// 装饰条：underline 在基线下方 [0.10, 0.15]em 带、line-through 在
    /// 基线上方 0.26em 中心带、overline 在 cap 高带；条宽 = run advance，
    /// 外缘恰落在带界。
    #[test]
    fn decoration_bar_bands() {
        let fs = 20.0_f32;
        let baseline = 40.0_f32;
        for (kind, top_em, bot_em) in [
            // 车道BG：Chrome 预言机 0.05em 下移 + 0.10em 厚
            (Deco::UNDERLINE, 0.05, 0.15),
            (Deco::LINE_THROUGH, 0.26 - 0.05, 0.26 + 0.05),
            (Deco::OVERLINE, 0.715, 0.715 + 0.10),
        ] {
            let mut g = PathGeom::default();
            decoration_bar(
                kind,
                10.0,
                30.0,
                baseline,
                fs,
                Mat2x3::IDENTITY,
                &mut g,
                0.0,
            );
            // 闭合环经 stroke_quads 展开：环高内缩后为 0 → 竖直边退化被
            // 跳过，余上下两条横向四边形，恰好铺满 [top,bot] 整带
            assert_eq!(g.subs.len(), 2, "{kind:?} 带铺满");
            let (mut y0, mut y1) = (f32::MAX, f32::MIN);
            let (mut xa, mut xb) = (f32::MAX, f32::MIN);
            for sub in &g.subs {
                for &(x, y) in &sub.pts {
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                    xa = xa.min(x);
                    xb = xb.max(x);
                }
            }
            // AABB 覆盖 = 整带（含中段无空洞）
            // y 向下：underline 带在基线下方（+），其余在上方（−）
            let (expect_lo, expect_hi) = if kind == Deco::UNDERLINE {
                (baseline + top_em * fs, baseline + bot_em * fs)
            } else {
                (baseline - bot_em * fs, baseline - top_em * fs)
            };
            assert!(
                y0 >= expect_lo - 0.2 && y1 <= expect_hi + 0.2,
                "{kind:?} y [{y0},{y1}] 应在 [{expect_lo},{expect_hi}]"
            );
            assert!(
                (xa - 10.0).abs() < 0.2 && (xb - 30.0).abs() < 0.2,
                "{kind:?} x 覆盖 run"
            );
        }
    }

    /// 字形表完整性：95 项；关键字形 ink_w/笔画数与 Hershey 原数据一致。
    #[test]
    fn glyph_table_shape() {
        assert_eq!(GLYPHS.len(), 96);
        assert!(GLYPHS[0].strokes.is_empty(), "空格无笔画");
        let a = &GLYPHS[('A' as usize) - 32];
        assert_eq!(a.ink_w, 18);
        assert_eq!(a.strokes.len(), 3);
        // 'A' 顶点 y 顶 = 21（cap 高）
        let ymax = a
            .strokes
            .iter()
            .flat_map(|s| s.iter())
            .map(|p| p.y)
            .max()
            .unwrap_or(0);
        assert_eq!(ymax, 21);
    }

    /// emit_run：'A'@fs20 的 ink 落在 advance 格内且居中；
    /// 返回值 = 消耗 advance。
    #[test]
    fn emit_run_geometry_and_advance() {
        let mut g = PathGeom::default();
        let adv = emit_run("A", 4.0, 20.0, 20.0, false, 0.0, Mat2x3::IDENTITY, &mut g);
        assert!((adv - 0.667 * 20.0).abs() < 1e-3);
        assert!(!g.subs.is_empty(), "A 有笔画四边形");
        // ink AABB：x 居中于 [4, 4+13.34]，y ∈ (基线上方 0..21·gs)
        let gs = 20.0_f32 * CAP_EM / CAP_UNITS;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for sub in &g.subs {
            for &(x, y) in &sub.pts {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        let cell = 4.0_f32 + 0.667 * 20.0;
        assert!(
            x0 >= 4.0 - 0.1 && x1 <= cell + 0.1,
            "ink x [{x0},{x1}] 格 [4,{cell}]"
        );
        // 用户空间 y 向下：cap 顶 ≈ baseline − 21·gs（端延伸 hw 上探）
        let cap_top = 20.0 - 21.0 * gs;
        assert!(
            (y0 - cap_top).abs() < 1.5,
            "cap 顶 {y0} vs 期望 {cap_top}（±端延伸）"
        );
        assert!(y1 <= 20.0 + 1.2, "基线 y=20（腿端延伸 ≤hw·√2）：y1={y1}");
        // 全部子路径闭合四边形（填充环）
        assert!(g.subs.iter().all(|s| s.closed && s.pts.len() == 4));
    }

    /// bold 笔画更宽（同字号 ink 行数更多 / 四边形数更多）。
    #[test]
    fn bold_widens_strokes() {
        let (mut reg, mut bold) = (PathGeom::default(), PathGeom::default());
        emit_run("H", 0.0, 40.0, 20.0, false, 0.0, Mat2x3::IDENTITY, &mut reg);
        emit_run("H", 0.0, 40.0, 20.0, true, 0.0, Mat2x3::IDENTITY, &mut bold);
        // 同一竖笔（'I' 单笔）宽度对比：H 左竖 x 采样
        let x_of = |g: &PathGeom| {
            let mut xs = Vec::new();
            for sub in &g.subs {
                for &(x, _) in &sub.pts {
                    xs.push(x);
                }
            }
            (
                xs.iter().cloned().fold(f32::MAX, f32::min),
                xs.iter().cloned().fold(f32::MIN, f32::max),
            )
        };
        let (r0, _) = x_of(&reg);
        let (b0, _) = x_of(&bold);
        assert!(b0 < r0, "bold 左缘更靠左（笔画更宽）：{b0} < {r0}");
    }

    /// 车道BK：字重参数化的几何断言——单线字模的笔画展开宽度跟随
    /// [`STROKE_EM`] 常量（测试读常量不硬编码档位，后续改档不破测）。
    /// 'I' = 单竖笔 (4,21)-(4,0)：identity 矩阵下展开四边形的 x 跨度
    /// = 2·hw = fs·(STROKE_EM [+ BOLD_EXTRA_EM])，精确无 AA。
    #[test]
    fn stem_width_follows_weight_const() {
        let fs = 100.0_f32;
        let width_of = |g: &PathGeom| {
            let mut lo = f32::MAX;
            let mut hi = f32::MIN;
            for sub in &g.subs {
                for &(x, _) in &sub.pts {
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
            hi - lo
        };
        let mut g = PathGeom::default();
        emit_run("I", 0.0, 40.0, fs, false, 0.0, Mat2x3::IDENTITY, &mut g);
        assert_eq!(g.subs.len(), 1, "'I' 应单笔单四边形");
        let w = width_of(&g);
        let want = fs * STROKE_EM;
        assert!(
            (w - want).abs() < 1e-3,
            "竖笔宽 {w} 应精确 = fs·STROKE_EM = {want}"
        );
        // bold 附加档同式耦合（车道BK 扫描不改 BOLD_EXTRA_EM 语义）
        let mut gb = PathGeom::default();
        emit_run("I", 0.0, 40.0, fs, true, 0.0, Mat2x3::IDENTITY, &mut gb);
        let wb = width_of(&gb);
        let wantb = fs * (STROKE_EM + BOLD_EXTRA_EM);
        assert!(
            (wb - wantb).abs() < 1e-3,
            "bold 竖笔宽 {wb} 应精确 = fs·(STROKE_EM+BOLD_EXTRA_EM) = {wantb}"
        );
    }

    // -------------------------------------------------------------------
    // 车道AL：textPath 轨道构建 / 弧长采样 / 沿路径发射
    // -------------------------------------------------------------------

    /// PathTrack 构建：水平线 total=100；采样点/切线角精确；
    /// 越界与空轨道 None。
    #[test]
    fn pathtrack_line_sample() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (100.0, 0.0)],
                closed: false,
            }],
        };
        let t = PathTrack::build(&g);
        assert!((t.total - 100.0).abs() < 1e-4);
        let (p, ang) = t.sample(50.0).expect("采样/构建应成功");
        assert!((p.0 - 50.0).abs() < 1e-4 && (p.1 - 0.0).abs() < 1e-4);
        assert!(ang.abs() < 1e-5, "水平切线角 0：{ang}");
        assert!(t.sample(-0.1).is_none(), "起点前 None");
        assert!(t.sample(100.1).is_none(), "终点后 None");
        assert!(t.sample(100.0).is_some(), "终点恰可采样");
        // 空几何 / 单点：空轨道
        assert_eq!(PathTrack::build(&PathGeom::default()), PathTrack::default());
        let one = PathGeom {
            subs: vec![Subpath {
                pts: vec![(1.0, 1.0)],
                closed: false,
            }],
        };
        assert_eq!(PathTrack::build(&one), PathTrack::default());
    }

    /// 折线拐角：L 形轨道逐段切线角（水平 0 → 垂直 π/2，y 向下坐标）；
    /// 闭合 Z 边计入 total（与 polyline_length 同口径）。
    #[test]
    fn pathtrack_polyline_and_closed() {
        let g = crate::path::flatten_d("M 0 0 L 50 0 L 50 50", Mat2x3::IDENTITY);
        let t = PathTrack::build(&g);
        assert!((t.total - 100.0).abs() < 1e-3);
        let (p1, a1) = t.sample(25.0).expect("采样/构建应成功");
        assert!((p1.0 - 25.0).abs() < 1e-3 && p1.1.abs() < 1e-3 && a1.abs() < 1e-4);
        let (p2, a2) = t.sample(75.0).expect("采样/构建应成功");
        assert!((p2.0 - 50.0).abs() < 1e-3 && (p2.1 - 25.0).abs() < 1e-3);
        assert!(
            (a2 - core::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "垂直段切线角 π/2：{a2}"
        );
        // Z 闭合：正方形周长 4×50 = 200（闭合边入轨）
        let gz = crate::path::flatten_d("M 0 0 L 50 0 L 50 50 L 0 50 Z", Mat2x3::IDENTITY);
        let tz = PathTrack::build(&gz);
        assert!((tz.total - 200.0).abs() < 1e-3, "Z 周长 200: {}", tz.total);
        // 闭合边上的采样（弧长 175 = 末边中点 (0,25)）
        let (pc, _) = tz.sample(175.0).expect("采样/构建应成功");
        assert!((pc.0 - 0.0).abs() < 1e-3 && (pc.1 - 25.0).abs() < 1e-3);
    }

    /// 圆弧轨道：半圆弧长 ≈ π·r（细细分弦长和，容差 0.5%）。
    #[test]
    fn pathtrack_arc_length() {
        let g = crate::path::flatten_d("M 10 100 A 40 40 0 0 1 90 100", Mat2x3::IDENTITY);
        let t = PathTrack::build(&g);
        let expect = core::f32::consts::PI * 40.0;
        assert!(
            (t.total - expect).abs() / expect < 0.005,
            "半圆弧长 {} vs π·40={expect}",
            t.total
        );
        // 中点采样落在弧顶 (50, 60)
        let (p, _) = t.sample(t.total * 0.5).expect("采样/构建应成功");
        assert!(
            (p.0 - 50.0).abs() < 0.2 && (p.1 - 60.0).abs() < 0.2,
            "{p:?}"
        );
    }

    /// 沿水平路径发射：字形基线贴路径、格中点在采样点上（ink 上探）、
    /// 结束弧长 = Σ advance。
    #[test]
    fn emit_on_path_horizontal() {
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 100.0), (200.0, 100.0)],
                closed: false,
            }],
        };
        let t = PathTrack::build(&g);
        let mut out = PathGeom::default();
        let end = emit_run_on_path("AB", 0.0, &t, 20.0, false, 0.0, 0.0, 0.0, &mut out, None);
        let adv_ab = 20.0 * (0.667 + 0.667);
        assert!((end - adv_ab).abs() < 1e-3, "结束弧长 {end} vs {adv_ab}");
        assert!(!out.subs.is_empty(), "路径发射产生几何");
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for sub in &out.subs {
            for &(_, y) in &sub.pts {
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        // 基线贴路径 y=100：ink 全在基线上方（用户 y 向下）
        assert!(y1 <= 100.0 + 0.6, "ink 底 {y1} 不越过基线（端延伸级）");
        assert!(y0 >= 100.0 - 15.1, "ink 顶 {y0} ≈ cap 高+笔画半宽内");
    }

    /// 拐角旋转：弧长越过 90° 拐角后字形切线旋转 π/2——'E'（笔画
    /// 13×21：正常高>宽）旋转后 ink 宽>高。
    #[test]
    fn emit_on_path_rotates_at_corner() {
        let g = crate::path::flatten_d("M 0 0 L 100 0 L 100 100", Mat2x3::IDENTITY);
        let t = PathTrack::build(&g);
        let mut out = PathGeom::default();
        // startOffset 106：'E'（adv 13.34）格中点 112.67 落在垂直段
        let _ = emit_run_on_path("E", 106.0, &t, 20.0, false, 0.0, 0.0, 0.0, &mut out, None);
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for sub in &out.subs {
            for &(x, y) in &sub.pts {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        let (w, h) = (x1 - x0, y1 - y0);
        // 未旋转 E ink ≈ 9.9 宽×15.3 高（含笔画半宽）；旋转 90° 后互换
        assert!(w > h + 2.0, "旋转后 ink 宽 {w} 应大于高 {h}");
        // 基线贴垂直段 x=100：ink 以 x≈100 为轴、沿 y 前进
        assert!(
            (x0 - 100.0).abs() < 15.0 && (x1 - 100.0).abs() < 15.0,
            "ink x [{x0},{x1}] 贴 x=100 垂直段"
        );
    }

    /// 轨道末端截断：格中点越出 [0,total] 的字符不描绘（advance 照常
    /// 消耗）。
    #[test]
    fn emit_on_path_truncates_at_end() {
        // 'l' 单笔画：每字符恰 1 个四边形环
        let g = PathGeom {
            subs: vec![Subpath {
                pts: vec![(0.0, 0.0), (30.0, 0.0)],
                closed: false,
            }],
        };
        let t = PathTrack::build(&g);
        let mut out = PathGeom::default();
        let end = emit_run_on_path(
            "llllllll", 0.0, &t, 20.0, false, 0.0, 0.0, 0.0, &mut out, None,
        );
        let adv = 20.0 * 0.222;
        // mid_i = (i+0.5)·4.44 ≤ 30 → i ≤ 6.25 → 7 个渲染，advance 全消耗
        assert_eq!(out.subs.len(), 7, "7 个字形落轨");
        assert!((end - 8.0 * adv).abs() < 1e-3, "advance 照常走完 {end}");
        // 字形必须沿路径**散开**（每字形格中心贴各自采样点）而非全部
        // 叠在起点——回归锚：平移量取半格而非运行绝对 mid
        let mut centers: Vec<f32> = out
            .subs
            .iter()
            .map(|sp| sp.pts.iter().map(|&(x, _)| x).sum::<f32>() / sp.pts.len() as f32)
            .collect();
        centers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        for w in centers.windows(2) {
            assert!(w[1] - w[0] > 2.0, "相邻字形中心应错开 ≥2：{centers:?}");
        }
    }

    /// 车道DC（on-path 游标同源）：注入字体下逐字符 advance 走 hmtx、
    /// 总弧长 = `run_advance_ls_font` 求和（与发射相位同源）；无字体 =
    /// AFM 原口径逐位不变。修复前游标恒 AFM——钉字体下整行欠宽
    ///（WPT opacity.svg Tier C "textPath" 行 ink 182 vs chrome 207）。
    #[test]
    fn on_path_cursor_matches_emit_font() {
        let font = Font::parse(include_bytes!("../testdata/PinSans.ttf"))
            .expect("testdata/PinSans.ttf 应可解析");
        let track = PathTrack::build(&{
            let mut g = PathGeom::default();
            g.subs.push(Subpath {
                pts: vec![(0.0, 0.0), (600.0, 0.0)],
                closed: false,
            });
            g
        });
        let (s, fs, ls) = ("textPath", 50.0_f32, 0.0_f32);
        let mut g = PathGeom::default();
        let end = emit_run_on_path(s, 0.0, &track, fs, false, ls, 0.0, 0.0, &mut g, Some(&font));
        let want = run_advance_ls_font(s, fs, ls, 0.0, &font);
        assert!(
            (end - want).abs() < 1e-3,
            "on-path 末弧 {end} vs hmtx {want}"
        );
        // 判别力钉子：该串 hmtx ≠ AFM（否则本测试失去判别力）
        let afm = run_advance_ls(s, fs, ls, 0.0);
        assert!((want - afm).abs() > fs * 0.2, "hmtx({want}) ≈ AFM({afm})");
        // 无字体口径不变
        let mut g2 = PathGeom::default();
        let end0 = emit_run_on_path(s, 0.0, &track, fs, false, ls, 0.0, 0.0, &mut g2, None);
        assert!(
            (end0 - afm).abs() < 1e-3,
            "无字体 on-path {end0} vs AFM {afm}"
        );
    }

    /// 车道DC（x-height 合成）：PinSans 的 OS/2 为 v1（无 sxHeight），
    /// `x_height_em` 从 'x' 字形轮廓顶合成 ≈0.547（chrome 钉字体探针
    /// 实测 0.543，fonttools 1120/2048=0.5469）；与 Hershey 回落常数
    /// X_HEIGHT_EM 劈叉（判别力钉子）。
    #[test]
    fn font_x_height_synth() {
        let font = Font::parse(include_bytes!("../testdata/PinSans.ttf"))
            .expect("testdata/PinSans.ttf 应可解析");
        let xh = font
            .x_height_em()
            .expect("PinSans 'x' 轮廓应可合成 x-height");
        assert!(
            (xh - 1120.0 / 2048.0).abs() < 0.01,
            "合成 x-height {xh} vs 轮廓顶 0.5469"
        );
        assert!(
            (xh - X_HEIGHT_EM).abs() > 0.01,
            "合成值 {xh} ≈ Hershey 常数 {X_HEIGHT_EM}，判别力失效"
        );
    }

    /// 车道NA（stress-corpus）：缺字 ASCII 别名——'−'（U+2212，
    /// matplotlib 负刻度全用）等 Hershey 必缺形态按 ASCII 同源渲染，
    /// 不再出 tofu 框。三钉：advance 同源、发射几何逐位同 '-'、
    /// 宽度函数与发射一致（textLength 锚定不劈叉）。
    #[test]
    fn missing_glyph_ascii_alias() {
        // advance 同源：别名后的 advance 必须等于 ASCII 形态
        assert_eq!(advance_em('\u{2212}'), advance_em('-'));
        assert_eq!(advance_em('\u{2013}'), advance_em('-'));
        assert_eq!(advance_em('\u{00A0}'), advance_em(' '));
        assert_eq!(advance_em('\u{2019}'), advance_em('\''));
        // 非 ASCII 且无别名：回落 1em 不变（CJK 语义不动）
        assert_eq!(advance_em('中'), FALLBACK_ADV_EM);
        // 1:1 映射：advance_count 不变（textLength 分配分母）
        assert_eq!(advance_count("\u{2212}0.5"), advance_count("-0.5"));
        // 发射几何逐位同 '-'：同参数 emit_run，'−' 与 '-' 的 PathGeom
        // 完全相等（别名在字形臂前消费，无 tofu 框混入）
        let m = Mat2x3::IDENTITY;
        let mut g_minus = PathGeom::default();
        emit_run("\u{2212}", 10.0, 100.0, 40.0, false, 0.0, m, &mut g_minus);
        let mut g_hyph = PathGeom::default();
        emit_run("-", 10.0, 100.0, 40.0, false, 0.0, m, &mut g_hyph);
        assert_eq!(g_minus, g_hyph, "'−' 发射应与 '-' 逐位一致（无 tofu）");
        assert!(!g_minus.subs.is_empty(), "别名后必须出字形墨迹");
        // 混排行宽度同源：run_advance_ls 消费同一 advance_em
        assert_eq!(
            run_advance_ls("1\u{2212}2", 20.0, 0.0, 0.0),
            run_advance_ls("1-2", 20.0, 0.0, 0.0)
        );
    }
}
