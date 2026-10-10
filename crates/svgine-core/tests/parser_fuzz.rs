//! 解析器健壮性 fuzz（车道CI 一轮 + 车道DI 二轮 + 车道NU 三轮）——
//! no_std MCU 场景的鲁棒性常驻审计。
//!
//! 威胁模型：畸形输入（截断/超长/深度嵌套/UTF-8 多字节边界/负数/超大
//! 数值/空输入/仅分隔符/实体风暴/未闭合括号引号）不得 panic、不得无限
//! 循环、不得无界内存膨胀（MCU 上没有「重启浏览器」）。
//!
//! 车道DI 二轮扩展（组合形态与递归结构）：①递归嵌套生成器（use 链+
//! 回边环 / 嵌套 svg / pattern-grad 引用链环 / foreignObject 夹心 /
//! svg-in-image href 形态）②组合形态（滤镜链 8 长 × clip/mask/pattern/
//! marker 区域属性堆叠）③合法但极端（NaN·inf·1e±30·全零矩阵值注入，
//! 值全部可解析、看数值传播到 raster 的循环安全）。消费面新增 16×16
//! raster 相（瓦片烘焙/滤镜求值/裁剪循环的资源炸弹都在消费相暴露）+
//! DL 规模护栏断言。
//!
//! 车道NU 三轮扩展（48 波新增解析面定向语料，六族生成器）：①绝对单位
//! 乱串打 NF 的 11 消费点（pt/pc/in/mm/cm/q 系数换算 × 几何/描绘/文本/
//! 滤镜子区域/CSS transform/SMIL values）②CSS Color 4 乱串打 NG
//! （rgba/hsla 斜杠与 4 参/8hex/transparent/turn·rad·grad/none 分量）
//! ③syncbase 病态打 NH 时间图（自引/互引环/逆文档序深链/负偏移混合/
//! repeat 溢出/巨型 begin 列表/动画 id 与渲染 id 碰撞=NC 双注册面）
//! ④mask-type 混合值打 ND ⑤style 实体解码风暴打 NE ⑥dasharray 乱值
//! （负值/带单位/NaN 样串×病态路径几何）打 NC 真切向。消费面新增
//! **事件派发 + syncbase 清偿相**（dispatch_event →
//! reparse_preserving_smil → propagate_event_sync，仅 anims 非空时）。
//! 三轮战果：F1 挂死（fixpoint 每轮全量重建 O(N²) 次绑定构建，逆文档序
//! 深链 2000 实测 58.6s）——工作表化修复后 0.16s。
//!
//! 设计：确定性 xorshift64\* 语料生成器（变异种子语料 + 结构化病态生成）
//! × 各解析入口，`cargo test -p svgine-core` 常驻（默认 3000 轮、输入
//! ≤16KB，debug 下秒级）。离线大批量跑法（登记于
//! tests/oracle/reports/parser-fuzz-ci.md）：
//!
//! ```sh
//! FUZZ_ROUNDS=100000 FUZZ_MAXLEN=65536 cargo test --release -p svgine-core -- parser_fuzz --nocapture
//! ```
//!
//! 判定：任何 round 内 panic / 挂死（外层 timeout 杀）/ 结构不变量破坏
//! 都是 bug；round 号打点（每 500 轮 eprintln）+ 确定性 seed 保证挂死
//! 可复现（同 seed 重跑到该轮即得同一输入）。

use svgine_core::{builder, css, geom, path, raster, xml};

// ---------------------------------------------------------------------------
// 确定性 PRNG（xorshift64*；不引外部依赖，no_std 语料生成器语义一致）
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// [0, n) 均匀；n=0 → 0。
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next_u64() % 100 < pct
    }
}

/// 在 `s` 的随机**字符边界**处截断（UTF-8 安全：向前回退到边界）。
fn truncate_at_boundary(s: &str, byte_pos: usize) -> &str {
    let mut p = byte_pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    &s[..p]
}

// ---------------------------------------------------------------------------
// 种子语料 + token 池
// ---------------------------------------------------------------------------

/// 种子语料：覆盖 xml/path/transform/css/gradient/anim/text/filter/use。
const SEEDS: &[&str] = &[
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="64" height="64">
  <rect x="5" y="5" width="40" height="30" fill="#3366cc" rx="4"/>
  <circle cx="70" cy="30" r="18" fill="rgb(200,80,20)" stroke="black" stroke-width="2"/>
  <path d="M10,80 C30,60 50,100 80,70 Z" fill="url(#g1)"/>
  <linearGradient id="g1"><stop offset="0" stop-color="white"/><stop offset="1" stop-color="#000"/></linearGradient>
</svg>"##,
    r##"<svg viewBox="0 0 200 100"><style>
  rect { fill: red; stroke-width: 2px }
  .cls > path { fill: url(#grad); stroke-dasharray: 4 2; }
  #hero, g .deep:first-child { opacity: 0.5; transform: translate(10px,20px) rotate(45) scale(2) }
  [data-x~="a"] { filter: url(#f) }
</style>
<g class="cls"><path class="deep" d="M0,0 a11,11 0 11-.01 0z" pathLength="100"/></g>
<text x="10" y="90" font-size="12">Hello &#65; &amp; <![CDATA[raw < > &]]> tail</text>
</svg>"##,
    r##"<svg><defs><clipPath id="c"><rect width="50" height="50"/></clipPath>
<mask id="m"><rect width="10" height="10" fill="white"/></mask>
<pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><circle r="2"/></pattern>
<marker id="mk" markerWidth="4" markerHeight="4" refX="2" refY="2"><path d="M0,0L4,4"/></marker>
</defs>
<g clip-path="url(#c)" mask="url(#m)" fill="url(#p)" transform="translate(5,5) matrix(1,0,0,1,1,1)">
<path d="M0 0 L 30 0 30 30 Z" marker-mid="url(#mk)"/>
<use href="#sym" x="10" y="10"/><symbol id="sym"><rect width="8" height="8"/></symbol>
</g></svg>"##,
    r##"<?xml version="1.0"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://example.dtd" [
<!ENTITY shape "<path d='M0,0 l10,10'/>">
<!ENTITY w "40">
]>
<svg width="&w;" height="40"><g>&shape;&shape;<animate attributeName="x" values="0;10;20" dur="2s" repeatCount="indefinite"/></g></svg>"##,
    r##"<svg><animateMotion dur="3s" path="M0,0 C10,10 20,-10 30,0" keyPoints="0;0.5;1" keyTimes="0;0.5;1"><mpath href="#track"/></animateMotion>
<path id="track" d="M0,0 q 50,50 100,0 t 100,0"/>
<animateTransform attributeName="transform" type="rotate" from="0 50 50" to="360 50 50" dur="10s"/>
<animate attributeName="opacity" from="1" to="0" dur="1s" begin="0s;other.end" fill="freeze"/>
</svg>"##,
    r##"<svg viewBox="-50 -50 100 100"><filter id="f"><feGaussianBlur stdDeviation="2"/><feOffset dx="3" dy="3"/><feColorMatrix type="matrix" values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 1 0"/><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter>
<rect x="-40" y="-40" width="80" height="80" filter="url(#f)" fill="none" stroke="teal" stroke-dasharray="2,3,1" stroke-dashoffset="1.5"/></svg>"##,
    r##"<svg><text x="0" y="20" textLength="80" lengthAdjust="spacingAndGlyphs" direction="rtl" writing-mode="tb"><tspan dx="1 2 3" dy="-1">a</tspan><textPath href="#p" startOffset="25%">mid</textPath></text><path id="p" d="M10,50 A 30,20 45 1 0 90,50"/></svg>"##,
    r##"<svg><polyline points="0,0 10,10, 20,0 1e2,5 -3,4"/><polygon points="5,5 15,5 15,15"/>
<image href="data:image/png;base64,iVBORw0KGgo=" x="0" y="0" width="20" height="20"/>
<switch><g requiredFeatures="http://www.w3.org/TR/SVG11/feature#Shape"><rect width="9" height="9"/></g><circle r="4"/></switch></svg>"##,
];

/// 变异插入 token 池：结构性破坏符 + 数值边界 + UTF-8 多字节 + 实体 +
/// CSS/动画关键字。多字节字符专打「切片落在字符中间」类 panic。
const TOKENS: &[&str] = &[
    "<g>",
    "</g>",
    "<svg",
    ">",
    "/>",
    "<",
    "</",
    "\"",
    "'",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ":",
    ";",
    ",",
    "%",
    "&",
    "&#",
    "&#x",
    "&#0;",
    "&#x110000;",
    "&#xD800;",
    "&lt;",
    "&bogus;",
    "&",
    ";",
    "<![CDATA[",
    "]]>",
    "<!--",
    "-->",
    "<?xml ",
    "?>",
    "<!DOCTYPE",
    "<!ENTITY x \"y\">",
    "[<!ENTITY e \"&e;\">]",
    "é",
    "中",
    "\u{00A0}",
    "\u{2028}",
    "\u{FFFD}",
    "\u{10FFFF}",
    "\u{0007}",
    "M0 0",
    "z",
    "a11 11 0 11-.01 0",
    "L1.2.3",
    "23.",
    "1e",
    "1e+",
    "e5",
    "0.",
    ".5",
    "-",
    "+",
    ".",
    "1e40",
    "-1e-40",
    "9999999999999999999999e99999999",
    "3.4e38",
    "1e-45",
    "NaN",
    "inf",
    "-0",
    "translate(",
    "scale(",
    "rotate(",
    "matrix(1,2,3,4,5,6",
    "skewX(30)",
    "url(#",
    "url(' # red ')",
    "url(",
    "var(--a",
    "var(--a)",
    "<stop",
    "offset=",
    "stop-color=",
    "<animate ",
    "begin=",
    "dur=",
    "values=",
    "keyTimes=",
    "keySplines=",
    "pathLength=\"0\"",
    "stroke-dasharray=\"0,0\"",
    "stroke-dasharray=\"",
    "stroke-dashoffset=\"",
    "style=\"",
    "fill=",
    "d=",
    "transform=",
    "viewBox=",
    "preserveAspectRatio=",
    "xMinYMax",
    "slice",
    "<text",
    "<tspan",
    "<textPath",
    "<filter",
    "<feTurbulence",
    "baseFrequency=\"0 0\"",
    "stdDeviation=\"1e10\"",
    "tableValues=\"",
    "first-child",
    "~",
    "rgba(",
    "#",
    "<use href=\"#",
    "id=\"",
    "class=\"",
    "g g g g g",
    "calc(",
    "px",
    "em",
    "@media",
    "@import",
    "!important",
    // 车道NU 三轮：单位/颜色/时间/token 池扩展
    "pt",
    "in",
    "q",
    "mm",
    "cm",
    "pc",
    "1pt",
    "2mm3q",
    "turn",
    "deg",
    "grad",
    "rad",
    "hsl(",
    "hsla(",
    "rgb(",
    " / 0.5)",
    "#12345678",
    "transparent",
    "none",
    "mask-type",
    "alpha",
    "luminance",
    "beginEvent",
    "click",
    "indefinite",
    ".begin+1s",
    ".end-2s",
    "repeat(3)",
    "id=\"a\"",
    "dur=\"1s\"",
    "<animate id=\"x\" ",
    "&amp;gt;",
];

// ---------------------------------------------------------------------------
// 结构化病态生成器（每类一个 `fn(&mut Rng, usize) -> String`）
// ---------------------------------------------------------------------------

/// 深度嵌套（闭合/不闭合混合——树构建与栈平衡压力）。
fn gen_deep_nesting(rng: &mut Rng, n: usize) -> String {
    let mut s = String::from("<svg viewBox=\"0 0 10 10\">");
    for _ in 0..n {
        s.push_str(rng.pick(&["<g>", "<g fill='red'>", "<svg>", "<text y='1'>"]));
        if rng.chance(10) {
            s.push_str("<rect width='1'/>");
        }
    }
    for _ in 0..n / 2 {
        s.push_str("</g>");
    }
    s
}

/// 超长单属性（属性臂扫描压力）+ 数值边界。
fn gen_huge_attr(rng: &mut Rng, n: usize) -> String {
    let which = rng.below(5);
    let fill = "1".repeat(n);
    match which {
        0 => format!("<svg><path d=\"M0 0 {}\"/></svg>", "l 1 1 ".repeat(n.min(20_000))),
        1 => format!("<svg viewBox=\"0 0 {v} {v}\"><rect r=\"1e30\" stroke-dasharray=\"{d}\" stroke-dashoffset=\"-1e20\"/></svg>", v = fill, d = "0.001,"),
        2 => format!("<svg><rect transform=\"{}\"/></svg>", "translate(1e20,1e-20) ".repeat(n.min(20_000))),
        3 => format!("<svg><text font-size=\"1e9\">{}</text></svg>", "ab".repeat(n.min(20_000))),
        _ => format!("<svg><rect stroke-dasharray=\"{}\"/></svg>", "0,".repeat(n.min(20_000))),
    }
}

/// 实体风暴：DOCTYPE 声明 + 大量引用（展开输出体积压力）。
/// 离线模式用大尺寸探测内存上限；常驻模式尺寸温和（展开输出有界断言）。
fn gen_entity_storm(rng: &mut Rng, value_len: usize) -> String {
    let value = "x".repeat(value_len);
    let refs = "&e;".repeat((rng.below(400) + 50).min(if value_len > 1024 { 20 } else { 400 }));
    format!(
        "<!DOCTYPE svg [<!ENTITY e \"{v}\"><!ENTITY % pe \"{v}\"><!ENTITY f SYSTEM \"u\">]><svg>{r}</svg>",
        v = value,
        r = refs
    )
}

/// CSS 组合器链 × 深树（后代匹配工作量压力——指数级匹配挂死探针）。
/// 常驻模式 chain/depth 温和（有界工作量断言）；离线模式放大。
fn gen_css_chain(rng: &mut Rng, chain: usize) -> String {
    let depth = chain * (2 + rng.below(3));
    let sel = "g ".repeat(chain);
    let mut body = String::new();
    for _ in 0..depth {
        body.push_str("<g>");
    }
    body.push_str("<rect width='3' height='3'/>");
    for _ in 0..depth {
        body.push_str("</g>");
    }
    format!(
        "<svg><style>{s} rect {{ fill: red }} g {{ fill: blue }}</style>{b}</svg>",
        s = sel,
        b = body
    )
}

/// 未闭合括号/引号/标签 + 多字节字符贴脸（切片边界 panic 探针）。
fn gen_unclosed(rng: &mut Rng, n: usize) -> String {
    let mut s = String::new();
    for _ in 0..(n / 8 + 1) {
        match rng.below(8) {
            0 => s.push_str("translate(é中"),
            1 => s.push_str("<svg attr=\"未闭合"),
            2 => s.push_str("rotate("),
            3 => s.push_str("<g><path d='M0 0"),
            4 => s.push_str("matrix(1,2,3"),
            5 => s.push_str("<!-- 未闭合"),
            6 => s.push_str("scale(é"),
            _ => s.push_str("<![CDATA[未闭合"),
        }
        s.push_str(rng.pick(&["", " ", ",", "1", "é", "中"]));
    }
    s
}

/// 仅分隔符 / 空输入 / 纯符号。
fn gen_degenerate(rng: &mut Rng, n: usize) -> String {
    match rng.below(6) {
        0 => String::new(),
        1 => " ".repeat(n),
        2 => ",".repeat(n),
        3 => {
            let mut s = String::new();
            for _ in 0..n {
                s.push_str(rng.pick(&[",", " ", "\t", "\n", "-"]));
            }
            s
        }
        4 => "<".repeat(n.min(4096)),
        _ => ">".repeat(n.min(4096)),
    }
}

// ---------------------------------------------------------------------------
// 车道DI 二轮生成器：递归嵌套 / 组合形态 / 合法但极端
// ---------------------------------------------------------------------------

/// ① 递归嵌套：svg-in-image / use 链与环 / g-in-g / 嵌套 svg /
/// pattern-grad 引用链/环 / foreignObject 夹心。深度边界探测
/// （闭合/不闭合混合 + 自引用与二环形态）。
fn gen_recursive_nesting(rng: &mut Rng, n: usize) -> String {
    let kind = rng.below(7);
    let n = n.clamp(2, 20_000);
    match kind {
        // use 链 + 周期性回边（环检测压力 + 实例化深度）
        0 => {
            let mut s = String::from(r##"<svg viewBox="0 0 100 100">"##);
            for i in 0..n {
                let nxt = if i + 1 < n {
                    format!(r##"<use href="#u{}"/>"##, i + 1)
                } else {
                    // 尾部回边造环（a→b→…→z→a）
                    r##"<use href="#u0"/>"##.to_string()
                };
                s.push_str(&format!(
                    r##"<g id="u{}"><rect width='2' height='2' fill='red'/>{}</g>"##,
                    i, nxt
                ));
            }
            s.push_str(r##"<use href="#u0"/></svg>"##);
            s
        }
        // 二环 / 自引用（use 环检测的定向形态）
        1 => {
            let mut s = String::from(r##"<svg viewBox="0 0 100 100">"##);
            for i in 0..2 {
                let other = (i + 1) % 2;
                s.push_str(&format!(
                    r##"<g id="c{}"><use href="#c{}"/><rect width='3' height='3' fill='blue'/></g>"##,
                    i, other
                ));
            }
            s.push_str(r##"<use href="#c0"/></svg>"##);
            s
        }
        // 嵌套 svg（视口递归）×N，随机缺闭合
        2 => {
            let mut s = String::from(r##"<svg viewBox="0 0 10 10">"##);
            for i in 0..n {
                if rng.chance(15) {
                    s.push_str("<svg>");
                } else {
                    s.push_str(&format!(
                        r##"<svg x="{}" y="{}" width="8" height="8">"##,
                        i % 3,
                        i % 5
                    ));
                }
                if rng.chance(20) {
                    s.push_str("<rect width='1' height='1' fill='red'/>");
                }
            }
            for _ in 0..n / 2 {
                s.push_str("</svg>");
            }
            s
        }
        // pattern 内容引用链/环（资源护栏定向：SS 复合与瓦片位图）
        3 => {
            let depth = (n % 9) + 1;
            let mut s = String::from(r##"<svg viewBox="0 0 100 100">"##);
            for i in 0..depth {
                // 尾级回指首级（环）或落叶（red）
                let fill = if i + 1 < depth {
                    format!(r##"url(#p{})"##, i + 1)
                } else if rng.chance(50) {
                    r##"url(#p0)"##.to_string()
                } else {
                    "red".to_string()
                };
                s.push_str(&format!(
                    r##"<pattern id="p{}" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="10" height="10" fill="{}"/></pattern>"##,
                    i, fill
                ));
            }
            s.push_str(r##"<rect width="100" height="100" fill="url(#p0)"/></svg>"##);
            s
        }
        // 渐变 href 链（长链解析工作量；stop 只钉尾或全缺）
        4 => {
            let depth = (n % 500) + 2;
            let mut s = String::from(r##"<svg viewBox="0 0 100 100">"##);
            for i in 0..depth {
                let href = if i + 1 < depth {
                    format!(r##" href="#g{}""##, i + 1)
                } else {
                    String::new()
                };
                let stop = if i + 1 == depth && rng.chance(60) {
                    r##"<stop offset="0" stop-color="red"/>"##
                } else {
                    ""
                };
                s.push_str(&format!(
                    r##"<linearGradient id="g{}"{}>{}</linearGradient>"##,
                    i, href, stop
                ));
            }
            s.push_str(r##"<rect width="100" height="100" fill="url(#g0)"/></svg>"##);
            s
        }
        // foreignObject × 外命名空间夹心（BI 抑制 + 树深度）
        5 => {
            let mut s = String::from(r##"<svg viewBox="0 0 64 64">"##);
            for _ in 0..(n / 8 + 1).min(400) {
                s.push_str(rng.pick(&[
                    r##"<foreignObject width='64' height='64'><foo:bar xmlns:foo="urn:x">"##,
                    r##"<foo:bar xmlns:foo="urn:x"><svg viewBox="0 0 4 4">"##,
                    r##"<g><rect width='2' height='2'/>"##,
                ]));
            }
            s.push_str("<rect width='4' height='4' fill='red'/>");
            for _ in 0..(n / 16 + 1).min(400) {
                s.push_str(rng.pick(&[r##"</foo:bar></foreignObject>"##, "</svg>", "</g>"]));
            }
            s.push_str("</svg>");
            s
        }
        // svg-in-image href 形态（core 不描绘 svg href；宿主递归语义
        // 由 CLI 侧测试钉）——各种自指/回指文件名 + data URI
        _ => {
            let mut s = String::from(r##"<svg viewBox="0 0 64 64">"##);
            for i in 0..(n / 4 + 1).min(200) {
                let href = rng.pick(&[
                    "self.svg",
                    "a.svg",
                    "../loop.svg",
                    "data:image/svg+xml;base64,PHN2Zy8+",
                    "data:image/svg+xml,%3Csvg%3E%3Crect/%3E%3C/svg%3E",
                    "#frag",
                    "",
                ]);
                s.push_str(&format!(
                    r##"<image href="{}" x="{}" y="0" width="32" height="32"/>"##,
                    href,
                    i % 2
                ));
            }
            s.push_str("</svg>");
            s
        }
    }
}

/// ② 组合形态：滤镜链 × transform 嵌套 × clipPath/mask/pattern/marker
/// 叠加。容器属性随机堆叠（每层 0-4 个区域性属性臂）。
fn gen_combo_stack(rng: &mut Rng, n: usize) -> String {
    let n = n.clamp(2, 4_000);
    let mut s = String::from(r##"<svg viewBox="0 0 64 64"><defs>"##);
    // 池：clip / mask / pattern / filter / marker 各一枚（id 稳定）
    s.push_str(r##"<clipPath id="c"><rect x="2" y="2" width="60" height="60"/></clipPath>"##);
    s.push_str(r##"<mask id="m"><rect width="64" height="64" fill="white"/></mask>"##);
    s.push_str(
        r##"<pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse"><rect width="8" height="8" fill="yellow"/></pattern>"##,
    );
    // 滤镜链 8 长（CN 的 17 primitive 面——取 8 个高频 primitive 级联）
    let prims = [
        r##"<feGaussianBlur stdDeviation="1"/>"##,
        r##"<feOffset dx="1" dy="1"/>"##,
        r##"<feColorMatrix type="saturate" values="0.5"/>"##,
        r##"<feComponentTransfer><feFuncR type="linear" slope="0.8"/></feComponentTransfer>"##,
        r##"<feBlend in="SourceGraphic" in2="SourceAlpha" mode="multiply"/>"##,
        r##"<feComposite operator="atop" in2="SourceAlpha"/>"##,
        r##"<feFlood flood-color="red" result="fl"/><feMerge><feMergeNode in="fl"/><feMergeNode in="SourceGraphic"/></feMerge>"##,
        r##"<feMorphology operator="dilate" radius="0.5"/>"##,
    ];
    let chain_len = 1 + rng.below(8.min(n));
    s.push_str(r##"<filter id="f" x="-10%" y="-10%" width="120%" height="120%">"##);
    for _ in 0..chain_len {
        s.push_str(prims[rng.below(prims.len())]);
    }
    s.push_str("</filter>");
    s.push_str(
        r##"<marker id="mk" markerWidth="4" markerHeight="4" refX="2" refY="2"><path d="M0,0L4,4"/></marker>"##,
    );
    s.push_str("</defs>");
    // N 层容器，每层随机 0-4 个区域性属性
    let region_attrs = [
        r##" clip-path="url(#c)""##,
        r##" mask="url(#m)""##,
        r##" filter="url(#f)""##,
        r##" fill="url(#p)""##,
        r##" opacity="0.5""##,
        r##" transform="translate(1,1) scale(0.9) rotate(1)""##,
        r##" clip-path="url(#missing)""##,
        r##" mask="url(#missing)""##,
        r##" filter="url(#missing)""##,
        r##" fill="url(#missing) red""##,
    ];
    for _ in 0..n {
        s.push_str("<g");
        let k = rng.below(5);
        for _ in 0..k {
            s.push_str(region_attrs[rng.below(region_attrs.len())]);
        }
        s.push('>');
        if rng.chance(25) {
            s.push_str("<rect width='6' height='6' marker-mid='url(#mk)' stroke='teal' stroke-width='0.5'/>");
        }
    }
    s.push_str("<rect width='8' height='8' fill='red'/>");
    for _ in 0..n {
        s.push_str("</g>");
    }
    s.push_str("</svg>");
    s
}

/// ③ 合法但极端：超大坐标 / 超小 viewBox / 全零奇异矩阵 / NaN·inf
/// 属性值注入（值全部可解析，重点看数值传播到 raster 的循环安全）。
fn gen_legal_extreme(rng: &mut Rng, _n: usize) -> String {
    // 数值候选：合法可解析但病态
    const NUMS: &[&str] = &[
        "NaN", "-NaN", "inf", "-inf", "1e30", "1e-30", "-1e30", "0", "-0", "1e18", "1e-18",
        "3.4e38", "1e-45", "1e9", "0.5", "65504", "65505",
    ];
    // 注入点：数值型呈现属性（值面全覆盖——viewport/几何/描绘/滤镜）
    let attr_inject = |rng: &mut Rng, s: &mut String| {
        let v = NUMS[rng.below(NUMS.len())];
        let w = NUMS[rng.below(NUMS.len())];
        match rng.below(14) {
            0 => s.push_str(&format!(r##" viewBox="{v} 0 {w} 100""##)),
            1 => s.push_str(&format!(r##" width="{v}" height="{w}""##)),
            2 => s.push_str(&format!(r##" transform="matrix({v},{w},-{w},{v},{v},0)""##)),
            3 => s.push_str(&format!(
                r##" transform="scale({v}) rotate({w}) translate({w},{v})""##
            )),
            4 => s.push_str(&format!(
                r##" stroke-dasharray="{v} {w}" stroke-dashoffset="-{v}""##
            )),
            5 => s.push_str(&format!(r##" stroke-width="{v}""##)),
            6 => s.push_str(&format!(r##" opacity="{v}" fill-opacity="{w}""##)),
            7 => s.push_str(&format!(r##" r="{v}" cx="{w}""##)),
            8 => s.push_str(&format!(r##" x="{v}" y="{w}" rx="{v}""##)),
            9 => s.push_str(&format!(r##" font-size="{v}""##)),
            10 => s.push_str(&format!(r##" pathLength="{v}""##)),
            11 => s.push_str(&format!(r##" stdDeviation="{v}" dx="{w}""##)),
            12 => s.push_str(&format!(r##" offset="{v}" stop-opacity="{w}""##)),
            _ => s.push_str(&format!(r##" textLength="{v}" font-size="{w}""##)),
        }
    };
    let mut s = String::from("<svg");
    // 根 viewBox 3 档：病态 / 超小 / 正常
    match rng.below(3) {
        0 => {
            let v = NUMS[rng.below(NUMS.len())];
            let w = NUMS[rng.below(NUMS.len())];
            s.push_str(&format!(r##" viewBox="0 0 {v} {w}""##));
        }
        1 => s.push_str(r##" viewBox="0 0 1e-20 1e-20""##),
        _ => s.push_str(r##" viewBox="0 0 100 100""##),
    }
    if rng.chance(60) {
        attr_inject(rng, &mut s);
    }
    s.push('>');
    // defs：渐变坐标病态 + 全零 gradientTransform（奇异逆）
    if rng.chance(60) {
        let v = NUMS[rng.below(NUMS.len())];
        let w = NUMS[rng.below(NUMS.len())];
        s.push_str(&format!(
            r##"<linearGradient id="g" x1="{v}" y1="0" x2="{w}" y2="1" gradientTransform="matrix(0,0,0,0,0,0)"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>"##
        ));
        s.push_str(&format!(
            r##"<radialGradient id="rg" cx="{v}" cy="0" r="{w}" fr="{v}" fx="{w}"><stop offset="0" stop-color="green"/></radialGradient>"##
        ));
    }
    // N 个注入元素
    for _ in 0..(1 + rng.below(8)) {
        let mut el = String::from(*rng.pick(&["<rect", "<circle", "<path", "<text", "<g"]));
        if el == "<path" {
            let v = NUMS[rng.below(NUMS.len())];
            el.push_str(&format!(r##" d="M{v} {v} L100 100 A {v} {v} 0 1 0 0 0""##));
            s.push_str(&el);
            attr_inject(rng, &mut s);
            s.push_str("/>");
            continue;
        }
        if el == "<text" {
            el.push_str(r##" x="1" y="5">ab</text>"##);
            s.push_str(&el);
            continue;
        }
        if el == "<g" {
            el.push('>');
            s.push_str(&el);
            attr_inject(rng, &mut s);
            s.push_str("<rect width='4' height='4' fill='url(#rg)'/></g>");
            continue;
        }
        s.push_str(&el);
        attr_inject(rng, &mut s);
        if rng.chance(40) {
            s.push_str(r##" fill="url(#g)""##);
        }
        s.push_str("/>");
    }
    s.push_str("</svg>");
    s
}

// ---------------------------------------------------------------------------
// 车道NU 三轮生成器：48 波新增解析面定向语料（六族）
// ---------------------------------------------------------------------------

/// 乱值池：绝对单位族 × 数值边界 × 多字节 × 结构破坏（单位/数值混排、
/// 双符号、十六进制样、指数残缺、单位连写）。
const UNIT_JUNK: &[&str] = &[
    "12pt",
    "1e3pt",
    "--in",
    "pt",
    "0x10mm",
    "5q",
    "Q",
    "-3PC",
    "1.2.3pc",
    "12pt8px",
    "3中mm",
    "1em",
    "1rem",
    "10%",
    "1e30cm",
    "-1e-30q",
    "2.5",
    ".5in",
    "1e",
    "1e+",
    "e5",
    "NaNpt",
    "infmm",
    "++4cm",
    "1__pt",
    "0pt",
    "-0pc",
    "1e309999999q",
    "9999999999999999999999mm",
    "1.5e-45in",
    "1z",
    "1ptx",
    "中",
    "😀",
    "1,,2cm",
    " ;3q",
    "+-1mm",
    "1e2e3pt",
    "0.Q",
];

/// ① 绝对单位乱串 × NF 的 11 消费点（属性×位全覆盖：XML 属性位 /
/// CSS 声明位 / SMIL values 位）。
fn gen_abs_units(rng: &mut Rng, n: usize) -> String {
    let mut s = String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"##);
    let u = |rng: &mut Rng| -> String { UNIT_JUNK[rng.below(UNIT_JUNK.len())].to_string() };
    let u2 = |rng: &mut Rng| -> String {
        format!(
            "{}{}",
            UNIT_JUNK[rng.below(UNIT_JUNK.len())],
            UNIT_JUNK[rng.below(UNIT_JUNK.len())]
        )
    };
    // 根尺寸消费点（parse_len_str：pt/in→非零系数面）
    s.push_str(&format!(r##" width="{}" height="{}""##, u(rng), u(rng)));
    s.push('>');
    for _ in 0..(2 + rng.below(6)) {
        match rng.below(8) {
            // 几何消费点：x/y/width/height/rx + viewBox 单位（非法但不得崩）
            0 => s.push_str(&format!(
                r##"<rect x="{}" y="{}" width="{}" height="{}" rx="{}"/>"##,
                u(rng), u(rng), u(rng), u(rng), u(rng)
            )),
            // 描绘消费点：stroke-width / dasharray 多值带单位 / dashoffset
            1 => s.push_str(&format!(
                r##"<path d="M0 0 L40 0" stroke="red" stroke-width="{}" stroke-dasharray="{} {} {}" stroke-dashoffset="{}"/>"##,
                u(rng), u(rng), u2(rng), u(rng), u(rng)
            )),
            // 文本消费点：font-size / letter-spacing / dx/dy 列表 / textLength
            2 => s.push_str(&format!(
                r##"<text x="{}" y="{}" font-size="{}" letter-spacing="{}" textLength="{}"><tspan dx="{} {}" dy="{}">ab</tspan></text>"##,
                u(rng), u(rng), u(rng), u(rng), u(rng), u(rng), u(rng), u(rng)
            )),
            // baseline-shift / marker 四参
            3 => s.push_str(&format!(
                r##"<text y="30" baseline-shift="{}">x</text><marker id="m" refX="{}" refY="{}" markerWidth="{}" markerHeight="{}"><path d="M0,0L2,2"/></marker>"##,
                u(rng), u(rng), u(rng), u(rng), u(rng)
            )),
            // 滤镜子区域（fe 子区域坐标 NF 面）
            4 => s.push_str(&format!(
                r##"<filter id="f"><feGaussianBlur x="{}" y="{}" width="{}" height="{}" stdDeviation="{}"/></filter><rect width="20" height="20" filter="url(#f)"/>"##,
                u(rng), u(rng), u(rng), u(rng), u(rng)
            )),
            // CSS 声明位：几何属性 + transform + transform-origin（NF
            // 新面 parse_transform_css / transform-origin）
            5 => s.push_str(&format!(
                r##"<rect style="x:{};y:{};width:{};height:{};transform:translate({}) rotate({}deg);transform-origin:{} {};stroke-width:{}"/>"##,
                u(rng), u(rng), u(rng), u(rng), u(rng), u(rng), u(rng), u(rng), u(rng)
            )),
            // SMIL values 带单位（NF：SMIL 带单位值格）
            6 => s.push_str(&format!(
                r##"<rect width="10" height="10"><animate attributeName="x" values="{};{};{}" dur="1s" begin="0s"/></rect>"##,
                u(rng), u(rng), u(rng)
            )),
            // 样式表声明位（同 5 走 css::parse 路径）
            _ => s.push_str(&format!(
                r##"<style>rect {{ x: {}; stroke-width: {}; font-size: {} }} .a {{ rx: {} }}</style><rect class="a" width="8" height="8"/>"##,
                u(rng), u(rng), u(rng), u(rng)
            )),
        }
    }
    let _ = n;
    s.push_str("</svg>");
    s
}

/// ② CSS Color 4 乱串打 NG 面（rgba 4 参/斜杠 alpha/8hex/transparent/
/// turn·rad·grad/none 分量/百分比溢出）。消费点：fill/stroke/stop-color/
/// flood-color + CSS 声明位 + SMIL 动画色。
const COLOR_JUNK: &[&str] = &[
    "rgba(",
    "rgba()",
    "rgba(1,2,3)",
    "rgb(1,2,3,4,5)",
    "rgba(1,2,3,0.5)",
    "rgb(1 2 3)",
    "rgb(1 2 3 / 0.5)",
    "rgb(1 2 3/0)",
    "rgb(50% 50% 50% / 50%)",
    "rgb(none none none)",
    "rgb(none 2 3 / none)",
    "rgba(255,0,0,-1)",
    "rgba(255,0,0,1e300)",
    "#12345",
    "#1234567",
    "#12345678",
    "#123456789",
    "#abcd",
    "#ABCDEF80",
    "#12",
    "#",
    "#zzzzzz",
    "transparent",
    "TRANSPARENT",
    "hsl(99999,-1,999)",
    "hsl(0.5turn,100%,50%)",
    "hsl(1grad 50% 50% / 0.2)",
    "hsla(-3rad,50%,50%,0.5)",
    "hsl(200 50% 50%)",
    "hsl(none 0% 0%)",
    "hsl(1deg,50%,50%,0.25,0.5)",
    "rgb(1,2)",
    "rgb()",
    "hsl()",
    "rgb(rgb(1,2,3))",
    "rgb((1,2,3)",
    "rgb(1,2,3))",
    "color(srgb 1 0 0)",
    "lab(50% 20 30)",
    "1e300%",
    "50%%",
    "é",
    "rgb(é,2,3)",
    "currentColor",
    "rgb( 1 , 2 , 3 )",
    "rgba(1 2 3 4)",
    "hsl(0turn 0turn 0turn)",
];

fn gen_css_colors(rng: &mut Rng, _n: usize) -> String {
    let c = |rng: &mut Rng| COLOR_JUNK[rng.below(COLOR_JUNK.len())];
    let mut s =
        String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><defs>"##);
    // stop-color 消费点（渐变采样管线的颜色解析）
    s.push_str(&format!(
        r##"<linearGradient id="g"><stop offset="0" stop-color="{}"/><stop offset="1" stop-color="{}"/><stop offset="{}" stop-color="{}"/></linearGradient>"##,
        c(rng), c(rng), c(rng), c(rng)
    ));
    // flood-color / lighting-color（滤镜管线）
    s.push_str(&format!(
        r##"<filter id="f"><feFlood flood-color="{}"/><feDiffuseLighting lighting-color="{}"><feDistantLight azimuth="45"/></feDiffuseLighting></filter>"##,
        c(rng), c(rng)
    ));
    s.push_str("</defs>");
    // 呈现属性位
    s.push_str(&format!(
        r##"<rect width="30" height="30" fill="{}" stroke="{}" stroke-opacity="{}" fill-opacity="{}"/>"##,
        c(rng), c(rng), c(rng), c(rng)
    ));
    // 渐变引用 + 滤镜引用（消费面深入）
    s.push_str(r##"<rect y="30" width="30" height="30" fill="url(#g)" filter="url(#f)"/>"##);
    // CSS 声明位（NE：style 实体解码后的声明解析同管）
    s.push_str(&format!(
        r##"<style>rect {{ fill: {}; stroke: {} }} .c {{ color: {} }}</style>"##,
        c(rng),
        c(rng),
        c(rng)
    ));
    // SMIL 动画色（NG：SMIL 走同管颜色插值）
    s.push_str(&format!(
        r##"<circle r="5"><animate attributeName="fill" values="{};{};{}" dur="2s" begin="0s"/></circle>"##,
        c(rng), c(rng), c(rng)
    ));
    s.push_str("</svg>");
    s
}

/// ③ syncbase 病态打 NH 前向时间图（环/深链/负偏移/repeat 溢出/巨型
/// 列表）+ NC 动画 id 双注册面（动画 id 与渲染 id 碰撞、动画间 id 冲突）。
fn gen_syncbase(rng: &mut Rng, n: usize) -> String {
    let n = (n % 120) + 3;
    let mut s = String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"##);
    // begin 条目池：合法/残缺/病态混合
    let item = |rng: &mut Rng, pool: &[String]| -> String {
        match rng.below(12) {
            0 => format!(
                "{}{}",
                pool[rng.below(pool.len())],
                ["+1s", "-0.5s", "+1e999s", "++1s", "+-", "+1", "s"][rng.below(7)]
            ),
            1 => format!(
                "{}.repeat({})",
                pool[rng.below(pool.len())],
                ["0", "4294967295", "99999999999999999999", "-1", "2.5", ""][rng.below(6)]
            ),
            2 => format!(
                "{}.{}",
                pool[rng.below(pool.len())],
                [
                    "begin",
                    "end",
                    "beginner",
                    "repeat()",
                    "repeat(-1)",
                    "BEGIN",
                    "click",
                    "beginEvent"
                ][rng.below(8)]
            ),
            3 => format!("..begin{}", ["+1s", ""][rng.below(2)]),
            4 => format!("a.b.begin+{}", ["1s", "-1s", "x"][rng.below(3)]),
            5 => "indefinite".to_string(),
            6 => "wallclock(2020)".to_string(),
            7 => format!(
                "{}.begin;{}.end",
                pool[rng.below(pool.len())],
                pool[rng.below(pool.len())]
            ),
            8 => "".to_string(),
            9 => format!(
                "{}.begin+{}s",
                pool[rng.below(pool.len())],
                if rng.chance(50) { "-1e30" } else { "1e-30" }
            ),
            10 => format!("{}.end", pool[rng.below(pool.len())]),
            _ => format!("{}.begin", pool[rng.below(pool.len())]),
        }
    };
    let kind = rng.below(7);
    // id 池：一半与渲染元素同名（NC 双注册碰撞面）
    let ids: Vec<String> = (0..n)
        .map(|i| {
            if rng.chance(30) {
                "tgt".to_string() // 渲染元素 id 大量同名
            } else {
                format!("a{i}")
            }
        })
        .collect();
    match kind {
        // 纯环：i → i+1 → … → 0（互引，循环定义）
        0 => {
            for i in 0..n {
                let dep = (i + 1) % n;
                s.push_str(&format!(
                    r##"<rect width="4" height="4"><animate id="{}" attributeName="x" from="0" to="60" dur="1s" begin="{}.begin+0.1s"/></rect>"##,
                    ids[i], ids[dep]
                ));
            }
        }
        // 自引环
        1 => {
            for i in 0..n {
                s.push_str(&format!(
                    r##"<rect width="4" height="4"><animate id="a{i}" attributeName="x" from="0" to="60" dur="1s" begin="a{i}.begin"/></rect>"##
                ));
            }
        }
        // 逆文档序深链（fuzz F1 原形态：每轮只解锁一枚的传播形态）
        2 => {
            for i in (1..n).rev() {
                s.push_str(&format!(
                    r##"<rect width="4" height="4"><animate id="a{i}" attributeName="x" from="0" to="60" dur="1s" begin="a{}.begin+0.1s"/></rect>"##,
                    i - 1
                ));
            }
            s.push_str(r##"<rect width="4" height="4"><animate id="a0" attributeName="x" from="0" to="60" dur="1s" begin="0s"/></rect>"##);
        }
        // 负 dur / 负偏移 / indefinite dur 与前向引用混合
        3 => {
            for i in 0..n {
                let dur = ["-1s", "0s", "indefinite", "1e30s", "1s", "-0.5s"][rng.below(6)];
                let dep = if i > 0 { i - 1 } else { n - 1 };
                s.push_str(&format!(
                    r##"<rect width="4" height="4"><animate id="a{i}" attributeName="x" from="0" to="60" dur="{dur}" begin="a{dep}.begin{}s"/></rect>"##,
                    if rng.chance(50) { "-" } else { "+" }
                ));
            }
        }
        // 巨型 begin 列表（单动画数百依赖项）
        4 => {
            let pool: Vec<String> = (0..n).map(|i| format!("a{i}")).collect();
            let items: Vec<String> = (0..(n * 4)).map(|_| item(rng, &pool)).collect();
            s.push_str(&format!(
                r##"<rect width="4" height="4"><animate id="big" attributeName="x" from="0" to="60" dur="1s" begin="{}"/></rect>"##,
                items.join(";")
            ));
        }
        // NC 双注册：动画 id 与渲染元素 id 同名 + 多动画同 id
        5 => {
            s.push_str(r##"<rect id="tgt" width="6" height="6" fill="red"/>"##);
            for i in 0..n {
                let id = if rng.chance(50) { "tgt" } else { "a" };
                let dep = if rng.chance(50) { "tgt" } else { "a" };
                s.push_str(&format!(
                    r##"<rect width="4" height="4"><animate id="{id}{i}" attributeName="x" from="0" to="60" dur="1s" begin="{dep}.begin+0.1s"/></rect>"##
                ));
            }
        }
        // 事件锚链（NB propagate_event_sync 清偿面）：事件基动画 + syncbase 后继链
        _ => {
            s.push_str(r##"<rect id="tgt" width="6" height="6"/><animate id="e0" xlink:href="#tgt" attributeName="x" from="0" to="60" dur="1s" begin="tgt.click"/>"##);
            for i in 1..n {
                s.push_str(&format!(
                    r##"<rect width="4" height="4"><animate id="e{i}" attributeName="x" from="0" to="60" dur="1s" begin="e{}.begin+0.1s"/></rect>"##,
                    i - 1
                ));
            }
        }
    }
    let _ = item;
    s.push_str("</svg>");
    s
}

/// ④ mask-type 混合值打 ND 面（XML 属性位 + 内联 style + 样式表三臂）。
fn gen_mask_type(rng: &mut Rng, _n: usize) -> String {
    const VALS: &[&str] = &[
        "alpha",
        "luminance",
        "ALPHA",
        "Alpha",
        "a",
        "alpha luminance",
        "alpha,alpha",
        "alpha;",
        "",
        "none",
        "0",
        "1",
        "alpha ",
        " alpha",
        "match-source",
        "alpha!important",
        "alphaé",
        "中alpha",
        "alpha/",
    ];
    let v = |rng: &mut Rng| VALS[rng.below(VALS.len())];
    let mut s = String::from(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"##);
    let n = 2 + rng.below(4);
    for i in 0..n {
        let which = rng.below(3);
        let val = v(rng);
        match which {
            // XML 属性位（ND：Chrome 属性位生效面）
            0 => s.push_str(&format!(
                r##"<mask id="m{i}" mask-type="{}"><rect width="64" height="64" fill="{}"/></mask>"##,
                val, v(rng)
            )),
            // 内联 style 声明位
            1 => s.push_str(&format!(
                r##"<mask id="m{i}" style="mask-type:{}"><rect width="64" height="64" fill="{}"/></mask>"##,
                val, v(rng)
            )),
            // 样式表声明位 + mask 嵌套（mask 内引 mask）
            _ => s.push_str(&format!(
                r##"<style>#m{i} {{ mask-type: {} }}</style><mask id="m{i}"><rect width="64" height="64" fill="{}"/></mask>"##,
                val, v(rng)
            )),
        }
    }
    // 消费面：内容套 mask 链（黑内容 alpha 蒙版是 ND 修复面）
    for i in 0..n {
        s.push_str(&format!(
            r##"<g mask="url(#m{i})"><rect width="20" height="20" fill="black"/></g>"##
        ));
    }
    s.push_str("</svg>");
    s
}

/// ⑤ style 实体解码风暴打 NE 面（实体残留在选择器/声明/值域；
/// 截断实体；`&amp;gt;` 双解形态；CDATA/注释夹心）。
fn gen_style_entities(rng: &mut Rng, _n: usize) -> String {
    const ENT: &[&str] = &[
        "&amp;",
        "&lt;",
        "&gt;",
        "&quot;",
        "&apos;",
        "&nbsp;",
        "&#0;",
        "&#x110000;",
        "&#xD800;",
        "&bogus;",
        "&",
        "&#;",
        "&#x;",
        "&amp;gt;",
        "&amp;amp;",
        "é",
        "中",
        "&#x0;",
        "&LT;",
        "&#0000065;",
    ];
    let e = |rng: &mut Rng| ENT[rng.below(ENT.len())];
    let mut sel = format!(".a {} .b", e(rng));
    if rng.chance(30) {
        sel = format!("{sel} {} .c", e(rng));
    }
    let body = format!(
        "fill: {} ; stroke: {} ; stroke-width: {}px",
        e(rng),
        e(rng),
        e(rng)
    );
    let mut s = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">");
    match rng.below(5) {
        // 选择器实体（NE 原型：.a &amp;gt; .b 整条丢弃面）
        0 => s.push_str(&format!("<style>{sel} {{ {body} }}</style>")),
        // style 文本截断（实体落在串尾/CDATA 未闭合）
        1 => s.push_str(&format!(
            "<style>.a {{ fill: {}{}\n.c {{ x: 1</style>",
            e(rng),
            e(rng)
        )),
        // DOCTYPE 实体定义 + style 内引用（展开后进 CSS 管线）
        2 => s.push_str(&format!(
            "<!DOCTYPE svg [<!ENTITY col \"{}{}{}\">]><style>.a {{ fill: &col; }} {}</style>",
            e(rng),
            e(rng),
            e(rng),
            sel
        )),
        // 注释/CDATA 夹心 + 实体
        3 => s.push_str(&format!(
            "<style>/* {} */ .a {{ fill: red }} <![CDATA[ {} ]]> .b {{ x: {} }}</style>",
            e(rng),
            e(rng),
            e(rng)
        )),
        // 巨型实体值风暴（展开体积压力，CI 模式温和）
        _ => s.push_str(&format!(
            "<!DOCTYPE svg [<!ENTITY big \"{}{}\">]><style>{}</style>",
            "x".repeat(64 * (1 + rng.below(32))),
            e(rng),
            sel
        )),
    }
    s.push_str(r##"<g class="a"><rect class="b" width="10" height="10" fill="red"/></g></svg>"##);
    s
}

/// ⑥ dasharray 乱值 × 病态路径几何打 NC 真切向面（负值/带单位/NaN 样/
/// 超长列表 × 零长段/共线点/自交曲线——中心差分切向的退化输入）。
fn gen_dash_garbage(rng: &mut Rng, _n: usize) -> String {
    const DASH: &[&str] = &[
        "-1,2",
        "1pt,-2mm",
        "1e30q,-1e-30in",
        "0",
        "0,0",
        "-0,-0",
        "NaN",
        "inf,-inf",
        "1,2,3,4,5,6,7,8,9,10",
        "-1,-2,-3",
        "1e-45",
        "1e30",
        "5%",
        "1q",
        "0.5,0.25",
        "1e309999999pt",
        "--1",
        "1,,2",
        ";1;2",
        "1 2\t3",
        "0x10",
    ];
    const PATHS: &[&str] = &[
        // 零长段 / 重复点（差分除零面）
        "M10,10 L10,10 L10,10 L30,10 Z",
        // 共线点（切向回退弦向=直线逐字节不变面）
        "M0,0 L10,0 L20,0 L30,0 L40,0",
        // 急拐角（join 楔形真拐角面）
        "M10,50 L11,51 L10,52 L11,53 L10,54",
        // 曲线 + 弧（参数化切向）
        "M10,50 C30,10 50,90 70,50 A 20,10 30 1 0 60,50",
        // 自交 8 字
        "M10,10 C40,60 60,-20 30,40 Z",
        // pathLength 归一化 × 负 dash（NC 修复面的相位/覆盖组合）
        "M0,0 L60,0",
        // 单点 moveto（空子路径）
        "M5,5",
    ];
    let d = DASH[rng.below(DASH.len())];
    let d2 = DASH[rng.below(DASH.len())];
    let p = PATHS[rng.below(PATHS.len())];
    let pl = [
        "",
        " pathLength=\"0\"",
        " pathLength=\"-5\"",
        " pathLength=\"1e30\"",
        " pathLength=\"0.001\"",
    ][rng.below(5)];
    let off = [
        "",
        " stroke-dashoffset=\"-1e30\"",
        " stroke-dashoffset=\"3mm\"",
        " stroke-dashoffset=\"NaN\"",
        " stroke-dashoffset=\"-0.5q\"",
    ][rng.below(5)];
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><path d="{p}"{pl} fill="none" stroke="red" stroke-width="{sw}" stroke-dasharray="{d} {d2}"{off}/><path d="{p}" fill="none" stroke="blue" stroke-width="{sw2}" stroke-dasharray="{d2},{d}"/></svg>"##,
        sw = ["0", "-1", "1e-6", "1e30", "2mm", "0.5q"][rng.below(6)],
        sw2 = ["1", "0", "-2pt", "1e20"][rng.below(4)],
    )
}

// ---------------------------------------------------------------------------
// 变异
// ---------------------------------------------------------------------------

fn mutate(rng: &mut Rng, base: &str) -> String {
    let mut bytes = base.as_bytes().to_vec();
    let ops = 1 + rng.below(4);
    for _ in 0..ops {
        if bytes.is_empty() {
            bytes.extend_from_slice(rng.pick(TOKENS).as_bytes());
            continue;
        }
        match rng.below(6) {
            0 => {
                // 字节替换（可能造出非法 UTF-8 → from_utf8_lossy 兜底进 &str 管线）
                let i = rng.below(bytes.len());
                bytes[i] = *rng.pick(&[
                    b'"', b'\'', b'<', b'>', b'/', b'&', b'=', b'(', 0x00, 0x7F, 0xC3, 0xA9,
                ]);
            }
            1 => {
                // 插入 token
                let i = rng.below(bytes.len() + 1);
                let t = rng.pick(TOKENS).as_bytes();
                let i = i.min(bytes.len());
                bytes.splice(i..i, t.iter().copied());
            }
            2 => {
                // 截断（字符边界安全）
                let s = String::from_utf8_lossy(&bytes);
                let cut = truncate_at_boundary(&s, rng.below(s.len()));
                bytes = cut.as_bytes().to_vec();
                // 截断后必再插一个 token（避免语料单调变短收敛到空）
                let t = rng.pick(TOKENS).as_bytes();
                bytes.extend_from_slice(t);
            }
            3 => {
                // 复制一段（重复结构/不成对标签）
                if bytes.len() > 4 {
                    let a = rng.below(bytes.len());
                    let len = rng.below(bytes.len() - a + 1).min(256);
                    let seg = bytes[a..a + len].to_vec();
                    let j = rng.below(bytes.len() + 1);
                    bytes.splice(j..j, seg.iter().copied());
                }
            }
            4 => {
                // 删除一段
                if bytes.len() > 2 {
                    let a = rng.below(bytes.len());
                    let len = rng.below((bytes.len() - a).min(128) + 1);
                    bytes.drain(a..a + len);
                }
            }
            _ => {
                // 两枚 token 连插（不成对结构高发）
                let t1 = rng.pick(TOKENS);
                let t2 = rng.pick(TOKENS);
                let i = rng.below(bytes.len() + 1);
                bytes.splice(
                    i..i,
                    t1.as_bytes().iter().chain(t2.as_bytes().iter()).copied(),
                );
            }
        }
        // 体积护栏（防止复制算子滚雪球拖慢测试）
        if bytes.len() > 1 << 17 {
            bytes.truncate(1 << 17);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

// ---------------------------------------------------------------------------
// 入口调用：每个生成的输入喂全部解析面
// ---------------------------------------------------------------------------

fn feed_all_inputs(input: &str, round: usize) {
    // 1) 文档级全管线：XML → 树/属性/动画/CSS 收集 → DisplayList
    //（两个时刻）→ 小视口 raster 消费面（车道DI：raster 是 F2 类
    // 资源炸弹的暴露点——瓦片烘焙/滤镜求值/裁剪循环都在消费相）。
    if let Ok(doc) = builder::parse_document(input) {
        let dl = builder::build_display_list_at(&doc, 64.0, 64.0, 0.0);
        let _dl2 = builder::build_display_list_at(&doc, 64.0, 64.0, 1234.5);
        // DL 规模护栏（内存爆炸代理断言）：16KB 输入的正常展开
        // 远低于此；嵌套滤镜/pattern 的指数形展开会被此拦下。
        assert!(
            dl.cmds.len() <= 1 << 21,
            "round {round}: DL 规模失控 {} cmds",
            dl.cmds.len()
        );
        let vp = raster::Viewport {
            width: 16,
            height: 16,
            format: raster::PixFmt::Rgba8,
            aa: 1,
        };
        let mut out = alloc_rgba(16 * 16 * 4);
        raster::render(&dl, &vp, &mut out);
        // 结构不变量（builder.rs：节点 id == shapes 下标）——可达节点数
        // 不得超过 shapes 容量（防止越界下标在后续消费点 panic）。
        let mut count = 0usize;
        let mut stack = vec![doc.tree.root()];
        while let Some(id) = stack.pop() {
            count += 1;
            let n = doc.tree.get(id);
            if n.first_child != svgine_core::tree::NULL {
                stack.push(n.first_child);
            }
            if n.next_sibling != svgine_core::tree::NULL {
                stack.push(n.next_sibling);
            }
            // 环防护：节点数封顶（树是 arena 一次性构建的，环只在 use
            // 实例化层存在，此处链接均单指；防御性上限即可）
            if count > 1 << 20 {
                break;
            }
        }
        assert!(
            count <= doc.shapes.len(),
            "round {round}: 可达节点 {count} > shapes {}（id==下标不变量破坏）",
            doc.shapes.len()
        );
        // 车道NU 三轮：事件派发 + syncbase 清偿消费相（NC 动画 id 双注册
        // / NB propagate_event_sync / GE 事件哨兵的运行期面）。只对含
        // 动画的文档跑（避免无谓的全量重解析拖慢常驻轮）。
        #[cfg(feature = "anim")]
        if !doc.anims.is_empty() {
            let mut doc = doc;
            let target = doc.tree.root();
            let _ = svgine_core::hit_test::dispatch_event(&mut doc, "click", target, 500.0);
            let _ = svgine_core::hit_test::dispatch_event_bubble(
                &mut doc,
                "beginEvent",
                target,
                1000.0,
            );
            let interaction = builder::InteractionState::default();
            if let Ok(doc2) = builder::reparse_preserving_smil(input, &doc, &interaction) {
                let dl3 = builder::build_display_list_at(&doc2, 64.0, 64.0, 42.0);
                assert!(
                    dl3.cmds.len() <= 1 << 21,
                    "round {round}: 重解析 DL 规模失控 {} cmds",
                    dl3.cmds.len()
                );
            }
        }
    }
    // 2) 独立解析面轮转（每轮聚焦一个，逐轮覆盖全部）
    match round % 6 {
        0 => {
            let sheet = css::parse(input);
            for r in &sheet.rules {
                assert!(r.specificity < 1 << 30, "round {round}: specificity 溢出");
            }
            let _ = css::parse_decls(input);
        }
        1 => {
            let m = geom::parse_transform(input);
            let _ = m.apply(1.0, 1.0);
        }
        2 => {
            if let Some(cmds) = path::parse_d(input) {
                let _ = path::flatten_cmds(&cmds, svgine_core::geom::Mat2x3::IDENTITY);
            }
            let _ = path::parse_d_recover(input);
        }
        3 => {
            // XML 事件面直喂（含 attrs_into 窗口语义）
            let mut p = xml::Parser::new(input);
            let mut attrs = alloc_vec();
            let mut guard = 0usize;
            loop {
                guard += 1;
                if guard > 4 << 20 {
                    panic!("round {round}: XML 事件流不终止");
                }
                match p.next_event() {
                    xml::Event::Eof | xml::Event::Error { .. } => break,
                    xml::Event::StartElement { .. } => p.attrs_into(&mut attrs),
                    _ => {}
                }
            }
        }
        4 => {
            let mut out = alloc_string();
            xml::decode_entities(input, &mut out);
            assert!(
                out.len() <= input.len() + input.len() / 4 + 64,
                "round {round}: decode_entities 输出无界膨胀 {} → {}",
                input.len(),
                out.len()
            );
            let _ = xml::expand_dtd_entities(input);
        }
        _ => {
            if let Some((x, y, w, h)) = geom::parse_viewbox(input) {
                let _ = geom::viewport_map(100.0, 100.0, (x, y, w, h), input);
            }
        }
    }
}

// no_std 前例：integration test 侧直接用 std（tests crate 自带 std）
fn alloc_vec() -> std::vec::Vec<xml::Attr<'static>> {
    std::vec::Vec::new()
}
fn alloc_string() -> std::string::String {
    std::string::String::new()
}
/// raster 消费面缓冲（16×16 RGBA）。
fn alloc_rgba(n: usize) -> std::vec::Vec<u8> {
    std::vec![0u8; n]
}

// ---------------------------------------------------------------------------
// 主循环
// ---------------------------------------------------------------------------

fn fuzz_rounds(seed: u64, rounds: usize, max_len: usize, offline: bool) {
    let mut rng = Rng::new(seed);
    for round in 0..rounds {
        if round % 500 == 0 {
            eprintln!("[parser-fuzz] seed={seed} round={round}/{rounds}");
        }
        // 15% 结构化病态生成（CI 六类 + 车道DI 三类 + 车道NU 六族），
        // 85% 种子变异
        let input = if rng.chance(15) {
            let n = 1 + rng.below(if offline { 40_000 } else { 4_000 });
            let branch = rng.below(15);
            let nest = 1 + rng.below(if offline { 8_000 } else { 800 });
            let ent_val = if offline {
                1024 * (1 + rng.below(16))
            } else {
                64 * (1 + rng.below(8))
            };
            let chain = if offline {
                8 + rng.below(64)
            } else {
                2 + rng.below(6)
            };
            match branch {
                0 => gen_deep_nesting(&mut rng, nest),
                1 => gen_huge_attr(&mut rng, n * 3 + 8),
                2 => gen_entity_storm(&mut rng, ent_val),
                3 => gen_css_chain(&mut rng, chain),
                4 => gen_unclosed(&mut rng, n / 4 + 4),
                5 => gen_degenerate(&mut rng, n.min(2_000)),
                // 车道DI 二轮：递归嵌套 / 组合形态 / 合法但极端
                6 => gen_recursive_nesting(&mut rng, nest),
                7 => {
                    let m = 2 + rng.below(if offline { 2_000 } else { 200 });
                    gen_combo_stack(&mut rng, m)
                }
                8 => gen_legal_extreme(&mut rng, n),
                // 车道NU 三轮：48 波新增解析面定向族
                9 => gen_abs_units(&mut rng, n),
                10 => gen_css_colors(&mut rng, n),
                11 => gen_syncbase(&mut rng, nest),
                12 => gen_mask_type(&mut rng, n),
                13 => gen_style_entities(&mut rng, n),
                _ => gen_dash_garbage(&mut rng, n),
            }
        } else {
            let seed_input = rng.pick(SEEDS);
            mutate(&mut rng, seed_input)
        };
        let input = if input.len() > max_len {
            String::from(truncate_at_boundary(&input, max_len))
        } else {
            input
        };
        // FUZZ_VERBOSE=1：逐轮打印输入（repro 提取；挂死/panic 的 round
        // 号 + 输入直接从 stderr 尾部取）
        if std::env::var_os("FUZZ_VERBOSE").is_some() {
            eprintln!("[parser-fuzz] round {round} input:\n{input}\n[/parser-fuzz]");
        }
        feed_all_inputs(&input, round);
    }
}

/// 常驻：确定性 seed × 中等轮数（debug 下秒级）。
#[test]
fn fuzz_parser_robustness() {
    let rounds = std::env::var("FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_000);
    let max_len = std::env::var("FUZZ_MAXLEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16 * 1024);
    let offline = std::env::var("FUZZ_ROUNDS").is_ok();
    fuzz_rounds(0x53_56_47_49_4E_45_F0_01, rounds, max_len, offline);
}

/// 第二 seed（结构化生成器分支覆盖不同；变异路径与主 seed 独立）。
#[test]
fn fuzz_parser_robustness_seed2() {
    let rounds = std::env::var("FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_500);
    let offline = std::env::var("FUZZ_ROUNDS").is_ok();
    fuzz_rounds(
        0x0000_BE5F_C0DEu64.wrapping_mul(0x5DEECE66D),
        rounds,
        16 * 1024,
        offline,
    );
}
