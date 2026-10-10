//! DisplayList —— 平台无关的绘制命令流（ADR-0002：唯一移植边界）。
//!
//! 平台后端只消费这里定义的类型。M1 设计：
//! - **变换烘焙**：viewBox/transform 在构建期烘焙进点（设备空间），
//!   后端无矩阵运算（M3 冻结 v1 时再评估恢复 per-cmd 变换以支持复用）。
//! - **描边即填充**：stroke 在构建期展开为轮廓多边形（`path::stroke_outline`），
//!   后端只需实现多边形填充。
//! - **环分隔**：点池用 NaN 点分隔子路径环（多环填充/挖洞语义），
//!   后端遇 NaN 即闭合当前环。

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// 无滤镜哨兵（filter 索引，M8）。
pub const NO_FILTER: u32 = u32::MAX;

/// 指向 Document 折线点池的一段连续点（x, y 交错；含 NaN 环分隔）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathRef {
    pub first: u32,
    pub count: u32,
}

/// 渐变停靠点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradStop {
    /// [0,1]；超出范围的停靠点按 SVG 规范排序后钳到边界色。
    pub offset: f32,
    pub color: Rgba8,
}

/// 超出渐变轴/半径区域的延展方式（spreadMethod）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpreadMethod {
    Pad,
    Reflect,
    Repeat,
}

/// 线性渐变（端点已烘焙到设备空间）。
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub p0: (f32, f32),
    pub p1: (f32, f32),
    pub stops: Vec<GradStop>,
    pub spread: SpreadMethod,
}

/// 径向渐变（圆心/焦点已烘焙到设备空间）。
///
/// 等值线语义由 (`sx`,`sy`,`shx`,`shy`) 选择（wt/radial-focal 车道，
/// 车道PD 泛化为完整 2×2 逆映射）：
/// - `sx == sy == 1.0 && shx == shy == 0.0`：梯度空间 = 设备空间，`r`
///   为设备半径、等值线为圆（userSpaceOnUse / 正方形 bbox oBB——保持
///   既有路径逐位不变）；
/// - 其他（`shx == shy == 0.0`）：梯度空间 = 设备坐标差按 (`sx`,`sy`)
///   逆缩放后的「bbox 单位空间」，`r` 为单位空间半径、设备空间等值线
///   为轴对齐椭圆（Chrome/Skia 在 bbox 缩放空间算圆锥再仿射回设备的
///   语义，oBB 非正方形 bbox 用）。此时 `c`/`f` 仍存设备空间点，采样
///   时坐标差才缩放（见 `gradient::sample_radial`）；
/// - `shx`/`shy` 非零（车道PD）：完整逆线性映射 `S = lin(T)⁻¹`
///   （T = 元素 CTM ∘ gradientTransform），梯度差 =
///   `(Δx·sx + Δy·shx, Δx·shy + Δy·sy)`，`r`/`fr` 为梯度定义空间
///   半径、设备等值线为**任意朝向椭圆**（noto-emoji Illustrator 导出
///   族的非相似 gradientTransform 实证）。
#[derive(Clone, Debug, PartialEq)]
pub struct RadialGradient {
    pub c: (f32, f32),
    pub r: f32,
    /// 焦点圆半径（SVG2 `fr`，与 `r` 同一梯度空间；恒 `0.0` = SVG1.1
    /// 焦点语义，采样走既有路径逐位不变，见 `gradient::sample_radial`）。
    pub fr: f32,
    pub f: (f32, f32),
    pub stops: Vec<GradStop>,
    pub spread: SpreadMethod,
    /// 设备→梯度空间逆映射 2×2（行优先）：x 列 `sx`、y 列 `shx`、
    /// x 行交叉 `shy`、y 列 `sy`——见结构体文档。oBB：`sx`=1/bbox 宽、
    /// `sy`=1/bbox 高、交叉恒 0；圆语义全零/恒 1。
    pub sx: f32,
    /// 设备→梯度空间 y 逆缩放（oBB：1/bbox 高；圆语义恒 1.0）。
    pub sy: f32,
    /// 逆映射交叉项 `Δy` 对梯度 x 的贡献（轴对齐语境恒 0.0；车道PD）。
    pub shx: f32,
    /// 逆映射交叉项 `Δx` 对梯度 y 的贡献（轴对齐语境恒 0.0；车道PD）。
    pub shy: f32,
}

/// 描绘服务器。渐变数据内联（M3 简化：同一渐变被多形状引用会复制；
/// 池化优化记录在 M3 收尾评估）。pattern 在 M4 进入。
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Rgba8),
    LinearGradient(LinearGradient),
    RadialGradient(RadialGradient),
}

/// 填充规则（SVG fill-rule；clipPath 子元素经 clip-rule 复用同一语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// 「无裁切」哨兵（clip 索引）。
pub const NO_CLIP: u32 = u32::MAX;

/// 「无蒙版」哨兵（mask 索引，M4）。
pub const NO_MASK: u32 = u32::MAX;

/// 「无平铺图案」哨兵（pattern 索引，M4）。
pub const NO_PATTERN: u32 = u32::MAX;

/// pattern 平铺瓦片定义（M4 `<pattern>`）：内容子显示列表 + 平铺参数。
///
/// 语义：瓦片坐标系中瓦片覆盖 [0,tw]×[0,th]，瓦片→设备的仿射 F 由
/// 构建期合成（引用元素用户空间 ∘ 瓦片定位 ∘ patternTransform）并求逆
/// 存入 [`Self::inv`]。raster 侧把内容子显示列表渲染到
/// ceil(tw·sx)×ceil(th·sy) 的离屏瓦片位图（复用 mask 的离屏机制），
/// 逐像素以 F⁻¹ 把设备坐标映回瓦片空间、对 (tw,th) 取模后最近邻采样。
///
/// 设计注记：pattern 不进 [`Paint`] 枚举而走 `Cmd::Fill::pattern` 独立
/// 通道——`Paint` 的穷举匹配在 gradient.rs（SpanEval），本里程碑的
/// 文件边界不允许为新增变体改动该匹配；引用点附加 alpha（fill-opacity）
/// 折进命令的 opacity 字段。
#[derive(Clone, Debug, PartialEq)]
pub struct PatternTile {
    /// 瓦片内容子显示列表（瓦片空间坐标，已按 (sx,sy) 烘焙到位图像素
    /// 坐标；自含 points/clips 池）。
    pub dl: DisplayList,
    /// 瓦片周期（瓦片空间单位）：patternUnits=userSpaceOnUse 时为用户
    /// 单位；objectBoundingBox 时为分数×bbox 尺寸（已解析）。
    pub tw: f32,
    pub th: f32,
    /// F⁻¹（设备像素 → 瓦片空间），Mat2x3 布局 `[a,b,c,d,e,f]`：
    /// `x' = a·x + c·y + e`、`y' = b·x + d·y + f`。
    pub inv: [f32; 6],
    /// 位图分辨率：每瓦片单位的像素数（|F·êx| / |F·êy|，构建期算好）。
    pub sx: f32,
    pub sy: f32,
}

/// 亮度/α 蒙版定义（M4 `<mask>`）：内容子显示列表 + 有效区域。
/// raster 侧把内容渲染到离屏，按
/// `factor = (0.2126R + 0.7152G + 0.0722B)/255 × A/255`（直色 × alpha；
/// `alpha = true` 时跳过亮度项，因子 = A/255——车道ND mask-type:alpha）
/// 折算每像素 0..255 因子，与目标 coverage/alpha 相乘；region 外恒 0。
#[derive(Clone, Debug, PartialEq)]
pub struct MaskDef {
    /// 蒙版内容子显示列表（引用元素用户空间烘焙到设备空间；自含
    /// points/clips 池，可嵌套自身携带 clipPath——mask 内 mask 由
    /// raster 递归处理）。
    pub dl: DisplayList,
    /// 蒙版有效区域（设备空间 AABB；maskUnits 解析后的 x/y/w/h）。
    pub region: [f32; 4],
    /// 车道ND：mask-type:alpha（CSS Masking 属性；样式表/内联 style/
    /// XML 属性位三路同源——Chrome fd1 探针实证属性位生效）。true =
    /// α 通道语义；false = 缺省 luminance。Figma/Sketch 导出（gemini/
    /// vite/nodejs 压力语料）黑内容 + alpha 语义，按亮度折算恒 0 会
    /// 整体吞掉被蒙内容。
    pub alpha: bool,
}

/// 矢量裁切路径（M4 clipPath）：子形状合并的环集（设备空间烘焙，
/// 点池内嵌，NaN 环分隔语义与填充路径一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathClip {
    pub d: PathRef,
    pub rule: FillRule,
}

// ---------------------------------------------------------------------------
// M8：`<filter>` 滤镜四件套（feDropShadow / feGaussianBlur / feColorMatrix /
// feOffset + feFlood + feComposite + feMerge）
// ---------------------------------------------------------------------------

/// primitive 输入（构建期已把 `result=` 命名引用解析为链上索引，
/// raster 期零字符串查找）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeInput {
    /// `in` 缺省：上一 primitive 的输出（链首 = SourceGraphic）。
    Prev,
    /// 引用元素滤前渲染结果。
    SourceGraphic,
    /// SourceGraphic 的 alpha（RGB=0）。
    SourceAlpha,
    /// 链上第 N 个 primitive 的输出（`result=` 命名解析产物）。
    Output(u16),
}

/// feComposite 的 Porter-Duff 算子（`in=` 为源 S、`in2=` 为目标 D）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeCompositeOp {
    Over,
    In,
    Out,
    Atop,
    Xor,
    /// feComposite operator="arithmetic"：premultiplied 线性域
    /// `i = k1·i1·i2 + k2·i1 + k3·i2 + k4`（i1=in、i2=in2）。
    Arithmetic {
        k1: f32,
        k2: f32,
        k3: f32,
        k4: f32,
    },
}

/// feBlend 混合模式。SVG 1.1 五种 + Filter Effects 1.0 扩展（= CSS
/// Compositing 全集；Chrome 全支持，Inkscape 水彩滤镜依赖 overlay）。
/// 混合函数 B(Cb,Cs) 与 [`BlendMode`] 同源共享（raster blend_css），
/// 工作域按滤镜 `color-interpolation-filters`（缺省 linearRGB）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeBlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    /// 非可分离四模式（逐像素 RGB 三通道联立）。
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl FeBlendMode {
    /// → CSS 混合枚举（B 函数复用；両枚举变体一一对应）。
    pub fn to_blend_mode(self) -> BlendMode {
        match self {
            FeBlendMode::Normal => BlendMode::Normal,
            FeBlendMode::Multiply => BlendMode::Multiply,
            FeBlendMode::Screen => BlendMode::Screen,
            FeBlendMode::Overlay => BlendMode::Overlay,
            FeBlendMode::Darken => BlendMode::Darken,
            FeBlendMode::Lighten => BlendMode::Lighten,
            FeBlendMode::ColorDodge => BlendMode::ColorDodge,
            FeBlendMode::ColorBurn => BlendMode::ColorBurn,
            FeBlendMode::HardLight => BlendMode::HardLight,
            FeBlendMode::SoftLight => BlendMode::SoftLight,
            FeBlendMode::Difference => BlendMode::Difference,
            FeBlendMode::Exclusion => BlendMode::Exclusion,
            FeBlendMode::Hue => BlendMode::Hue,
            FeBlendMode::Saturation => BlendMode::Saturation,
            FeBlendMode::Color => BlendMode::Color,
            FeBlendMode::Luminosity => BlendMode::Luminosity,
        }
    }
}

/// CSS 合成混合模式（车道BL，`mix-blend-mode` 全集）——元素/组结果与
/// backdrop 的混合语义（CSS Compositing & Blending L1）。
///
/// 与 [`FeBlendMode`] 的关系：前五种（Normal..Lighten）混合函数同源，
/// 但**工作域不同**——feBlend 按滤镜 `color-interpolation-filters`
/// （缺省 linearRGB），本枚举恒在 **sRGB 直色域**求值（Chrome 页面
/// 合成口径，bl-*.svg 探针矩阵 16 模式 × Chrome 逐值 ±1 实证）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    /// 非可分离四模式（逐像素 RGB 三通道联立）。
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// feComponentTransfer 单通道传递函数（车道F；值域 0..=1，构建期已
/// 归一 tableValues/255 由 raster 侧做——这里存原始 0..1 分数）。
#[derive(Clone, Debug, PartialEq)]
pub enum FeTransferFn {
    Identity,
    /// type=table：N 点线性插值 `k=floor(N·C), C'=lerp(tv[k], tv[k+1], N·C−k)`。
    Table(Vec<f32>),
    /// type=discrete：阶梯 `C' = tv[floor(N·C)]`。
    Discrete(Vec<f32>),
    /// type=linear：`C' = slope·C + intercept`。
    Linear {
        slope: f32,
        intercept: f32,
    },
    /// type=gamma：`C' = amplitude·C^exponent + offset`。
    Gamma {
        amplitude: f32,
        exponent: f32,
        offset: f32,
    },
}

/// feTurbulence 噪声类型（车道AQ）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeTurbType {
    /// type="fractalNoise"：噪声 [-1,1] → (v·0.5+0.5) 映射色值。
    FractalNoise,
    /// type="turbulence"（缺省）：逐 octave |噪声| 累加 → 直接色值。
    Turbulence,
}

/// feConvolveMatrix edgeMode（车道CN）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeEdgeMode {
    /// 缺省：边缘外取最近像素复制。
    Duplicate,
    /// 平铺回卷（对图像宽高取模）。
    Wrap,
    /// 边缘外透明。
    None,
}

/// feDisplacementMap 通道选择子（xChannelSelector/yChannelSelector）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeChannel {
    R,
    G,
    B,
    A,
}

/// feLighting 光源（feDistantLight/fePointLight/feSpotLight，车道AQ）。
/// Point/Spot 的 x/y 已烘焙到区域局部设备像素（构建期 m·(x,y)−(ox,oy)），
/// z 按每轴均方缩放（z 无轴语义，取近似；identity 下恒等）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeLight {
    /// 无限远方向光（azimuth/elevation，角度）。
    Distant { az: f32, el: f32 },
    /// 点光源。
    Point { x: f32, y: f32, z: f32 },
    /// 锥形聚光灯（cone = limitingConeAngle 弧度，None = 无锥限制）。
    Spot {
        x: f32,
        y: f32,
        z: f32,
        pax: f32,
        pay: f32,
        paz: f32,
        exp: f32,
        cone: Option<f32>,
    },
}

/// primitive 种类（参数全部烘焙到**设备像素**：σ/偏移已乘引用元素
/// 用户空间的每轴缩放，flood 色已乘 flood-opacity）。
#[derive(Clone, Debug, PartialEq)]
pub enum FeKind {
    /// feGaussianBlur：三次盒模糊近似高斯（σ 设备像素，x/y 各轴）。
    GaussianBlur { sx: f32, sy: f32 },
    /// feDropShadow（语法糖）：blur(SourceAlpha) 着 flood 色偏移 +
    /// 原图合成。参数为设备像素偏移与 σ。
    DropShadow {
        dx: f32,
        dy: f32,
        sx: f32,
        sy: f32,
        color: Rgba8,
    },
    /// feColorMatrix：5×4 矩阵（行 R/G/B/A，列 R/G/B/A/offset，
    /// linearRGB 直色语义——车道F 对齐 Chrome 默认
    /// color-interpolation-filters）。
    ColorMatrix { m: [f32; 20] },
    /// feOffset：位图平移（设备像素）。
    Offset { dx: f32, dy: f32 },
    /// feFlood：纯色填充（色已乘 flood-opacity）。
    Flood { color: Rgba8 },
    /// feComposite：`in`（源）与 `in2`（目标）的 Porter-Duff 合成。
    Composite { in2: FeInput, op: FeCompositeOp },
    /// feMerge：按文档序 src-over 叠层各 feMergeNode 输入。
    Merge { nodes: Vec<FeInput> },
    /// feBlend：`in`（源）与 `in2`（backdrop）的 separable 混合
    ///（linearRGB 直色语义，Chrome/Skia 同款）。
    Blend { in2: FeInput, mode: FeBlendMode },
    /// feComponentTransfer：R/G/B/A 传递函数（linearRGB 直色，
    /// alpha 通道不经 gamma 转换）。
    ComponentTransfer { fns: [FeTransferFn; 4] },
    /// feTile：以引用输入的子区域为瓦片平铺填充自身子区域。
    /// `src` = 瓦片源矩形（区域局部设备像素，构建期从被引用
    /// primitive 的子区域解析）。
    Tile { src: [u32; 4] },
    /// feImage：`href="#id"` 内部元素引用——目标元素在滤镜区域
    /// 用户空间渲染的子显示列表（区域局部坐标烘焙，与
    /// SourceGraphic 同款）。data URI/外链由宿主注入（宪法边界）。
    ///
    /// 车道CN 契约扩展：`bitmap_as_linear` = 位图字节是否按 linear 域
    /// 再解释（out = to_srgb(b/255)·255，alpha 不动）。Chrome/Skia 的
    /// feImage 不做 sRGB→linear 解码直注滤镜缓冲；两个存储基线互相
    /// 矛盾（displace-01 自然尺寸位图被提亮 / image-01-b 显式子区域
    /// 不提亮），判别器 = 是否显式 width/height 子区域：缺省（自然
    /// 尺寸放置）→ true；显式拉伸 → false（颜色校正解码路径）。
    /// 内部 #id 引用（矢量子列表，非位图）→ false。
    Image {
        dl: DisplayList,
        bitmap_as_linear: bool,
    },
    /// feTurbulence（车道AQ）：Perlin 分形噪声场（不消费 in）。bf =
    /// 每设备像素基频（user 基频 ÷ 每轴设备缩放；identity 下即原值），
    /// octaves 已钳 [0,255]，seed 已截断取整，stitch 瓦片 = primitive
    /// 子区域像素尺寸。数值口径 = Chrome/Skia raster pipeline
    /// perlin_noise stage（与 SVG1.1 附录参考实现的三处已知偏离见
    /// raster::turbulence_bitmap 文档）。
    ///
    /// 车道CN 契约扩展：`origin` = 滤镜区域原点（设备像素）。规范语义
    /// 噪声以**元素用户空间坐标**为参量——区域不在原点时若用区域局部
    /// 坐标采样，噪声场整体错相（filters-turb-01-f 六 tile 100% 错配的
    /// 根因）。采样坐标 = (origin + 区域局部像素 + 1.0) × bf。
    Turbulence {
        ty: FeTurbType,
        bf: (f32, f32),
        octaves: u32,
        seed: i32,
        stitch: bool,
        origin: [f32; 2],
    },
    /// feDisplacementMap（车道AQ）：in2 的两通道值（工作色域 [0,1]）
    /// 折算位移 `scale·(c−0.5)`，in 按位移 nearest 重采样。scale 已按
    /// 每轴烘焙设备像素（位移是用户空间长度）。
    DisplacementMap {
        in2: FeInput,
        scale_x: f32,
        scale_y: f32,
        xch: FeChannel,
        ych: FeChannel,
    },
    /// feConvolveMatrix（车道CN 契约扩展）：kernel 行主序（oy×ox），
    /// divisor/bias 规范缺省已在构建期结算（divisor 缺省 = 核和、核和
    /// 0 → 1；bias 0）；target = 锚点在核内的列/行；edge = 边缘取值
    /// 语义。工作色域直色卷积（含 alpha，preserve_alpha 时跳过 alpha）。
    ConvolveMatrix {
        kernel: Vec<f32>,
        ox: u32,
        oy: u32,
        divisor: f32,
        bias: f32,
        tx: u32,
        ty: u32,
        edge: FeEdgeMode,
        preserve_alpha: bool,
    },
    /// feMorphology（车道CN 契约扩展）：erode=逐通道最小/dilate=最大
    ///（含 alpha，premul 字节域），rx/ry 已烘焙设备像素并取整 ≥0。
    Morphology { dilate: bool, rx: i32, ry: i32 },
    /// feDiffuseLighting（车道AQ）：Phong 漫反射（输出 alpha=1 不透明
    /// 图）。surface_scale 已烘焙设备像素，kd = diffuseConstant，
    /// color = lighting-color（sRGB 原值；求值期按工作色域转换）。
    DiffuseLighting {
        light: FeLight,
        surface_scale: f32,
        kd: f32,
        color: Rgba8,
    },
    /// feSpecularLighting（车道AQ）：Phong 镜面（alpha = max(R,G,B)）。
    SpecularLighting {
        light: FeLight,
        surface_scale: f32,
        ks: f32,
        exp: f32,
        color: Rgba8,
    },
}

/// 链上一个 primitive：种类 + 输入 + 子区域。
#[derive(Clone, Debug, PartialEq)]
pub struct FePrim {
    pub kind: FeKind,
    pub in_: FeInput,
    /// primitive 子区域（区域局部设备像素 AABB x0/y0/x1/y1，
    /// 已交裁到离屏区域；输入与输出都按此硬裁——Chrome 语义）。
    /// 默认 = 整个滤镜区域（解析期 [0,0,w,h]）。
    pub sub: [u32; 4],
}

/// `<filter>` 定义（M8）：SourceGraphic 子显示列表（区域局部坐标
/// 烘焙——内容已平移 `(−ox,−oy)`，离屏视口即 `[0,w)×[0,h)`）+
/// primitive 链 + 合成参数。raster 期：子列表离屏渲染 → 按链逐
/// primitive 变换位图 → 末链输出（空链 = 原图直通）按 `opacity`
/// src-over 合成回 `(ox,oy)` 起的区域。
/// 逐位对齐）。PartialEq（车道RL retained 差分消费）：全字段语义等值
/// ——`dl` 递归（嵌套 mask/pattern 自含子列表），f32 字段 NaN 安全
/// 方向 = 等值判定 false → 差分保守判脏。
#[derive(Clone, Debug, PartialEq)]
pub struct FilterDef {
    /// SourceGraphic 子显示列表（区域局部坐标）。
    pub dl: DisplayList,
    /// 区域左上角（设备像素，画布坐标）。
    pub ox: i32,
    pub oy: i32,
    /// 区域像素尺寸（构建期已按画布交裁 + 上限钳制；0 = 空区域）。
    pub w: u32,
    pub h: u32,
    /// primitive 链（文档序）。
    pub prims: Vec<FePrim>,
    /// 元素级 opacity（滤后施加，规范顺序 filter→opacity）。
    pub opacity: f32,
    /// 滤镜工作色域（`color-interpolation-filters` 继承解析产物，车道X）：
    /// true = linearRGB（规范缺省值/auto；Chrome 缺省同款，a1 探针矩阵
    /// 实证），false = sRGB（属性显式声明，各 primitive 跳过 EOTF 转换
    /// 直在 sRGB 字节域求值）。域是滤镜级（整链统一），不进 [`FeKind`]。
    pub linear: bool,
}

/// 裁切定义（M4）：矩形（嵌套 svg/symbol 视口）或矢量路径（clipPath）。
/// 多层裁切经 `parent` 链求交——raster 侧解析链：各层 bounds 取交收紧
/// 扫描范围，链上全部矢量裁切的每像素覆盖相乘（无路径层的链保持
/// M1 矩形语义逐字节不变）。parent 恒小于自身索引（构建期保证），
/// 解析链必然终止。
#[derive(Clone, Debug, PartialEq)]
pub struct ClipDef {
    /// 设备空间 AABB（x0, y0, x1, y1）：矩形裁切即矩形本体；路径裁切为
    /// 几何 AABB（链解析时再与父裁切 bounds 相交）。
    pub bounds: [f32; 4],
    /// 矢量裁切（None = 纯矩形裁切）。
    pub path: Option<PathClip>,
    /// 父裁切索引（NO_CLIP = 无父）。
    pub parent: u32,
}

/// 位图数据（M8 `<image>`，feature `image-decode` 解码注入；MCU 宿主
/// 亦可解码后直接入池——池与命令在 DL 上无条件存在，解码路径才是
/// feature 门控的边界）。
///
/// 车道hwB（行带并行一期）：原内嵌的 mip 链 `RefCell` 缓存（车道BB）
/// 已移除——内嵌内部可变态使 DisplayList !Sync，同图跨 band 并发首建
/// 会 `borrow_mut` 冲突；链的驻留点上移到渲染调用方（全帧 = 帧内局部
/// 缓存，band = `raster::FrameCache` 预生成共享只读），确定性重建输出
/// 逐字节一致，本结构因此是可跨线程只读共享的纯数据（Send+Sync）。
/// 语义相等性（PartialEq/Eq/Clone/Debug）只看 w/h/rgba（原缓存本就
/// 不参与）。
pub struct ImgData {
    pub w: u32,
    pub h: u32,
    /// 直 RGBA8（非预乘，行主序，len == w*h*4）。
    pub rgba: Vec<u8>,
}

impl Clone for ImgData {
    fn clone(&self) -> Self {
        Self {
            w: self.w,
            h: self.h,
            rgba: self.rgba.clone(),
        }
    }
}

impl core::fmt::Debug for ImgData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ImgData")
            .field("w", &self.w)
            .field("h", &self.h)
            .field("rgba.len", &self.rgba.len())
            .finish()
    }
}

impl PartialEq for ImgData {
    fn eq(&self, other: &Self) -> bool {
        self.w == other.w && self.h == other.h && self.rgba == other.rgba
    }
}

impl Eq for ImgData {}

/// 多边形纯色填充命令（描边已展开为填充轮廓）。
/// `clip` 索引 [`DisplayList::clips`]，`NO_CLIP` 表示不裁；
/// `mask` 索引 [`DisplayList::masks`]，`NO_MASK` 表示无蒙版（M4）；
/// `pattern` 索引 [`DisplayList::patterns`]，`NO_PATTERN` 表示非平铺
/// 填充（M4；此时 `paint` 为常规描绘，pattern 填充时 `paint` 为透明
/// 占位、引用点的 fill-opacity 已折进 `opacity`）。
///
/// PartialEq（车道RL retained 差分消费）：字段级比较；注意
/// `PathRef` 是**索引**等值——被指点的内容等值由差分方按点池 bits
/// 另行判定（NaN 环分隔使 derive 的 f32 `==` 不可用，见
/// [`DisplayList::eq`] 注记）。f32 字段出现 NaN 时等值判定恒 false
/// → 差分保守判脏（安全方向：最多丢缓存命中，不错报不变）。
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Fill {
        d: PathRef,
        paint: Paint,
        opacity: f32,
        rule: FillRule,
        clip: u32,
        mask: u32,
        pattern: u32,
    },
    /// 组级 opacity 离屏层开始（S2 真组级语义）：层内命令先在离屏
    /// 缓冲互相合成（src-over），到 [`Cmd::PopLayer`] 时整层乘
    /// `opacity` 后单次 src-over 合成回下层——重叠区不二次加深、
    /// 半透明组内的纯色保持纯色。构建期只在容器有效 opacity < 1 时
    /// 发射（opacity ≥ 1 零开销不建层）；与 PopLayer 严格配对
    /// （遍历栈哨兵保证，见 builder::walk）。
    PushLayer {
        /// 组不透明度 [0,1]（构建期已钳位）。
        opacity: f32,
        /// 车道BL：层结果合成回下层时的混合模式（`mix-blend-mode`；
        /// Normal = 既有 src-over 语义逐字节不变）。isolation 建层时
        /// 恒 Normal（隔离只截断 backdrop，不改合成算子）。
        blend: BlendMode,
    },
    /// 离屏层结束：整层 × 对应 PushLayer 的 opacity 合成回下层
    /// （嵌套层递归——先合成内层再合成外层）。手写 DL 的孤儿
    /// PopLayer 由 raster 忽略、未闭合层在命令流末尾防御性合成。
    PopLayer,
    /// M8 滤镜求值：[`FilterDef`] 的 SourceGraphic 子列表在区域离屏
    /// 渲染、按 primitive 链变换，末链输出按 def.opacity src-over
    /// 合成进当前目标（层栈感知）。`filter` 索引
    /// [`DisplayList::filters`]；`clip`/`mask` 索引语义同 [`Cmd::Fill`]
    ///（规范顺序：filter → clip → mask → opacity，滤后施加）。
    Filter { filter: u32, clip: u32, mask: u32 },
    /// M8 位图求值（`<image>`）：`img` 索引 [`DisplayList::images`]，
    /// `xf` 为图像像素空间 → 设备的仿射（Mat2x3 布局 `[a,b,c,d,e,f]`，
    /// 构建期已按 preserveAspectRatio meet/slice/none 合成）。raster 逐
    /// 像素逆映射采样（车道BB）：`pixelated=false` 走 Chrome kMedium
    /// 等价管线（mip 链按需生成 + 16.16 定点双线性，见
    /// [`crate::mipmap`]）；`pixelated=true`（image-rendering:
    /// pixelated/crisp-edges）保持最近邻点采样。之后 src-over 合成；
    /// `opacity`/`clip`/`mask` 语义同 [`Cmd::Fill`]
    ///（元素级 opacity 折进命令——位图是原子命令，无重叠语义，
    /// 不开组层）。
    Image {
        img: u32,
        xf: [f32; 6],
        opacity: f32,
        clip: u32,
        mask: u32,
        /// image-rendering: pixelated/crisp-edges（true = 点采样）。
        pixelated: bool,
    },
}

/// 一帧的绘制命令序列（自包含：点池内嵌，C ABI 上一个结构体跨后端）。
#[derive(Clone, Default)]
pub struct DisplayList {
    pub cmds: Vec<Cmd>,
    /// 折线点池（x, y 交错；`(NaN, NaN)` 为环分隔）。
    pub points: Vec<f32>,
    /// 裁切池（M4 起矩形/矢量统一为 [`ClipDef`]；嵌套 svg 视口、clipPath）。
    pub clips: Vec<ClipDef>,
    /// 亮度蒙版池（M4 `<mask>` 引用）。
    pub masks: Vec<MaskDef>,
    /// 平铺图案池（M4 `<pattern>` 引用）。
    pub patterns: Vec<PatternTile>,
    /// 滤镜定义池（M8 `<filter>` 引用）。
    pub filters: Vec<FilterDef>,
    /// 位图池（M8 `<image>` 引用；`Cmd::Image::img` 索引此处）。
    pub images: Vec<ImgData>,
    /// 车道NM：clip 实例去重 memo（单条目，语义缓存）。键命中 ⇒
    /// [`ClipDef`] 重建产物逐字节相同 ⇒ 复用既有索引，raster 侧
    /// 覆盖只光栅化一次。字段在 DL 上 = 天然按索引池作用域隔离
    /// （mask/pattern 子列表各自的 DL 索引空间独立，跨 DL 复用会
    /// 错索引）；不参与渲染语义（Clone 随拷、Debug 不打）。
    #[cfg(feature = "clip-path")]
    pub clip_memo: Option<(ClipMemoKey, u32)>,
}

// ---------------------------------------------------------------------------
// hwC L1：分层发射产物（CA 混合模型契约，feature `anim`）
// 设计源：docs/research/hw-layering-design.md §3.1（2026-09-16 契约冻结）。
// 红线：不改变标准 DisplayList/渲染路径任何字节（回退协议保证产品
// 行为=ADR-0003 不变）；本节纯新增类型。
// ---------------------------------------------------------------------------

/// 分层发射产物：base 段 + 层序列。`segments` 顺序 = walk 遍历序的
/// 线性化（z 序保真是构造保证——层=连续子树，层与 base 段不交错）。
#[cfg(feature = "anim")]
#[derive(Clone, Default)]
pub struct LayeredDisplayList {
    /// 绘制序段列：`Base` 携带该位置的非层命令（PushLayer/PopLayer
    /// 限段内配对），`Layer(idx)` 引用 [`Self::layers`]。
    pub segments: Vec<Segment>,
    pub layers: Vec<LayerOut>,
}

/// 段占位：渲染序 = 段列顺序（宿主合成期按序摆 layer frame）。
/// 段数每帧个位数、DisplayList 移动 = Vec 指针拷贝——不为尺寸装箱。
#[cfg(feature = "anim")]
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum Segment {
    /// 非层命令段（每帧引擎重建+光栅）。
    Base(DisplayList),
    /// 层占位（纹理一次光栅 + 合成期仿射/opacity）。
    Layer(usize),
}

/// 层产物：子树以**层局部空间**发射（父链仿射不烘焙，记 [`Self::base_xf`]）。
#[cfg(feature = "anim")]
#[derive(Clone)]
pub struct LayerOut {
    /// 层局部空间子 DL（静态：不含层根动画的时变仿射——纹理缓存源）。
    pub dl: DisplayList,
    /// 层根之上父链累积仿射（构建期静态部分；播放期实际仿射 =
    /// `base_xf ∘ anim(t)`，由采样器给出、宿主应用于合成）。
    pub base_xf: crate::geom::Mat2x3,
    /// 层子树 AABB（**层局部空间**，宿主纹理尺寸基准 = bbox 尺寸×dsr）。
    pub bbox: [f32; 4],
    /// 层根 = 动画目标元素（树节点 id；采样器绑定查找键 + 引擎路径
    /// 再走通道的子树入口）。
    pub target: crate::tree::NodeId,
}

/// 车道NM：clip 去重 memo 的键（全部分量按位比较，f32 转 bits 防
/// NaN != NaN）。键 ⇒ [`ClipDef`] 产物的一一对应：
/// - `node`：clipPath 定义元素（子环几何/clip-rule/自身 transform
///   全由它决定；`Document::clips` 解析后不可变）；
/// - `m`：引用元素最终用户空间矩阵（环的设备烘焙输入）；
/// - `bbox`：objectBoundingBox 单位的锚定盒（[0]=Some 标记，
///   [1..5] = 四分量 bits；userSpaceOnUse 时全零）；
/// - `parent`：父裁切链索引（链求交语义的一部分）。
#[cfg(feature = "clip-path")]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ClipMemoKey {
    pub node: u32,
    pub parent: u32,
    pub m: [u32; 6],
    pub bbox: [u32; 5],
}

/// 摘要式 Debug（点池/命令流可达兆级，不逐元素打印）。
impl core::fmt::Debug for DisplayList {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DisplayList")
            .field("cmds", &self.cmds.len())
            .field("points", &self.points.len())
            .field("clips", &self.clips.len())
            .field("masks", &self.masks.len())
            .field("patterns", &self.patterns.len())
            .field("filters", &self.filters.len())
            .field("images", &self.images.len())
            .finish()
    }
}

/// 语义等值（车道RL retained 帧差分消费）：命令流 + 点池 + 五池的全
/// 字段比较。两条实现纪律：
/// - 点池按 **bits** 比较（`f32::eq` 对 NaN 恒 false——NaN 是环分隔
///   哨兵值，同位置的 NaN 必须判等；逐位比较两帧几何逐字节一致 ⇔
///   光栅产物逐字节一致，raster 是同输入的确定性纯函数）；
/// - `clip_memo` 是纯构建期缓存（NM 设计：键命中 ⇒ 重建产物逐字节
///   相同），不进渲染语义，不参与等值——memo 有无/指向不同不算差异。
///
/// NaN 泄漏到其余字段（病态输入）时等值恒 false → 差分保守判脏：
/// 最多丢缓存命中，绝不错报「未变」。
impl PartialEq for DisplayList {
    fn eq(&self, other: &Self) -> bool {
        if self.cmds != other.cmds
            || self.clips != other.clips
            || self.masks != other.masks
            || self.patterns != other.patterns
            || self.filters != other.filters
            || self.images != other.images
            || self.points.len() != other.points.len()
        {
            return false;
        }
        self.points
            .iter()
            .zip(other.points.iter())
            .all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl DisplayList {
    /// 追加一组环（每环 [(x,y)..]，环间自动插 NaN 分隔）并返回引用。
    pub fn push_rings(&mut self, rings: &[&[(f32, f32)]]) -> PathRef {
        let first = self.points.len() as u32;
        let mut count = 0u32;
        for (i, ring) in rings.iter().enumerate() {
            if i > 0 {
                self.points.extend_from_slice(&[f32::NAN, f32::NAN]);
                count += 1;
            }
            for (x, y) in ring.iter() {
                self.points.extend_from_slice(&[*x, *y]);
                count += 1;
            }
        }
        PathRef { first, count }
    }

    /// 追加矩形裁切（设备空间 x0, y0, x1, y1），返回索引。
    pub fn push_clip(&mut self, rect: [f32; 4]) -> u32 {
        self.push_clip_def(ClipDef {
            bounds: rect,
            path: None,
            parent: NO_CLIP,
        })
    }

    /// 追加裁切定义（矩形或矢量路径），返回索引。
    pub fn push_clip_def(&mut self, def: ClipDef) -> u32 {
        self.clips.push(def);
        (self.clips.len() - 1) as u32
    }

    /// 追加亮度蒙版定义，返回索引。
    pub fn push_mask_def(&mut self, def: MaskDef) -> u32 {
        self.masks.push(def);
        (self.masks.len() - 1) as u32
    }

    /// 追加平铺图案定义，返回索引。
    pub fn push_pattern_def(&mut self, def: PatternTile) -> u32 {
        self.patterns.push(def);
        (self.patterns.len() - 1) as u32
    }

    /// 追加滤镜定义，返回索引（M8）。
    pub fn push_filter_def(&mut self, def: FilterDef) -> u32 {
        self.filters.push(def);
        (self.filters.len() - 1) as u32
    }

    /// 追加位图数据，返回索引（M8 `<image>`）。
    pub fn push_image_def(&mut self, def: ImgData) -> u32 {
        self.images.push(def);
        (self.images.len() - 1) as u32
    }

    /// 取一段路径的点切片（x, y 交错，含 NaN 分隔）。
    pub fn path_points(&self, d: &PathRef) -> &[f32] {
        let s = d.first as usize;
        &self.points[s..s + d.count as usize * 2]
    }

    /// 点池 AABB（x0, y0, x1, y1；NaN 环分隔符跳过）。
    /// hwC L1 分层发射的层 bbox 基准（层局部空间）。
    /// 空池/全 NaN 防御返回 `[0, 0, 0, 0]`。
    pub fn points_aabb(&self) -> [f32; 4] {
        let mut bbox = [0.0f32; 4];
        let mut got = false;
        let mut i = 0;
        while i + 1 < self.points.len() {
            let (x, y) = (self.points[i], self.points[i + 1]);
            i += 2;
            if x.is_nan() || y.is_nan() {
                continue;
            }
            if got {
                bbox[0] = bbox[0].min(x);
                bbox[1] = bbox[1].min(y);
                bbox[2] = bbox[2].max(x);
                bbox[3] = bbox[3].max(y);
            } else {
                bbox = [x, y, x, y];
                got = true;
            }
        }
        bbox
    }
}
