//! 参考软光栅化后端（ADR-0004：全平台默认同一实现，保证 golden 测试
//! 像素一致）。M1：通用扫描线多边形填充（nonzero/evenodd + src-over
//! alpha 混合）。M2-AA：抗锯齿覆盖率混合。M3：渐变 paint 逐像素求值
//! （像素中心采样，`gradient::sample_paint`；AA 路径 coverage × 渐变 alpha）。
//! M2-RC4：aa>1 从「aa×aa 子点采样」升级为「解析式覆盖」（y 向 4 子
//! 扫描线 + x 向解析累计）。车道KC 起 aa 为两档质量选择器：2 = legacy
//! （1/4 网格吸附）；≥3 = 精档（浮点端点去吸附——吸附 ±0.125px =
//! ±32LSB 是 24px 图标海量集 tol32 细环主误差源，小视口自动升档，
//! 见 [`AA_FINE_MAX_SIDE`]）。
//! M4：clipPath 矢量裁切——裁切路径按与填充同款的覆盖模型烘成每像素
//! 0..255 蒙版（aa=1 像素中心 0/255；aa>1 解析式覆盖），与目标 coverage
//! 相乘；按命令携带的裁切索引（链头）缓存一次，链上多路径取覆盖积。
//! S2：组级 opacity 真语义——PushLayer/PopLayer 命令对之间的 Fill 画进
//! 全视口离屏缓冲（池复用），PopLayer 时整层 × 组 opacity 单次 src-over
//! 合成回下层（重叠区不二次加深）；嵌套层递归合成。

use alloc::vec::Vec;
#[cfg(feature = "host-perf")]
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use crate::dl::{
    BlendMode, ClipDef, Cmd, DisplayList, FillRule, Paint, PatternTile, Rgba8, NO_CLIP,
};
// 车道BF3：draw_image 挂 image-decode 门后，ImgData 仅门内代码引用
#[cfg(feature = "image-decode")]
use crate::dl::ImgData;
// 门控类型的消费路径（ADR-0009；dl 契约本体恒编译）
#[cfg(feature = "clip-path")]
use crate::dl::PathClip;
#[cfg(feature = "pattern")]
use crate::dl::NO_PATTERN;
#[cfg(feature = "filters")]
use crate::dl::{
    FeBlendMode, FeChannel, FeCompositeOp, FeEdgeMode, FeInput, FeKind, FeLight, FeTransferFn,
    FeTurbType, FilterDef,
};
#[cfg(feature = "mask")]
use crate::dl::{MaskDef, NO_MASK};
#[cfg(feature = "gradient")]
use crate::gradient;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixFmt {
    /// 紧凑 RGBA，每像素 4 字节（桌面/移动默认；MCU 上可选）
    Rgba8,
    /// 每像素 2 字节（ESP32 常见面板格式，M7 进入）
    Rgb565,
}

/// 小视口 AA 精档阈值（车道KC）：长边 ≤ 此值的视口，CLI 渲染入口自动
/// 把 `aa` 升到精档（≥3，浮点端点去吸附）。图标海量集全部落在 24-72px，
/// 而 WPT 最小 100×100、主语料最短边 130——阈值 80 把收益精确框在
/// 图标域，大视口保持 legacy 口径逐字节不变（冻结基线/平台 golden 零
/// 漂移）。与 pattern_supersample 的「质量档位随画幅」同构。
pub const AA_FINE_MAX_SIDE: u32 = 80;

/// 宿主性能快路的 A/B 总闸。产品缺省开启；Android 性能取证可在同一
/// 二进制内关闭，消除编译器布局、签名包与安装批次差异。MCU 不开启
/// `host-perf`，setter 退化为空操作且查询恒 false。
#[cfg(feature = "host-perf")]
static HOST_FILL_RUN_OPT: AtomicBool = AtomicBool::new(true);

pub fn set_host_fill_run_opt(enabled: bool) {
    #[cfg(feature = "host-perf")]
    HOST_FILL_RUN_OPT.store(enabled, Ordering::Relaxed);
    #[cfg(not(feature = "host-perf"))]
    let _ = enabled;
}

#[cfg(feature = "host-perf")]
#[inline]
fn host_fill_run_opt() -> bool {
    HOST_FILL_RUN_OPT.load(Ordering::Relaxed)
}

#[cfg(not(feature = "host-perf"))]
#[inline]
fn host_fill_run_opt() -> bool {
    false
}

/// coverage 累计执行形态的同二进制 A/B 总闸（ADR-0027）。
/// 0 = 权威逐子扫描线切片累计；1 = full-span 差分累计（事件横扫，
/// 字节等价证明见 [`cov_apply_diff`]）；2 = 1 + 活动交点顺序复用。
/// 产品缺省 0（权威）：MI 8 实测 1/2 两臂在 pelican/churn 复杂场景
/// 净回归（+4.2%/+5.7%，A75 上 4×NEON 向量化切片与 driftsort 本已
/// 廉价，差分事件横扫与复用掩码/修复的开销反超；Mac 宿主 arm1 广谱
/// 小赢但移动端是性能主目标）——「机制有效但产品收益不足，默认关闭」。
/// 取证通道保留：Android `--ei coverageOpt`、CLI `--covopt` 同 APK/
/// 同二进制切臂；字节等值与开关值无关（单测 + 全语料 sweep 钉死）。
/// MCU（host-perf 关）恒 0 = 权威路径。
#[cfg(feature = "host-perf")]
static COVERAGE_OPT: AtomicU8 = AtomicU8::new(0);

pub fn set_coverage_opt(mode: u8) {
    #[cfg(feature = "host-perf")]
    COVERAGE_OPT.store(mode, Ordering::Relaxed);
    #[cfg(not(feature = "host-perf"))]
    let _ = mode;
}

#[cfg(feature = "host-perf")]
#[inline]
fn coverage_opt() -> u8 {
    COVERAGE_OPT.load(Ordering::Relaxed)
}

#[cfg(not(feature = "host-perf"))]
#[inline]
fn coverage_opt() -> u8 {
    0
}

/// ADR-0027 阶段二：交点顺序复用的行级回退计数（NaN 交点 / 修复预算
/// 超限）。诊断通道——取证时读一次取快照；不影响渲染语义。
#[cfg(feature = "host-perf")]
static COVERAGE_REUSE_FALLBACKS: AtomicU32 = AtomicU32::new(0);

pub fn coverage_reuse_fallbacks() -> u32 {
    #[cfg(feature = "host-perf")]
    {
        COVERAGE_REUSE_FALLBACKS.load(Ordering::Relaxed)
    }
    #[cfg(not(feature = "host-perf"))]
    {
        0
    }
}

/// 诊断快照清零（取证读数后复位；测试串行窗口也用它隔离并发污染）。
pub fn reset_coverage_reuse_fallbacks() {
    #[cfg(feature = "host-perf")]
    COVERAGE_REUSE_FALLBACKS.store(0, Ordering::Relaxed);
}

/// 视口与帧缓冲约定：缓冲按 `width * 每像素字节数 * height` 紧凑排布。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub format: PixFmt,
    /// 抗锯齿模式（M2-AA 引入，RC4 升级为解析式）：`1`（或 0）= 关闭，
    /// 走像素中心采样路径；`2` = 解析式覆盖（y 向 4 子扫描线 + x 向
    /// 1/4 网格吸附累计，legacy 档）；`≥3` = 解析式覆盖浮点端点精档
    /// （车道KC：去吸附——吸附每端点 ±0.125px = ±32LSB 覆盖误差，是
    /// 24px 图标海量集 tol32 细环的主误差源，8x 收敛探针实证；对角
    /// 三角测例 aa=3 精确命中真值覆盖 128，legacy 档给 96/159）。
    /// 子渲染（pattern 烘焙/滤镜区域/feImage）继承本值，同画布模式一致。
    pub aa: u8,
}

/// 每帧复用的光栅化草稿缓冲（render 持有、跨命令复用容量，
/// 省去每 cmd 的 edges/xs/cov 分配；cov 依赖「行末只清脏段」保持全零不变式）。
/// cov 为 u8（wt/perf-raster）：累计值恒为 0..255 的精确整数（加数为
/// 64/63 或 ov×64 截断值；精档浮点端点下部分覆盖加数不再恒 16k 倍数），
/// saturating_add 与原 (v+add).min(255.0) 逐值一致且可向量化
/// （SQADD）；行末 coverage = acc as f32 / 255.0 与原 f32 累计同位。
#[derive(Default)]
struct Scratch {
    edges: Vec<(f32, f32, f32, f32, i32)>,
    xs: Vec<(f32, i32)>,
    cov: Vec<u8>,
    /// 当前像素行带（4 子扫描线 y+0.125..=y+0.875）内可能相交的边索引
    act: Vec<u32>,
    /// coverage 差分累计（host-perf，ADR-0027）的行事件表：(列, ±full)。
    /// 每行 clear；span 满档区间只记起止事件，4 子行后一次横扫合并。
    /// MCU 面（host-perf 关）不编译事件机械——T1-T4 零增量（ADR-0026 纪律）。
    #[cfg(feature = "host-perf")]
    diff: Vec<(i32, i32)>,
    /// ADR-0027 阶段二：交点顺序复用表 (x, dir, ei, 行级 4 子行活性掩码)。
    /// 行首权威生成 + (x, ei) 全序排序；后续子行原地重算/剔除/入列/修复。
    #[cfg(feature = "host-perf")]
    reuse: Vec<(f32, i32, u32, u8)>,
    /// ADR-0027 阶段二：行级活性掩码表 (ei, mask)——每行一次，4 子行共用。
    #[cfg(feature = "host-perf")]
    submask: Vec<(u32, u8)>,
    // ── 车道NT 行带边桶（y 分桶）：把「每行扫描全部边」的 O(rows×edges)
    // 预筛换成一次构建的每行边表。桶边界用与扫描完全相同的 f32 谓词
    // 校验修正，边索引按原序入桶 → act/xs 与旧扫描内容+顺序完全一致。
    bk_first: Vec<i32>,
    bk_last: Vec<i32>,
    bk_diff: Vec<i32>,
    bk_off: Vec<u32>,
    bk_cur: Vec<u32>,
    bk_flat: Vec<u32>,
}

/// 一条命令的有效裁切（裁切链解析结果）：矩形区间 + 矢量覆盖蒙版。
#[derive(Clone, Copy)]
struct ClipCtx<'a> {
    /// 链上全部裁切 bounds 的交（设备空间 x0,y0,x1,y1）。
    rect: [f32; 4],
    /// 链上全部矢量裁切的每像素覆盖积（0..255）；纯矩形链 = None
    /// （走 M1 矩形语义，与历史逐字节一致）。
    cov: Option<&'a [u8]>,
    /// 车道OL（perf-round3）：all-rect 链的内区（cm≡255 像素域）。
    /// Some 时 fill 行末可对内区行/列恢复满段直写（coverage ≡ acc/255）。
    rect_interior: Option<RectInterior>,
}

/// 激活的离屏层（S2）：全视口缓冲 + 组 opacity + 内容脏区
/// （`[x0, y0, x1, y1)` 像素坐标；`x0 > x1` = 空）。fill_path 把扫描
/// 范围并入脏区（保守超集，形状裁切/剔除后的可能着墨区），
/// PopLayer 只扫描脏区合成——层外像素保持全 0，合成必然跳过，
/// 输出与全视口扫描逐字节一致（纯性能优化：frog 的 4 个组层
/// 只覆盖视口局部，全视口合成每层多扫 ~1M 像素）。
struct Layer {
    buf: Vec<u8>,
    opacity: f32,
    /// 车道BL：合成回下层时的混合模式（Normal = 既有 src-over 逐字节不变）。
    blend: BlendMode,
    dirty: [usize; 4],
    /// 已分配（且为全零）的行数——车道AH 惰性行增长：层缓冲不再按全视口
    /// 分配/清零（frog 级场景每帧 4 层 × 11.6MB 全新 malloc+缺页+free
    /// 实测 ~14ms），而是随 Fill 的 y 范围按倍增扩到实际内容高度；
    /// 复用池只存缓冲本体（旧内容在 resize 扩展时整段重清零）。
    rows: usize,
}

/// 确保层缓冲覆盖前 `upto` 行（设备行）。增长策略：需求行数与倍增取大，
/// 钳位到视口高；本层缓冲先 `clear()`（len=0，容量保留）再 `resize`，
/// 故每次扩展都会把 `[0, new_rows)` 全段重新补零，天然满足「层底色全零」
/// 不变式（旧内容——包括上次使用的脏区——一并抹掉，池侧无需按脏区清理）。
fn ensure_layer_rows(l: &mut Layer, upto: usize, w: usize, h: usize) {
    let upto = upto.min(h);
    if upto > l.rows {
        let new_rows = upto.max(l.rows.saturating_mul(2)).max(64).min(h);
        let row_bytes = w * 4;
        l.buf.resize(new_rows.saturating_mul(row_bytes), 0);
        l.rows = new_rows;
    }
}

/// 路径点池的设备空间 y 范围 → 层需覆盖的行数上界
///（ceil(y_max)+1，与 fill_path 的扫描上界一致；分隔符 (NaN,NaN) 跳过；
/// 视口高钳位在 [`ensure_layer_rows`] 内做）。
fn path_rows_needed(pts: &[f32]) -> usize {
    let mut y_max = f32::NEG_INFINITY;
    let mut i = 1;
    while i < pts.len() {
        let y = pts[i];
        if y.is_finite() && y > y_max {
            y_max = y;
        }
        i += 2;
    }
    if y_max == f32::NEG_INFINITY {
        return 0;
    }
    let rows = libm::ceilf(y_max) as i32 + 1;
    rows.max(0) as usize
}

/// 渲染一帧：DisplayList → 像素缓冲（清屏为全透明，再按命令序绘制）。
/// S2：`Cmd::PushLayer` 起的命令画进离屏缓冲（全视口、透明底、缓冲池
/// 复用），`Cmd::PopLayer` 时整层乘组 opacity 单次 src-over 合成回下层
/// （真组级语义：重叠区不二次加深）；嵌套层递归（合成到栈下一层）。
/// M7：按 `vp.format` 分发——RGBA8 直渲染；RGB565 内部仍走完整 RGBA8
/// 管线，帧末统一打包（见 [`render_rgba`] 注记）。
///
/// 初始化域（ADR-0010）：本入口 = 透明底（宿主缺省语义）；CLI 差分
/// 管线用 [`render_bg`] 以白底初始化（对齐 Chrome 白页单次舍入）。
pub fn render(dl: &DisplayList, vp: &Viewport, out: &mut [u8]) {
    let mut cache = RenderCache::default();
    render_with_cache(dl, vp, out, &mut cache);
}

/// 跨帧渲染缓存（车道AH）：目前只承载离屏层缓冲池。**语义不变式**：
/// 缓存只承载临时缓冲，任何时刻输出与无缓存 [`render`] 逐位一致
/// （层缓冲入栈时恒为全零，见 render_rgba 的池项恢复逻辑）。
/// 播放器宿主（FFI/CLI bench）逐帧复用同一缓存，免去每帧
/// malloc/缺页/清零整屏层缓冲（frog 级场景实测 ~7ms/帧）。
///
/// 池条目受 [`LAYER_POOL_CAP`] 病态防御上限约束（车道FD，EI 稳定性
/// 长跑登记的库级加固）：无上限时池随「文档峰值并发层深」无界增长。
#[derive(Default)]
pub struct RenderCache {
    layer_pool: Vec<(Vec<u8>, usize)>,
}

/// 层缓冲池条目上限：池只承载临时缓冲、无淘汰时随文档峰值并发层深
/// 无界增长（病态深嵌套文档理论上 N×整屏驻留）。正常语料峰值层深
/// 2-4，永不触顶；触顶淘汰最旧——只损失复用，不触碰「出池即全零、
/// 输出与无缓存 [`render`] 逐位一致」的语义不变式。
const LAYER_POOL_CAP: usize = 64;

/// 带（可复用）缓存的渲染：`cache` 传 `&mut RenderCache::default()` 与
/// [`render`] 逐位等价；跨帧复用同一 cache 即池化层缓冲。
pub fn render_with_cache(dl: &DisplayList, vp: &Viewport, out: &mut [u8], cache: &mut RenderCache) {
    render_with_cache_bg(dl, vp, out, cache, BG_TRANSPARENT);
}

/// 渲染目标初始化域常量（ADR-0010）：主缓冲清屏色（直色域 u8，
/// 含 alpha）。透明 = 宿主缺省（FFI/Android/ESP32 语义不变）；
/// 白底 = 不透明白（premul 域 round(255)=255），CLI 差分管线对齐
/// Chrome 白页——半透明内容对白底 src-over 单次舍入，消除「透明
/// 存储 + 管线后置白合成」的双重舍入架构地板。
pub const BG_TRANSPARENT: Rgba8 = Rgba8 {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};
pub const BG_WHITE: Rgba8 = Rgba8 {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

/// 以指定底色初始化渲染一帧（ADR-0010）。`bg` 只作用于**主缓冲**；
/// 离屏层缓冲与蒙版/图案/滤镜子渲染（内部走透明缺省 [`render`]）仍
/// 透明初始化——蒙版亮度 = 直色×α 的语义依赖透明底，不得跟随 bg。
pub fn render_bg(dl: &DisplayList, vp: &Viewport, out: &mut [u8], bg: Rgba8) {
    let mut cache = RenderCache::default();
    render_with_cache_bg(dl, vp, out, &mut cache, bg);
}

/// [`render_bg`] 的跨帧缓存版（层缓冲池化，语义同 [`render_with_cache`]）。
pub fn render_with_cache_bg(
    dl: &DisplayList,
    vp: &Viewport,
    out: &mut [u8],
    cache: &mut RenderCache,
    bg: Rgba8,
) {
    // 车道M3：四类惰性缓存（裁切链/蒙版因子/pattern 瓦片/滤镜输出+mip）
    // 的驻留结构上移为 [`FrameCache`]（惰性模式：池槽全空，ensure_* 在
    // 渲染循环内按需补齐）——与 hwB 预计算路径共用同一池结构，跨帧
    // 缓存（[`CrossFrameCache`]）因此能整体携带，条目内容与求值口径
    // 双路径逐位一致。
    let mut fc = FrameCache::lazy(dl);
    match vp.format {
        PixFmt::Rgba8 => {
            debug_assert!(out.len() >= vp.width as usize * vp.height as usize * 4);
            render_rgba(dl, vp, out, cache, bg, &mut fc);
        }
        PixFmt::Rgb565 => {
            // M7 实现取舍：离屏层/蒙版/滤镜/白底预折全部依赖 RGBA 缓冲
            // 语义（RGB565 无 alpha 通道，逐行直写会丢中间透明度、
            // 半透明叠半透明的结果会偏离），故「内部 RGBA 全帧 → 帧末
            // 打包」是与 RGBA 路径逐位一致的最小侵入实现。代价：565
            // 模式额外一块 w*h*4 草稿（ESP32 上由堆分配，真机预算见
            // apps/esp32 审计）；v2 若做直写需并行 alpha 辅助位面。
            let px = vp.width as usize * vp.height as usize;
            debug_assert!(out.len() >= px * 2);
            let mut rgba = alloc::vec![0u8; px * 4];
            render_rgba(dl, vp, &mut rgba, cache, bg, &mut fc);
            pack_rgb565(&rgba, out);
        }
    }
}

/// 主缓冲初始化：透明走 memset 快路径；白底/自定义底色按 4 字节
/// 模式填充（等值字节时 LLVM 仍折叠为 memset，白底即 4×255）。
fn fill_init(out: &mut [u8], bg: Rgba8) {
    if bg == BG_TRANSPARENT {
        out.fill(0);
    } else {
        for px in out.chunks_exact_mut(4) {
            px[0] = bg.r;
            px[1] = bg.g;
            px[2] = bg.b;
            px[3] = bg.a;
        }
    }
}

/// RGBA8 紧凑帧 → RGB565 打包：R 取高 5 位、G 取中 6 位、B 取低 5 位
/// （截断量化，面板惯例）；每像素 u16 小端（ESP32 内存/DMA 原生序）；
/// alpha 丢弃（565 无 alpha 通道，显示语义由 RGBA 路径的白底预折兜底）。
fn pack_rgb565(rgba: &[u8], out: &mut [u8]) {
    for (px, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(2)) {
        let v = ((px[0] as u16 >> 3) << 11) | ((px[1] as u16 >> 2) << 5) | (px[2] as u16 >> 3);
        dst[0] = v as u8;
        dst[1] = (v >> 8) as u8;
    }
}

/// RGBA8 直渲染体（`render` 的两格式共用管线；vp.format 由调用侧分发，
/// 内部子渲染（pattern 瓦片/滤镜离屏）固定构造 Rgba8 视口）。
/// `bg` = 主缓冲初始化域（ADR-0010）：只影响首行 fill_init，混合管线
/// 对底色无感知（通用 src-over，不透明底自动取单次舍入语义）。
/// `fc` = 四类惰性缓存 + mip 链的驻留槽（车道M3 自局部变量上移）：
/// 调用方传 [`FrameCache::lazy`] 产物 == 原惰性语义；预计算/跨帧携带
/// 的已就位条目由 `ensure_*` 首行短路零重复计算（条目是同参纯函数
/// 产物，携带 == 重算，[`precompute_frame_move_pools`] 同款契约）。
#[allow(clippy::too_many_arguments)]
fn render_rgba(
    dl: &DisplayList,
    vp: &Viewport,
    out: &mut [u8],
    cache: &mut RenderCache,
    bg: Rgba8,
    fc: &mut FrameCache,
) {
    fill_init(out, bg);
    let mut sc = Scratch::default();
    // 裁切链/蒙版因子/pattern 瓦片/滤镜/mip 的驻留槽已上移 `fc`
    // （车道M3）：惰性求值条件与次序逐条不变，空槽照旧按需补齐。
    // 离屏层栈（S2）：Fill 画进栈顶，PopLayer 弹出按脏区合成回新栈顶/
    // 主缓冲；pool 回收已合成缓冲供后续层复用（免每层重新分配，重开
    // 层时整体清零）。
    let mut layers: Vec<Layer> = Vec::new();
    let pool = &mut cache.layer_pool;
    for cmd in &dl.cmds {
        match cmd {
            Cmd::PushLayer { opacity, blend } => {
                // 车道AH：层缓冲惰性行增长——按实际内容高度分配（不再
                // 整屏），出池即全零（PopLayer 侧对已用行整段归零后备池，
                // 不依赖脏区记账的精确性；扩容见 ensure_layer_rows）。
                let (buf, rows) = pool.pop().unwrap_or_default();
                layers.push(Layer {
                    buf,
                    opacity: opacity.clamp(0.0, 1.0),
                    blend: *blend,
                    dirty: [usize::MAX, usize::MAX, 0, 0],
                    rows,
                });
            }
            Cmd::PopLayer => {
                if let Some(layer) = layers.pop() {
                    // 合成目标：下层离屏（并把自己的脏区并入其脏区
                    // 记账——内层着墨对下层而言也是着墨）或主缓冲。
                    // 下层先按本层脏区底边扩行（车道AH 惰性层：无自有
                    // Fill 的容器层也可能收到子层合成，行数以子层脏区
                    // 为准先行就位）。
                    let d3 = layer.dirty[3].min(vp.height as usize);
                    let (dst, dst_dirty): (&mut [u8], Option<&mut [usize; 4]>) =
                        match layers.last_mut() {
                            Some(l) => {
                                ensure_layer_rows(l, d3, vp.width as usize, vp.height as usize);
                                let Layer { buf, dirty, .. } = l;
                                (&mut buf[..], Some(dirty))
                            }
                            None => (out, None),
                        };
                    composite_layer(
                        &layer.buf,
                        layer.opacity,
                        layer.blend,
                        &layer.dirty,
                        vp.width as usize,
                        dst,
                    );
                    if let Some(d) = dst_dirty {
                        dirty_union_rect(d, &layer.dirty);
                    }
                    // 车道AH：已用行整段归零后备池（「层底色全零」不变式的
                    // 唯一维护点——不依赖各写路径的脏区记账；行数 > 整屏的
                    // 情形不存在，rows ≤ h 恒成立）。
                    let mut buf = layer.buf;
                    let zero_len = layer.rows * vp.width as usize * 4;
                    buf[..zero_len].fill(0);
                    pool.push((buf, layer.rows));
                    // 车道FD（EI 登记）：池病态防御上限——超限淘汰最旧。
                    // 池是 LIFO 栈：复用热点恒在栈尾，最久未用 = 队首
                    // index 0。淘汰只损失复用；「入池恒归零、出池即全
                    // 零、输出与无缓存 render 逐位一致」不受影响。
                    if pool.len() > LAYER_POOL_CAP {
                        pool.remove(0);
                    }
                }
                // 孤儿 PopLayer（手写 DL）：无层可闭合，忽略
            }
            Cmd::Fill {
                d,
                paint,
                opacity,
                rule,
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
                #[cfg_attr(not(feature = "pattern"), allow(unused_variables))]
                pattern,
            } => {
                let ctx = if *clip == NO_CLIP {
                    None
                } else {
                    ensure_chain(dl, *clip as usize, vp, &mut fc.chains);
                    fc.chains[*clip as usize]
                        .as_ref()
                        .map(|(rect, cov, ri)| ClipCtx {
                            rect: *rect,
                            cov: cov.as_deref(),
                            rect_interior: *ri,
                        })
                };
                #[cfg(feature = "mask")]
                let mask_fac = if *mask == NO_MASK {
                    None
                } else {
                    ensure_mask_factor(dl, *mask as usize, vp, &mut fc.mfactors);
                    fc.mfactors[*mask as usize].as_deref()
                };
                #[cfg(not(feature = "mask"))]
                let mask_fac: Option<&[u8]> = None;
                #[cfg(feature = "pattern")]
                let pat = if *pattern == NO_PATTERN {
                    None
                } else {
                    let pidx = *pattern as usize;
                    ensure_pattern_tile(dl, pidx, vp, &mut fc.ptiles);
                    fc.ptiles[pidx].as_ref().map(|t| PatCtx {
                        tile: &dl.patterns[pidx],
                        bmp: &t.px,
                        bw: t.w,
                        bh: t.h,
                        ss: pattern_supersample(
                            dl.patterns[pidx].tw,
                            dl.patterns[pidx].th,
                            dl.patterns[pidx].sx,
                            dl.patterns[pidx].sy,
                            &dl.patterns[pidx].inv,
                        ),
                    })
                };
                #[cfg(not(feature = "pattern"))]
                let pat: Option<PatCtx> = None;
                // 绘制目标：层激活时画进栈顶离屏（并把扫描范围并入
                // 层脏区），否则主缓冲。层缓冲按路径 y 范围先行扩行
                //（车道AH 惰性增长；fill_path 的行上界 cy1 ≤ 行数上界，
                // 裁切只会收紧 → 保守安全）。
                let (target, dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut()
                {
                    Some(l) => {
                        ensure_layer_rows(
                            l,
                            path_rows_needed(dl.path_points(d)),
                            vp.width as usize,
                            vp.height as usize,
                        );
                        let Layer { buf, dirty, .. } = l;
                        (&mut buf[..], Some(dirty))
                    }
                    None => (out, None),
                };
                fill_path(
                    dl.path_points(d),
                    target,
                    vp,
                    paint,
                    *opacity,
                    *rule,
                    ctx,
                    mask_fac,
                    pat,
                    dirty,
                    0,
                    vp.height as usize,
                    &mut sc,
                );
            }
            // M8：SourceGraphic 离屏渲染 → primitive 链 → 区域合成。
            // feature `filters` 关闭：构建期不发 Cmd::Filter（手写 DL 的
            // 滤镜命令按防御语义跳过，ADR-0009）。
            #[cfg(feature = "filters")]
            Cmd::Filter { filter, clip, mask } => {
                let fidx = *filter as usize;
                if fidx >= dl.filters.len() {
                    continue; // 防御坏 DL
                }
                let fdef = &dl.filters[fidx];
                if fdef.opacity <= 0.0 {
                    continue; // 不可见：跳过整条链求值（t=0 呼吸零点零成本）
                }
                let ctx = if *clip == NO_CLIP {
                    None
                } else {
                    ensure_chain(dl, *clip as usize, vp, &mut fc.chains);
                    fc.chains[*clip as usize]
                        .as_ref()
                        .map(|(rect, cov, ri)| ClipCtx {
                            rect: *rect,
                            cov: cov.as_deref(),
                            rect_interior: *ri,
                        })
                };
                let mask_fac = if *mask == NO_MASK {
                    None
                } else {
                    ensure_mask_factor(dl, *mask as usize, vp, &mut fc.mfactors);
                    fc.mfactors[*mask as usize].as_deref()
                };
                ensure_filter_output(dl, fidx, vp, &mut fc.fcache);
                let Some(fb) = fc.fcache[fidx].as_ref() else {
                    continue;
                };
                // 合成目标：层激活时进栈顶离屏（脏区记账），否则主缓冲
                let (target, dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut()
                {
                    Some(l) => {
                        // 滤镜/位图区域坐标在子模块内解析——防御性按全高扩行
                        ensure_layer_rows(
                            l,
                            vp.height as usize,
                            vp.width as usize,
                            vp.height as usize,
                        );
                        let Layer { buf, dirty, .. } = l;
                        (&mut buf[..], Some(dirty))
                    }
                    None => (out, None),
                };
                composite_filter(
                    fb,
                    fdef.opacity,
                    target,
                    vp,
                    ctx.as_ref(),
                    mask_fac,
                    dirty,
                    0,
                    vp.height as usize,
                );
            }
            #[cfg(not(feature = "filters"))]
            Cmd::Filter { .. } => {
                // 门控降级：构建期不发此命令；手写 DL 防御跳过（ADR-0009）
            }
            // 车道BF3 门控修正：消费路径才是门（image-decode 先例）。
            // BB 车道 kMedium 采样（mip 链 + f64 累加）曾无门进 T1，
            // 把 compiler_builtins f64 软浮点链（__adddf3/__divdf3/
            // __muldf3 等 ≈5.2KB）拖回 MCU 预算——T1 无解码器不发
            // Cmd::Image，消费臂挂 image-decode 门，mipmap/f64 链随
            // gc-sections 剥离。
            #[cfg(feature = "image-decode")]
            Cmd::Image {
                img,
                xf,
                opacity,
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
                pixelated,
            } => {
                // M8：位图逐像素逆映射采样（draw_image；层栈感知
                // 与 Fill 同款）。车道BB：auto = kMedium（mip+定点双
                // 线性），pixelated = 点采样。病态池项（手写 DL 不信任
                // 字段）跳过。
                let data = match dl.images.get(*img as usize) {
                    Some(d)
                        if d.w > 0
                            && d.h > 0
                            && d.rgba.len() >= d.w as usize * d.h as usize * 4 =>
                    {
                        d
                    }
                    _ => continue,
                };
                if *opacity <= 0.0 {
                    continue;
                }
                let ctx = if *clip == NO_CLIP {
                    None
                } else {
                    ensure_chain(dl, *clip as usize, vp, &mut fc.chains);
                    fc.chains[*clip as usize]
                        .as_ref()
                        .map(|(rect, cov, ri)| ClipCtx {
                            rect: *rect,
                            cov: cov.as_deref(),
                            rect_interior: *ri,
                        })
                };
                #[cfg(feature = "mask")]
                let mask_fac = if *mask == NO_MASK {
                    None
                } else {
                    ensure_mask_factor(dl, *mask as usize, vp, &mut fc.mfactors);
                    fc.mfactors[*mask as usize].as_deref()
                };
                #[cfg(not(feature = "mask"))]
                let mask_fac: Option<&[u8]> = None;
                let (target, dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut()
                {
                    Some(l) => {
                        // 滤镜/位图区域坐标在子模块内解析——防御性按全高扩行
                        ensure_layer_rows(
                            l,
                            vp.height as usize,
                            vp.width as usize,
                            vp.height as usize,
                        );
                        let Layer { buf, dirty, .. } = l;
                        (&mut buf[..], Some(dirty))
                    }
                    None => (out, None),
                };
                // 车道hwB：mip 链按需取自帧内缓存（pixelated 无链）。
                // 车道M3：驻留槽与 hwB FrameCache 对齐（img 索引 → 链，
                // `slot.len() < need` 时补建）——原 pair 形式
                // `mip_chain_idx` 退役；链长超出本命令 need 时 draw_image
                // 取第 need-1 级，与「链长恰为 need」逐位相同（mipmap
                // 构链是同源纯函数）。
                let mip = if *pixelated {
                    None
                } else {
                    let need = mip_need(xf);
                    let slot = fc.mips[*img as usize].get_or_insert_with(Vec::new);
                    if (slot.len() as u32) < need {
                        *slot = crate::mipmap::build_chain(data, need);
                    }
                    Some(slot.as_slice())
                };
                draw_image(
                    data,
                    xf,
                    *opacity,
                    *pixelated,
                    target,
                    vp,
                    ctx.as_ref(),
                    mask_fac,
                    dirty,
                    mip,
                    0,
                    vp.height as usize,
                );
            }
            #[cfg(not(feature = "image-decode"))]
            Cmd::Image { .. } => {
                // 门控降级：构建期不发此命令；手写 DL 防御跳过（ADR-0009）
            }
        }
    }
    // 防御：未闭合的层（手写 DL / 构建期 bug）按嵌套序补合成，
    // 内容仍可见而非静默丢失
    while let Some(layer) = layers.pop() {
        let d3 = layer.dirty[3].min(vp.height as usize);
        let (dst, dst_dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut() {
            Some(l) => {
                ensure_layer_rows(l, d3, vp.width as usize, vp.height as usize);
                let Layer { buf, dirty, .. } = l;
                (&mut buf[..], Some(dirty))
            }
            None => (out, None),
        };
        composite_layer(
            &layer.buf,
            layer.opacity,
            layer.blend,
            &layer.dirty,
            vp.width as usize,
            dst,
        );
        if let Some(d) = dst_dirty {
            dirty_union_rect(d, &layer.dirty);
        }
    }
}

// ---------------------------------------------------------------------------
// hwB 行带并行一期：每帧串行预计算（FrameCache）+ 行窗渲染（render_band）。
// 架构依据 docs/research/hwaccel-b-parallel-raster.md §3.1：切分维度按行、
// 全画布缓存（mask/filter/pattern/clip）前置为每帧一次的串行预计算、
// Scratch/层池 per-band 一份；core 保持零线程（并行是宿主的事）。
// ---------------------------------------------------------------------------

/// 每帧一次的串行预计算产物（车道hwB）：原 [`render_rgba`] 内四类惰性
/// 缓存（裁切链覆盖积 / mask 因子表 / pattern 瓦片 / 滤镜输出）的前置化。
/// 条目内容与惰性路径逐字节相同（同参纯函数），band 渲染间**只读共享**
/// ——宿主多线程 fan-out 各渲染一个行窗时本结构跨线程 `&` 共享
/// （全部字段为 plain data，自动 Send+Sync；core 保持零线程）。
pub struct FrameCache {
    chains: Vec<Option<ChainEntry>>,
    /// 亮度蒙版因子缓存（mask 关闭：蒙版池恒空、因子恒缺席，ADR-0009）。
    #[cfg(feature = "mask")]
    mfactors: Vec<Option<Vec<u8>>>,
    /// pattern 瓦片位图缓存。
    #[cfg(feature = "pattern")]
    ptiles: Vec<Option<TileBmp>>,
    /// 滤镜求值结果缓存。
    #[cfg(feature = "filters")]
    fcache: Vec<Option<FilterBmp>>,
    /// mip 链预生成（img 索引 → 链，链长 = 帧内该图命令的最大 need）。
    /// 车道hwB：原 ImgData 内嵌 RefCell 缓存的 band 侧驻留点上移——
    /// 预生成后 band 线程只读共享，RefCell 的并发变异通道不复存在。
    #[cfg(feature = "image-decode")]
    mips: Vec<Option<Vec<crate::mipmap::MipLevel>>>,
    /// 车道hwB2：命令→设备空间 span 索引（平行于 `dl.cmds`，见
    /// [`build_band_index`]）。band 重放据此跳过与窗无交的命令——
    /// hwB 一期 band 重放整条命令流、每带重复付 collect_edges/Scratch
    /// 的 per-command 固定成本（hwB 报告 §四）；索引随本结构一次
    /// 构建、band 间只读共享。x 区间供 [`balanced_band_cuts`] 权重。
    yranges: Vec<BandSpan>,
}

/// 预计算一帧（hwB）：按命令流出现序触发与惰性路径**完全相同**的
/// ensure_* 条件（含 Filter 的 `opacity ≤ 0` 整链剪枝、Image 的池项
/// 有效性防御）——缓存条目内容与求值次序都与 render 内惰性求值一致，
/// [`render_band`] 的输出因此与整帧渲染逐字节一致。未被任何命令引用
/// 的池条目保持缺席（与惰性路径不预热的形态一致，零浪费）。
pub fn precompute_frame(dl: &DisplayList, vp: &Viewport) -> FrameCache {
    let mut fc = FrameCache {
        chains: alloc::vec![None; dl.clips.len()],
        #[cfg(feature = "mask")]
        mfactors: alloc::vec![None; dl.masks.len()],
        #[cfg(feature = "pattern")]
        ptiles: alloc::vec![None; dl.patterns.len()],
        #[cfg(feature = "filters")]
        fcache: alloc::vec![None; dl.filters.len()],
        #[cfg(feature = "image-decode")]
        mips: (0..dl.images.len()).map(|_| None).collect(),
        yranges: build_band_index(dl),
    };
    ensure_frame_caches(dl, vp, &mut fc);
    fc
}

/// 车道RL（retained 增量渲染）：池携带式预计算——把 `pools_prev` 的五类
/// 缓存条目**移入**新 [`FrameCache`]，yranges 按新命令流重建，再走与
/// [`precompute_frame`] 完全相同的 ensure 补齐循环。
///
/// 契约与等值依据：调用方保证池定义与上一帧**逐位一致**（retained 的
/// 池等值判定）——池条目是同参纯函数产物（裁切链覆盖积 / mask 因子表
/// / pattern 瓦片 / 滤镜输出 / mip 链），同池定义 + 同 vp ⇒ 携带条目
/// == 重算条目。ensure 循环幂等（`ensure_*` 首行 `is_some` 短路）：
/// 已就位条目零重复计算；新命令新引用的条目照常补齐；mip 需求档位按
/// 命令流重查 top-up（`slot.len() < need` 重建链）。语义 ==
/// [`precompute_frame`]（同池同命令流逐位同产物）。
pub fn precompute_frame_move_pools(
    dl: &DisplayList,
    vp: &Viewport,
    pools_prev: &mut FrameCache,
) -> FrameCache {
    /// 池向量对齐新 DL 的池长度（契约池一致时本为同长；防御手写 DL
    /// 的长度漂移——多余条目截断、缺条目补 `None` 走 ensure 补齐）。
    fn align<T>(v: &mut Vec<Option<T>>, n: usize) {
        v.truncate(n);
        v.resize_with(n, || None);
    }
    let mut fc = FrameCache {
        chains: core::mem::take(&mut pools_prev.chains),
        #[cfg(feature = "mask")]
        mfactors: core::mem::take(&mut pools_prev.mfactors),
        #[cfg(feature = "pattern")]
        ptiles: core::mem::take(&mut pools_prev.ptiles),
        #[cfg(feature = "filters")]
        fcache: core::mem::take(&mut pools_prev.fcache),
        #[cfg(feature = "image-decode")]
        mips: core::mem::take(&mut pools_prev.mips),
        yranges: Vec::new(),
    };
    align(&mut fc.chains, dl.clips.len());
    #[cfg(feature = "mask")]
    align(&mut fc.mfactors, dl.masks.len());
    #[cfg(feature = "pattern")]
    align(&mut fc.ptiles, dl.patterns.len());
    #[cfg(feature = "filters")]
    align(&mut fc.fcache, dl.filters.len());
    #[cfg(feature = "image-decode")]
    align(&mut fc.mips, dl.images.len());
    fc.yranges = build_band_index(dl);
    ensure_frame_caches(dl, vp, &mut fc);
    fc
}

impl FrameCache {
    /// 惰性模式构造（车道M3）：池槽全空、yranges 不建——条目由
    /// [`render_rgba`] 渲染循环内的 `ensure_*` 按需补齐，语义 ==
    /// 原先在 `render_rgba` 内就地声明的局部缓存。整帧渲染
    /// （非 band）用本构造；band 路径仍用 [`precompute_frame`]。
    /// `yranges` 恒空：`render_band` 只消费 [`precompute_frame`]/
    /// [`precompute_frame_move_pools`] 产物（入口有 debug_assert）。
    fn lazy(dl: &DisplayList) -> Self {
        Self {
            chains: alloc::vec![None; dl.clips.len()],
            #[cfg(feature = "mask")]
            mfactors: alloc::vec![None; dl.masks.len()],
            #[cfg(feature = "pattern")]
            ptiles: alloc::vec![None; dl.patterns.len()],
            #[cfg(feature = "filters")]
            fcache: alloc::vec![None; dl.filters.len()],
            #[cfg(feature = "image-decode")]
            mips: (0..dl.images.len()).map(|_| None).collect(),
            yranges: Vec::new(),
        }
    }
}

/// [`precompute_frame`]/[`precompute_frame_move_pools`] 共享的 ensure
/// 补齐循环：按命令流出现序触发与惰性路径**完全相同**的 ensure_*
/// 条件（含 Filter 的 `opacity ≤ 0` 整链剪枝、Image 的池项有效性
/// 防御）；幂等——已就位条目由 `ensure_*` 首行短路零重复计算。
fn ensure_frame_caches(dl: &DisplayList, vp: &Viewport, fc: &mut FrameCache) {
    for cmd in &dl.cmds {
        match cmd {
            Cmd::Fill {
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
                #[cfg_attr(not(feature = "pattern"), allow(unused_variables))]
                pattern,
                ..
            } => {
                if *clip != NO_CLIP {
                    ensure_chain(dl, *clip as usize, vp, &mut fc.chains);
                }
                #[cfg(feature = "mask")]
                if *mask != NO_MASK {
                    ensure_mask_factor(dl, *mask as usize, vp, &mut fc.mfactors);
                }
                #[cfg(feature = "pattern")]
                if *pattern != NO_PATTERN {
                    ensure_pattern_tile(dl, *pattern as usize, vp, &mut fc.ptiles);
                }
            }
            #[cfg(feature = "filters")]
            Cmd::Filter {
                filter,
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
            } => {
                let fidx = *filter as usize;
                // 与命令循环同条件：越界防御跳过 + opacity ≤ 0 整链剪枝
                if fidx < dl.filters.len() && dl.filters[fidx].opacity > 0.0 {
                    ensure_filter_output(dl, fidx, vp, &mut fc.fcache);
                    if *clip != NO_CLIP {
                        ensure_chain(dl, *clip as usize, vp, &mut fc.chains);
                    }
                    #[cfg(feature = "mask")]
                    if *mask != NO_MASK {
                        ensure_mask_factor(dl, *mask as usize, vp, &mut fc.mfactors);
                    }
                }
            }
            #[cfg(feature = "image-decode")]
            Cmd::Image {
                img,
                xf,
                opacity,
                pixelated,
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
                ..
            } => {
                // 与命令循环同条件：病态池项（手写 DL 不信任字段）跳过
                let valid = dl.images.get(*img as usize).is_some_and(|d| {
                    d.w > 0 && d.h > 0 && d.rgba.len() >= d.w as usize * d.h as usize * 4
                });
                if valid && *opacity > 0.0 {
                    if *clip != NO_CLIP {
                        ensure_chain(dl, *clip as usize, vp, &mut fc.chains);
                    }
                    #[cfg(feature = "mask")]
                    if *mask != NO_MASK {
                        ensure_mask_factor(dl, *mask as usize, vp, &mut fc.mfactors);
                    }
                    // mip 链预生成（车道hwB：band 间共享只读的前提；
                    // pixelated 点采样无链）。链长 = 帧内该图最大 need。
                    if !*pixelated {
                        let need = mip_need(xf);
                        let slot = fc.mips[*img as usize].get_or_insert_with(Vec::new);
                        if (slot.len() as u32) < need {
                            *slot = crate::mipmap::build_chain(&dl.images[*img as usize], need);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 车道M3：跨帧缓存（CrossFrameCache）——同会话连续帧间的池条目复用。
// 动机：`render` 每帧重建四类惰性缓存（裁切链覆盖/蒙版因子/pattern
// 瓦片/滤镜输出 + mip 链），动画播放时这些池定义 ~95% 帧逐位不变
// （变的是命令几何），条目重算纯浪费（大视口 mask/滤镜文档实测
// 1-2ms/帧）。条目是「五池定义 + 视口」的纯函数（RL 池携带先例），
// 命中携带 == 重算，字节等值由此保证。
// ---------------------------------------------------------------------------

/// 五池逐位指纹快照（跨帧命中的比较基准）。
///
/// **命中键 = 视口三元组 (w, h, aa) 相等 且 本快照与当帧五池逐位相等**
/// （含矢量裁切引用的点池切片，f32 按 bits）。选「池逐位指纹」而非
/// 任务简报里的「文档哈希」：池条目只依赖五池定义与视口、不依赖命令
/// 几何——动画**几何**文档逐帧命中；动画**池**文档（动画 clipPath/
// mask 内容等）池逐位变 → 自动判失效，文档哈希会假命中（错帧）。
/// 「新 parse 失效」语义由持有方生命周期实现（一个会话/句柄 = 一次
/// parse = 一个 cache；跨会话不复用）。比较是 memcmp 型纯读，无哈希
/// 碰撞面；任何不可判情形一律 miss 重算（保守方向：丢命中不丢正确）。
struct PoolSnapshot {
    /// 裁切池（含 PathRef 索引与 rule；PartialEq 字段级）。
    clips: Vec<ClipDef>,
    /// 矢量裁切引用的点池切片 `(first, count, 点序列)`——PathRef 是
    /// 索引、内容在 `dl.points`，ClipDef 等值不覆盖被指点；按
    /// path-bearing 裁切的文档序对齐存储，`matches` 位对位比较。
    clip_pts: Vec<(u32, u32, Vec<f32>)>,
    /// 亮度/α 蒙版池（子 DL 自含 points/clips，MaskDef PartialEq 深递归）。
    #[cfg(feature = "mask")]
    masks: Vec<MaskDef>,
    /// pattern 池（同上）。
    #[cfg(feature = "pattern")]
    patterns: Vec<PatternTile>,
    /// 滤镜池（同上）。
    #[cfg(feature = "filters")]
    filters: Vec<FilterDef>,
    /// 位图池（ImgData PartialEq 覆盖 w/h/rgba 全字节——mip 链的源）。
    #[cfg(feature = "image-decode")]
    images: Vec<ImgData>,
}

impl PoolSnapshot {
    fn empty() -> Self {
        Self {
            clips: Vec::new(),
            clip_pts: Vec::new(),
            #[cfg(feature = "mask")]
            masks: Vec::new(),
            #[cfg(feature = "pattern")]
            patterns: Vec::new(),
            #[cfg(feature = "filters")]
            filters: Vec::new(),
            #[cfg(feature = "image-decode")]
            images: Vec::new(),
        }
    }

    // ---- 逐池等值（车道M3 二轮：独立判失效、独立失效独立重建）----
    // 池条目只依赖「本池定义 + 视口」，因此失效面按池切分：动画 mask
    // 文档的裁切链/pattern 瓦片/滤镜输出/ mip 链照常携带（整缓存失效
    // 会把这些也丢掉重算）。比较是 memcmp 型纯读，无分配无哈希碰撞面。

    /// 裁切池逐位等值（含 PathClip 引用的点池切片）。
    fn clips_eq(&self, dl: &DisplayList) -> bool {
        if self.clips != dl.clips {
            return false;
        }
        // 被指点切片：与 path-bearing 裁切同序位对位（clips 等值 ⇒
        // path 形态一致 ⇒ 序列长度一致）。
        let mut stored = self.clip_pts.iter();
        for c in &dl.clips {
            let Some(pc) = &c.path else { continue };
            let Some((first, count, pts)) = stored.next() else {
                return false;
            };
            if *first != pc.d.first || *count != pc.d.count {
                return false;
            }
            let s = pc.d.first as usize;
            let e = s + pc.d.count as usize * 2;
            if e > dl.points.len() || e - s != pts.len() {
                return false; // 越界/漂移：保守 miss（重算不借鉴）
            }
            if !dl.points[s..e]
                .iter()
                .zip(pts.iter())
                .all(|(a, b)| a.to_bits() == b.to_bits())
            {
                return false;
            }
        }
        true
    }

    /// 刷新裁切池快照（失效侧调用）。
    fn set_clips(&mut self, dl: &DisplayList) {
        self.clip_pts.clear();
        for c in &dl.clips {
            if let Some(pc) = &c.path {
                let s = pc.d.first as usize;
                let e = (s + pc.d.count as usize * 2).min(dl.points.len());
                let pts = if s < e {
                    dl.points[s..e].to_vec()
                } else {
                    Vec::new()
                };
                self.clip_pts.push((pc.d.first, pc.d.count, pts));
            }
        }
        self.clips = dl.clips.clone();
    }

    #[cfg(feature = "mask")]
    fn masks_eq(&self, dl: &DisplayList) -> bool {
        self.masks == dl.masks
    }
    #[cfg(feature = "mask")]
    fn set_masks(&mut self, dl: &DisplayList) {
        self.masks = dl.masks.clone();
    }
    #[cfg(feature = "pattern")]
    fn patterns_eq(&self, dl: &DisplayList) -> bool {
        self.patterns == dl.patterns
    }
    #[cfg(feature = "pattern")]
    fn set_patterns(&mut self, dl: &DisplayList) {
        self.patterns = dl.patterns.clone();
    }
    #[cfg(feature = "filters")]
    fn filters_eq(&self, dl: &DisplayList) -> bool {
        self.filters == dl.filters
    }
    #[cfg(feature = "filters")]
    fn set_filters(&mut self, dl: &DisplayList) {
        self.filters = dl.filters.clone();
    }
    #[cfg(feature = "image-decode")]
    fn images_eq(&self, dl: &DisplayList) -> bool {
        self.images == dl.images
    }
    #[cfg(feature = "image-decode")]
    fn set_images(&mut self, dl: &DisplayList) {
        self.images = dl.images.clone();
    }
}

/// 跨帧命中/失效统计（宿主可观测性；测试断言通道）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrossFrameStats {
    /// 池指纹命中帧数（条目整体携带，`ensure_*` 只补缺席槽）。
    pub hits: u64,
    /// 失效帧数（首帧 / 视口变化 / 五池指纹变化 / invalidate 后首帧）。
    pub misses: u64,
}

/// 跨帧渲染缓存（车道M3）：驻留上一帧的池条目 + 池指纹 + 层缓冲池，
/// 同会话（同一次 parse 的文档）连续帧渲染时命中携带、免条目重算。
///
/// - **字节等值红线**：输出与逐帧调用 [`render`] 逐字节一致。依据与
///   [`precompute_frame_move_pools`]（车道RL）同款定理——池条目
///   （裁切链覆盖/蒙版因子/pattern 瓦片/滤镜输出/mip 链）是「五池
///   定义 + 视口」的纯函数：指纹逐位相等 ⇒ 携带条目 == 重算条目；
///   `ensure_*` 幂等（缺席槽照常补齐）；mip 长链取第 need-1 级与
///   恰长链逐位相同（draw_image 契约）。命令几何变化不经指纹（池
///   条目不依赖命令流），天然逐帧命中。
/// - **失效条件**：视口三元组 (w,h,aa) 变化 / 五池指纹变化（含动画
///   clipPath 等 t 依赖池、宿主改图）/ [`Self::invalidate`] / 新会话
///   （持有方丢弃即失效——「新 parse」语义）。
/// - **持有契约**：cache 只可被同一文档会话复用；跨文档复用不产生
///   错误输出（指纹含五池全量逐位），只会 miss。`&mut self` 独占
///   （单线程入口，core 零线程纪律不变）。
/// - **内存账**：快照深拷贝五池定义（含 image 字节——精确比较的
///   代价，大图文档驻留翻倍）；命中帧零分配零比较分配。
///
/// 用法（宿主播放器形态，FFI 会话侧同构）：
/// ```no_run
/// use svgine_core::raster::{CrossFrameCache, PixFmt, Viewport};
/// use svgine_core::dl::DisplayList;
/// let vp = Viewport { width: 480, height: 360, format: PixFmt::Rgba8, aa: 2 };
/// let mut cf = CrossFrameCache::new();
/// // 播放循环：同文档逐 t 重建 DL（几何可变），render 内部判指纹
/// for t in 0..60 {
///     let dl: DisplayList = unimplemented!("build_display_list_at(t)");
///     let mut buf = vec![0u8; 480 * 360 * 4];
///     cf.render(&dl, &vp, &mut buf); // 首帧 miss，后续帧命中携带
/// }
/// ```
pub struct CrossFrameCache {
    /// 视口三元组 (width, height, aa)——池条目的视口依赖面
    /// （pattern 烘焙/裁切链覆盖/aa=1 内区捷径都读这三值）。
    key: (u32, u32, u8),
    snap: PoolSnapshot,
    fc: FrameCache,
    /// 层缓冲池顺带跨帧（车道AH 语义：出池即全零，逐位安全）。
    layers: RenderCache,
    stats: CrossFrameStats,
}

impl Default for CrossFrameCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossFrameCache {
    /// 空 cache（首帧必然 miss 走全新预计算）。
    pub fn new() -> Self {
        Self {
            key: (0, 0, 0),
            snap: PoolSnapshot::empty(),
            fc: FrameCache::lazy(&DisplayList::default()),
            layers: RenderCache::default(),
            stats: CrossFrameStats::default(),
        }
    }

    /// 命中/失效统计快照。
    pub fn stats(&self) -> CrossFrameStats {
        self.stats
    }

    /// 显式作废：丢弃全部驻留（池条目/指纹/层缓冲池），下一帧全量
    /// 重算。宿主缓冲/文档被外部改写后可调用（正常无需——指纹自动判）。
    /// 统计不清（会话累计口径，[`Self::stats`]）。
    pub fn invalidate(&mut self) {
        self.key = (0, 0, 0);
        self.snap = PoolSnapshot::empty();
        self.fc = FrameCache::lazy(&DisplayList::default());
        self.layers.layer_pool.clear();
    }

    /// 渲染一帧（透明底，语义同 [`render`]）：指纹命中 → 池条目整体
    /// 携带（`ensure_*` 只补缺席槽）；失效 → 重建快照与空池。输出与
    /// 逐帧 [`render`] 逐字节一致（红线依据见结构体文档）。
    pub fn render(&mut self, dl: &DisplayList, vp: &Viewport, out: &mut [u8]) {
        self.render_inner(dl, vp, out, BG_TRANSPARENT);
    }

    /// [`Self::render`] 的指定底色版（语义同 [`render_bg`]）。
    pub fn render_bg(&mut self, dl: &DisplayList, vp: &Viewport, out: &mut [u8], bg: Rgba8) {
        self.render_inner(dl, vp, out, bg);
    }

    fn render_inner(&mut self, dl: &DisplayList, vp: &Viewport, out: &mut [u8], bg: Rgba8) {
        // 逐池失效（车道M3 二轮）：视口三元组变化 = 全失效；否则每池
        // 独立判逐位等值，失效池单独刷新快照 + 丢弃对应缓存条目——
        // 动画 mask 文档的裁切链/pattern/滤镜/mip 条目照常携带。
        let vp_ok = self.key == (vp.width, vp.height, vp.aa);
        let clips_eq = vp_ok && self.snap.clips_eq(dl);
        #[cfg(feature = "mask")]
        let masks_eq = vp_ok && self.snap.masks_eq(dl);
        #[cfg(feature = "pattern")]
        let patterns_eq = vp_ok && self.snap.patterns_eq(dl);
        #[cfg(feature = "filters")]
        let filters_eq = vp_ok && self.snap.filters_eq(dl);
        #[cfg(feature = "image-decode")]
        let images_eq = vp_ok && self.snap.images_eq(dl);
        if !vp_ok {
            // 层缓冲按新视口重生长（旧尺寸条目弃用防驻留膨胀）。
            self.layers.layer_pool.clear();
        }
        let mut all_ok = vp_ok;
        if clips_eq {
            // 携带：裁切链条目原样有效。
        } else {
            all_ok = false;
            self.snap.set_clips(dl);
            self.fc.chains = alloc::vec![None; dl.clips.len()];
        }
        #[cfg(feature = "mask")]
        {
            all_ok = all_ok && masks_eq;
            if !masks_eq {
                self.snap.set_masks(dl);
                self.fc.mfactors = alloc::vec![None; dl.masks.len()];
            }
        }
        #[cfg(feature = "pattern")]
        {
            all_ok = all_ok && patterns_eq;
            if !patterns_eq {
                self.snap.set_patterns(dl);
                self.fc.ptiles = alloc::vec![None; dl.patterns.len()];
            }
        }
        #[cfg(feature = "filters")]
        {
            all_ok = all_ok && filters_eq;
            if !filters_eq {
                self.snap.set_filters(dl);
                self.fc.fcache = alloc::vec![None; dl.filters.len()];
            }
        }
        #[cfg(feature = "image-decode")]
        {
            all_ok = all_ok && images_eq;
            if !images_eq {
                self.snap.set_images(dl);
                self.fc.mips = (0..dl.images.len()).map(|_| None).collect();
            }
        }
        if all_ok {
            self.stats.hits += 1;
        } else {
            self.stats.misses += 1;
            self.key = (vp.width, vp.height, vp.aa);
        }
        match vp.format {
            PixFmt::Rgba8 => {
                debug_assert!(out.len() >= vp.width as usize * vp.height as usize * 4);
                render_rgba(dl, vp, out, &mut self.layers, bg, &mut self.fc);
            }
            PixFmt::Rgb565 => {
                // 与 render_with_cache_bg 同构：内部 RGBA 全帧 → 帧末打包。
                let px = vp.width as usize * vp.height as usize;
                debug_assert!(out.len() >= px * 2);
                let mut rgba = alloc::vec![0u8; px * 4];
                render_rgba(dl, vp, &mut rgba, &mut self.layers, bg, &mut self.fc);
                pack_rgb565(&rgba, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 车道hwB2：命令→行区间索引（band 只处理与窗相交的命令）。
// hwB 一期加速比卡 2.07x 的定谳根因 = 每带重放整条命令流、collect_edges/
// Scratch 预热等 per-command 固定成本 ×N 份重复付（hwB 报告 §四；调研
// 报告 hwaccel-b §1.2 登记的二期杠杆）。索引 = 每条命令预计算设备空间
// 行区间，band 重放时整条跳过无交命令（Chrome tile→picture cull 同型）。
// ---------------------------------------------------------------------------

/// 索引行区间外扩护栏（行）。三条写入口（fill_path/composite_filter/
/// draw_image）的扫描行上界全部由同一批 f32 源值经 floor/ceil 整数化
/// 得出，索引按同式复算本就精确（0 外扩安全，依据逐条见
/// [`cmd_band_span`]）；+1 行是对「索引口径与光栅口径未来漂移」的
/// 冗余护栏（后继写路径若引入核半径/滤波扩散不会立刻错跳），代价是
/// 跳过率对薄形状略降（1 行 / 全帧，可忽略）。宁可少跳不可错跳。
const BAND_IDX_PAD: f32 = 1.0;

/// 单条命令的设备空间 span（车道hwB2）：y 区间供 band 跳过判定，
/// x 区间供 [`balanced_band_cuts`] 的面积权重（rows×cols ∝ 像素
/// 工作量的一阶估计）。
#[derive(Clone, Copy)]
struct BandSpan {
    y0: f32,
    y1: f32,
    x0: f32,
    x1: f32,
}

impl BandSpan {
    /// 空区间（任何 band 窗 y0 ≥ 0，跳过判式 `y1 ≤ wy0` 恒真 → 恒跳）。
    const EMPTY: BandSpan = BandSpan {
        y0: 0.0,
        y1: 0.0,
        x0: 0.0,
        x1: 0.0,
    };
}

/// 单条命令的设备空间 span 索引：y = `[y0, y1)` 半开、帧坐标行域，
/// 命令可能着墨行窗的保守超集。区间与 band 窗无交 ⟺ 命令对窗内
/// 像素零贡献（三个写入口的行域钳制各自实证，见 [`render_band_rgba`]
/// 消费点注释）。
fn cmd_band_span(dl: &DisplayList, cmd: &Cmd) -> BandSpan {
    match cmd {
        // 层栈状态机：跳过即错位（栈配对破坏）——哨兵区间恒与窗相交。
        // PushLayer 的层缓冲惰性行增长（无相交 Fill 不扩行）与
        // PopLayer 的脏区空判早退（composite_layer 对空脏区零扫描）
        // 使「保留但不工作」的层命令成本 ≈ 0，无需显式快路径。
        // 哨兵 span 不参与均衡切分的面积权重（层命令是固定成本不是
        // 像素工作），见 balanced_band_cuts。
        Cmd::PushLayer { .. } | Cmd::PopLayer => BandSpan {
            y0: f32::NEG_INFINITY,
            y1: f32::INFINITY,
            x0: f32::NEG_INFINITY,
            x1: f32::INFINITY,
        },
        // Fill：点池 span 的有限坐标 min/max。fill_path 的扫描行域 =
        // [floor(边 y 最小), ceil(边 y 最大))——子扫描线 yc = y+d ∈
        // [y, y+1) 恒不越行、裁切/蒙版/pattern 只收紧不扩张，方向
        // 安全；全水平退化点（不入边表、不产像素）含入索引只会更宽，
        // 保守方向。NaN 环分隔跳过。
        Cmd::Fill { d, .. } => {
            let pts = dl.path_points(d);
            let (mut ymin, mut ymax) = (f32::INFINITY, f32::NEG_INFINITY);
            let (mut xmin, mut xmax) = (f32::INFINITY, f32::NEG_INFINITY);
            let mut i = 0;
            while i + 1 < pts.len() {
                let (x, y) = (pts[i], pts[i + 1]);
                if x.is_finite() && y.is_finite() {
                    if y < ymin {
                        ymin = y;
                    }
                    if y > ymax {
                        ymax = y;
                    }
                    if x < xmin {
                        xmin = x;
                    }
                    if x > xmax {
                        xmax = x;
                    }
                }
                i += 2;
            }
            if ymin > ymax {
                return BandSpan::EMPTY; // 无有限点（空 span）：渲染同样无像素
            }
            BandSpan {
                y0: (libm::floorf(ymin) - BAND_IDX_PAD).max(0.0),
                y1: libm::ceilf(ymax) + BAND_IDX_PAD,
                x0: xmin,
                x1: xmax,
            }
        }
        // Filter：区域行窗。composite_filter 的行循环恰好只写
        // [oy, oy+fb.h) ∩ 窗（fb 高度 = def.h 的同款 2048 钳）；blur
        // 等 primitive 全部有界于区域位图内，无区域外扩散。渲染侧对
        // 坏索引/`opacity ≤ 0` 整链剪枝在零成本 `continue`——索引同
        // 条件给空区间（跳过 = 与执行后零写入逐字节等价）。
        #[cfg(feature = "filters")]
        Cmd::Filter { filter, .. } => match dl.filters.get(*filter as usize) {
            Some(fdef) if fdef.opacity > 0.0 => {
                let y0 = fdef.oy.max(0) as f32;
                let y1 = (fdef.oy + fdef.h.min(2048) as i32) as f32;
                let x0 = fdef.ox.max(0) as f32;
                let x1 = (fdef.ox + fdef.w.min(2048) as i32) as f32;
                BandSpan {
                    y0,
                    y1: y1 + BAND_IDX_PAD,
                    x0,
                    x1,
                }
            }
            _ => BandSpan::EMPTY,
        },
        #[cfg(not(feature = "filters"))]
        Cmd::Filter { .. } => BandSpan::EMPTY, // 门控降级：渲染侧防御跳过
        // Image：xf 四角仿射投影的 AABB（与 draw_image 的扫描上界
        // 同式同源——含非有限角跳过）；逆映射越界像素透明，footprint
        // ⊆ AABB。病态池项/`opacity ≤ 0` 渲染侧零成本跳过——同条件
        // 空区间。
        #[cfg(feature = "image-decode")]
        Cmd::Image {
            img, xf, opacity, ..
        } => {
            let valid = dl.images.get(*img as usize).is_some_and(|d| {
                d.w > 0 && d.h > 0 && d.rgba.len() >= d.w as usize * d.h as usize * 4
            });
            if !valid || *opacity <= 0.0 {
                return BandSpan::EMPTY;
            }
            let img_data = &dl.images[*img as usize];
            let (iw, ih) = (img_data.w as f32, img_data.h as f32);
            // 投影消费 b/d/f（y' = b·x + d·y + f）与 a/c/e（x' = a·x + c·y + e）
            let [a, b, c, d, e, f] = *xf;
            let (mut ymin, mut ymax) = (f32::INFINITY, f32::NEG_INFINITY);
            let (mut xmin, mut xmax) = (f32::INFINITY, f32::NEG_INFINITY);
            for cx in [0.0, iw] {
                for cy in [0.0, ih] {
                    let py = b * cx + d * cy + f;
                    let px = a * cx + c * cy + e;
                    if py.is_finite() {
                        if py < ymin {
                            ymin = py;
                        }
                        if py > ymax {
                            ymax = py;
                        }
                    }
                    if px.is_finite() {
                        if px < xmin {
                            xmin = px;
                        }
                        if px > xmax {
                            xmax = px;
                        }
                    }
                }
            }
            if ymin > ymax {
                return BandSpan::EMPTY; // 全非有限 y 角：draw_image 同样不写
            }
            BandSpan {
                y0: (libm::floorf(ymin) - BAND_IDX_PAD).max(0.0),
                y1: libm::ceilf(ymax) + BAND_IDX_PAD,
                x0: xmin,
                x1: xmax,
            }
        }
        #[cfg(not(feature = "image-decode"))]
        Cmd::Image { .. } => BandSpan::EMPTY, // 门控降级：渲染侧防御跳过
    }
}

/// 全帧命令→行区间索引（[`FrameCache.yranges`] 的构建）：平行于
/// `dl.cmds`，O(Σ命令点数) 一次付出。选平行数组而非排序辅助表的
/// 取舍：band 重放保命令序（层栈/重叠合成语义本就要求原序遍历），
/// 平行数组 zip 零间接、构建零排序；排序表只在「命令数极大且命中
/// 稀疏」时有优势，实测语料最大 ~5K 命令、线性扫 16B/条的成本远低
/// 于被跳命令的 collect_edges 固定成本。
fn build_band_index(dl: &DisplayList) -> Vec<BandSpan> {
    dl.cmds.iter().map(|cmd| cmd_band_span(dl, cmd)).collect()
}

/// 车道RL：命令→设备空间保守着墨区间 `[y0, y1, x0, x1]`（f32 半开
/// 域）——[`build_band_index`] 的 plain-array 形态（retained 模块跨
/// 模块消费；`BandSpan` 为光栅器私有类型不外泄）。层哨兵命令
/// （PushLayer/PopLayer）y = `[-inf, +inf]`、x 同为无穷。行域保守性
/// 与 band 跳过判式同一条定理（区间与行窗无交 ⟺ 该命令对窗内像素
/// 零贡献，见 [`render_band_rgba`] 消费点注释）——retained 的脏行
/// 推导据此保证「未标脏的行，新旧帧像素必然逐字节一致」。
pub fn band_index_spans(dl: &DisplayList) -> Vec<[f32; 4]> {
    build_band_index(dl)
        .into_iter()
        .map(|s| [s.y0, s.y1, s.x0, s.x1])
        .collect()
}

/// 车道hwB2：**工作均衡**的 band 切分——索引的二阶产出。hwB 一期
/// 均匀切分在内容偏斜的语料（pelican/frog 主体居中下部）下 wall =
/// 最重带（实测逐带工作量 2.8x 失衡，是 4 带只到 2.1x 的真正残差
/// ——命令→行区间索引消除重复固定成本后归因定谳，见
/// reports/hw-band-idx.md §四）。本函数按索引把每命令的 span 面积
/// （rows×cols ∝ 像素工作量一阶估计）均匀摊到其行域上，再按累计
/// 剖面等分取切口：每带 ≈ 等量像素工作。输出语义同 CLI
/// `--band-cuts`（严格递增、∈ (0,h)）；剖面全零（空画布）回退均匀
/// 切。切分策略只动「谁渲哪几行」，渲染语义零变化（band 拼接对任意
/// 切分逐字节等值——odd cuts sweep 实证）。
pub fn balanced_band_cuts(dl: &DisplayList, vp: &Viewport, bands: u32) -> Vec<usize> {
    let h = vp.height as usize;
    if bands <= 1 || h == 0 {
        return Vec::new();
    }
    let spans = build_band_index(dl);
    let w = vp.width as f32;
    // 每行工作剖面：命令面积 / span 行数 = 该命令在此行的权重。
    // 层命令哨兵（x0 = −∞）无像素工作，不摊。
    let mut profile = alloc::vec![0.0f64; h];
    for s in &spans {
        if !s.x0.is_finite() && !s.x1.is_finite() {
            continue; // 哨兵（Push/Pop）
        }
        let y0 = (s.y0.max(0.0) as usize).min(h);
        let y1 = (s.y1.max(0.0) as usize).min(h);
        if y1 <= y0 {
            continue;
        }
        let cols = (s.x1 - s.x0).max(0.0).min(w) as f64;
        if cols <= 0.0 {
            continue;
        }
        let per_row = cols / (y1 - y0) as f64;
        for row in &mut profile[y0..y1] {
            *row += per_row;
        }
    }
    let total: f64 = profile.iter().sum();
    // 均匀切基线（剖面退化时回退）
    let mut cuts: Vec<usize> = (1..bands as usize)
        .map(|i| i * h / bands as usize)
        .collect();
    if total > 0.0 {
        cuts.clear();
        let mut acc = 0.0f64;
        let mut k = 1usize;
        for (row, &p) in profile.iter().enumerate() {
            acc += p;
            // 等分阈值到达即切口（累计口径含当前行——第 k 切口放在
            // 「前 k/band 工作量」收尾处；严格递增由 k 推进保证）
            if acc >= total * k as f64 / bands as f64 && row + 1 < h {
                cuts.push(row + 1);
                k += 1;
                if k == bands as usize {
                    break;
                }
            }
        }
        // 剖面偏斜导致切口数不足（尾部重载）：补均匀切口兜底
        let mut prev = 0usize;
        let mut fixed: Vec<usize> = Vec::with_capacity(cuts.len());
        for &c in &cuts {
            if c > prev {
                fixed.push(c);
                prev = c;
            }
        }
        while fixed.len() < bands as usize - 1 {
            let c = (fixed.len() + 1) * h / bands as usize;
            if c > prev && c < h {
                fixed.push(c);
                prev = c;
            } else {
                break; // 无法再插（h 过小）：交付已有切口（≥0 个，合法）
            }
        }
        cuts = fixed;
    }
    cuts
}

/// 车道hwB2 诊断面：band 索引跳过率观测（渲染零依赖，std 宿主
/// CLI/测试/报告消费；core 渲染面不调用）。`cuts` 语义同 CLI
/// `--band-cuts`（严格递增、∈ (0,h)；空 = 单带全帧）。返回每带
/// `(窗内需处理命令数, 索引跳过命令数)`——PushLayer/PopLayer 哨兵
/// 恒计入「处理」。
pub fn band_index_stats(dl: &DisplayList, vp: &Viewport, cuts: &[usize]) -> Vec<(usize, usize)> {
    let yranges = build_band_index(dl);
    let h = vp.height as usize;
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut prev = 0usize;
    for &cut in cuts.iter().chain(core::iter::once(&h)) {
        let end = cut.min(h);
        let (y0, y1) = (prev as f32, end as f32);
        let mut skipped = 0usize;
        for span in &yranges {
            if span.y1 <= y0 || span.y0 >= y1 {
                skipped += 1;
            }
        }
        out.push((yranges.len() - skipped, skipped));
        prev = end;
    }
    out
}

/// 行窗渲染一帧（hwB 行带并行一期）：只写 `band` 内容对应的帧行
/// `[y0, y1)`——`band` 为**全宽**行窗缓冲（≥ `(y1-y0) * w * 4` 字节，
/// 行 0 = 帧 y0 行）。语义 = [`render_with_cache_bg`]（Rgba8、同 `bg`）
/// 的该行区间逐字节子集；`shared` = [`precompute_frame`] 产物（band
/// 间只读共享），`cache` = **本 band 专属**的层缓冲池（每 band 一份，
/// 禁止跨 band 复用同一 `&mut RenderCache`）。
///
/// 并行契约（宿主负责行互斥）：多线程对同一 `(dl, vp, shared)` 并发
/// 调用本函数时，各调用的 `[y0,y1)` 必须互斥、并集 ⊆ [0,h)，各自传
/// 独立 `cache` 与独立 band 缓冲。可变态全部 band-local
/// （Scratch/层池/层栈），跨线程共享面只有 `&DisplayList`/`&Viewport`/
/// `&FrameCache` 三个只读结构。
///
/// band 恒全宽是字节等值的前提（渐变 tile 档位按视口宽选档、bayer
/// 抖动相位按绝对 (x,y)——行切分不动宽度，档位与相位不变）。
/// 一期限定：仅 Rgba8 直写（RGB565 全帧打包路径走
/// [`render_with_cache_bg`]，非 band 面；格式不符时本函数无操作）。
#[allow(clippy::too_many_arguments)]
pub fn render_band(
    dl: &DisplayList,
    vp: &Viewport,
    band: &mut [u8],
    y0: usize,
    y1: usize,
    bg: Rgba8,
    shared: &FrameCache,
    cache: &mut RenderCache,
) {
    if vp.format != PixFmt::Rgba8 {
        return; // 一期限定 Rgba8 直写（565 走全帧路径，见上）
    }
    let w = vp.width as usize;
    let h = vp.height as usize;
    let y0 = y0.min(h);
    let y1 = y1.min(h);
    if y0 >= y1 {
        return; // 空窗（越界防御：契约要求并集 ⊆ [0,h)）
    }
    let rows = y1 - y0;
    debug_assert!(band.len() >= rows * w * 4, "band 缓冲须 ≥ rows*w*4");
    render_band_rgba(dl, vp, band, y0, rows, bg, shared, cache);
}

/// [`render_band`] 的 RGBA8 直渲染体：命令流**原样重放**，与
/// [`render_rgba`] 的差异只有四处机械变换——①四类缓存从 FrameCache
/// 只读消费（等价于惰性缓存已全部就位）；②层缓冲/脏区 band-local
/// （行 0 = 帧 y0 行，钳位域 = band 行数）；③三个写入口
/// （fill_path/composite_filter/draw_image）传 band 行窗；④车道hwB2
/// 命令→行区间索引（[`FrameCache.yranges`]）跳过与窗无交的命令
/// （PushLayer/PopLayer 除外——层栈状态机恒重放）。行内像素
/// 计算全部按帧坐标进行，与全帧渲染同式同值（字节 sweep 验收，
/// 见 tests/oracle/band_sweep.py）。
#[allow(clippy::too_many_arguments)]
fn render_band_rgba(
    dl: &DisplayList,
    vp: &Viewport,
    band: &mut [u8],
    y0: usize,
    rows: usize,
    bg: Rgba8,
    shared: &FrameCache,
    cache: &mut RenderCache,
) {
    fill_init(band, bg);
    let mut sc = Scratch::default();
    let bh = rows; // band 行数：层缓冲/脏区的钳位域
                   // 四类缓存只读消费（precompute_frame 已按命令流序触发同条件
                   // ensure_*；缺项 = 惰性路径同样缺项——防御坏索引语义一致）
    let chains = &shared.chains;
    #[cfg(feature = "mask")]
    let mfactors = &shared.mfactors;
    #[cfg(feature = "pattern")]
    let ptiles = &shared.ptiles;
    #[cfg(feature = "filters")]
    let fcache = &shared.fcache;
    #[cfg(feature = "image-decode")]
    let shared_mips = &shared.mips;
    // 离屏层栈（band-local）：层缓冲行 0 = 帧 y0 行，行数 ≤ band 行数
    let mut layers: Vec<Layer> = Vec::new();
    let pool = &mut cache.layer_pool;
    // 车道hwB2：命令→行区间索引消费——y 区间与 band 窗 [y0, y0+rows)
    // 无交的命令整条跳过（PushLayer/PopLayer 哨兵区间恒相交，永不跳）。
    // 跳过 ⟺ 零像素贡献的逐字节等价依据：三个写入口的扫描行域都被
    // 与索引同式的 floor/ceil 口径钳进各自 y 区间——fill_path 的
    // cy0/cy1 双钳（几何 y_min/y_max → band 窗）在无交时行循环空转、
    // 脏区更新因 y0 ≥ y1 跳过；composite_filter 逐行判窗 continue、
    // 脏区 yb0/yb1 退缩为空；draw_image 在 py1 ≤ py0 时整段提前返回。
    // 层激活时的 ensure_layer_rows 差异只影响 band-local 缓冲行数，
    // 不进输出；Scratch/层池 band-local，跳过与「执行后零写入」对
    // 后续命令的状态完全一致。
    let wy0 = y0 as f32;
    let wy1 = (y0 + rows) as f32;
    // 车道M3 防御：band 索引只由 precompute_frame/precompute_frame_move_pools
    // 构建（FrameCache::lazy 的 yranges 恒空——误传惰性缓存会整帧静默
    // 空转，此处 debug 期即炸）。
    debug_assert_eq!(shared.yranges.len(), dl.cmds.len());
    for (cmd, yr) in dl.cmds.iter().zip(shared.yranges.iter()) {
        if yr.y1 <= wy0 || yr.y0 >= wy1 {
            continue;
        }
        match cmd {
            Cmd::PushLayer { opacity, blend } => {
                let (buf, lr) = pool.pop().unwrap_or_default();
                layers.push(Layer {
                    buf,
                    opacity: opacity.clamp(0.0, 1.0),
                    blend: *blend,
                    dirty: [usize::MAX, usize::MAX, 0, 0],
                    rows: lr,
                });
            }
            Cmd::PopLayer => {
                if let Some(layer) = layers.pop() {
                    // 合成目标：下层离屏（脏区 band-local 并账）或主 band
                    let d3 = layer.dirty[3].min(bh);
                    let (dst, dst_dirty): (&mut [u8], Option<&mut [usize; 4]>) =
                        match layers.last_mut() {
                            Some(l) => {
                                ensure_layer_rows(l, d3, vp.width as usize, bh);
                                let Layer { buf, dirty, .. } = l;
                                (&mut buf[..], Some(dirty))
                            }
                            None => (band, None),
                        };
                    composite_layer(
                        &layer.buf,
                        layer.opacity,
                        layer.blend,
                        &layer.dirty,
                        vp.width as usize,
                        dst,
                    );
                    if let Some(d) = dst_dirty {
                        dirty_union_rect(d, &layer.dirty);
                    }
                    let mut buf = layer.buf;
                    let zero_len = layer.rows * vp.width as usize * 4;
                    buf[..zero_len].fill(0);
                    pool.push((buf, layer.rows));
                    if pool.len() > LAYER_POOL_CAP {
                        pool.remove(0);
                    }
                }
                // 孤儿 PopLayer（手写 DL）：无层可闭合，忽略
            }
            Cmd::Fill {
                d,
                paint,
                opacity,
                rule,
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
                #[cfg_attr(not(feature = "pattern"), allow(unused_variables))]
                pattern,
            } => {
                let ctx = if *clip == NO_CLIP {
                    None
                } else {
                    chains[*clip as usize]
                        .as_ref()
                        .map(|(rect, cov, ri)| ClipCtx {
                            rect: *rect,
                            cov: cov.as_deref(),
                            rect_interior: *ri,
                        })
                };
                #[cfg(feature = "mask")]
                let mask_fac = if *mask == NO_MASK {
                    None
                } else {
                    mfactors[*mask as usize].as_deref()
                };
                #[cfg(not(feature = "mask"))]
                let mask_fac: Option<&[u8]> = None;
                #[cfg(feature = "pattern")]
                let pat = if *pattern == NO_PATTERN {
                    None
                } else {
                    let pidx = *pattern as usize;
                    ptiles[pidx].as_ref().map(|t| PatCtx {
                        tile: &dl.patterns[pidx],
                        bmp: &t.px,
                        bw: t.w,
                        bh: t.h,
                        ss: pattern_supersample(
                            dl.patterns[pidx].tw,
                            dl.patterns[pidx].th,
                            dl.patterns[pidx].sx,
                            dl.patterns[pidx].sy,
                            &dl.patterns[pidx].inv,
                        ),
                    })
                };
                #[cfg(not(feature = "pattern"))]
                let pat: Option<PatCtx> = None;
                // 绘制目标：层激活时画进栈顶离屏（band-local 脏区），
                // 否则主 band 切片。层扩行 = 帧行数上界转 band-local。
                let (target, dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut()
                {
                    Some(l) => {
                        ensure_layer_rows(
                            l,
                            path_rows_needed(dl.path_points(d)).saturating_sub(y0),
                            vp.width as usize,
                            bh,
                        );
                        let Layer { buf, dirty, .. } = l;
                        (&mut buf[..], Some(dirty))
                    }
                    None => (band, None),
                };
                fill_path(
                    dl.path_points(d),
                    target,
                    vp,
                    paint,
                    *opacity,
                    *rule,
                    ctx,
                    mask_fac,
                    pat,
                    dirty,
                    y0,
                    rows,
                    &mut sc,
                );
            }
            #[cfg(feature = "filters")]
            Cmd::Filter { filter, clip, mask } => {
                let fidx = *filter as usize;
                if fidx >= dl.filters.len() {
                    continue; // 防御坏 DL
                }
                let fdef = &dl.filters[fidx];
                if fdef.opacity <= 0.0 {
                    continue; // 不可见：整链已剪枝（precompute 同条件）
                }
                let ctx = if *clip == NO_CLIP {
                    None
                } else {
                    chains[*clip as usize]
                        .as_ref()
                        .map(|(rect, cov, ri)| ClipCtx {
                            rect: *rect,
                            cov: cov.as_deref(),
                            rect_interior: *ri,
                        })
                };
                let mask_fac = if *mask == NO_MASK {
                    None
                } else {
                    mfactors[*mask as usize].as_deref()
                };
                let Some(fb) = fcache[fidx].as_ref() else {
                    continue;
                };
                // 合成目标：层激活时进栈顶离屏（band-local 脏区记账），
                // 否则主 band 切片；防御性按 band 全高扩行
                let (target, dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut()
                {
                    Some(l) => {
                        ensure_layer_rows(l, bh, vp.width as usize, bh);
                        let Layer { buf, dirty, .. } = l;
                        (&mut buf[..], Some(dirty))
                    }
                    None => (band, None),
                };
                composite_filter(
                    fb,
                    fdef.opacity,
                    target,
                    vp,
                    ctx.as_ref(),
                    mask_fac,
                    dirty,
                    y0,
                    rows,
                );
            }
            #[cfg(not(feature = "filters"))]
            Cmd::Filter { .. } => {
                // 门控降级：构建期不发此命令；手写 DL 防御跳过（ADR-0009）
            }
            #[cfg(feature = "image-decode")]
            Cmd::Image {
                img,
                xf,
                opacity,
                clip,
                #[cfg_attr(not(feature = "mask"), allow(unused_variables))]
                mask,
                pixelated,
            } => {
                // 位图逐像素逆映射采样（与 render_rgba 同防御：病态池项
                // 跳过、opacity ≤ 0 跳过）
                let data = match dl.images.get(*img as usize) {
                    Some(d)
                        if d.w > 0
                            && d.h > 0
                            && d.rgba.len() >= d.w as usize * d.h as usize * 4 =>
                    {
                        d
                    }
                    _ => continue,
                };
                if *opacity <= 0.0 {
                    continue;
                }
                let ctx = if *clip == NO_CLIP {
                    None
                } else {
                    chains[*clip as usize]
                        .as_ref()
                        .map(|(rect, cov, ri)| ClipCtx {
                            rect: *rect,
                            cov: cov.as_deref(),
                            rect_interior: *ri,
                        })
                };
                #[cfg(feature = "mask")]
                let mask_fac = if *mask == NO_MASK {
                    None
                } else {
                    mfactors[*mask as usize].as_deref()
                };
                #[cfg(not(feature = "mask"))]
                let mask_fac: Option<&[u8]> = None;
                let (target, dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut()
                {
                    Some(l) => {
                        // 位图区域坐标在子模块内解析——防御性按 band 全高扩行
                        ensure_layer_rows(l, bh, vp.width as usize, bh);
                        let Layer { buf, dirty, .. } = l;
                        (&mut buf[..], Some(dirty))
                    }
                    None => (band, None),
                };
                // 车道hwB：mip 链取自 FrameCache 预生成（band 间只读共享）
                let mip = if *pixelated {
                    None
                } else {
                    shared_mips[*img as usize].as_deref()
                };
                draw_image(
                    data,
                    xf,
                    *opacity,
                    *pixelated,
                    target,
                    vp,
                    ctx.as_ref(),
                    mask_fac,
                    dirty,
                    mip,
                    y0,
                    rows,
                );
            }
            #[cfg(not(feature = "image-decode"))]
            Cmd::Image { .. } => {
                // 门控降级：构建期不发此命令；手写 DL 防御跳过（ADR-0009）
            }
        }
    }
    // 防御：未闭合的层（手写 DL / 构建期 bug）按嵌套序补合成（band-local）
    while let Some(layer) = layers.pop() {
        let d3 = layer.dirty[3].min(bh);
        let (dst, dst_dirty): (&mut [u8], Option<&mut [usize; 4]>) = match layers.last_mut() {
            Some(l) => {
                ensure_layer_rows(l, d3, vp.width as usize, bh);
                let Layer { buf, dirty, .. } = l;
                (&mut buf[..], Some(dirty))
            }
            None => (band, None),
        };
        composite_layer(
            &layer.buf,
            layer.opacity,
            layer.blend,
            &layer.dirty,
            vp.width as usize,
            dst,
        );
        if let Some(d) = dst_dirty {
            dirty_union_rect(d, &layer.dirty);
        }
    }
}

/// 脏区矩形并入（空矩形无操作）：内层合成/子命令扫描范围对下层
/// 层的着墨记账。
fn dirty_union_rect(d: &mut [usize; 4], r: &[usize; 4]) {
    if r[0] >= r[2] || r[1] >= r[3] {
        return;
    }
    if r[0] < d[0] {
        d[0] = r[0];
    }
    if r[1] < d[1] {
        d[1] = r[1];
    }
    if r[2] > d[2] {
        d[2] = r[2];
    }
    if r[3] > d[3] {
        d[3] = r[3];
    }
}

/// 离屏层合成回下层（S2 真组级 opacity）：脏区内源直色像素的有效
/// alpha = `像素 alpha × 组 opacity`，对目标做单次 src-over（复用
/// blend_px 的直色混合模型——元素填充与层合成共用同一 alpha 四舍五入
/// 策略，色通道保持 M1 起的截断；组内纯色 → rgb 原样、
/// alpha=round(a·255)）。脏区外层缓冲恒全 0（清零 + 未着墨），
/// 合成必然无操作——跳过即逐字节等价。组 opacity ≤ 0 整层不可见。
///
/// 车道BL：`blend != Normal` 时走 [`blend_composite_px`]（CSS 合成，
/// sRGB 直色域）；Normal 保持既有 blend_px 路径逐字节不变。
fn composite_layer(
    src: &[u8],
    opacity: f32,
    blend: BlendMode,
    dirty: &[usize; 4],
    w: usize,
    dst: &mut [u8],
) {
    if opacity <= 0.0 || dirty[0] >= dirty[2] || dirty[1] >= dirty[3] {
        return;
    }
    // 车道BL：混合消费路径挂 css 门（ADR-0009 消费侧门控先例）——
    // mix-blend-mode 仅 CSS 声明位可达（Chrome 实测 XML 属性位忽略），
    // css 关闭时 blend 恒 Normal，混合数学整段不可达（T1 体积零增量）。
    #[cfg(feature = "css")]
    if blend == BlendMode::Normal {
        composite_layer_src_over(src, opacity, dirty, w, dst);
    } else {
        composite_layer_blend(src, opacity, blend, dirty, w, dst);
    }
    #[cfg(not(feature = "css"))]
    {
        let _ = blend;
        composite_layer_src_over(src, opacity, dirty, w, dst);
    }
}

/// 既有 src-over 层合成（车道BL 前的 composite_layer 本体，逐字节不动）。
fn composite_layer_src_over(
    src: &[u8],
    opacity: f32,
    dirty: &[usize; 4],
    w: usize,
    dst: &mut [u8],
) {
    if opacity <= 0.0 || dirty[0] >= dirty[2] || dirty[1] >= dirty[3] {
        return;
    }
    for y in dirty[1]..dirty[3] {
        let row = y * w;
        let mut x = dirty[0];
        while x < dirty[2] {
            // u64 字扫透明段：层缓冲底色全零，未着墨 8 像素组 = 32 字节
            // 全零（着墨处 alpha>0）。跳过与逐像素 a<=0.0 → continue 完全
            // 等价，字节级行为不变。
            while x + 8 <= dirty[2] {
                let o = (row + x) * 4;
                if src[o..o + 32] != [0u8; 32] {
                    break;
                }
                x += 8;
            }
            if x >= dirty[2] {
                break;
            }
            let o = (row + x) * 4;
            let a = src[o + 3] as f32 / 255.0 * opacity;
            if a <= 0.0 {
                x += 1;
                continue;
            }
            let c = Rgba8 {
                r: src[o],
                g: src[o + 1],
                b: src[o + 2],
                a: 255,
            };
            blend_px(&mut dst[o..o + 4], c, a);
            x += 1;
        }
    }
}

/// 混合模式层合成（车道BL）：脏区内层像素（直色 sRGB + 有效 alpha
/// `a8/255 × opacity`）按 CSS Compositing & Blending L1 对下层 backdrop
/// 混合。αs=0 或 αb=0 的像素混合项数学上为 0（B 项不参与），与脏区
/// 跳过/逐像素跳过语义一致——层外未着墨像素对目标无操作。
///
/// 工作域 = **sRGB 直色**（Chrome 页面合成口径；feBlend 是 linearRGB
/// 滤镜域——车道U atop 教训，两域差一个 gamma，勿混）。混合色量化 =
/// round（新路径无历史字节约束），alpha 沿用 blend_px 的 round 语义。
#[cfg(feature = "css")]
fn composite_layer_blend(
    src: &[u8],
    opacity: f32,
    mode: BlendMode,
    dirty: &[usize; 4],
    w: usize,
    dst: &mut [u8],
) {
    for y in dirty[1]..dirty[3] {
        let row = y * w;
        let mut x = dirty[0];
        while x < dirty[2] {
            // 同 src_over 路径的 u64 字扫透明段跳过（层缓冲底色全零，
            // 未着墨 8 像素组 = 32 字节全零；αs=0 对目标是恒等）。
            while x + 8 <= dirty[2] {
                let o = (row + x) * 4;
                if src[o..o + 32] != [0u8; 32] {
                    break;
                }
                x += 8;
            }
            if x >= dirty[2] {
                break;
            }
            let o = (row + x) * 4;
            let a = src[o + 3] as f32 / 255.0 * opacity;
            if a <= 0.0 {
                x += 1;
                continue;
            }
            let c = Rgba8 {
                r: src[o],
                g: src[o + 1],
                b: src[o + 2],
                a: 255,
            };
            blend_composite_px(&mut dst[o..o + 4], c, a, mode);
            x += 1;
        }
    }
}

/// 单像素 CSS 混合合成（车道BL）：`c`/`a` = 源直色与有效 alpha，
/// `px` = 目标（backdrop）直色像素。公式（CSS Compositing L1 §10）：
/// `Co = αs·(1−αb)·Cs + αs·αb·B(Cb,Cs) + (1−αs)·αb·Cb`（premultiplied），
/// 直色输出 `Co/αo`，`αo = αs + αb·(1−αs)`。αb=0 时 B 未定义（Cb 无意义）
/// 且系数为 0——跳过 B 求值（防非可分离模式的 0·NaN = NaN）。
#[cfg(feature = "css")]
fn blend_composite_px(px: &mut [u8], c: Rgba8, a: f32, mode: BlendMode) {
    let da = px[3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0 {
        return;
    }
    let cs = [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0];
    let cb = [
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    ];
    // B(Cb, Cs) 逐像素求一次（非可分离模式三通道联立，无法逐通道算）
    let b = if da > 0.0 {
        blend_css(cb, cs, mode)
    } else {
        [0.0; 3]
    };
    for ch in 0..3 {
        let pm = a * (1.0 - da) * cs[ch] + a * da * b[ch] + (1.0 - a) * da * cb[ch];
        let v = libm::roundf(pm / out_a * 255.0);
        px[ch] = if v < 0.0 {
            0
        } else if v > 255.0 {
            255
        } else {
            v as u8
        };
    }
    px[3] = (out_a * 255.0 + 0.5) as u8;
}

/// CSS 混合函数 B(Cb, Cs)（sRGB 直色 [0,1]）：可分离 11 模式逐通道 +
/// 非可分离 4 模式（Hue/Saturation/Color/Luminosity，Chrome bl-*.svg
/// 探针矩阵逐值实证 ±1）。
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css(cb: [f32; 3], cs: [f32; 3], mode: BlendMode) -> [f32; 3] {
    match mode {
        BlendMode::Multiply => [cb[0] * cs[0], cb[1] * cs[1], cb[2] * cs[2]],
        BlendMode::Screen => [
            cb[0] + cs[0] - cb[0] * cs[0],
            cb[1] + cs[1] - cb[1] * cs[1],
            cb[2] + cs[2] - cb[2] * cs[2],
        ],
        BlendMode::Overlay => [
            blend_css_hard_light(cs[0], cb[0]),
            blend_css_hard_light(cs[1], cb[1]),
            blend_css_hard_light(cs[2], cb[2]),
        ],
        BlendMode::Darken => [cb[0].min(cs[0]), cb[1].min(cs[1]), cb[2].min(cs[2])],
        BlendMode::Lighten => [cb[0].max(cs[0]), cb[1].max(cs[1]), cb[2].max(cs[2])],
        BlendMode::ColorDodge => [
            blend_css_dodge(cb[0], cs[0]),
            blend_css_dodge(cb[1], cs[1]),
            blend_css_dodge(cb[2], cs[2]),
        ],
        BlendMode::ColorBurn => [
            blend_css_burn(cb[0], cs[0]),
            blend_css_burn(cb[1], cs[1]),
            blend_css_burn(cb[2], cs[2]),
        ],
        BlendMode::HardLight => [
            blend_css_hard_light(cb[0], cs[0]),
            blend_css_hard_light(cb[1], cs[1]),
            blend_css_hard_light(cb[2], cs[2]),
        ],
        BlendMode::SoftLight => [
            blend_css_soft_light(cb[0], cs[0]),
            blend_css_soft_light(cb[1], cs[1]),
            blend_css_soft_light(cb[2], cs[2]),
        ],
        BlendMode::Difference => [
            (cb[0] - cs[0]).abs(),
            (cb[1] - cs[1]).abs(),
            (cb[2] - cs[2]).abs(),
        ],
        BlendMode::Exclusion => [
            cb[0] + cs[0] - 2.0 * cb[0] * cs[0],
            cb[1] + cs[1] - 2.0 * cb[1] * cs[1],
            cb[2] + cs[2] - 2.0 * cb[2] * cs[2],
        ],
        BlendMode::Hue => {
            let mut c = blend_css_set_sat(cs, blend_css_sat(cb));
            blend_css_set_lum(&mut c, blend_css_lum(cb));
            c
        }
        BlendMode::Saturation => {
            let mut c = blend_css_set_sat(cb, blend_css_sat(cs));
            blend_css_set_lum(&mut c, blend_css_lum(cb));
            c
        }
        BlendMode::Color => {
            let mut c = cs;
            blend_css_set_lum(&mut c, blend_css_lum(cb));
            c
        }
        BlendMode::Luminosity => {
            let mut c = cb;
            blend_css_set_lum(&mut c, blend_css_lum(cs));
            c
        }
        // Normal 不走本函数（composite_layer 分流 src-over）；防御臂按源色
        BlendMode::Normal => cs,
    }
}

#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= 0.0 {
        return 0.0;
    }
    if cs >= 1.0 {
        return 1.0;
    }
    (cb / (1.0 - cs)).min(1.0)
}

#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 {
        return 1.0;
    }
    if cs <= 0.0 {
        return 0.0;
    }
    1.0 - ((1.0 - cb) / cs).min(1.0)
}

#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        blend_css_mul(cb, 2.0 * cs)
    } else {
        blend_css_scr(cb, 2.0 * cs - 1.0)
    }
}

/// soft-light 的 D(x) 分支（CSS 规范多项式 / sqrt 形态）。
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_soft_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
    } else {
        let d = if cb <= 0.25 {
            ((16.0 * cb - 12.0) * cb + 4.0) * cb
        } else {
            libm::sqrtf(cb)
        };
        cb + (2.0 * cs - 1.0) * (d - cb)
    }
}

#[inline]
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_mul(a: f32, b: f32) -> f32 {
    a * b
}

#[inline]
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_scr(a: f32, b: f32) -> f32 {
    a + b - a * b
}

/// 非可分离模式亮度（CSS 规范系数 0.3/0.59/0.11）。
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_sat(c: [f32; 3]) -> f32 {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    mx - mn
}

/// ClipColor：把色彩拉回 [0,1] 立方体（沿亮度轴缩放越界分量）。
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_clip(c: &mut [f32; 3]) {
    let l = blend_css_lum(*c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        let d = l - n;
        for v in c.iter_mut() {
            *v = l + (*v - l) * l / d;
        }
    }
    if x > 1.0 {
        let d = x - l;
        for v in c.iter_mut() {
            *v = l + (*v - l) * (1.0 - l) / d;
        }
    }
}

#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_set_lum(c: &mut [f32; 3], l: f32) {
    let d = l - blend_css_lum(*c);
    for v in c.iter_mut() {
        *v += d;
    }
    blend_css_clip(c);
}

/// SetSat：按分量大小排序后 min'=0、max'=s、mid' 线性内插（CSS 规范）。
/// 手写下标扫描（3 元素；slice sort_by 会单态化整套 driftsort——
/// AGENTS 体积教训，T1 预算不收这种 guests）。
#[cfg(any(feature = "css", feature = "filters"))]
fn blend_css_set_sat(mut c: [f32; 3], s: f32) -> [f32; 3] {
    let mut hi = 0usize;
    let mut lo = 0usize;
    for i in 1..3 {
        if c[i] > c[hi] {
            hi = i;
        }
        if c[i] < c[lo] {
            lo = i;
        }
    }
    // 三下标之和恒 3；全相等时 hi==lo，mid 取任意其余位（该分支三通道
    // 全清零，选哪个无差别）
    let mid = if hi != lo { 3 - hi - lo } else { (hi + 1) % 3 };
    if c[hi] > c[lo] {
        c[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        c[hi] = s;
    } else {
        c[mid] = 0.0;
        c[hi] = 0.0;
    }
    c[lo] = 0.0;
    c
}

/// 蒙版定义 → 每像素亮度因子（0..255）：内容渲染到离屏（透明底），
/// `factor = (0.2126R + 0.7152G + 0.0722B) × A / 255²`（直色 × alpha，
/// SVG mask 亮度语义：白=可见、黑=不可见、半透明按 alpha 折扣），
/// region 外恒 0。结果缓存进 `cache[idx]`。mask 内容自身携带的
/// clipPath/蒙版由递归 render 处理（深度由构建期上限保证有限）。
#[cfg(feature = "mask")]
fn ensure_mask_factor(dl: &DisplayList, idx: usize, vp: &Viewport, cache: &mut [Option<Vec<u8>>]) {
    if idx >= dl.masks.len() || cache[idx].is_some() {
        return;
    }
    let def: &MaskDef = &dl.masks[idx];
    let w = vp.width as usize;
    let h = vp.height as usize;
    let mut buf = alloc::vec![0u8; w * h * 4];
    render(&def.dl, vp, &mut buf);
    let mut fac = alloc::vec![0u8; w * h];
    for y in 0..h {
        let yc = y as f32 + 0.5;
        for x in 0..w {
            let xc = x as f32 + 0.5;
            // region 外（设备空间 AABB 判定）恒不可见
            if xc < def.region[0]
                || yc < def.region[1]
                || xc >= def.region[2]
                || yc >= def.region[3]
            {
                continue;
            }
            let o = (y * w + x) * 4;
            // 直色缓冲：rgb 为直色值、a 为覆盖率——白 0.35 不透明度
            // → lum=1.0 × a≈89 → 因子 89。
            // 车道ND：mask-type:alpha（def.alpha）跳过亮度项——因子 =
            // α 通道（Figma/Sketch 导出黑内容 + alpha 语义，按亮度折算
            // 恒 0 会整体吞掉被蒙内容；fd1 探针 Chrome 逐行实证）。
            let f = if def.alpha {
                buf[o + 3] as f32
            } else {
                (0.2126 * buf[o] as f32 + 0.7152 * buf[o + 1] as f32 + 0.0722 * buf[o + 2] as f32)
                    * buf[o + 3] as f32
                    / 255.0
            };
            fac[y * w + x] = libm::roundf(f.clamp(0.0, 255.0)) as u8;
        }
    }
    cache[idx] = Some(fac);
}

/// pattern 瓦片位图（ensure_pattern_tile 的缓存项）：瓦片内容子显示
/// 列表在 ceil(tw·sx·ss)×ceil(th·sy·ss) 视口下的离屏渲染结果（透明底，
/// ss = pattern_supersample，轴对齐瓦片 ss=1 与旧口径逐字节一致）。
#[derive(Clone)]
#[cfg(feature = "pattern")]
struct TileBmp {
    px: Vec<u8>,
    w: u32,
    h: u32,
}

/// texel 预乘值 (r·a/255, g·a/255, b·a/255, a)——车道ED 分数窗角点取值。
/// 注意不门 pattern：PatCtx::at_boxed 在无 pattern 特性构建下仍编译。
#[inline(always)]
fn texel_premul(bmp: &[u8], o: usize) -> (f32, f32, f32, f32) {
    let a = bmp[o + 3] as f32;
    let w = a / 255.0;
    (
        bmp[o] as f32 * w,
        bmp[o + 1] as f32 * w,
        bmp[o + 2] as f32 * w,
        a,
    )
}

/// 车道AV：旋转/斜切 pattern 瓦片的采样超采样倍数（唯一事实源——
/// builder 烘焙内容变换与 raster 位图尺寸/采样必须取同一值）。
/// 采样网格与位图网格平行（inv 非对角耦合 ≈0）时 texel 中心最近邻
/// 采样已精确，返回 1（原路径零改动）；否则从 8 逐级降档到 2，直至
/// SS× 位图尺寸守卫（2048² / 4M 像素，与单瓦片钳制同限）通过；全超
/// 限回退 1（最近邻，行为同旧版）。动机：s4-03 rotate(30) 瓦片在
/// 最近邻下产生二值锯齿边（Chrome 为解析覆盖平滑边，区域 >32LSB
/// 差 924px），探针 tests/oracle/repros/av1-pattern-rotate30-tile.svg。
#[cfg(feature = "pattern")]
pub(crate) fn pattern_supersample(tw: f32, th: f32, sx: f32, sy: f32, inv: &[f32; 6]) -> u32 {
    if !(inv[1].is_finite() && inv[2].is_finite()) {
        return 1;
    }
    if inv[1].abs() < 1e-4 && inv[2].abs() < 1e-4 {
        // 车道BF：轴对齐且设备周期为整数（texel↔device 1:1 相位对齐）
        // → 保持 1（最近邻精确，旧口径逐字节不变：s4-03、paint-context-
        // 004 等整数相位全量）。非整数周期（如 oBB 锚定经 1.2× 视口
        // 缩放 → 38.4px 瓦片，paint-context-008）最近邻采样逐格量化
        // 抖动 ±0.5px（边缘 hard-step vs Chrome 解析平滑）→ 落入下方
        // SS 降档盒式滤波 ≈ 设备像素足迹面积覆盖。
        let jx = tw * sx - libm::floorf(tw * sx + 0.5);
        let jy = th * sy - libm::floorf(th * sy + 0.5);
        if jx.abs() < 1e-3 && jy.abs() < 1e-3 {
            return 1;
        }
    }
    for ss in [8u32, 4, 2] {
        let s = ss as f32;
        let w = libm::ceilf(tw * sx * s);
        let h = libm::ceilf(th * sy * s);
        if w >= 1.0 && h >= 1.0 && w <= 2048.0 && h <= 2048.0 && w * h <= 4_000_000.0 {
            return ss;
        }
    }
    1
}

/// pattern 采样上下文：瓦片定义 + 离屏位图。逐像素把设备坐标经 F⁻¹
/// 映回瓦片空间、对 (tw,th) 取模后采样位图；ss>1（旋转/斜切瓦片）
/// 时改为设备像素足迹 SS×SS texel 的预乘盒式滤波采样。
struct PatCtx<'a> {
    tile: &'a PatternTile,
    bmp: &'a [u8],
    bw: u32,
    bh: u32,
    ss: u32,
}

impl PatCtx<'_> {
    /// 设备坐标（像素中心）→ 瓦片取模采样色（瓦片外区域透明 → a=0）。
    fn at(&self, x: f32, y: f32) -> Rgba8 {
        let t = self.tile;
        let tx = t.inv[0] * x + t.inv[2] * y + t.inv[4];
        let ty = t.inv[1] * x + t.inv[3] * y + t.inv[5];
        if !(tx.is_finite() && ty.is_finite()) {
            return Rgba8 {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            };
        }
        // floor 取模：负坐标映回 [0,tw)×[0,th)
        let u = tx - libm::floorf(tx / t.tw) * t.tw;
        let v = ty - libm::floorf(ty / t.th) * t.th;
        if self.ss > 1 {
            return self.at_boxed(u, v);
        }
        // u∈[0,tw) → u·sx∈[0,tw·sx)⊆[0,bw)；浮点边界钳到 bw-1
        let bi = ((u * t.sx) as u32).min(self.bw - 1);
        let bj = ((v * t.sy) as u32).min(self.bh - 1);
        let o = ((bj * self.bw + bi) * 4) as usize;
        Rgba8 {
            r: self.bmp[o],
            g: self.bmp[o + 1],
            b: self.bmp[o + 2],
            a: self.bmp[o + 3],
        }
    }

    /// 车道AV：旋转瓦片的盒式滤波采样。设备像素在瓦片空间的足迹
    /// （1 设备像素 = SS×SS texel）内做预乘空间平均再转直 alpha——
    /// 直 alpha 空间平均会在透明邻接 texel 上产生暗边（rgb 被拖向 0
    /// 且 alpha 再衰减，双重变暗）。近似解析覆盖（Chrome/Skia 对旋转
    /// pattern 的行为——车道ED 法证：Chrome 对整数瓦片尺寸输出与逐点
    /// 解析覆盖一致到 ±1LSB，见 tests/oracle/reports/near-axis-ed.md），
    /// 消除最近邻的二值量化锯齿。
    ///
    /// 车道ED：分数窗。足迹 [fx−s/2, fx+s/2)×[fy−s/2, fy+s/2)（texel
    /// 单位）的精确积分 = 边界 texel 按 texel 内分数 (1−f)/f 加权，共
    /// (n+1)² texel。旧整数窗（floor 吸附 + 全权 n×n）把足迹位置量化到
    /// 1/ss 设备像素：旋转 pattern 相位随扫描线连续游走，输出呈 ±0.5
    /// texel 台阶锯齿（da1/ed 矩阵：全角度 >32LSB ~1260px/max 57，
    /// 90° 足迹恰对齐 texel 网格才免病）。
    ///
    /// 实现：分数窗 = 相邻整窗的线性插值（texel 值窗内分段常量），按
    /// 行/列带分解为（W=V+fa(R−L) 按行、再按列消去，V=整窗行段）：
    ///   真 = S + fa·(colR−colL) + fb·(rowB−rowT)
    ///        + fa·fb·(BR − BL − TR + TL)
    /// 其中 S = [i0,i0+n)×[j0,j0+n) 整窗和（车道DA 优化内圈原样保留），
    /// colL/colR = 列 i0 / i0+n（行 j0..j0+n−1）列和，rowT/rowB = 行
    /// j0 / j0+n（列 i0..i0+n−1）行和，T*/B* = 四角 texel。全部紧致
    /// 短循环、逐 texel 零权重乘法零分支（distinct 81 texel）。
    fn at_boxed(&self, u: f32, v: f32) -> Rgba8 {
        let t = self.tile;
        let s = self.ss as f32;
        let n = self.ss as i32;
        // 足迹中心 = 采样点（位图 texel 网格 = 设备像素网格 × SS）。
        // SNAP：F⁻¹ 的浮点噪声（如 rotate(90) 的 cos≈-4.4e-8 乘以设备
        // 坐标）会让理论整数值的相位带上 ~1e-6 偏差。分数窗插值本身
        // 连续（无 floor 悬崖），SNAP 只负责把噪声带内的近整数相位钳回
        // 精确整窗（fa 钳 0/1，不混入相邻窗的 1e-3 级污染）。
        const SNAP: f32 = 1e-3;
        let fx = u * t.sx * s;
        let fy = v * t.sy * s;
        let i0 = libm::floorf(fx - s * 0.5 + SNAP) as i32;
        let j0 = libm::floorf(fy - s * 0.5 + SNAP) as i32;
        let fa = (fx - s * 0.5 - i0 as f32).clamp(0.0, 1.0);
        let fb = (fy - s * 0.5 - j0 as f32).clamp(0.0, 1.0);
        let bw = self.bw as i32;
        let bh = self.bh as i32;
        // 快路界：分数窗最右达 i0+n 列/最下达 j0+n 行；越界相位走慢路。
        let fast = i0 >= 0 && i0 + n < bw && j0 >= 0 && j0 + n < bh;
        let (mut pr, mut pg, mut pb, mut pa) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        if fast {
            // 主整窗 S：车道DA 优化内圈（行基 + 顺序访存），逐 texel
            // 无权重无分支。
            for dj in 0..n {
                let row = ((j0 + dj) as u32) * self.bw;
                let mut o = ((row + i0 as u32) * 4) as usize;
                for _di in 0..n {
                    let a = self.bmp[o + 3] as f32;
                    let w = a / 255.0;
                    pr += self.bmp[o] as f32 * w;
                    pg += self.bmp[o + 1] as f32 * w;
                    pb += self.bmp[o + 2] as f32 * w;
                    pa += a;
                    o += 4;
                }
            }
            let stride = (self.bw * 4) as usize;
            // 列带 colL/colR（行 j0..j0+n−1）
            let (mut lr, mut lg, mut lb, mut la) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let (mut rr, mut rg, mut rb, mut ra) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let mut ol = (((j0 as u32) * self.bw + i0 as u32) * 4) as usize;
            let mut orr = ol + (n as usize) * 4;
            for _dj in 0..n {
                let a = self.bmp[ol + 3] as f32;
                let w = a / 255.0;
                lr += self.bmp[ol] as f32 * w;
                lg += self.bmp[ol + 1] as f32 * w;
                lb += self.bmp[ol + 2] as f32 * w;
                la += a;
                let a = self.bmp[orr + 3] as f32;
                let w = a / 255.0;
                rr += self.bmp[orr] as f32 * w;
                rg += self.bmp[orr + 1] as f32 * w;
                rb += self.bmp[orr + 2] as f32 * w;
                ra += a;
                ol += stride;
                orr += stride;
            }
            // 行带 rowT/rowB（列 i0..i0+n−1）
            let (mut tr, mut tg, mut tb, mut ta) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let (mut br2, mut bg2, mut bb2, mut ba2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            let mut ot = (((j0 as u32) * self.bw + i0 as u32) * 4) as usize;
            let mut ob = (((j0 + n) as u32) * self.bw + i0 as u32) as usize * 4;
            for _di in 0..n {
                let a = self.bmp[ot + 3] as f32;
                let w = a / 255.0;
                tr += self.bmp[ot] as f32 * w;
                tg += self.bmp[ot + 1] as f32 * w;
                tb += self.bmp[ot + 2] as f32 * w;
                ta += a;
                let a = self.bmp[ob + 3] as f32;
                let w = a / 255.0;
                br2 += self.bmp[ob] as f32 * w;
                bg2 += self.bmp[ob + 1] as f32 * w;
                bb2 += self.bmp[ob + 2] as f32 * w;
                ba2 += a;
                ot += 4;
                ob += 4;
            }
            // 四角（TL/TR 已在列带起点、BL/BR 在行带端点，直接取值）
            let tl = texel_premul(self.bmp, (((j0 as u32) * self.bw + i0 as u32) * 4) as usize);
            let trc = texel_premul(
                self.bmp,
                (((j0 as u32) * self.bw + (i0 + n) as u32) * 4) as usize,
            );
            let bl = texel_premul(
                self.bmp,
                ((((j0 + n) as u32) * self.bw + i0 as u32) * 4) as usize,
            );
            let brc = texel_premul(
                self.bmp,
                ((((j0 + n) as u32) * self.bw + (i0 + n) as u32) * 4) as usize,
            );
            // 分数插值（逐通道同式；pa 为覆盖率权，与预乘 rgb 同形）
            pr += fa * (rr - lr) + fb * (br2 - tr) + fa * fb * (brc.0 - bl.0 - trc.0 + tl.0);
            pg += fa * (rg - lg) + fb * (bg2 - tg) + fa * fb * (brc.1 - bl.1 - trc.1 + tl.1);
            pb += fa * (rb - lb) + fb * (bb2 - tb) + fa * fb * (brc.2 - bl.2 - trc.2 + tl.2);
            pa += fa * (ra - la) + fb * (ba2 - ta) + fa * fb * (brc.3 - bl.3 - trc.3 + tl.3);
        } else {
            // 慢路（越界相位/极小瓦片）：逐 texel 回卷 + 边界权重，
            // (n+1)² texel 直接求和——正确性优先，性能不敏感。
            for dj in 0..=n {
                let wy = if dj == 0 {
                    1.0 - fb
                } else if dj == n {
                    fb
                } else {
                    1.0
                };
                let mut jj = j0 + dj;
                while jj < 0 {
                    jj += bh;
                }
                while jj >= bh {
                    jj -= bh;
                }
                let row = (jj as u32) * self.bw;
                for di in 0..=n {
                    let wx = if di == 0 {
                        1.0 - fa
                    } else if di == n {
                        fa
                    } else {
                        1.0
                    };
                    let mut ii = i0 + di;
                    while ii < 0 {
                        ii += bw;
                    }
                    while ii >= bw {
                        ii -= bw;
                    }
                    let o = ((row + ii as u32) * 4) as usize;
                    let wt = wx * wy;
                    let a = self.bmp[o + 3] as f32;
                    let w = a / 255.0 * wt;
                    pr += self.bmp[o] as f32 * w;
                    pg += self.bmp[o + 1] as f32 * w;
                    pb += self.bmp[o + 2] as f32 * w;
                    pa += a * wt;
                }
            }
        }
        let cnt = (n * n) as f32;
        let a = pa / cnt;
        if a < 0.5 {
            return Rgba8 {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            };
        }
        // 预乘 → 直 alpha：rgb = premul / (a/255)
        let af = a / 255.0;
        Rgba8 {
            r: libm::roundf(pr / cnt / af).clamp(0.0, 255.0) as u8,
            g: libm::roundf(pg / cnt / af).clamp(0.0, 255.0) as u8,
            b: libm::roundf(pb / cnt / af).clamp(0.0, 255.0) as u8,
            a: libm::roundf(a).clamp(0.0, 255.0) as u8,
        }
    }
}

/// 瓦片定义 → 离屏位图：内容子显示列表渲染到 ceil(tw·sx·ss)×
/// ceil(th·sy·ss) 视口（与主视口同 aa 模式；内容坐标系已按同口径
/// SS 烘焙到位图像素坐标，见 build_pattern_paint）。结果缓存进
/// `cache[idx]`。尺寸与构建期同式重导并防御性钳制
/// （手写 DL 不信任任何字段）。
#[cfg(feature = "pattern")]
fn ensure_pattern_tile(dl: &DisplayList, idx: usize, vp: &Viewport, cache: &mut [Option<TileBmp>]) {
    if idx >= dl.patterns.len() || cache[idx].is_some() {
        return;
    }
    let def: &PatternTile = &dl.patterns[idx];
    if !(def.tw.is_finite() && def.th.is_finite() && def.tw > 0.0 && def.th > 0.0) {
        return;
    }
    let ss = pattern_supersample(def.tw, def.th, def.sx, def.sy, &def.inv) as f32;
    let w = libm::ceilf(def.tw * def.sx * ss).clamp(1.0, 2048.0) as u32;
    let h = libm::ceilf(def.th * def.sy * ss).clamp(1.0, 2048.0) as u32;
    let tvp = Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa: vp.aa,
    };
    let mut px = alloc::vec![0u8; w as usize * h as usize * 4];
    render(&def.dl, &tvp, &mut px);
    cache[idx] = Some(TileBmp { px, w, h });
}

// ---------------------------------------------------------------------------
// M8：滤镜链求值（SourceGraphic 离屏 → primitive 逐个变换 → 区域合成）
// ---------------------------------------------------------------------------

/// 滤镜求值结果（ensure_filter_output 的缓存项）：末链输出位图（区域
/// 局部坐标）+ 区域原点。
#[derive(Clone)]
#[cfg(feature = "filters")]
struct FilterBmp {
    px: Vec<u8>,
    ox: i32,
    oy: i32,
    w: u32,
    h: u32,
}

/// 滤镜定义 → 求值位图（缓存）。子显示列表在区域尺寸离屏视口渲染
/// （内容已烘焙到区域局部坐标），按 primitive 链逐个变换；末链输出
/// 为最终结果（空链 = 原图直通）。病态尺寸（手写 DL 不信任字段）
/// 钳制到空位图。
#[cfg(feature = "filters")]
fn ensure_filter_output(
    dl: &DisplayList,
    idx: usize,
    vp: &Viewport,
    cache: &mut [Option<FilterBmp>],
) {
    if idx >= dl.filters.len() || cache[idx].is_some() {
        return;
    }
    let def: &FilterDef = &dl.filters[idx];
    let w = def.w.min(2048) as usize;
    let h = def.h.min(2048) as usize;
    let empty = FilterBmp {
        px: Vec::new(),
        ox: def.ox,
        oy: def.oy,
        w: 0,
        h: 0,
    };
    if w == 0 || h == 0 || w * h > 4_000_000 {
        cache[idx] = Some(empty);
        return;
    }
    let fvp = Viewport {
        width: w as u32,
        height: h as u32,
        format: PixFmt::Rgba8,
        aa: vp.aa,
    };
    let mut src = alloc::vec![0u8; w * h * 4];
    render(&def.dl, &fvp, &mut src);
    // 车道X：滤镜工作色域（color-interpolation-filters 继承解析产物）
    // 整链统一传入——linear 走 EOTF 查表路径，sRGB 恒等直算。
    let out = run_fe_chain(
        &def.prims,
        src,
        w,
        h,
        vp.aa,
        FeColorCtx { linear: def.linear },
    );
    cache[idx] = Some(FilterBmp {
        px: out,
        ox: def.ox,
        oy: def.oy,
        w: w as u32,
        h: h as u32,
    });
}

/// primitive 链求值：输入解析（Prev/SourceGraphic/SourceAlpha/命名
/// 输出）→ 变换 → 推进；末链输出即结果（空链原图直通）。未解析的
/// 命名输入按全透明位图处理（构建期宽容约定）。`aa` = 宿主视口
/// 抗锯齿模式（feImage 子列表离屏渲染沿用，与 SourceGraphic 同源）。
/// `cs` = 滤镜工作色域上下文（车道X，整链统一）。
#[cfg(feature = "filters")]
fn run_fe_chain(
    prims: &[crate::dl::FePrim],
    src: Vec<u8>,
    w: usize,
    h: usize,
    aa: u8,
    cs: FeColorCtx,
) -> Vec<u8> {
    if prims.is_empty() {
        return src;
    }
    let mut outputs: Vec<Vec<u8>> = Vec::with_capacity(prims.len());
    let empty = alloc::vec![0u8; w * h * 4];
    // 车道DI 防御：输入缓冲长度必须与区域一致（w·h·4），短缓冲一律以
    // 全透明位图顶替——来源包括 need_alpha 预计算漏臂、未产出的命名
    // 输出等；primitive 级降级为不贡献，绝不越界 panic（与 Merge 臂的
    // 长度防御、「手写 DL 不信任字段」同口径）。
    fn sized<'a>(b: &'a [u8], empty: &'a [u8], w: usize, h: usize) -> &'a [u8] {
        if b.len() >= w * h * 4 {
            b
        } else {
            empty
        }
    }
    // SourceAlpha 按需预计算（链上任何 in=/in2= 引用才生成）。车道DI：
    // in2 预计算臂必须覆盖全部带 in2 的 FeKind（Composite /
    // DisplacementMap / **Blend**）——漏臂时 alpha 恒空 Vec，下游
    // blend_fe 索引越界 panic（fuzz 二轮实证：feBlend in2=SourceAlpha）。
    let need_alpha = prims.iter().any(|p| {
        p.in_ == FeInput::SourceAlpha
            || matches!(&p.kind, FeKind::Composite { in2, .. } if *in2 == FeInput::SourceAlpha)
            || matches!(&p.kind, FeKind::Blend { in2, .. } if *in2 == FeInput::SourceAlpha)
            || matches!(&p.kind, FeKind::DisplacementMap { in2, .. } if *in2 == FeInput::SourceAlpha)
            || matches!(&p.kind, FeKind::Merge { nodes } if nodes.contains(&FeInput::SourceAlpha))
    });
    let alpha: Vec<u8> = if need_alpha {
        source_alpha(&src, w, h)
    } else {
        Vec::new()
    };
    for (pi, p) in prims.iter().enumerate() {
        let input = sized(
            resolve_fe_input(p.in_, &src, &outputs, &alpha, &empty),
            &empty,
            w,
            h,
        );
        let mut out = match &p.kind {
            FeKind::GaussianBlur { sx, sy } => blur_rgba(input, w, h, *sx, *sy, cs),
            FeKind::DropShadow {
                dx,
                dy,
                sx,
                sy,
                color,
            } => drop_shadow(input, w, h, *dx, *dy, *sx, *sy, *color),
            FeKind::ColorMatrix { m } => color_matrix(input, w, h, m, cs),
            FeKind::Offset { dx, dy } => offset_bitmap(input, w, h, *dx, *dy),
            FeKind::Flood { color } => flood_bitmap(w, h, *color),
            FeKind::Composite { in2, op } => {
                let d = sized(
                    resolve_fe_input(*in2, &src, &outputs, &alpha, &empty),
                    &empty,
                    w,
                    h,
                );
                composite_pd(input, d, w, h, *op, cs)
            }
            FeKind::Blend { in2, mode } => {
                let bd = sized(
                    resolve_fe_input(*in2, &src, &outputs, &alpha, &empty),
                    &empty,
                    w,
                    h,
                );
                blend_fe(input, bd, w, h, *mode, cs)
            }
            FeKind::ComponentTransfer { fns } => component_transfer(input, w, h, fns, cs),
            // feTile（车道CN）：规范语义 = 「in 输入裁到**输入 primitive**
            // 的子区域，作为瓦片单元平铺填满 feTile 自身子区域」。瓦片单
            // 元取输入的 sub（此前近似取自身 sub——filters-tile-01-b 中
            // feTile 无显式子区域、feOffset 有，两者不一致 → 平铺全失）。
            // builder 存的 src 槽保留但不再消费（见 dl.rs Tile 注释）。
            FeKind::Tile { src: _ } => {
                let cell_sub = match p.in_ {
                    FeInput::Prev => pi.checked_sub(1).map(|i| prims[i].sub),
                    FeInput::Output(j) => prims.get(j as usize).map(|q| q.sub),
                    _ => None,
                };
                let cell = cell_sub.unwrap_or(p.sub);
                tile_bitmap(input, w, h, &cell)
            }
            // feTurbulence（车道AQ）：噪声场不消费 in（输入被整体替换）。
            FeKind::Turbulence {
                ty,
                bf,
                octaves,
                seed,
                stitch,
                origin,
            } => turbulence_bitmap(w, h, *ty, *bf, *octaves, *seed, *stitch, *origin, p.sub, cs),
            // feDisplacementMap（车道AQ）：in2 通道折算位移，in 重采样。
            FeKind::DisplacementMap {
                in2,
                scale_x,
                scale_y,
                xch,
                ych,
            } => {
                let map = sized(
                    resolve_fe_input(*in2, &src, &outputs, &alpha, &empty),
                    &empty,
                    w,
                    h,
                );
                displacement_map(input, map, w, h, *scale_x, *scale_y, *xch, *ych, cs)
            }
            // feConvolveMatrix / feMorphology（车道CN）。
            FeKind::ConvolveMatrix {
                kernel,
                ox,
                oy,
                divisor,
                bias,
                tx,
                ty,
                edge,
                preserve_alpha,
            } => convolve_matrix(
                input,
                w,
                h,
                kernel,
                *ox,
                *oy,
                *divisor,
                *bias,
                *tx,
                *ty,
                *edge,
                *preserve_alpha,
                cs,
            ),
            FeKind::Morphology { dilate, rx, ry } => {
                morphology_bitmap(input, w, h, *dilate, *rx, *ry)
            }
            // feLighting 双 primitive（车道AQ）：alpha 高度图 + 光照核。
            FeKind::DiffuseLighting {
                light,
                surface_scale,
                kd,
                color,
            } => lighting_diffuse(input, w, h, *light, *surface_scale, *kd, *color, cs),
            FeKind::SpecularLighting {
                light,
                surface_scale,
                ks,
                exp,
                color,
            } => lighting_specular(input, w, h, *light, *surface_scale, *ks, *exp, *color, cs),
            // feImage：内部 #id 引用的目标元素子列表（构建期已烘焙到
            // 区域局部坐标）离屏渲染到区域大小位图——与 SourceGraphic
            // 同机制（ensure_filter_output 的 render 路径）。规范语义
            // feImage 不消费 in（输入被整体替换）。外链/未知 id 的全
            // 透明占位由构建期 Flood a=0 表达，不走此臂。
            FeKind::Image {
                dl,
                bitmap_as_linear,
            } => {
                let ivp = Viewport {
                    width: w as u32,
                    height: h as u32,
                    format: PixFmt::Rgba8,
                    aa,
                };
                let mut px = alloc::vec![0u8; w * h * 4];
                render(dl, &ivp, &mut px);
                // 车道CN：Chrome（Skia feImage）不把位图做 sRGB→linear
                // 解码就注入 linear 工作域滤镜缓冲——位图字节被当作
                // linear 值，链输出再统一 linear→sRGB 提亮。字节口径 =
                // out = to_srgb(byte/255)·255（displace-01 tile2 逐位
                // 实证：源 127 → 预言机 187；本式 = 187）。sRGB 域滤镜
                // 下位图直通恒等。alpha 无 gamma 不动。
                // 车道DB 细化：bitmap_as_linear 仅对**无 gAMA 标记**位图
                // 为真（Chrome 视作 linear 原样注入）；gAMA 管理位图由
                // builder 解码为 srgb_enc(x^(1/g)) 工作域编码、跳过本
                // LUT——Chrome 对 tagged 位图做色域管理到工作域，再套
                // LUT = 双重提亮（displace-01 g=1.0 贴图 189 vs 130）。
                if cs.linear && *bitmap_as_linear {
                    let lut = fe_img_as_linear_lut();
                    for i in 0..w * h {
                        let o = i * 4;
                        px[o] = lut[px[o] as usize];
                        px[o + 1] = lut[px[o + 1] as usize];
                        px[o + 2] = lut[px[o + 2] as usize];
                    }
                }
                px
            }
            FeKind::Merge { nodes } => {
                // 逐节点 src-over 叠层（文档序，后者在上）。**工作色域
                // 直色合成**（车道AQ）：Chrome/Skia 滤镜合成在 premul-
                // linear 域（车道U a1 实证），旧版字节域 blend_px 折叠在
                // 半透明叠层上 gamma 错（ltg-03 实证：高光 g 112 vs 148）。
                // 节点输入按链约定取直色字节（SourceGraphic 的预乘存储
                // 沿用旧版同一近似），累加器保持直色。
                let s2l_m = srgb_to_linear_table();
                let mut acc = alloc::vec![0u8; w * h * 4];
                for &n in nodes {
                    let src_px = resolve_fe_input(n, &src, &outputs, &alpha, &empty);
                    if src_px.len() != acc.len() {
                        continue; // 防御长度不齐（手写 DL）
                    }
                    for i in 0..w * h {
                        let o = i * 4;
                        let sa = f32::from(src_px[o + 3]) / 255.0;
                        if sa <= 0.0 {
                            continue;
                        }
                        let da = f32::from(acc[o + 3]) / 255.0;
                        let oa = sa + da * (1.0 - sa);
                        for ch in 0..3 {
                            let sc = cs.to_working(&s2l_m, src_px[o + ch]);
                            let dc = cs.to_working(&s2l_m, acc[o + ch]);
                            let pm = sc * sa + dc * da * (1.0 - sa);
                            acc[o + ch] = cs.to_byte(if oa > 0.0 { pm / oa } else { 0.0 });
                        }
                        acc[o + 3] = (oa * 255.0 + 0.5) as u8;
                    }
                }
                acc
            }
        };
        // 车道CN：primitive 输出统一裁到自身子区域（SVG1.1「各 primitive
        // 的作用域限于其 x/y/width/height 子区域，缺省 = 整个滤镜区域」）。
        // 此前除 Turbulence 外全部忽略——feFlood 曾铺满整个区域
        //（filters-tile-01-b 全红根因）。缺省 sub = 全区域 → 裁切恒等，
        // 存量语料字节零漂移。
        clip_subregion(&mut out, p.sub, w, h);
        outputs.push(out);
    }
    match outputs.into_iter().last() {
        Some(o) => o,
        None => src,
    }
}

/// 位图裁到子区域矩形 [x0,y0,x1,y1]（区域局部像素）：界外清透明。
#[cfg(feature = "filters")]
fn clip_subregion(out: &mut [u8], sub: [u32; 4], w: usize, h: usize) {
    let (x0, y0, x1, y1) = (
        sub[0] as usize,
        sub[1] as usize,
        (sub[2] as usize).min(w),
        (sub[3] as usize).min(h),
    );
    if x0 >= x1 || y0 >= y1 || (x0 == 0 && y0 == 0 && x1 == w && y1 == h) {
        return; // 空/全区域：恒等
    }
    for y in 0..h {
        for x in 0..w {
            if x < x0 || x >= x1 || y < y0 || y >= y1 {
                let o = (y * w + x) * 4;
                out[o] = 0;
                out[o + 1] = 0;
                out[o + 2] = 0;
                out[o + 3] = 0;
            }
        }
    }
}

/// 输入枚举 → 位图切片（Prev = 上一输出，链首 = SourceGraphic）。
#[cfg(feature = "filters")]
fn resolve_fe_input<'a>(
    i: FeInput,
    src: &'a [u8],
    outputs: &'a [Vec<u8>],
    alpha: &'a [u8],
    empty: &'a [u8],
) -> &'a [u8] {
    match i {
        FeInput::Prev => outputs.last().map_or(src, |o| o.as_slice()),
        FeInput::SourceGraphic => src,
        FeInput::SourceAlpha => alpha,
        FeInput::Output(j) => outputs.get(j as usize).map_or(empty, |o| o.as_slice()),
    }
}

/// SourceGraphic → SourceAlpha（RGB=0、alpha 原样，规范语义）。
#[cfg(feature = "filters")]
fn source_alpha(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    for i in 0..w * h {
        out[i * 4 + 3] = src[i * 4 + 3];
    }
    out
}

/// σ（设备像素）→ 盒模糊通道序列（W3C filter-effects 附录算法，
/// Chrome/Skia 同款）：`d = floor(σ·3√(2π)/4 + 0.5)`；d 奇 = 3 个宽 d
/// 的居中盒；d 偶 = 2 个宽 d 的半像素偏置盒（中心落在像素边界）+
/// 1 个宽 d+1 的居中盒。返回每通道 (start, width)：窗口覆盖输入
/// `[x+start, x+start+width)`。空序列 = 无模糊（σ≈0）。
///
/// 依据：ARM Chromium 预言机（chrome-headless-shell 148）实测——
/// σ=0 源位图施加 box d=9×3 后与 σ=5 输出逐像素吻合（mean |Δα|
/// 0.018、max 1，纯 8bit 舍入）；精确高斯核（erf）mean 1.66、
/// /Applications 的较新 Chromium 才与精确高斯吻合（Skia 新 blur
/// engine，非冻结预言机）——**别换**。
#[cfg(feature = "filters")]
fn sigma_box_passes(sigma: f32) -> Vec<(i32, u32)> {
    if !sigma.is_finite() || sigma <= 0.0 {
        return Vec::new();
    }
    // 3·√(2π)/4 ≈ 1.87997（σ→盒宽换算常数，规范原文值）
    let d = libm::floorf(sigma * 1.8799712 + 0.5) as i32;
    if d <= 0 || d > 4096 {
        return Vec::new();
    }
    if d % 2 == 1 {
        // 奇宽居中盒：taps [−(d−1)/2, (d−1)/2]
        let r = d / 2;
        alloc::vec![(-r, d as u32), (-r, d as u32), (-r, d as u32)]
    } else {
        // 偶宽：中心落在像素边界（±0.5）→ 整数 taps
        // [−d/2, d/2−1] 与镜像 [−d/2+1, d/2]；补一个奇宽 d+1 居中盒
        let r = d / 2;
        alloc::vec![(-r, d as u32), (-r + 1, d as u32), (-r, (d + 1) as u32)]
    }
}

/// feGaussianBlur：直色 → 预乘（**P 通道按工作域**——linearRGB 缺省 =
/// EOTF 查表 `s2l[c]·a/255`（车道GA 冻结探针 fg-blur-step 逐位验证：
/// Chrome 148 = 同款 W3C 三盒序列 + linearRGB 工作域，σ=2 偶宽盒全列
/// byte 级吻合、σ=10 主体 ±1 LSB；旧 sRGB 域模糊的 50% 越渡点在线性
/// 域映回后呈 −0.78σ「相位」位移，即车道FG 登记的模型分歧根因）；
/// sRGB 声明 = c·a 恒等原式）→ 每轴按规范盒序列滑窗 → 逆 EOTF 回
/// sRGB。alpha 无 gamma：A 通道两域同式 a·255。盒外按透明零填充
/// （能量守恒，色雾不越区域）。
#[cfg(feature = "filters")]
fn blur_rgba(input: &[u8], w: usize, h: usize, sx: f32, sy: f32, cs: FeColorCtx) -> Vec<u8> {
    let px = sigma_box_passes(sx);
    let py = sigma_box_passes(sy);
    if px.is_empty() && py.is_empty() {
        return input.to_vec();
    }
    let s2l = srgb_to_linear_table();
    // 车道NO：常量除数魔数化（255/510，BoxDiv 精确口径同证明）——
    // 与原 u32 除法逐位同值，rv32 上除法是子程序调用。
    let div255 = BoxDiv::new(255);
    // 线性域逆 EOTF 阈值表：区域够大才建（小区域直算 powf 更快；
    // 两路口径逐字节全等——构造即以原函数为神谕，见 lin2srgb_thresholds）。
    let lin_lut = if cs.linear && w * h >= LIN_LUT_MIN_PX {
        Some(lin2srgb_thresholds())
    } else {
        None
    };
    // 直色 → 预乘 u16×4（线性域：P = round(s2l[c]·a/255)、A = a·255；
    // sRGB 域：P = c·a；值域 0..=65025/65535）
    let mut cur = alloc::vec![0u16; w * h * 4];
    premul_direct_u16(input, cur.as_mut_slice(), cs, &s2l, &div255);
    // 车道NO 第二刀（大 σ 降采样，Skia 同思路）：双轴 σ ≥ DS_SIGMA_MIN
    // 且区域 ≥ DS_MIN_SIDE 才启用——2×2 盒均值降到 1/2 分辨率（预乘域，
    // 能量守恒）、按 σ/2 走同款盒序列、双线性升回全分辨率后统一逆预乘。
    // 必然改变输出字节（≠ 规范全分辨率口径），启用阈值/精度证据链见
    // tests/oracle/reports/blur-perf-no.md。
    if sx >= DS_SIGMA_MIN && sy >= DS_SIGMA_MIN && w >= DS_MIN_SIDE && h >= DS_MIN_SIDE {
        let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
        let small = downsample2_premul(&cur, w, h, w2, h2);
        let mut cur2 = small;
        let mut nxt2 = alloc::vec![0u16; w2 * h2 * 4];
        for (start, width, horiz) in sigma_box_passes(sx * 0.5)
            .iter()
            .map(|&(s, wd)| (s, wd, true))
            .chain(
                sigma_box_passes(sy * 0.5)
                    .iter()
                    .map(|&(s, wd)| (s, wd, false)),
            )
        {
            box_blur_axis(&cur2, &mut nxt2, w2, h2, start, width, horiz);
            core::mem::swap(&mut cur2, &mut nxt2);
        }
        drop(nxt2);
        // 双线性升采样回全分辨率（预乘域）→ 逆预乘直色
        let mut up = alloc::vec![0u16; w * h * 4];
        upsample_bilinear_premul(&cur2, w2, h2, up.as_mut_slice(), w, h);
        return unpremul_to_direct(&up, w * h, cs, &div255, lin_lut.as_ref());
    }
    let mut nxt = alloc::vec![0u16; w * h * 4];
    for (start, width, horiz) in px
        .iter()
        .map(|&(s, wd)| (s, wd, true))
        .chain(py.iter().map(|&(s, wd)| (s, wd, false)))
    {
        box_blur_axis(&cur, &mut nxt, w, h, start, width, horiz);
        core::mem::swap(&mut cur, &mut nxt);
    }
    unpremul_to_direct(&cur, w * h, cs, &div255, lin_lut.as_ref())
}

/// 预乘直色 u8 → 预乘 u16×4（线性域 P = round(s2l[c]·a/255)、A = a·255；
/// sRGB 域 P = c·a）。车道NO 自 blur_rgba 抽出。
#[cfg(feature = "filters")]
fn premul_direct_u16(
    input: &[u8],
    out: &mut [u16],
    cs: FeColorCtx,
    s2l: &[u16; 256],
    div255: &BoxDiv,
) {
    for i in 0..input.len() / 4 {
        let a = input[i * 4 + 3] as u32;
        for ch in 0..3 {
            let c = input[i * 4 + ch] as usize;
            let p = if cs.linear {
                div255.round(s2l[c] as u32 * a + 127)
            } else {
                c as u32 * a
            };
            out[i * 4 + ch] = p as u16;
        }
        out[i * 4 + 3] = (a * 255) as u16;
    }
}

/// 预乘 u16×4 → 直色 u8：a = round(A'/255)；颜色 = 逆 EOTF（线性域
/// C_lin = P·255/(A·257)，a=255 时退化 P/65535 与验证模型同式；sRGB
/// 域 round(P'·255/A')）。车道NO 自 blur_rgba 抽出。
/// 车道OL（perf-round3 刀②，NO 残余杠杆移交件）：sRGB 分支的变量
/// 除法魔数化——除数 d = 2·a16 为像素内三通道共享，每像素只求一次
/// 倒数魔数 m = ceil(2^32/d)（1 次 u32 除法，替代原 3 次 udiv），
/// 每通道 `mulhu + 至多一次条件减法校正` 精确还原 floor(n/d)，
/// 输出与原 `(510p+a16)/(2a16)` 逐位同值（界证明见循环内注释，
/// 全域穷举单测 ol_unpremul_magic_matches_direct_division）。
#[cfg(feature = "filters")]
fn unpremul_to_direct(
    cur: &[u16],
    px_count: usize,
    cs: FeColorCtx,
    div255: &BoxDiv,
    lin_lut: Option<&[f32; 257]>,
) -> Vec<u8> {
    let mut out = alloc::vec![0u8; px_count * 4];
    for i in 0..px_count {
        let a16 = cur[i * 4 + 3] as u32;
        if a16 == 0 {
            continue;
        }
        // (2a16+255)/510 = round_half_up(a16/255)（余数界逐位恒等证明
        // 同 BoxDiv 文档），即 div255.round(a16)。
        out[i * 4 + 3] = div255.round(a16).min(255) as u8;
        if cs.linear {
            for ch in 0..3 {
                let p = cur[i * 4 + ch] as u32;
                let l = p as f32 * 255.0 / (a16 as f32 * 257.0);
                out[i * 4 + ch] = match lin_lut {
                    Some(t) => lin2srgb_lut_byte(t, l),
                    None => lin2srgb_u8(l),
                };
            }
        } else {
            // n = 510p + a16 ≤ 2^25，d = 2a16 ∈ [2, 2^17)，均为 u32 安全。
            // m = ceil(2^32/d) = ⌊(2^32−1)/d⌋+1（ceil 恒等式，d ≥ 2 无溢出）。
            // q0 = ⌊n·m/2^32⌋：m ≥ 2^32/d ⟹ q0 ≥ q = ⌊n/d⌋；又
            // m·d − 2^32 < d ⟹ n·m/2^32 < n/d + n/2^32 < n/d + 1
            // ⟹ q0 ≤ q+1。故一次条件减法（q0·d > n ⟹ q0−1）即精确 q，
            // 与原除法逐位同值（q0·d ≤ n+d ≤ 2^25+2^17，u32 内无溢出）。
            let d = 2 * a16;
            let m = (u32::MAX / d) + 1;
            for ch in 0..3 {
                let p = cur[i * 4 + ch] as u32;
                let n = p * 510 + a16;
                let mut q = ((n as u64 * m as u64) >> 32) as u32;
                if q * d > n {
                    q -= 1;
                }
                out[i * 4 + ch] = q.min(255) as u8;
            }
        }
    }
    out
}

/// 车道NO 第二刀：预乘 u16 缓冲 2×2 盒均值降采样（末行/末列按实际
/// 贡献数取均值——round-half-up，cnt ≥ 1）。预乘域平均能量守恒。
#[cfg(feature = "filters")]
fn downsample2_premul(cur: &[u16], w: usize, h: usize, w2: usize, h2: usize) -> Vec<u16> {
    let mut small = alloc::vec![0u16; w2 * h2 * 4];
    for y2 in 0..h2 {
        let y0 = y2 * 2;
        let y1 = (y0 + 1).min(h - 1);
        for x2 in 0..w2 {
            let x0 = x2 * 2;
            let x1 = (x0 + 1).min(w - 1);
            let cnt = (if y1 > y0 { 2 } else { 1 } * if x1 > x0 { 2 } else { 1 }) as u32;
            for ch in 0..4 {
                let sum = cur[(y0 * w + x0) * 4 + ch] as u32
                    + cur[(y0 * w + x1) * 4 + ch] as u32
                    + cur[(y1 * w + x0) * 4 + ch] as u32
                    + cur[(y1 * w + x1) * 4 + ch] as u32;
                small[(y2 * w2 + x2) * 4 + ch] = ((sum * 2 + cnt) / (2 * cnt)) as u16;
            }
        }
    }
    small
}

/// 车道NO 第二刀：预乘域双线性升采样（系数 2x：源坐标 f = (t−1)/2，
/// 偶 t 取 f 半整数权重 1/2、奇 t 取整数 1；边界钳制重复边缘像素；
/// 全整数四分权重算式 r = (v0·(4−q) + v1·q + 2)/4，q ∈ {0,2}）。
#[cfg(feature = "filters")]
fn upsample_bilinear_premul(
    small: &[u16],
    w2: usize,
    h2: usize,
    up: &mut [u16],
    w: usize,
    h: usize,
) {
    // 预取每目标 x 的 (x0, x1, q)（q = 分数 ×4 ∈ {0,2}）
    let xs: alloc::vec::Vec<(usize, usize, u32)> = (0..w)
        .map(|x| {
            let f = x as i64 - 1; // 2·f = x−1（半整数坐标 ×2）
            let (x0, half) = if f < 0 {
                (0usize, true)
            } else {
                ((f >> 1) as usize, f & 1 == 1)
            };
            let x0 = x0.min(w2 - 1);
            let x1 = (x0 + (half as usize)).min(w2 - 1);
            (x0, x1, if half { 2 } else { 0 })
        })
        .collect();
    for y in 0..h {
        let f = y as i64 - 1;
        let (y0, half_y) = if f < 0 {
            (0usize, true)
        } else {
            ((f >> 1) as usize, f & 1 == 1)
        };
        let y0 = y0.min(h2 - 1);
        let y1 = (y0 + (half_y as usize)).min(h2 - 1);
        let qy = if half_y { 2u32 } else { 0 };
        let rowa = y0 * w2;
        let rowb = y1 * w2;
        for (x, &(x0, x1, qx)) in xs.iter().enumerate() {
            let po = (y * w + x) * 4;
            for ch in 0..4 {
                let v00 = small[(rowa + x0) * 4 + ch] as u32;
                let v01 = small[(rowa + x1) * 4 + ch] as u32;
                let v10 = small[(rowb + x0) * 4 + ch] as u32;
                let v11 = small[(rowb + x1) * 4 + ch] as u32;
                // 垂直插值（×4 权重，+2 四舍五入）
                let a = (v00 * (4 - qy) + v10 * qy + 2) / 4;
                let b = (v01 * (4 - qy) + v11 * qy + 2) / 4;
                up[po + ch] = ((a * (4 - qx) + b * qx + 2) / 4) as u16;
            }
        }
    }
}

/// 盒模糊「四舍五入均值」的精确魔数除法（车道NO）：原式
/// `(2s + win)/(2·win)` 代数恒等于 `floor((s + win/2)/win)`——
/// `(2s+win)/(2win) = (s+⌊win/2⌋)/win + δ`，`δ = (win mod 2)/(2win)`，
/// 而余数项 `(s+⌊win/2⌋) mod win ≤ win−1` 使 `frac + δ < 1` 恒成立
/// （(2win−1)/(2win) < 1），floor 不变。除法以 `(m, sh)` 魔数乘移位
/// 替代：`m = ceil(2^sh/win)`，写 `m·win = 2^sh + e`（0 ≤ e < win），
/// 则 `floor(u·m/2^sh) = floor(u/win + u·e/(win·2^sh))`，只要
/// `u·(win−1) < 2^sh` 误差项不越 1/win 即逐位精确。取 `sh = 29 + k`
///（k = ceil(log2(win))，sigma_box_passes 上限 4097 ⇒ k ≤ 13）：
/// u ≤ 65535·4097 + 2048 < 2^28.01，故 u·e < 2^(28.01+k) < 2^sh ✓；
/// u·m ≤ 2^28.01·(2^31+1) < 2^60，u64 内不溢出 ✓；且 m ≤ 2^31+1
/// < 2^32，可作 u32 装入 NEON 通道（aarch64 路径 vmull 逐通道同式）。
/// rv32imac 无硬件除法（udiv = libgcc 子程序），此替换对 ESP32 同样
/// 是数量级收益。
#[cfg(feature = "filters")]
struct BoxDiv {
    m: u32,
    sh: u32,
    half: u32,
}

#[cfg(feature = "filters")]
impl BoxDiv {
    #[inline]
    fn new(win: u32) -> Self {
        debug_assert!(win >= 1);
        let k = 32 - (win - 1).leading_zeros(); // ceil(log2(win))
        let sh = 29 + k;
        let m = ((1u64 << sh).div_ceil(win as u64)) as u32;
        debug_assert!(m as u64 * win as u64 >= 1u64 << sh);
        Self {
            m,
            sh,
            half: win / 2,
        }
    }

    /// floor((s + win/2)/win)，逐位等于原式 (2s+win)/(2win)。
    #[inline]
    fn round(&self, s: u32) -> u32 {
        (((s + self.half) as u64 * self.m as u64) >> self.sh) as u32
    }
}

/// 单轴盒模糊（窗口 = 输入 `[x+start, x+start+width)`，窗外透明零
/// 填充、恒除以 width）。`horizontal` = 沿 x 滑窗，否则沿 y。u16×4
/// 滑窗求和（u32 累加器：65535×窗口 ≤ 65535·4097 < 2²⁹ 无溢出），
/// 逐通道四舍五入。车道NO 重排：两轴都按行主序遍历（垂直轴维护
/// w×4 列累加器——每列的加/减序列与旧列主序逐项相同，u32 求和值
/// 逐位一致），均值除法走 [`BoxDiv`] 精确魔数（证明见其文档），
/// 热内圈无子切片调用；aarch64/x86_64 分别走 NEON/SSE2 4 通道内圈
///（车道NO/OY，逐通道整数运算与标量路径同构，逐字节同源）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
fn box_blur_axis(
    src: &[u16],
    dst: &mut [u16],
    w: usize,
    h: usize,
    start: i32,
    width: u32,
    horizontal: bool,
) {
    let win = width;
    if w == 0 || h == 0 || win == 0 {
        return;
    }
    let div = BoxDiv::new(win);
    let win_lo = start;
    let win_hi = start + win as i32;
    let row_len = w * 4;
    // 车道NO：水平轴前缀和暂存（(w+1)·4 u32，aarch64/x86_64 SIMD 路径用）
    let mut p_scratch = alloc::vec![0u32; row_len + 4];
    if horizontal {
        for line in 0..h {
            let base = line * row_len;
            let row = &src[base..base + row_len];
            let out = &mut dst[base..base + row_len];
            box_blur_h_row(row, out, w, win_lo, win_hi, &div, &mut p_scratch);
        }
    } else {
        // 行主序垂直轴：sums[y][ch] 对应旧列主序的逐列累加器，加/减
        // 序列逐项一致（见函数级注释）。
        let mut sums = alloc::vec![0u32; row_len];
        let lo = win_lo.max(0);
        let hi = win_hi.min(h as i32).max(lo);
        for k in lo as usize..hi as usize {
            let r = &src[k * row_len..(k + 1) * row_len];
            for (s, v) in sums.iter_mut().zip(r.iter()) {
                *s += *v as u32;
            }
        }
        for y in 0..h {
            let out = &mut dst[y * row_len..(y + 1) * row_len];
            #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
            svgine_simd::box_round_row(sums.as_slice(), out, div.half, div.m, div.sh);
            #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
            for (o, s) in out.iter_mut().zip(sums.iter()) {
                *o = div.round(*s).min(u16::MAX as u32) as u16;
            }
            let add = y as i32 + win_hi;
            if add >= 0 && add < h as i32 {
                let r = &src[add as usize * row_len..][..row_len];
                #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
                svgine_simd::acc_add_row(sums.as_mut_slice(), r);
                #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
                for (s, v) in sums.iter_mut().zip(r.iter()) {
                    *s += *v as u32;
                }
            }
            let sub = y as i32 + win_lo;
            if sub >= 0 && sub < h as i32 {
                let r = &src[sub as usize * row_len..][..row_len];
                #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
                svgine_simd::acc_sub_row_saturating(sums.as_mut_slice(), r);
                #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
                for (s, v) in sums.iter_mut().zip(r.iter()) {
                    *s = s.saturating_sub(*v as u32);
                }
            }
        }
    }
}

/// 水平轴单行热内圈（车道NO）：滑窗窗和 + 4 通道舍入写出。
/// aarch64 = [`svgine_simd::box_h_round_row`]（NEON 前缀和 + vld4q/vst4q
/// 内圈）；x86_64 = 同名 SSE2 版（车道OY，前缀和 + PMULUDQ 扩位乘，
/// 与 scalar/NEON 逐字节同源）；其余目标 = 标量滑窗（直索引）。
#[cfg(feature = "filters")]
fn box_blur_h_row(
    row: &[u16],
    out: &mut [u16],
    w: usize,
    win_lo: i32,
    win_hi: i32,
    div: &BoxDiv,
    p_scratch: &mut [u32],
) {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    {
        svgine_simd::box_h_round_row(
            row, p_scratch, out, w, win_lo, win_hi, div.half, div.m, div.sh,
        );
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        let _ = p_scratch;
        let w_i = w as i32;
        let mut sums = [0u32; 4];
        let lo = win_lo.max(0).min(w_i) as usize;
        let hi = win_hi.min(w_i).max(win_lo.max(0).min(w_i)) as usize;
        for k in lo..hi {
            let kb = k * 4;
            sums[0] += row[kb] as u32;
            sums[1] += row[kb + 1] as u32;
            sums[2] += row[kb + 2] as u32;
            sums[3] += row[kb + 3] as u32;
        }
        for x in 0..w {
            let ob = x * 4;
            out[ob] = div.round(sums[0]).min(u16::MAX as u32) as u16;
            out[ob + 1] = div.round(sums[1]).min(u16::MAX as u32) as u16;
            out[ob + 2] = div.round(sums[2]).min(u16::MAX as u32) as u16;
            out[ob + 3] = div.round(sums[3]).min(u16::MAX as u32) as u16;
            let add = x as i32 + win_hi;
            if add >= 0 && add < w_i {
                let ab = add as usize * 4;
                sums[0] += row[ab] as u32;
                sums[1] += row[ab + 1] as u32;
                sums[2] += row[ab + 2] as u32;
                sums[3] += row[ab + 3] as u32;
            }
            let sub = x as i32 + win_lo;
            if sub >= 0 && sub < w_i {
                let sb = sub as usize * 4;
                sums[0] = sums[0].saturating_sub(row[sb] as u32);
                sums[1] = sums[1].saturating_sub(row[sb + 1] as u32);
                sums[2] = sums[2].saturating_sub(row[sb + 2] as u32);
                sums[3] = sums[3].saturating_sub(row[sb + 3] as u32);
            }
        }
    }
}

/// 单轴盒模糊（u16 单通道，drop_shadow 的 alpha 域专用）——语义与
/// [`box_blur_axis`] 同款（窗口 `[x+start, x+start+width)`、窗外零
/// 填充、恒除以 width）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
fn box_blur_axis_1ch(
    src: &[u16],
    dst: &mut [u16],
    w: usize,
    h: usize,
    start: i32,
    width: u32,
    horizontal: bool,
) {
    let win = width;
    if w == 0 || h == 0 || win == 0 {
        return;
    }
    let div = BoxDiv::new(win);
    let win_lo = start;
    let win_hi = start + win as i32;
    if horizontal {
        for line in 0..h {
            let base = line * w;
            let row = &src[base..base + w];
            let out = &mut dst[base..base + w];
            let mut sum = 0u32;
            let lo = win_lo.max(0).min(w as i32) as usize;
            let hi = win_hi.min(w as i32).max(lo as i32) as usize;
            for &v in &row[lo..hi] {
                sum += v as u32;
            }
            for (x, o) in out.iter_mut().enumerate() {
                *o = div.round(sum).min(u16::MAX as u32) as u16;
                let add = x as i32 + win_hi;
                if add >= 0 && add < w as i32 {
                    sum += row[add as usize] as u32;
                }
                let sub = x as i32 + win_lo;
                if sub >= 0 && sub < w as i32 {
                    sum = sum.saturating_sub(row[sub as usize] as u32);
                }
            }
        }
    } else {
        let mut sums = alloc::vec![0u32; w];
        let lo = win_lo.max(0);
        let hi = win_hi.min(h as i32).max(lo);
        for k in lo as usize..hi as usize {
            let r = &src[k * w..(k + 1) * w];
            for (s, v) in sums.iter_mut().zip(r.iter()) {
                *s += *v as u32;
            }
        }
        for y in 0..h {
            let out = &mut dst[y * w..(y + 1) * w];
            for (o, s) in out.iter_mut().zip(sums.iter()) {
                *o = div.round(*s).min(u16::MAX as u32) as u16;
            }
            let add = y as i32 + win_hi;
            if add >= 0 && add < h as i32 {
                let r = &src[add as usize * w..][..w];
                for (s, v) in sums.iter_mut().zip(r.iter()) {
                    *s += *v as u32;
                }
            }
            let sub = y as i32 + win_lo;
            if sub >= 0 && sub < h as i32 {
                let r = &src[sub as usize * w..][..w];
                for (s, v) in sums.iter_mut().zip(r.iter()) {
                    *s = s.saturating_sub(*v as u32);
                }
            }
        }
    }
}

/// feDropShadow（语法糖，规范等价链 blur→offset→flood∩in→merge 的
/// 融合实现）：SourceAlpha 偏移 (dx,dy) → 盒模糊×3 → 着 flood 色
///（alpha × flood-opacity）→ 原图 src-over 叠上。全程预乘 u16 域。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
fn drop_shadow(
    input: &[u8],
    w: usize,
    h: usize,
    dx: f32,
    dy: f32,
    sx: f32,
    sy: f32,
    color: Rgba8,
) -> Vec<u8> {
    let px = sigma_box_passes(sx);
    let py = sigma_box_passes(sy);
    let shift_x = libm::roundf(dx) as i32;
    let shift_y = libm::roundf(dy) as i32;
    // 阴影 alpha 位图（预乘 A 域）：SourceAlpha 偏移
    let mut sh = alloc::vec![0u16; w * h]; // 仅 A 通道（P 在着色期合成）
    for y in 0..h {
        let ty = y as i32 - shift_y;
        if ty < 0 || ty >= h as i32 {
            continue;
        }
        for x in 0..w {
            let tx = x as i32 - shift_x;
            if tx < 0 || tx >= w as i32 {
                continue;
            }
            let a = input[(ty as usize * w + tx as usize) * 4 + 3] as u32;
            sh[y * w + x] = (a * 255) as u16;
        }
    }
    // 盒模糊（A 单通道滑窗，规范盒序列）
    if !px.is_empty() || !py.is_empty() {
        let mut tmp = alloc::vec![0u16; w * h];
        for (start, width, horiz) in px
            .iter()
            .map(|&(st, wd)| (st, wd, true))
            .chain(py.iter().map(|&(st, wd)| (st, wd, false)))
        {
            box_blur_axis_1ch(&sh, &mut tmp, w, h, start, width, horiz);
            core::mem::swap(&mut sh, &mut tmp);
        }
    }
    // 着色 + 原图叠上：阴影直色 rgb=flood、a=A'/255×flood.a/255；
    // 随后 blend_px(src 像素) 在阴影之上。原图（前景）经 Chrome 的
    // linear-8888 中转面量化（sRGB→linear8→sRGB 往返：预言机实测
    // #4A90D9 的 R 74→73——8bit 线性中间量舍入损失），此处对齐复刻
    //（阴影为纯 flood 色，黑/灰在该量化下不变，无需处理）。
    let fa = color.a as u32;
    let mut out = alloc::vec![0u8; w * h * 4];
    for i in 0..w * h {
        let a16 = sh[i] as u32;
        if a16 > 0 {
            // a_shadow = round(A'/255 × fa/255 × 255) = round(A'·fa/65025·255)
            let ash = ((a16 * fa * 2 + 65025) / 130050).min(255) as u8;
            if ash > 0 {
                out[i * 4] = color.r;
                out[i * 4 + 1] = color.g;
                out[i * 4 + 2] = color.b;
                out[i * 4 + 3] = ash;
            }
        }
        let sa = input[i * 4 + 3] as f32 / 255.0;
        if sa > 0.0 {
            let c = Rgba8 {
                r: lin8_roundtrip(input[i * 4]),
                g: lin8_roundtrip(input[i * 4 + 1]),
                b: lin8_roundtrip(input[i * 4 + 2]),
                a: 255,
            };
            blend_px(&mut out[i * 4..i * 4 + 4], c, sa);
        }
    }
    out
}

/// sRGB 字节 → linear（8bit 量化）→ sRGB 字节：复刻 Chrome 滤镜 DAG
/// 的 linear-8888 中转面舍入（feDropShadow 前景实测 74→73）。
#[cfg(feature = "filters")]
fn lin8_roundtrip(c: u8) -> u8 {
    let v = c as f32 / 255.0;
    let lin = if v <= 0.04045 {
        v / 12.92
    } else {
        libm::powf((v + 0.055) / 1.055, 2.4)
    };
    let q = (lin * 255.0 + 0.5) as u8;
    let l = q as f32 / 255.0;
    let srgb = if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * libm::powf(l, 1.0 / 2.4) - 0.055
    };
    (srgb * 255.0 + 0.5) as u8
}

/// feColorMatrix：5×4 矩阵（直色语义）。字节域等价形式
/// `C' = Σ m·C_byte + off·255`（行 R/G/B/A，列 R/G/B/A/offset）。
/// 颜色插值域随 `cs`（车道X）：linearRGB = 规范缺省（color-interpolation
/// -filters；Chrome 实测一致——sRGB 域直算会系统性偏离预言机，见 M8
/// 评估）；sRGB = 属性显式声明，字节域直算。
#[cfg(feature = "filters")]
fn color_matrix(input: &[u8], w: usize, h: usize, m: &[f32; 20], cs: FeColorCtx) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    // 字节 → 工作域 LUT（linear：EOTF；sRGB：恒等）；alpha 行不换域
    //（alpha 无 gamma）。
    let mut lut = [0.0f32; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *v = if !cs.linear {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            libm::powf((c + 0.055) / 1.055, 2.4)
        };
    }
    let enc = |v: f32| -> u8 {
        let c = v.clamp(0.0, 1.0);
        let s = if !cs.linear {
            c
        } else if c <= 0.0031308 {
            c * 12.92
        } else {
            1.055 * libm::powf(c, 1.0 / 2.4) - 0.055
        };
        (s * 255.0 + 0.5) as u8
    };
    let apply = |row: usize, lin: [f32; 3], a: u8| -> u8 {
        let base = row * 5;
        let v = m[base] * lin[0]
            + m[base + 1] * lin[1]
            + m[base + 2] * lin[2]
            + m[base + 3] * a as f32 / 255.0
            + m[base + 4];
        enc(v)
    };
    let apply_a = |row: usize, lin: [f32; 3], a: u8| -> u8 {
        let base = row * 5;
        // 车道CN：alpha 行的 RGB 系数输入同样在工作色域求值（EOTF 后；
        // alpha 自身无 gamma 恒直值）——此前用原始字节导致 linear 缺省下
        // alpha 行系统性偏大（filters-color-01-b 灰度带 209 vs 预言机
        // 221 的根因）。sRGB 模式 lut 恒等 → 字节零漂移。
        let v = m[base] * lin[0]
            + m[base + 1] * lin[1]
            + m[base + 2] * lin[2]
            + m[base + 3] * a as f32 / 255.0
            + m[base + 4];
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    for i in 0..w * h {
        let p = &input[i * 4..i * 4 + 4];
        let lin = [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]];
        out[i * 4] = apply(0, lin, p[3]);
        out[i * 4 + 1] = apply(1, lin, p[3]);
        out[i * 4 + 2] = apply(2, lin, p[3]);
        out[i * 4 + 3] = apply_a(3, lin, p[3]);
    }
    out
}

/// feOffset：位图平移（设备像素已四舍五入，子像素偏移为已知近似），
/// 露出部分透明。
#[cfg(feature = "filters")]
fn offset_bitmap(input: &[u8], w: usize, h: usize, dx: f32, dy: f32) -> Vec<u8> {
    let sx = libm::roundf(dx) as i32;
    let sy = libm::roundf(dy) as i32;
    let mut out = alloc::vec![0u8; w * h * 4];
    if sx.unsigned_abs() >= w as u32 || sy.unsigned_abs() >= h as u32 {
        return out; // 整体移出区域
    }
    for y in 0..h {
        let ty = y as i32 - sy;
        if ty < 0 || ty >= h as i32 {
            continue;
        }
        for x in 0..w {
            let tx = x as i32 - sx;
            if tx < 0 || tx >= w as i32 {
                continue;
            }
            let (o, t) = ((y * w + x) * 4, (ty as usize * w + tx as usize) * 4);
            out[o] = input[t];
            out[o + 1] = input[t + 1];
            out[o + 2] = input[t + 2];
            out[o + 3] = input[t + 3];
        }
    }
    out
}

/// feFlood：区域纯色填充（flood-color × flood-opacity 已烘焙进 a）。
#[cfg(feature = "filters")]
fn flood_bitmap(w: usize, h: usize, color: Rgba8) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    if color.a > 0 {
        for i in 0..w * h {
            out[i * 4] = color.r;
            out[i * 4 + 1] = color.g;
            out[i * 4 + 2] = color.b;
            out[i * 4 + 3] = color.a;
        }
    }
    out
}

/// feComposite arithmetic 的合成 alpha（对 alpha 通道做同式）。
#[inline]
#[cfg(feature = "filters")]
fn out_a_arith(sa: f32, da: f32, k1: f32, k2: f32, k3: f32, k4: f32) -> f32 {
    (k1 * sa * da + k2 * sa + k3 * da + k4).clamp(0.0, 1.0)
}

/// 滤镜工作色域上下文（车道X，`color-interpolation-filters`）。
/// true = linearRGB（规范缺省值/auto；Chrome 缺省同款——a1 探针矩阵
/// 实证），false = sRGB（属性显式声明）。整链统一（滤镜级域），以单
/// 参数传入各域敏感 primitive 求值器——转换集中在 [`FeColorCtx`] 两个
/// 方法里，不在求值循环内散布 if。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "filters")]
struct FeColorCtx {
    linear: bool,
}

#[cfg(feature = "filters")]
impl FeColorCtx {
    /// 规范缺省（linearRGB）上下文（既有单测用；库内路径由 FilterDef
    /// 携带的域标志构造）。
    #[cfg(test)]
    const LINEAR: Self = Self { linear: true };

    /// sRGB 8bit → 工作域 [0,1] 直色。linear：EOTF 查表（l·65535 存
    /// u16 的 `srgb_to_linear_table`）；sRGB：/255 恒等（跳过 EOTF，
    /// 车道V 改造前行为）。`s2l` 由调用方预计算（每滤镜一次）。
    #[inline]
    fn to_working(self, s2l: &[u16; 256], b: u8) -> f32 {
        if self.linear {
            s2l[b as usize] as f32 / 65535.0
        } else {
            f32::from(b) / 255.0
        }
    }

    /// 工作域 [0,1] → sRGB 8bit。linear：逆 EOTF（`lin2srgb_u8`）；
    /// sRGB：钳位 ×255 四舍五入直出。
    #[inline]
    fn to_byte(self, v: f32) -> u8 {
        if self.linear {
            lin2srgb_u8(v)
        } else {
            (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
        }
    }
}

/// sRGB 8bit → linear 查表（EOTF 2.4 分段；l·65535 存 u16）。feBlend/
/// feComposite 共享（linearRGB 工作域——`color-interpolation-filters`
/// 规范缺省，Chrome/Skia 同款；sRGB 切域由 [`FeColorCtx`] 恒等映射）。
#[cfg(feature = "filters")]
fn srgb_to_linear_table() -> [u16; 256] {
    let mut s2l = [0u16; 256];
    for (i, v) in s2l.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        let l = if c <= 0.04045 {
            c / 12.92
        } else {
            libm::powf((c + 0.055) / 1.055, 2.4)
        };
        *v = (l * 65535.0 + 0.5) as u16;
    }
    s2l
}

/// linear → sRGB 8bit（逆 EOTF；钳 [0,1] 后四舍五入）。
#[inline]
#[cfg(feature = "filters")]
fn lin2srgb_u8(l: f32) -> u8 {
    let c = if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * libm::powf(l, 1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// 线性域逆 EOTF 阈值表启用下限（像素数）：建表 ≈ 8K 次 powf，直算
/// 每像素 3 次 powf，打平点 ≈ 2.7K px，取整档留足余量；小于此直算，
/// 两路口径逐字节全等（表以原函数为神谕构造，见下）。
#[cfg(feature = "filters")]
const LIN_LUT_MIN_PX: usize = 8192;

/// 大 σ 降采样启用阈值（车道NO 第二刀，双轴设备 σ 同时 ≥ 此值才降
/// 采样；σ/2 后盒宽 d′ = floor(σ/2·1.88+0.5) ≥ 3 仍有三盒近似保真）。
/// 6.0 的定值依据：主语料 s7-02（σ_dev 3/5）在其下保持字节级不动
/// （strict/tol2 口径零回归），gemini 仍覆盖 9/11 枚大 σ 模糊；证据
/// 链见 tests/oracle/reports/blur-perf-no.md。
#[cfg(feature = "filters")]
const DS_SIGMA_MIN: f32 = 6.0;

/// 降采样最小区域边长（双轴同时 ≥；小区域降采样边缘占比过高、且
/// 绝对耗时已小，直算）。
#[cfg(feature = "filters")]
const DS_MIN_SIDE: usize = 64;

/// 线性域逆 EOTF 精确阈值表（车道NO）：`t[b]` = 满足
/// `lin2srgb_u8(l) ≥ b` 的最小 f32 `l`（t[0]=0、t[256]=+∞）。构造以
/// [`lin2srgb_u8`] 原函数为神谕做 f32 位型二分——热路
/// （`lin2srgb_lut_byte`）对任意 l 返回与原函数相同的字节
/// （单调阈值刻画；blur_rgba 单测对可达域分层抽样 + 每个阈值 ±4ULP
/// 边界带逐位校验）。建表 ≈ 255×32 次 powf，替代每像素 3 次逐算。
#[cfg(feature = "filters")]
fn lin2srgb_thresholds() -> [f32; 257] {
    let mut t = [f32::INFINITY; 257];
    t[0] = 0.0;
    for (b, tb) in t.iter_mut().enumerate().skip(1) {
        // 不变式：orig(lo) < b ≤ orig(hi)；非负 f32 位型即大小序
        let (mut lo, mut hi) = (0f32, 1f32);
        debug_assert!(lin2srgb_u8(1.0) >= b as u8);
        while hi.to_bits() - lo.to_bits() > 1 {
            let mid = f32::from_bits((lo.to_bits() + hi.to_bits()) / 2);
            if lin2srgb_u8(mid) >= b as u8 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        *tb = hi;
    }
    t
}

/// 阈值表查字节：`max{b : l ≥ t[b]}`（逐位等于 [`lin2srgb_u8`]（l），
/// 含 l<0 / NaN → 0、l ≥ t[255] → 255 的边界）。
#[inline]
#[cfg(feature = "filters")]
fn lin2srgb_lut_byte(t: &[f32; 257], l: f32) -> u8 {
    let mut lo = 0usize;
    let mut hi = 256usize;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if l >= t[mid] {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo as u8
}

/// feBlend（CSS Compositing L1 Co 公式，premul 工作域）：
/// `co_pm = (1−αb)·Cs_pm + (1−αs)·Cb_pm + αs·αb·B(Cb,Cs)`，直色输出
/// `co_pm/αo`。B 复用 CSS 混合函数（blend_css，16 模式同源共享；在
/// 滤镜工作域直色上求值——separable 多项式域无关，非可分离四模式按
/// 工作域定义）。车道IA：旧实现把**直色链缓冲输入再除 α**（inv_s=
/// v/sa）= 二次反预乘，半透明输入直色 >255 时 `& 0xFF` 静默回绕
/// （feFlood α=0.65 的 B 通道 168→258→2，openclipart 狼形整屏绿移
/// 根因；旧枚举无 overlay，mode="overlay" 静默降级 Normal）——现按
/// premul 域混合，输入直色只做色域转换、除法只出现在结果反预乘
/// （pm ≤ αo 恒成立，无越界）。
#[cfg(feature = "filters")]
fn blend_fe(s: &[u8], d: &[u8], w: usize, h: usize, mode: FeBlendMode, cs: FeColorCtx) -> Vec<u8> {
    let s2l = srgb_to_linear_table();
    let css_mode = mode.to_blend_mode();
    let mut out = alloc::vec![0u8; w * h * 4];
    for i in 0..w * h {
        let o = i * 4;
        let sa = s[o + 3] as f32 / 255.0;
        let da = d[o + 3] as f32 / 255.0;
        let oa = sa + da * (1.0 - sa);
        if oa <= 0.0 {
            continue;
        }
        // 直色字节 → 工作色域直色（linear 查表 / sRGB 恒等；α=0 像素
        // RGB 未定义，不读）
        let mut sc = [0.0f32; 3];
        let mut dc = [0.0f32; 3];
        if sa > 0.0 {
            for (ch, v) in sc.iter_mut().enumerate() {
                *v = cs.to_working(&s2l, s[o + ch]);
            }
        }
        if da > 0.0 {
            for (ch, v) in dc.iter_mut().enumerate() {
                *v = cs.to_working(&s2l, d[o + ch]);
            }
        }
        // B(Cb, Cs) 直色域逐像素求一次（非可分离模式三通道联立；
        // αs·αb=0 时 B 项系数恒 0，跳过防 0·NaN）
        let b = if sa > 0.0 && da > 0.0 {
            blend_css(dc, sc, css_mode)
        } else {
            [0.0; 3]
        };
        for ch in 0..3 {
            let pm = (1.0 - da) * sc[ch] * sa + (1.0 - sa) * dc[ch] * da + sa * da * b[ch];
            // 工作色域 → sRGB（linear 逆 EOTF / sRGB 直出）后直色输出
            out[o + ch] = cs.to_byte(pm / oa);
        }
        out[o + 3] = (oa * 255.0 + 0.5) as u8;
    }
    out
}

/// feComponentTransfer：R/G/B/A 传递函数。RGB 传递函数在**工作色域**
/// 0..1 直色上应用（车道X：linear 缺省 = sRGB 字节→EOTF→函数→逆
/// EOTF，与 blend/composite/colorMatrix 的缺省域一致；sRGB 属性声明 =
/// 编码值直算）。A 通道对 alpha 本身应用（不经 gamma 转换；结果即新
/// alpha，随后与原直色组合输出）。type=identity 跳过。
#[cfg(feature = "filters")]
fn component_transfer(
    input: &[u8],
    w: usize,
    h: usize,
    fns: &[FeTransferFn; 4],
    cs: FeColorCtx,
) -> Vec<u8> {
    #[inline]
    fn apply_fn(v: f32, f: &FeTransferFn) -> f32 {
        match f {
            FeTransferFn::Identity => v,
            FeTransferFn::Table(tv) => {
                let n = tv.len();
                if n == 0 {
                    return v;
                }
                if n == 1 {
                    return tv[0];
                }
                // 车道CN：规范分段数 = n−1（k = floor(v·(n−1))）——此前
                // 按 v·n 分段，四值表的中间过渡整体错位（filters-comptran
                // -01-b Table 带 61.9% 失配的根因；Chrome 线性域同式实证）。
                let pos = (v * (n - 1) as f32).clamp(0.0, (n - 1) as f32 - 0.0001);
                let k = pos as usize;
                let u = pos - k as f32;
                let hi = if k + 1 < n { tv[k + 1] } else { tv[n - 1] };
                tv[k] + (hi - tv[k]) * u
            }
            FeTransferFn::Discrete(tv) => {
                let n = tv.len();
                if n == 0 {
                    return v;
                }
                tv[((v * n as f32).clamp(0.0, n as f32 - 0.0001)) as usize]
            }
            FeTransferFn::Linear { slope, intercept } => slope * v + intercept,
            FeTransferFn::Gamma {
                amplitude,
                exponent,
                offset,
            } => amplitude * libm::powf(v.max(0.0), *exponent) + offset,
        }
    }
    let s2l = srgb_to_linear_table();
    let mut out = alloc::vec![0u8; w * h * 4];
    for i in 0..w * h {
        let o = i * 4;
        let a = input[o + 3] as f32 / 255.0;
        for ch in 0..3 {
            // RGB：工作色域直色进出（linear 经 EOTF/逆 EOTF，sRGB 恒等）
            let v = cs.to_working(&s2l, input[o + ch]);
            let v = apply_fn(v, &fns[ch]);
            out[o + ch] = cs.to_byte(v);
        }
        // alpha 通道独立传递（不经 gamma 转换）；RGB 保持传递函数结果
        //（直色，规范语义——预乘合成由消费方 blend_px 做）
        let na = apply_fn(a, &fns[3]);
        out[o + 3] = (na.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    out
}

/// feTile：以 `tile`（[x0,y0,x1,y1] 区域局部像素）内的输入为瓦片
/// 平铺填充整个滤镜区域。tile 为空（宽/高 0）→ 全透明。
#[cfg(feature = "filters")]
fn tile_bitmap(input: &[u8], w: usize, h: usize, tile: &[u32; 4]) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    let (tx0, ty0, tx1, ty1) = (
        tile[0] as usize,
        tile[1] as usize,
        tile[2] as usize,
        tile[3] as usize,
    );
    let (tw, th) = (tx1.saturating_sub(tx0), ty1.saturating_sub(ty0));
    if tw == 0 || th == 0 {
        return out;
    }
    for y in 0..h {
        let sy = ty0 + (y % th);
        for x in 0..w {
            let sx = tx0 + (x % tw);
            let (o, so) = ((y * w + x) * 4, (sy * w + sx) * 4);
            out[o] = input[so];
            out[o + 1] = input[so + 1];
            out[o + 2] = input[so + 2];
            out[o + 3] = input[so + 3];
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 车道AQ：重 primitive（feTurbulence / feDisplacementMap / feLighting）
// ---------------------------------------------------------------------------

/// Park–Miller 最小标准随机数列（Skia PaintingData::random 同式；i32 无
/// 溢出：16807×127772 < 2³¹−1）。
#[cfg(feature = "filters")]
struct TurbRng(i32);

#[cfg(feature = "filters")]
impl TurbRng {
    fn next(&mut self) -> i32 {
        let r = 16807 * (self.0 % 127773) - 2836 * (self.0 / 127773);
        self.0 = if r <= 0 { r + 2147483647 } else { r };
        self.0
    }
}

/// Perlin 噪声表（Skia PaintingData::init 忠实移植）：4 通道 × 256 梯度。
/// 梯度以 u16 对量化——编码 `(g+1)·32767.5` round（Chrome 同款，
/// 精度地板 ~3e-5，逐字节对比允许 ±1 LSB）——建表期随即经
/// [`turb_grad`] 展开成 f32 存储（车道DA：内圈逐像素 u16→f32 转换
/// 外提，同一转换算式在建表期执行，结果逐位一致）。
#[cfg(feature = "filters")]
struct TurbTable {
    selector: [u8; 256],
    // 车道EF：scalar 布局在 aarch64/x86_64 上仅剩建表写入 + 测试消费
    //（生产读取走 grad_v 的 SIMD 臂）；其余架构（riscv 等）仍是唯一
    // 通路。dead_code 压制 = 桌面非 test 编译单元里该字段无读取点。
    #[cfg_attr(any(target_arch = "aarch64", target_arch = "x86_64"), allow(dead_code))]
    grad: [[[f32; 2]; 256]; 4],
    // 车道EF：SIMD 布局镜像 `grad_v[lattice][xy][ch]`（lane ch = 通道
    // ch）——值与 grad 完全相同（建表期同一算式直写两份），布局为向
    // 量加载排布（svgine_simd::GradTable 契约，每 (lattice,xy) 连续
    // 4-float）。
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    grad_v: svgine_simd::GradTable,
}

#[cfg(feature = "filters")]
fn turbulence_table(seed: i32) -> TurbTable {
    // 种子口径（setup_seed 同式；Rust 与 C 的 % 同为向零截断）：
    // 截断取整由 build 期 `as i32` 完成，这里抬升到 [1, 2³¹−2]。
    let mut s = seed;
    if s <= 0 {
        s = -(s % 2147483646) + 1;
    }
    if s > 2147483646 {
        s = 2147483646;
    }
    let mut rng = TurbRng(s);
    let mut selector = [0u8; 256];
    let mut raw = [[[0u16; 2]; 256]; 4];
    for (i, s) in selector.iter_mut().enumerate() {
        *s = i as u8;
    }
    for ch in raw.iter_mut() {
        for g in ch.iter_mut() {
            g[0] = (rng.next() % 512) as u16;
            g[1] = (rng.next() % 512) as u16;
        }
    }
    // Fisher–Yates（i = 255..1，j = next() % 256——与规范/Skia 同序）
    let mut i = 255usize;
    while i > 0 {
        let k = selector[i];
        let j = (rng.next() % 256) as usize;
        selector[i] = selector[j];
        selector[j] = k;
        i -= 1;
    }
    // 噪声数据按最终 selector 置换（PaintingData permutation 步）
    let mut permuted = raw;
    for i in 0..256 {
        for ch in 0..4 {
            permuted[ch][i] = raw[ch][selector[i] as usize];
        }
    }
    // 梯度：((n−256)/256, (n−256)/256) → 归一化 → (g+1)·32767.5 量化
    // → 建表期直接展开为 f32（同一 turb_grad 算式，逐位一致——车道DA
    // 把内圈每像素 32 次 u16→f32 转换搬到建表期一次性完成）。
    // 退化梯度（len=0）保持原值（SkPoint::normalize 失败不改写）。
    let mut grad = [[[0.0f32; 2]; 256]; 4];
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    let mut grad_v: svgine_simd::GradTable = [[[0.0f32; 4]; 2]; 256];
    for ch in 0..4 {
        for i in 0..256 {
            let gx = (permuted[ch][i][0] as f32 - 256.0) / 256.0;
            let gy = (permuted[ch][i][1] as f32 - 256.0) / 256.0;
            let len = libm::sqrtf(gx * gx + gy * gy);
            let (nx, ny) = if len > 0.0 {
                (gx / len, gy / len)
            } else {
                (gx, gy)
            };
            grad[ch][i][0] = turb_grad(((nx + 1.0) * 32767.5 + 0.5) as u16);
            grad[ch][i][1] = turb_grad(((ny + 1.0) * 32767.5 + 0.5) as u16);
            // 车道EF：SIMD 镜像表与 scalar 表同算式直写，值逐位相同。
            #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
            {
                grad_v[i][0][ch] = grad[ch][i][0];
                grad_v[i][1][ch] = grad[ch][i][1];
            }
        }
    }
    TurbTable {
        selector,
        grad,
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        grad_v,
    }
}

/// u16 量化梯度 → [-1,1]（Skia stage：`mad(cast(s), 2/65535, −1)`）。
#[inline]
#[cfg(feature = "filters")]
fn turb_grad(v: u16) -> f32 {
    v as f32 * (2.0 / 65535.0) - 1.0
}

/// 车道EF：内圈 floorf 位精确替代。aarch64 走 frintm 单指令（IEEE
/// roundToIntegralTowardNegative，与 libm::floorf 全域位型一致，见
/// svgine_simd::floor_exact 文档 + turb_floor_matches_libm_bit_exact
/// 扫描测试）；其余架构回退 libm::floorf（软件实现，内圈每 octave 每
/// 像素一次的热点）。stitch 校正/建表期 floorf 不在热点，保持 libm。
#[inline(always)]
#[cfg(feature = "filters")]
fn turb_floor(x: f32) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        svgine_simd::floor_exact(x)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        libm::floorf(x)
    }
}

/// 车道DA：单通道噪声求值 + 累加（算式与算序 = 旧内圈逐项照抄）。
/// 独立成按值传参的 always-inline fn 是刻意的：旧 `sum.iter_mut()`
/// 让累加器以 &mut 逃逸 → LLVM 无法寄存器提升，每 octave 4 次栈往返
/// （sample 实测内圈栈流量爆炸）；按值进出让累加器留在寄存器。
/// 通道间本就独立、单通道内算序不变 → 逐位等价。
#[inline(always)]
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
// 车道EF：aarch64/x86_64 生产路径走 turb_channels_vec（SIMD 臂），本 fn
// 在桌面非 test 编译单元无调用点（riscv 等架构仍是唯一通路；测试里是
// scalar 参考基准）。
#[cfg_attr(any(target_arch = "aarch64", target_arch = "x86_64"), allow(dead_code))]
fn turb_channel(
    g: &[[f32; 2]; 256],
    b00: usize,
    b10: usize,
    b01: usize,
    b11: usize,
    frx: f32,
    fry: f32,
    smx: f32,
    smy: f32,
    acc: f32,
    ratio: f32,
    abs_mode: bool,
) -> f32 {
    let dot = |b: usize, vx: f32, vy: f32| -> f32 { g[b][0] * vx + g[b][1] * vy };
    let u = dot(b00, frx, fry);
    let v = dot(b10, frx - 1.0, fry);
    let a = u + smx * (v - u);
    let u = dot(b01, frx, fry - 1.0);
    let v = dot(b11, frx - 1.0, fry - 1.0);
    let b = u + smx * (v - u);
    let mut c = a + smy * (b - a);
    if abs_mode {
        c = c.abs();
    }
    acc + c * ratio
}

/// feTurbulence 求值（车道AQ）。数值口径 = **Chrome/Skia raster pipeline
/// perlin_noise stage + SkPerlinNoiseShaderImpl PaintingData**（ARM
/// chrome-headless-shell 预言机真身），与 SVG1.1 附录参考实现同源但有
/// 已知偏离（探针 turb-* 实证）：
/// 1) 采样坐标 = `(像素 + 1.0) × 基频`——pipeline 像素中心 +0.5 与
///    stage 内 +0.5 叠加（亚像素扫描实证，+0.5 假设 mean|Δ| 13.25，
///    +1.0 残差 0.47 字节）；坐标相对**滤镜区域**原点，输出裁到
///    primitive 子区域（turb-08 实证）；
/// 2) 梯度表 u16 量化（see [`TurbTable`]）；
/// 3) lattice 索引一次 selector + y 求和后 `&0xFF`（表长 256，无规范
///    的 258 扩展表）。
///
/// 输出：工作域噪声 → 模拟 Chrome premul-工作域中转面量化
///（pm_byte = round(v·a_byte)）→ [`FeColorCtx::to_byte`] 编码直色。
/// `tile` = 子区域像素 AABB（stitch 瓦片尺寸 + 输出裁切）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
fn turbulence_bitmap(
    w: usize,
    h: usize,
    ty: FeTurbType,
    bf: (f32, f32),
    octaves: u32,
    seed: i32,
    stitch: bool,
    origin: [f32; 2],
    tile: [u32; 4],
    cs: FeColorCtx,
) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    let (sx0, sy0, sx1, sy1) = (
        tile[0] as usize,
        tile[1] as usize,
        tile[2] as usize,
        tile[3] as usize,
    );
    // Skia：0 octave 折叠为常数（fractal = 0.5 灰 / turbulence = 全透明）
    if octaves == 0 {
        if ty == FeTurbType::FractalNoise {
            for y in sy0..sy1.min(h) {
                for x in sx0..sx1.min(w) {
                    let o = (y * w + x) * 4;
                    out[o] = cs.to_byte(0.5);
                    out[o + 1] = cs.to_byte(0.5);
                    out[o + 2] = cs.to_byte(0.5);
                    out[o + 3] = 128;
                }
            }
        }
        return out;
    }
    // stitch 频率校正 + 瓦片尺寸（PaintingData::stitch；瓦片 = 子区域
    // 像素尺寸 round 成 int）
    let (mut bfx, mut bfy) = bf;
    let (mut sw, mut sh) = (0.0f32, 0.0f32);
    if stitch {
        let tw = tile[2].saturating_sub(tile[0]) as f32;
        let th = tile[3].saturating_sub(tile[1]) as f32;
        if tw > 0.0 && bfx != 0.0 {
            let lo = libm::floorf(tw * bfx) / tw;
            let hi = libm::ceilf(tw * bfx) / tw;
            // sk_ieee_float_divide 语义（lo=0 → +inf → 比较 false 取 hi）
            bfx = if bfx / lo < hi / bfx { lo } else { hi };
        }
        if th > 0.0 && bfy != 0.0 {
            let lo = libm::floorf(th * bfy) / th;
            let hi = libm::ceilf(th * bfy) / th;
            bfy = if bfy / lo < hi / bfy { lo } else { hi };
        }
        sw = libm::roundf(tw * bfx);
        sh = libm::roundf(th * bfy);
    }
    let tab = turbulence_table(seed);
    // 车道DA（字节级不变优化）：编码 memo——q 的 to_byte 入参完全由
    // (pm, a_byte) 两字节决定（pm = round(v·a_byte) ≤ a_byte ≤ 255），
    // linear 域 to_byte = lin2srgb_u8 逐次 powf，sample 实测 ~18% 全帧
    // → 64K 键预展开。建表成本 64K 次 to_byte，区域 ≥ 32K px 才启用
    // （阈值 = 建表/直算打平点 ~22K px 的整定）；sRGB 域 to_byte 已是
    // 直算恒等，不建表。memo 值 = 同一 to_byte 算式的结果 → 逐位一致。
    // y 侧 lattice 量（fry/smy/fly/cly）随行不变，外提到行循环的行不
    // 变量缓冲；x 侧 ratio/osx 倍增链保持逐像素运行局部（增量
    // ×0.5/×2.0 序列照抄原实现，序列值逐位一致）；通道求值走
    // [`turb_channel`]（累加器寄存器驻留）。所有浮点算式与算序与旧
    // 内圈逐项相同（重结合/换式都会变字节），只消冗余不换算式
    // ——u16→f32 梯度转换建表期完成见 turbulence_table。
    const ENCODE_MEMO_MIN_PX: usize = 32_768;
    let region_px = (sy1.min(h).saturating_sub(sy0)) * (sx1.min(w).saturating_sub(sx0));
    let mut memo: Option<alloc::boxed::Box<[u8; 65536]>> = None;
    if cs.linear && region_px >= ENCODE_MEMO_MIN_PX {
        let mut t = alloc::boxed::Box::new([0u8; 65536]);
        for (k, e) in t.iter_mut().enumerate() {
            let (pm, ab) = ((k >> 8) as u32, (k & 0xFF) as u32);
            *e = if ab == 0 {
                0
            } else {
                cs.to_byte(pm as f32 / ab as f32)
            };
        }
        memo = Some(t);
    }
    let n_oct = octaves as usize;
    // 行不变量缓冲（fry, smy, fly, cly）× octaves
    let mut row_inv = alloc::vec![(0.0f32, 0.0f32, 0.0f32, 0.0f32); n_oct];
    // 输出裁到 primitive 子区域（turb-08 实证 Chrome 同款；坐标仍相对
    // 滤镜区域原点）
    for y in sy0..sy1.min(h) {
        // 行级 ny/osy 倍增链（与逐像素增量算术逐位一致）
        let mut ny = (origin[1] + y as f32 + 1.0) * bfy;
        let mut osy = sh;
        for slot in row_inv.iter_mut() {
            let fy = turb_floor(ny);
            let fry = ny - fy;
            let (mut fly, mut cly) = (fy, fy + 1.0);
            if stitch {
                if fly >= osy {
                    fly -= osy;
                }
                if cly >= osy {
                    cly -= osy;
                }
            }
            *slot = (fry, fry * fry * (3.0 - 2.0 * fry), fly, cly);
            ny *= 2.0;
            osy *= 2.0;
        }
        for x in sx0..sx1.min(w) {
            // 采样坐标 = (设备像素 + 1.0) × 基频：Skia raster pipeline 的
            // fill 采样点在像素中心（x+0.5），stage 内再 +0.5——合计 +1.0
            //（turb-a 探针亚像素扫描实证：+0.5 假设 mean|Δ| 13.25，
            // +1.0 残差 0.47）。
            // 车道CN：设备像素 = 区域原点 + 区域局部坐标——规范以用户
            // 空间坐标为噪声参量，区域不在原点时漏加 origin 即整体错相
            //（filters-turb-01-f 六 tile 100% 错配根因）。
            let mut nx = (origin[0] + x as f32 + 1.0) * bfx;
            // 车道EF：aarch64/x86_64 上 4 通道累加器驻留 f32x4（lane =
            // R/G/B/A）；其余 target（riscv 等）保持 DA scalar 路径。
            #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
            let mut sum_v = svgine_simd::V4::splat(0.0);
            #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
            let mut sum = [0.0f32; 4];
            let mut ratio = 1.0f32;
            let mut osx = sw;
            for &(fry, smy, fly, cly) in row_inv.iter() {
                let fx = turb_floor(nx);
                let frx = nx - fx;
                let (mut flx, mut clx) = (fx, fx + 1.0);
                if stitch {
                    if flx >= osx {
                        flx -= osx;
                    }
                    if clx >= osx {
                        clx -= osx;
                    }
                }
                // lattice：一次 selector + y 求和后 &0xFF（Chrome 同款）
                let lx = tab.selector[((flx as i32) & 0xFF) as usize] as f32;
                let ly = tab.selector[((clx as i32) & 0xFF) as usize] as f32;
                let b00 = (((lx + fly) as i32) & 0xFF) as usize;
                let b10 = (((ly + fly) as i32) & 0xFF) as usize;
                let b01 = (((lx + cly) as i32) & 0xFF) as usize;
                let b11 = (((ly + cly) as i32) & 0xFF) as usize;
                let smx = frx * frx * (3.0 - 2.0 * frx);
                let abs_mode = ty == FeTurbType::Turbulence;
                // 车道EF：SIMD 臂——4 通道各占一 lane，单通道内算式与
                // 算序与 turb_channel 逐 op 一致（elementwise 指令 lane
                // 间无交互、无重结合、无 FMA 收缩）→ 字节级等价（tests
                // 钉死）。riscv 等其余架构走下方 scalar 原路径（DA 原文
                // 不动）。
                #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
                {
                    sum_v = svgine_simd::turb_channels_vec(
                        &tab.grad_v,
                        b00,
                        b10,
                        b01,
                        b11,
                        frx,
                        fry,
                        smx,
                        smy,
                        sum_v,
                        ratio,
                        abs_mode,
                    );
                }
                #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
                {
                    sum[0] = turb_channel(
                        &tab.grad[0],
                        b00,
                        b10,
                        b01,
                        b11,
                        frx,
                        fry,
                        smx,
                        smy,
                        sum[0],
                        ratio,
                        abs_mode,
                    );
                    sum[1] = turb_channel(
                        &tab.grad[1],
                        b00,
                        b10,
                        b01,
                        b11,
                        frx,
                        fry,
                        smx,
                        smy,
                        sum[1],
                        ratio,
                        abs_mode,
                    );
                    sum[2] = turb_channel(
                        &tab.grad[2],
                        b00,
                        b10,
                        b01,
                        b11,
                        frx,
                        fry,
                        smx,
                        smy,
                        sum[2],
                        ratio,
                        abs_mode,
                    );
                    sum[3] = turb_channel(
                        &tab.grad[3],
                        b00,
                        b10,
                        b01,
                        b11,
                        frx,
                        fry,
                        smx,
                        smy,
                        sum[3],
                        ratio,
                        abs_mode,
                    );
                }
                nx *= 2.0;
                osx *= 2.0;
                ratio *= 0.5;
            }
            // 车道EF：SIMD 累加器出环展开回标量（lane → 通道）。
            #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
            let sum: [f32; 4] = sum_v.to_array();
            // fractal：[-1,1] → [0,1]；turbulence：已是 [0,∞)
            let (vr, vg, vb, va) = if ty == FeTurbType::FractalNoise {
                (
                    sum[0] * 0.5 + 0.5,
                    sum[1] * 0.5 + 0.5,
                    sum[2] * 0.5 + 0.5,
                    sum[3] * 0.5 + 0.5,
                )
            } else {
                (sum[0], sum[1], sum[2], sum[3])
            };
            let o = (y * w + x) * 4;
            let a = va.clamp(0.0, 1.0);
            // Chrome 中转面 = premul-工作域 8888：alpha 字节量化后
            // pm_byte = round(v·a_byte)，直色 = pm_byte/a_byte。模拟该
            // 量化再编码（中低 alpha 噪声场的逐字节口径对齐，turb-01/03
            // tol2 93-100% → 见探针报告）。
            let a_byte = (a * 255.0 + 0.5) as u8;
            let q = |v: f32| -> u8 {
                if a_byte == 0 {
                    return 0;
                }
                let pm = (v.clamp(0.0, 1.0) * a_byte as f32 + 0.5) as u32;
                match &memo {
                    Some(t) => t[((pm << 8) | a_byte as u32) as usize],
                    None => cs.to_byte(pm as f32 / a_byte as f32),
                }
            };
            out[o] = q(vr);
            out[o + 1] = q(vg);
            out[o + 2] = q(vb);
            out[o + 3] = a_byte;
        }
    }
    out
}

/// feDisplacementMap（车道AQ）：in2 的 xch/ych 通道值（工作色域 [0,1]）
/// 折算位移 `scale·(c−0.5)`，in 位图重采样。采样口径 = **nearest**
///（round-half-up；Chrome/Skia SkDisplacementMapImageFilter kNearest
/// 实证，规范"推荐 bilinear"未采信——预言机裁决）。in2 读数按车道X
/// 工作色域换算（linear = EOTF 查表；premul 读数假设已实验否证——
/// disp-02/04 tol2 99.8%→88/92%）；A 通道选择子不换域（alpha 无
/// gamma）。in 的颜色空间不动（规范/Chrome 同款），出界 → 透明。
/// 已知残余：map 字节 ±LSB 量化差在 c≈0.5 处翻转 nearest 采样、
/// 硬边上放大成个别野点（disp-02/04 各 <0.2% 像素 >tol2）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
/// feImage 位图字节 → linear 工作域再解释 LUT（车道CN）。值 =
/// round(to_srgb(b/255)·255)：位图字节被当作 linear 值做输出编码。
#[cfg(feature = "filters")]
fn fe_img_as_linear_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let v = i as f32 / 255.0;
        let s = if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * libm::powf(v, 1.0 / 2.4) - 0.055
        };
        lut[i] = (s * 255.0 + 0.5) as u8;
        i += 1;
    }
    lut
}

/// 车道CN 实验开关：feConvolveMatrix 拟合域（true = sRGB 字节域直卷积）。
const CN_CONV_SRGB: bool = true;

/// feConvolveMatrix（车道CN）：工作色域直色卷积（RGBA 各通道独立，
/// 规范非预乘语义；preserve_alpha 时 alpha 不参与卷积直通）。out(x,y)
/// = Σ kernel[i·ox+j]·in(x−tx+j, y−ty+i) / divisor + bias，边缘按
/// edgeMode 取值。alpha 通道无 gamma：lut 恒等、量化不经 enc。
#[cfg(feature = "filters")]
#[allow(clippy::too_many_arguments)]
fn convolve_matrix(
    input: &[u8],
    w: usize,
    h: usize,
    kernel: &[f32],
    kox: u32,
    koy: u32,
    divisor: f32,
    bias: f32,
    tx: u32,
    ty: u32,
    edge: FeEdgeMode,
    preserve_alpha: bool,
    cs: FeColorCtx,
) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    if divisor.abs() < f32::EPSILON {
        return out;
    }
    let s2l = srgb_to_linear_table();
    let enc = |v: f32| -> u8 { cs.to_byte(v) };
    let sample_byte = |x: i32, y: i32, ch: usize| -> u8 {
        if x < 0 || x >= w as i32 || y < 0 || y >= h as i32 {
            return match edge {
                FeEdgeMode::Duplicate => {
                    let cx = x.clamp(0, w as i32 - 1) as usize;
                    let cy = y.clamp(0, h as i32 - 1) as usize;
                    input[(cy * w + cx) * 4 + ch]
                }
                FeEdgeMode::Wrap => {
                    let cx = x.rem_euclid(w as i32) as usize;
                    let cy = y.rem_euclid(h as i32) as usize;
                    input[(cy * w + cx) * 4 + ch]
                }
                FeEdgeMode::None => 0,
            };
        }
        input[(y as usize * w + x as usize) * 4 + ch]
    };
    let kox = kox as i32;
    let koy = koy as i32;
    let tx = tx as i32;
    let ty = ty as i32;
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            for ch in 0..4 {
                if preserve_alpha && ch == 3 {
                    out[o + 3] = input[o + 3];
                    continue;
                }
                let mut acc = 0.0f32;
                for i in 0..koy {
                    for j in 0..kox {
                        let kv = kernel[(i * kox + j) as usize];
                        if kv == 0.0 {
                            continue;
                        }
                        // Chrome/Skia = 相关（不翻转核）；规范公式含翻转。
                        // 存储基线逐值拟合：同一 kernel 翻转采样才与
                        // Chrome 逐位同向（emboss tile mean 47→对齐）。
                        let byte = sample_byte(
                            x as i32 - tx + (kox - 1 - j),
                            y as i32 - ty + (koy - 1 - i),
                            ch,
                        );
                        // 车道CN 实验：存储基线拟合判域（见函数注释）
                        acc += kv
                            * if ch == 3 || CN_CONV_SRGB {
                                f32::from(byte) / 255.0
                            } else {
                                cs.to_working(&s2l, byte)
                            };
                    }
                }
                if ch == 3 {
                    out[o + 3] = ((acc / divisor + bias).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                } else if CN_CONV_SRGB {
                    out[o + ch] = ((acc / divisor + bias).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                } else {
                    out[o + ch] = enc(acc / divisor + bias);
                }
            }
        }
    }
    out
}

/// feMorphology（车道CN）：premul 字节域逐通道腐蚀（最小）/扩张（最大），
/// 邻域 = (2rx+1)×(2ry+1) 矩形（含 alpha；规范 premul 语义）。r=0 退化
/// 单像素直通；结果回直色字节（链缓冲 = 直色约定）。
#[cfg(feature = "filters")]
fn morphology_bitmap(input: &[u8], w: usize, h: usize, dilate: bool, rx: i32, ry: i32) -> Vec<u8> {
    let mut out = alloc::vec![0u8; w * h * 4];
    let rx = rx.clamp(0, 64);
    let ry = ry.clamp(0, 64);
    if rx == 0 || ry == 0 {
        out.copy_from_slice(input);
        return out;
    }
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let mut acc = [if dilate { 0u8 } else { 255u8 }; 4];
            for dy in -ry..=ry {
                let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                for dx in -rx..=rx {
                    let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let so = (sy * w + sx) * 4;
                    for ch in 0..4 {
                        let p = input[so + ch];
                        acc[ch] = if dilate {
                            acc[ch].max(p)
                        } else {
                            acc[ch].min(p)
                        };
                    }
                }
            }
            out[o..o + 4].copy_from_slice(&acc);
        }
    }
    // premul 域 min/max → 直色字节
    for i in 0..w * h {
        let o = i * 4;
        let a = out[o + 3] as u32;
        if a == 0 {
            out[o] = 0;
            out[o + 1] = 0;
            out[o + 2] = 0;
        } else if a < 255 {
            for ch in 0..3 {
                let pm = out[o + ch] as u32;
                let straight = (pm * 255 + a / 2) / a;
                out[o + ch] = straight.min(255) as u8;
            }
        }
    }
    out
}

#[cfg(feature = "filters")]
#[allow(clippy::too_many_arguments)]
fn displacement_map(
    inp: &[u8],
    map: &[u8],
    w: usize,
    h: usize,
    scale_x: f32,
    scale_y: f32,
    xch: FeChannel,
    ych: FeChannel,
    cs: FeColorCtx,
) -> Vec<u8> {
    let s2l = srgb_to_linear_table();
    let chan = |mo: usize, c: FeChannel| -> f32 {
        match c {
            FeChannel::R => cs.to_working(&s2l, map[mo]),
            FeChannel::G => cs.to_working(&s2l, map[mo + 1]),
            FeChannel::B => cs.to_working(&s2l, map[mo + 2]),
            // alpha 无 gamma：恒 /255（to_working 的 linear 分支勿用）
            FeChannel::A => f32::from(map[mo + 3]) / 255.0,
        }
    };
    let mut out = alloc::vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let mo = (y * w + x) * 4;
            let fx = x as f32 + scale_x * (chan(mo, xch) - 0.5);
            let fy = y as f32 + scale_y * (chan(mo, ych) - 0.5);
            let ix = libm::floorf(fx + 0.5) as i64;
            let iy = libm::floorf(fy + 0.5) as i64;
            if ix < 0 || iy < 0 || ix >= w as i64 || iy >= h as i64 {
                continue; // 出界透明
            }
            let so = (iy as usize * w + ix as usize) * 4;
            out[mo..mo + 4].copy_from_slice(&inp[so..so + 4]);
        }
    }
    out
}

/// 光照法线（车道AQ）：alpha 高度图 + Sobel 3×3（规范 9 案例内核表，
/// dx=dy=1 设备像素）。返回已归一化 (nx, ny, nz)。kernelUnitLength ≠ 1
/// 近似为 1（登记 CONFORMANCE）；单像素轴无梯度信息 → (0,0,1)。
#[cfg(feature = "filters")]
fn surface_normal(input: &[u8], w: usize, h: usize, x: usize, y: usize, ss: f32) -> [f32; 3] {
    if w == 1 || h == 1 {
        return [0.0, 0.0, 1.0];
    }
    let (kx, ky, fax, fay) = light_kernel(w, h, x, y);
    // 高度采样（alpha/255，无 gamma）；kernel 行 = y 偏移、列 = x 偏移
    let ih = |ix: isize, iy: isize| -> f32 {
        f32::from(input[(iy as usize * w + ix as usize) * 4 + 3]) / 255.0
    };
    let mut gx = 0.0f32;
    let mut gy = 0.0f32;
    for j in 0..3isize {
        for i in 0..3isize {
            let (kvx, kvy) = (kx[j as usize][i as usize], ky[j as usize][i as usize]);
            if kvx != 0.0 || kvy != 0.0 {
                let v = ih(x as isize + i - 1, y as isize + j - 1);
                gx += kvx * v;
                gy += kvy * v;
            }
        }
    }
    // N = −ss·FACTOR·(Σ K·I)，z = 1 → 归一化
    let nx = -ss * fax * gx;
    let ny = -ss * fay * gy;
    let len = libm::sqrtf(nx * nx + ny * ny + 1.0);
    [nx / len, ny / len, 1.0 / len]
}

/// Sobel 3×3 内核类型（行 = y、列 = x）。
#[cfg(feature = "filters")]
type Kernel = [[f32; 3]; 3];

/// 规范 9 案例光照内核（SVG1.1 15.14 表）：返回 (Kx, Ky, FACTORx,
/// FACTORy)。角案例内核只触界内 2×2 块、边案例只触 2×3/3×2——天然无
/// 出界访问。
#[cfg(feature = "filters")]
fn light_kernel(w: usize, h: usize, x: usize, y: usize) -> (Kernel, Kernel, f32, f32) {
    const KX_TL: Kernel = [[0.0, 0.0, 0.0], [0.0, -2.0, 2.0], [0.0, -1.0, 1.0]];
    const KY_TL: Kernel = [[0.0, 0.0, 0.0], [0.0, -2.0, -1.0], [0.0, 2.0, 1.0]];
    const KX_TOP: Kernel = [[0.0, 0.0, 0.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
    const KY_TOP: Kernel = [[0.0, 0.0, 0.0], [-1.0, -2.0, -1.0], [1.0, 2.0, 1.0]];
    const KX_TR: Kernel = [[0.0, 0.0, 0.0], [-2.0, 2.0, 0.0], [-1.0, 1.0, 0.0]];
    const KY_TR: Kernel = [[0.0, 0.0, 0.0], [-1.0, -2.0, 0.0], [1.0, 2.0, 0.0]];
    const KX_LEFT: Kernel = [[0.0, -1.0, 1.0], [0.0, -2.0, 2.0], [0.0, -1.0, 1.0]];
    const KY_LEFT: Kernel = [[0.0, -2.0, -1.0], [0.0, 0.0, 0.0], [0.0, 2.0, 1.0]];
    const KX_INT: Kernel = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
    const KY_INT: Kernel = [[-1.0, -2.0, -1.0], [0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];
    const KX_RIGHT: Kernel = [[-1.0, 1.0, 0.0], [-2.0, 2.0, 0.0], [-1.0, 1.0, 0.0]];
    const KY_RIGHT: Kernel = [[-1.0, -2.0, 0.0], [0.0, 0.0, 0.0], [1.0, 2.0, 0.0]];
    const KX_BL: Kernel = [[0.0, -1.0, 1.0], [0.0, -2.0, 2.0], [0.0, 0.0, 0.0]];
    const KY_BL: Kernel = [[0.0, -2.0, -1.0], [0.0, 2.0, 1.0], [0.0, 0.0, 0.0]];
    const KX_BOT: Kernel = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [0.0, 0.0, 0.0]];
    const KY_BOT: Kernel = [[-1.0, -2.0, -1.0], [1.0, 2.0, 1.0], [0.0, 0.0, 0.0]];
    const KX_BR: Kernel = [[-1.0, 1.0, 0.0], [-2.0, 2.0, 0.0], [0.0, 0.0, 0.0]];
    const KY_BR: Kernel = [[-1.0, -2.0, 0.0], [1.0, 2.0, 0.0], [0.0, 0.0, 0.0]];

    let (left, right, top, bottom) = (x == 0, x + 1 == w, y == 0, y + 1 == h);
    match (top, bottom, left, right) {
        (true, _, true, _) => (KX_TL, KY_TL, 2.0 / 3.0, 2.0 / 3.0),
        (true, _, _, true) => (KX_TR, KY_TR, 2.0 / 3.0, 2.0 / 3.0),
        (true, _, _, _) => (KX_TOP, KY_TOP, 1.0 / 3.0, 1.0 / 2.0),
        (_, true, true, _) => (KX_BL, KY_BL, 2.0 / 3.0, 2.0 / 3.0),
        (_, true, _, true) => (KX_BR, KY_BR, 2.0 / 3.0, 2.0 / 3.0),
        (_, true, _, _) => (KX_BOT, KY_BOT, 1.0 / 3.0, 1.0 / 2.0),
        (_, _, true, _) => (KX_LEFT, KY_LEFT, 1.0 / 2.0, 1.0 / 3.0),
        (_, _, _, true) => (KX_RIGHT, KY_RIGHT, 1.0 / 2.0, 1.0 / 3.0),
        _ => (KX_INT, KY_INT, 1.0 / 4.0, 1.0 / 4.0),
    }
}

/// 单像素光照采样结果：L（表面→光源单位向量）+ 光色（工作域 [0,1]，
/// spot 锥衰减已乘）。
#[cfg(feature = "filters")]
struct LightSample {
    l: [f32; 3],
    lc: [f32; 3],
}

/// 光源求值（车道AQ；SVG1.1 15.14/15.15 公式）：像素点 (px,py,pz)（z =
/// surfaceScale·alpha）→ L 归一；spot 按 −L·S 幂衰减 + 锥体裁剪。
#[cfg(feature = "filters")]
fn sample_light(light: &FeLight, px: f32, py: f32, pz: f32, color: [f32; 3]) -> LightSample {
    const D2R: f32 = core::f32::consts::PI / 180.0;
    match *light {
        FeLight::Distant { az, el } => {
            let (azr, elr) = (az * D2R, el * D2R);
            LightSample {
                l: [
                    libm::cosf(azr) * libm::cosf(elr),
                    libm::sinf(azr) * libm::cosf(elr),
                    libm::sinf(elr),
                ],
                lc: color,
            }
        }
        FeLight::Point { x, y, z } => {
            let d = [x - px, y - py, z - pz];
            let n = libm::sqrtf(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
            let inv = if n > 0.0 { 1.0 / n } else { 0.0 };
            LightSample {
                l: [d[0] * inv, d[1] * inv, d[2] * inv],
                lc: color,
            }
        }
        FeLight::Spot {
            x,
            y,
            z,
            pax,
            pay,
            paz,
            exp,
            cone,
        } => {
            let d = [x - px, y - py, z - pz];
            let n = libm::sqrtf(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
            let inv = if n > 0.0 { 1.0 / n } else { 0.0 };
            let l = [d[0] * inv, d[1] * inv, d[2] * inv];
            // S：光源 → 照准点单位向量
            let s = [pax - x, pay - y, paz - z];
            let sn = libm::sqrtf(s[0] * s[0] + s[1] * s[1] + s[2] * s[2]);
            let sinv = if sn > 0.0 { 1.0 / sn } else { 0.0 };
            // dots = −L·S（< 0 = 背光无光；< cos(cone) = 锥外无光）
            let dots = -(l[0] * s[0] * sinv + l[1] * s[1] * sinv + l[2] * s[2] * sinv);
            // SkSL $spotlight_scale（Skia 主分支同款）：锥内幂衰减 + 锥缘
            // 0.016 cos 宽度的 AA 斜坡（cosAngle ∈ [cutoff, cutoff+0.016]
            // 线性升入）。
            const CONE_AA: f32 = 0.016;
            let att = match cone.map(libm::cosf) {
                Some(cc) if dots < cc => 0.0,
                Some(cc) => {
                    let p = libm::powf(dots.max(0.0), exp);
                    if dots < cc + CONE_AA {
                        p * (dots - cc) / CONE_AA
                    } else {
                        p
                    }
                }
                None => libm::powf(dots.max(0.0), exp),
            };
            LightSample {
                l,
                lc: [color[0] * att, color[1] * att, color[2] * att],
            }
        }
    }
}

/// feDiffuseLighting（车道AQ）：Phong 漫反射。Dr = kd·(N·L)·Lr（N·L 负
/// 值钳 0——表面背光），输出 alpha = 1 不透明。光色 sRGB → 工作色域
///（车道X），求值后编码回 sRGB 字节。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
fn lighting_diffuse(
    input: &[u8],
    w: usize,
    h: usize,
    light: FeLight,
    surface_scale: f32,
    kd: f32,
    color: Rgba8,
    cs: FeColorCtx,
) -> Vec<u8> {
    let s2l = srgb_to_linear_table();
    let lc_w = [
        cs.to_working(&s2l, color.r),
        cs.to_working(&s2l, color.g),
        cs.to_working(&s2l, color.b),
    ];
    let mut out = alloc::vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let io = (y * w + x) * 4;
            let pz = surface_scale * f32::from(input[io + 3]) / 255.0;
            let n = surface_normal(input, w, h, x, y, surface_scale);
            let ls = sample_light(&light, x as f32, y as f32, pz, lc_w);
            let nl = (n[0] * ls.l[0] + n[1] * ls.l[1] + n[2] * ls.l[2]).max(0.0);
            let o = io;
            for ch in 0..3 {
                out[o + ch] = cs.to_byte((kd * nl * ls.lc[ch]).clamp(0.0, 1.0));
            }
            out[o + 3] = 255;
        }
    }
    out
}

/// feSpecularLighting（车道AQ）：Phong 镜面。H = (L + (0,0,1))/|L+ẑ|，
/// coeff = pow(max(0,N·H), exp)。**Chrome/Skia 数值口径**（ltg-y/z 矩阵
/// 探针 + SkLightingImageFilter/SkSL 实证）：
/// 1) k（specularConstant）与光色**前置相乘**成 uniform（colorScale =
///    k/255），光色先换工作域再字节量化（Blink 边界出字节）；
/// 2) rgb = saturate(coeff·uniform)、alpha = max(rgb)——SkSL 以 premul
///    契约输出 `half4(color, max(rgb))`，故表面 premul == alpha（直线色
///    = 白），**非**规范直色 = coeff·lighting-color；
/// 3) alpha 字节不 gamma 编码；本链直色 RGBA 约定下直色字节 = v/max
///    （uniform 光色恒 255），设备/链内合成各自乘回 alpha 后与 Chrome
///    premul 浮点中转面同式（ltg-03 链式探针 210/148 逐字节复算吻合）。
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "filters")]
fn lighting_specular(
    input: &[u8],
    w: usize,
    h: usize,
    light: FeLight,
    surface_scale: f32,
    ks: f32,
    exp: f32,
    color: Rgba8,
    cs: FeColorCtx,
) -> Vec<u8> {
    let s2l = srgb_to_linear_table();
    // 光色 → 工作域 → 字节量化 → k 前置（Skia uniform 口径）
    let q = |b: u8| -> f32 { f32::from((cs.to_working(&s2l, b) * 255.0 + 0.5) as u8) / 255.0 };
    let lc_w = [q(color.r) * ks, q(color.g) * ks, q(color.b) * ks];
    let mut out = alloc::vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let io = (y * w + x) * 4;
            let pz = surface_scale * f32::from(input[io + 3]) / 255.0;
            let n = surface_normal(input, w, h, x, y, surface_scale);
            let ls = sample_light(&light, x as f32, y as f32, pz, lc_w);
            // H = (L + ẑ)/|L + ẑ|
            let hl = libm::sqrtf(
                ls.l[0] * ls.l[0] + ls.l[1] * ls.l[1] + (ls.l[2] + 1.0) * (ls.l[2] + 1.0),
            );
            let nh = if hl > 0.0 {
                (n[0] * ls.l[0] + n[1] * ls.l[1] + n[2] * (ls.l[2] + 1.0)) / hl
            } else {
                0.0
            };
            let coeff = libm::powf(nh.max(0.0), exp);
            let o = io;
            let mut sa = 0.0f32;
            for ch in 0..3 {
                let v = (coeff * ls.lc[ch]).clamp(0.0, 1.0);
                sa = sa.max(v);
            }
            // 链缓冲 = 直色 RGBA。Chrome premul == alpha ⇒ 直色 = 白
            //（uniform 光色恒 255；非 uniform 按比例 v/max）。alpha 字节
            // 不 gamma 编码。设备合成 = 255·a + dst·(1−a)（Chrome 实证
            // ltg-x/z 探针）；链内合成由 blend_fe/composite_pd 反预乘
            // 恢复 premul = a·白。
            let a_byte = (sa * 255.0 + 0.5) as u8;
            if sa > 0.0 {
                for ch in 0..3 {
                    let v = (coeff * ls.lc[ch]).clamp(0.0, 1.0);
                    out[o + ch] = cs.to_byte(v / sa);
                }
            }
            out[o + 3] = a_byte;
        }
    }
    out
}

/// feComposite：Porter-Duff。`s` = in（源）、`d` = in2（目标）。
/// **工作色域随 `cs`**（车道X；linearRGB = color-interpolation-filters
/// 规范缺省，与同文件 blend_fe 同款——车道U a1 矩阵实证 Chrome/Skia
/// 同为 premul-linear 域求值；sRGB 属性声明 = 恒等映射直算，车道V 改
/// 造前行为）。直色 sRGB 进出契约：输入 sRGB→工作域→预乘，各算子
/// F/FS/FD 系数按 SVG/Cairo 惯例求值后反预乘、逆转换回 sRGB 直色输出
///（预乘合成由消费方 blend_px 做）。
#[cfg(feature = "filters")]
fn composite_pd(
    s: &[u8],
    d: &[u8],
    w: usize,
    h: usize,
    op: FeCompositeOp,
    cs: FeColorCtx,
) -> Vec<u8> {
    let s2l = srgb_to_linear_table();
    // 直色 sRGB → premul 工作域（α=0 视为全 0，不读 RGB——透明像素
    // 直色值未定义）。
    let pm_lin = |px: &[u8], off: usize, a: f32| -> [f32; 3] {
        let mut c = [0.0f32; 3];
        if a > 0.0 {
            for (ch, v) in c.iter_mut().enumerate() {
                *v = cs.to_working(&s2l, px[off + ch]) * a;
            }
        }
        c
    };
    let mut out = alloc::vec![0u8; w * h * 4];
    for i in 0..w * h {
        let (o, so, doff) = (i * 4, i * 4, i * 4);
        let sa = s[so + 3] as f32 / 255.0;
        let da = d[doff + 3] as f32 / 255.0;
        if let FeCompositeOp::Arithmetic { k1, k2, k3, k4 } = op {
            // premul-linear 域：i = k1·i1·i2 + k2·i1 + k3·i2 + k4。
            // v 与合成 alpha 各自独立钳 [0,1]（Chrome/Skia 同款，车道U
            // a1-18 探针实证；k4 空区铺设随 α 同式生效）。
            let oa = out_a_arith(sa, da, k1, k2, k3, k4);
            if oa <= 0.0 {
                continue;
            }
            let spm = pm_lin(s, so, sa);
            let dpm = pm_lin(d, doff, da);
            for ch in 0..3 {
                let v = (k1 * spm[ch] * dpm[ch] + k2 * spm[ch] + k3 * dpm[ch] + k4).clamp(0.0, 1.0);
                // 直色输出：反预乘 v/oa 后逆转换（消费方 blend_px 预乘）
                out[o + ch] = cs.to_byte(v / oa);
            }
            out[o + 3] = (oa * 255.0 + 0.5) as u8;
            continue;
        }
        // (F, FS, FD)：out_a = F；out_pm_lin = FS·S_pm + FD·D_pm
        let (f, fs, fd) = match op {
            FeCompositeOp::Over => (sa + da * (1.0 - sa), 1.0, 1.0 - sa),
            FeCompositeOp::In => (sa * da, da, 0.0),
            FeCompositeOp::Out => (sa * (1.0 - da), 1.0 - da, 0.0),
            FeCompositeOp::Atop => (da, da, 1.0 - sa),
            FeCompositeOp::Xor => (sa + da - 2.0 * sa * da, 1.0 - da, 1.0 - sa),
            FeCompositeOp::Arithmetic { .. } => unreachable!(),
        };
        if f <= 0.0 {
            continue;
        }
        let spm = pm_lin(s, so, sa);
        let dpm = pm_lin(d, doff, da);
        let inv = 1.0 / f;
        for ch in 0..3 {
            let pm = fs * spm[ch] + fd * dpm[ch];
            out[o + ch] = cs.to_byte(pm * inv);
        }
        out[o + 3] = (f * 255.0 + 0.5) as u8;
    }
    out
}

/// 滤镜结果合成：区域位图按 `opacity` src-over 写进目标（层栈已由
/// 调用方解析）；引用元素的 clip 覆盖（矢量积）与亮度蒙版因子按
/// 画布坐标乘进有效 alpha（与 Fill 的像素模型一致）。
/// hwB 行带一期：`buf_y0`/`buf_rows` = 输出缓冲的可写行窗（帧坐标），
/// 语义与 [`fill_path`] 同名参数一致——全帧调用传 `(0, h)` 逐字节不变。
#[cfg(feature = "filters")]
#[allow(clippy::too_many_arguments)]
fn composite_filter(
    fb: &FilterBmp,
    opacity: f32,
    dst: &mut [u8],
    vp: &Viewport,
    clip: Option<&ClipCtx<'_>>,
    mask: Option<&[u8]>,
    dirty: Option<&mut [usize; 4]>,
    buf_y0: usize,
    buf_rows: usize,
) {
    if fb.w == 0 || fb.h == 0 {
        return;
    }
    let w = vp.width as usize;
    let h = vp.height as usize;
    // 层脏区记账：区域矩形 ∩ 画布（保守超集）∩ band 窗（y 转
    // band-local）。区域整段在窗上/窗下时区间为空，union_rect 跳过
    // （与原「空区域跳过」同路）；全帧调用 buf_y0=0 两钳空转。
    let x0 = fb.ox.max(0) as usize;
    let y0 = fb.oy.max(0) as usize;
    let x1 = (fb.ox + fb.w as i32).min(w as i32).max(0) as usize;
    let y1 = (fb.oy + fb.h as i32).min(h as i32).max(0) as usize;
    let yb0 = y0.clamp(buf_y0, buf_y0 + buf_rows) - buf_y0;
    let yb1 = y1.min(buf_y0 + buf_rows).saturating_sub(buf_y0);
    if let Some(d) = dirty {
        dirty_union_rect(d, &[x0, yb0, x1, yb1]);
    }
    let fw = fb.w as usize;
    for py in 0..fb.h as usize {
        let cy = fb.oy + py as i32;
        if cy < 0 || cy >= h as i32 {
            continue;
        }
        let cyu = cy as usize;
        if cyu < buf_y0 || cyu >= buf_y0 + buf_rows {
            continue; // 行在 band 窗外：整行不触碰
        }
        for pxf in 0..fw {
            let cx = fb.ox + pxf as i32;
            if cx < 0 || cx >= w as i32 {
                continue;
            }
            let so = (py * fw + pxf) * 4;
            let mut a = fb.px[so + 3] as f32 / 255.0 * opacity;
            if a <= 0.0 {
                continue;
            }
            let ci = cyu * w + cx as usize;
            if let Some(cm) = clip.and_then(|c| c.cov) {
                let k = cm[ci];
                if k == 0 {
                    continue;
                }
                a *= k as f32 / 255.0;
            }
            if let Some(mf) = mask {
                let k = mf[ci];
                if k == 0 {
                    continue;
                }
                a *= k as f32 / 255.0;
            }
            if a <= 0.0 {
                continue;
            }
            let c = Rgba8 {
                r: fb.px[so],
                g: fb.px[so + 1],
                b: fb.px[so + 2],
                a: 255,
            };
            let o = ((cyu - buf_y0) * w + cx as usize) * 4;
            blend_px(&mut dst[o..o + 4], c, a);
        }
    }
}

/// 裁切链解析缓存项：链上全部 bounds 的交 + 矢量覆盖积 + 矩形链内区。
type ChainEntry = ([f32; 4], Option<Vec<u8>>, Option<RectInterior>);

/// 车道OL（perf-round3 刀①）：all-rect 裁切链的**内区**（像素域，
/// 半开区间 cols [x0,x1) × rows [y0,y1)）——链上每枚矩形裁切的
/// 覆盖在内区恒为 255（证明见 [`rect_interior_of`]），覆盖积在此
/// 区域恒 ≡255 → fill 行末可按「无逐像素蒙版」口径恢复满段直写。
#[derive(Clone, Copy, Debug)]
struct RectInterior {
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

impl RectInterior {
    fn intersect(self, o: Self) -> Self {
        Self {
            x0: self.x0.max(o.x0),
            x1: self.x1.min(o.x1),
            y0: self.y0.max(o.y0),
            y1: self.y1.min(o.y1),
        }
    }
}

/// 车道OL：单轴对齐矩形（设备空间 bounds）在 aa>1 覆盖口径下的
/// 内区。clip_coverage 的累计结构：子行谓词用**原始**矩形 y 边
/// （y0 ≤ yc < y1，yc = y+(sub+0.5)·0.25），span 端点在 aa=2 吸附
/// 1/4 网格（roundf(x·4)·0.25）、aa≥3 浮点直通；像素 ov ≥ 1.0
/// ⟺ px ≥ L' ∧ px+1 ≤ H'（L'/H' = 吸附后 span 端点；由
/// ov = min(px+1,H') − max(px,L') 分四种位置情形展开即得，端点为
/// 1/4 网格值/整数像素比较，浮点精确无边界歧义）。故：
///   - 列 px 内区 ⟺ px ≥ L' ∧ px+1 ≤ H'
///     ⟺ px ∈ [ceil(L'), floor(H'))——注意右端是 **floor** 不是
///     ceil：H' 落在 1/4 网格非整数点（如 8.75）时 px=floor(H') 列的
///     ov = H'−px < 1（首跑 3 文件 DIFF 的根因，已修）；
///   - 行 y 内区（4 子行全中）⟺ y+0.125 ≥ y0 ∧ y+0.875 < y1
///     ⟺ y ∈ [ceil(y0−0.125), ceil(y1−0.875))；
///   - 内区像素每子行恰加 full（64/64/64/63）→ 累计恰 255.0 →
///     cov 字节恰 255；内区外任一子行缺席/任一列 ov<1 → 累计
///     严格 < 255（部分行 ≤ 64·3 = 192，部分列 ≤ ov·64·3+63 < 255）。
///
/// 即 `cm == 255 ⟺ 像素在内区`，双向成立。clamp 到视口后空区
/// （x0 ≥ x1 / y0 ≥ y1）由消费端 range 判空跳过。
#[cfg(feature = "clip-path")]
fn rect_interior_of(b: [f32; 4], vp: &Viewport) -> RectInterior {
    let aa = vp.aa.max(1);
    let snap = |v: f32| {
        if aa == 2 {
            libm::roundf(v * 4.0) * 0.25
        } else {
            v
        }
    };
    RectInterior {
        x0: (libm::ceilf(snap(b[0])) as i32).max(0),
        x1: (libm::floorf(snap(b[2])) as i32).min(vp.width as i32),
        y0: (libm::ceilf(b[1] - 0.125) as i32).max(0),
        y1: (libm::ceilf(b[3] - 0.875) as i32).min(vp.height as i32),
    }
}

/// 车道OL（perf-round3 刀①）：PathClip 是否为**单个轴对齐矩形环**。
/// 判定：点池无 NaN（= 单环，多子形状含环分隔一律 false）、剥掉闭合
/// 补点（poly_points closed=true 惯例：末点 == 首点）后恰 4 顶点、
/// 顺次 4 边水平/垂直交替且非零长。四条非零长轴对齐边首尾相接
/// ⟹ 四角必为 (x0,y0)(x1,y0)(x1,y2)(x0,y2) 型简单矩形（x 向两值、
/// y 向两值各出现两次），非零/偶奇规则下内部区域同为此矩形。
/// `<clipPath><rect/></clipPath>`（matplotlib/gh_flag 系海量形态）
/// 经 `poly_points("0,0 1,0 1,1 0,1", T·S(w,h), true)` 烘焙后恒命中；
/// 真曲线/多环/斜切变换/退化（零长边）一律 false 走原覆盖路径。
#[cfg(feature = "clip-path")]
fn clip_is_single_rect(dl: &DisplayList, pc: &PathClip) -> bool {
    let pts = dl.path_points(&pc.d);
    if pts.len() < 8 || pts.len() % 2 != 0 {
        return false;
    }
    let mut n = pts.len() / 2;
    for &v in pts {
        if !v.is_finite() {
            return false; // 环分隔 / 缺失坐标
        }
    }
    if n >= 5 && pts[0] == pts[pts.len() - 2] && pts[1] == pts[pts.len() - 1] {
        n -= 1; // 剥闭合补点
    }
    if n != 4 {
        return false;
    }
    let x = [pts[0], pts[2], pts[4], pts[6]];
    let y = [pts[1], pts[3], pts[5], pts[7]];
    for i in 0..4 {
        let j = (i + 1) & 3;
        // 轴对齐 = 恰一维相等；零长边（两维全等）与斜边（全不等）同拒
        if (x[i] == x[j]) == (y[i] == y[j]) {
            return false;
        }
    }
    true
}

/// 解析裁切链（head 起沿 parent 到 NO_CLIP）：bounds 取交、矢量覆盖取积，
/// 结果缓存进 `chains[head]`。已缓存 / 索引越界（防御坏 DL）直接返回。
#[cfg_attr(not(feature = "clip-path"), allow(unused_variables))]
fn ensure_chain(dl: &DisplayList, head: usize, vp: &Viewport, chains: &mut [Option<ChainEntry>]) {
    if head >= dl.clips.len() || chains[head].is_some() {
        return;
    }
    let mut rect = [f32::MIN, f32::MIN, f32::MAX, f32::MAX];
    #[cfg_attr(not(feature = "clip-path"), allow(unused_mut))]
    let mut product: Option<Vec<u8>> = None;
    // 车道OL：all-rect 链的内区（cm≡255 域，供 fill 行末直写恢复）；
    // 链上出现任一非矩形矢量裁切即作废（混合链边界带 = 矩形×路径
    // 覆盖积，逐像素回退）。
    #[cfg_attr(not(feature = "clip-path"), allow(unused_mut))]
    let mut interior: Option<RectInterior> = None;
    #[cfg_attr(not(feature = "clip-path"), allow(unused_mut))]
    let mut all_rect = true;
    let mut cur = head as u32;
    let mut guard = 0u32; // 防御手写 DL 的 parent 环
    while cur != NO_CLIP && guard < 64 {
        guard += 1;
        let cur_us = cur as usize;
        if cur_us >= dl.clips.len() {
            break;
        }
        let def: &ClipDef = &dl.clips[cur_us];
        rect = [
            rect[0].max(def.bounds[0]),
            rect[1].max(def.bounds[1]),
            rect[2].min(def.bounds[2]),
            rect[3].min(def.bounds[3]),
        ];
        // feature `clip-path` 关闭：clips 池只有矩形视口裁切（path 恒
        // None），矢量覆盖积分支缺席——行为与空池等价。
        #[cfg(feature = "clip-path")]
        if let Some(pc) = &def.path {
            let single_rect = clip_is_single_rect(dl, pc);
            if single_rect && vp.aa.max(1) == 1 {
                // 刀①a：aa=1（像素中心 0/255 硬门控）语境下，单轴对齐
                // 矩形环的覆盖与 ClipCtx 的 rect 限域（行区间钳制 +
                // span 端点钳到 [cx0,cx1] 后按像素中心落域采样）逐像素
                // 同谓词——跳过整幅覆盖光栅化与覆盖积，输出字节级零
                // 回归（谓词对照见 tests/oracle/reports/perf-round3-ol.md）。
            } else {
                let cov = clip_coverage(dl, pc, vp);
                match &mut product {
                    None => product = Some(cov),
                    Some(p) => {
                        // 覆盖积：(k/255)·(c/255)·255 = k·c/255（u16 中间量防溢出）
                        for (dst, k) in p.iter_mut().zip(cov.iter()) {
                            *dst = (*dst as u16 * *k as u16 / 255) as u8;
                        }
                    }
                }
                // 刀①b：aa>1 时矩形覆盖在 [rect_interior_of] 内区恒
                // 255（边界带仍需缓冲）——记录内区供 fill 直写恢复；
                // 覆盖缓冲照常构建，字节输出不受影响。
                if single_rect {
                    let ri = rect_interior_of(def.bounds, vp);
                    interior = Some(match interior {
                        Some(acc) => acc.intersect(ri),
                        None => ri,
                    });
                } else {
                    all_rect = false;
                    interior = None;
                }
            }
        }
        cur = def.parent;
    }
    #[cfg(feature = "clip-path")]
    let interior = if all_rect { interior } else { None };
    #[cfg(not(feature = "clip-path"))]
    let interior = None;
    chains[head] = Some((rect, product, interior));
}

/// 裁切路径 → 每像素覆盖 0..255 的整幅蒙版。aa=1：像素中心采样
/// （0/255 硬边）；aa>1：与 fill_path 同款的解析式覆盖（4 子行解析
/// 累计，边界部分覆盖；aa≥3 精档 = 浮点端点去吸附，车道KC）。
#[cfg(feature = "clip-path")]
fn clip_coverage(dl: &DisplayList, pc: &PathClip, vp: &Viewport) -> Vec<u8> {
    let aa_fine = vp.aa.max(1) > 2;
    let w = vp.width as usize;
    let h = vp.height as usize;
    let mut cov = alloc::vec![0u8; w * h];
    if w == 0 || h == 0 {
        return cov;
    }
    let pts = dl.path_points(&pc.d);
    let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new();
    let mut x_all_finite = true;
    let mut xmin = f32::MAX;
    let mut xmax = f32::MIN;
    collect_edges(pts, &mut edges, &mut x_all_finite, &mut xmin, &mut xmax);
    if edges.is_empty() {
        return cov;
    }
    let y_min = (libm::floorf(edges.iter().map(|e| e.0).fold(f32::MAX, f32::min)).max(0.0)) as i32;
    let y_max =
        (libm::ceilf(edges.iter().map(|e| e.1).fold(f32::MIN, f32::max)).min(h as f32)) as i32;
    let rule = pc.rule;
    let is_in = |wind: i32| match rule {
        FillRule::NonZero => wind != 0,
        FillRule::EvenOdd => wind % 2 != 0,
    };
    let mut xs: Vec<(f32, i32)> = Vec::new();
    let mut row_acc: Vec<f32> = Vec::new();
    for y in y_min.max(0)..y_max.max(0) {
        let row = y as usize * w;
        if vp.aa.max(1) == 1 {
            // ── 像素中心采样：中心落在 span 内 → 255 ──
            let yc = y as f32 + 0.5;
            xs.clear();
            for &(y0, y1, x0, slope, dir) in &edges {
                if y0 <= yc && yc < y1 {
                    xs.push((x0 + slope * (yc - y0), dir));
                }
            }
            sort_scan_x(&mut xs);
            let mut wind = 0i32;
            let mut span_start: Option<f32> = None;
            for &(x, dir) in &xs {
                wind += if rule == FillRule::NonZero {
                    dir
                } else if dir > 0 {
                    1
                } else {
                    -1
                };
                let now_in = is_in(wind);
                match (span_start, now_in) {
                    (None, true) => span_start = Some(x),
                    (Some(sx), false) => {
                        span_start = None;
                        // 中心 px+0.5 ∈ [sx, x)
                        let px0 = (libm::ceilf(sx - 0.5) as i32).max(0);
                        let px1 = (libm::ceilf(x - 0.5) as i32).min(w as i32);
                        for px in px0.max(0)..px1.max(0) {
                            cov[row + px as usize] = 255;
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        // ── 解析式覆盖（与 fill_path 的 aa>1 路径同构）──
        row_acc.clear();
        row_acc.resize(w, 0.0);
        for sub in 0..4i32 {
            let yc = y as f32 + (sub as f32 + 0.5) * 0.25;
            xs.clear();
            for &(y0, y1, x0, slope, dir) in &edges {
                if y0 <= yc && yc < y1 {
                    xs.push((x0 + slope * (yc - y0), dir));
                }
            }
            sort_scan_x(&mut xs);
            let full = if sub < 3 { 64.0 } else { 63.0 };
            let mut wind = 0i32;
            let mut span_start: Option<f32> = None;
            for &(x, dir) in &xs {
                wind += if rule == FillRule::NonZero {
                    dir
                } else if dir > 0 {
                    1
                } else {
                    -1
                };
                let now_in = is_in(wind);
                match (span_start, now_in) {
                    (None, true) => span_start = Some(x),
                    (Some(sx), false) => {
                        span_start = None;
                        // legacy 档吸附 1/4 网格（对齐参考实现的 fdot16 取
                        // 整）；精档（车道KC）浮点直通——吸附每端点
                        // ±0.125px = ±32LSB 覆盖误差，是 24px 图标 tol32
                        // 细环桶的主误差源（8x 收敛探针实证）。
                        let (lo, hi) = if aa_fine {
                            (sx, x)
                        } else {
                            (libm::roundf(sx * 4.0) * 0.25, libm::roundf(x * 4.0) * 0.25)
                        };
                        if hi <= lo {
                            continue;
                        }
                        let b0 = (libm::floorf(lo) as i32).max(0);
                        let b1 = (libm::floorf(hi) as i32).min(w as i32 - 1);
                        for px in b0..=b1 {
                            let pxf = px as f32;
                            let ov = (pxf + 1.0).min(hi) - pxf.max(lo);
                            if ov <= 0.0 {
                                continue;
                            }
                            let add = if ov >= 1.0 { full } else { ov * 64.0 };
                            let c = &mut row_acc[px as usize];
                            *c = (*c + add).min(255.0);
                        }
                    }
                    _ => {}
                }
            }
        }
        for (px, &acc) in row_acc.iter().enumerate() {
            if acc > 0.0 {
                cov[row + px] = acc.min(255.0) as u8;
            }
        }
    }
    cov
}

/// gradient 关闭时的 span 求值器哑类型（ADR-0009）：span 恒 None，
/// 方法体永不执行——仅保住 fill_path 调用点的编译。
#[cfg(not(feature = "gradient"))]
struct NoSpanEval;

#[cfg(not(feature = "gradient"))]
impl NoSpanEval {
    fn begin_row(&mut self, _yc: f32) {}
    fn vertical_row_colors(&mut self) -> Option<[Rgba8; 8]> {
        None
    }
    fn at(&mut self, _x: f32) -> Rgba8 {
        Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        }
    }
    fn at_premul(&mut self, _x: f32) -> ([f32; 3], f32) {
        ([0.0; 3], 0.0)
    }
}

/// 扫描线填充一段点池（NaN 分隔环；环自动闭合）。
/// 纯色走预计算常量色/alpha 快路径；渐变按像素中心逐像素求值
/// （AA 路径在行末按 coverage 混合，色值取像素中心采样）。
/// `clip`：裁切链上下文（rect 收紧扫描范围/列区间；cov 为矢量覆盖
/// 蒙版，aa=1 按 0/255 硬门控、aa>1 乘进 coverage）。
/// `mask`：亮度蒙版因子（0..255/像素，M4），乘进像素有效 alpha；
/// aa=1 硬门控 0，aa>1 乘进 coverage。
/// `pat`：pattern 瓦片采样上下文（M4）；Some 时 paint 为全透明占位，
/// 逐像素以 F⁻¹ 映回瓦片空间取模采样（像素中心求色，alpha 乘
/// opacity/coverage/kf，与渐变同款混合模型）。
/// `dirty`：层脏区记账（S2）——Some 时把本命令裁切/剔除后的扫描
/// 范围（保守超集）并进脏区矩形，PopLayer 据此只合成着墨区。
/// 扫描线交点按 x 排序的统一比较器（车道AF2-M2）：原四处闭包各自
/// 单态化一份 driftsort/quicksort 实例（≈2.9KB .text）；合并为单一
/// fn 项后排序算法只剩一份实例。语义与原 partial_cmp+unwrap_or(Equal)
/// 逐位相同（NaN 视为相等，保序）。
#[inline]
pub(crate) fn cmp_scan_x(a: &(f32, i32), b: &(f32, i32)) -> core::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal)
}

/// 扫描线交点排序快路。
///
/// 实际 SVG 填充的单条子扫描线通常只有 2–16 个交点；直接进入通用
/// driftsort 会支付远高于比较本身的分派成本。不超过 32 项时使用稳定
/// 插入排序；更大数组回到原 stable sort，控制 no_std 代码体积且保持
/// NaN/无穷病态输入的既有宽容语义。插入分支只在 `Less` 时搬移，因此与
/// 原排序同样稳定。
#[inline(never)]
pub(crate) fn sort_scan_x(xs: &mut [(f32, i32)]) {
    const INSERTION_MAX: usize = 32;
    if xs.len() > INSERTION_MAX {
        xs.sort_by(cmp_scan_x);
        return;
    }
    for i in 1..xs.len() {
        let item = xs[i];
        let mut j = i;
        while j > 0 && cmp_scan_x(&item, &xs[j - 1]) == core::cmp::Ordering::Less {
            xs[j] = xs[j - 1];
            j -= 1;
        }
        xs[j] = item;
    }
}

/// 车道OL（perf-round3 刀①b）：通用逐像素纯色混合（列区间半开
/// [p0,p1)）——算式与 fill_path 原 cov 迭代逐式相同
///（coverage = acc/255 × cm/255 × mf/255，acc=0/coverage≤0 跳过）。
#[inline]
#[allow(clippy::too_many_arguments)]
fn blend_solid_px_range(
    buf: &mut [u8],
    cov: &[u8],
    range: core::ops::Range<usize>,
    row: usize,
    c: Rgba8,
    a: f32,
    cm: Option<&[u8]>,
    mf: Option<&[u8]>,
    w: usize,
    y: usize,
) {
    for px in range {
        let acc = cov[px];
        if acc == 0 {
            continue;
        }
        let mut coverage = acc as f32 / 255.0;
        if let Some(cm) = cm {
            coverage *= cm[y * w + px] as f32 / 255.0;
        }
        if let Some(mf) = mf {
            coverage *= mf[y * w + px] as f32 / 255.0;
        }
        if coverage <= 0.0 {
            continue;
        }
        let o = row + px * 4;
        blend_px(&mut buf[o..o + 4], c, a * coverage);
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_path(
    pts: &[f32],
    buf: &mut [u8],
    vp: &Viewport,
    paint: &Paint,
    opacity: f32,
    rule: FillRule,
    clip: Option<ClipCtx>,
    mask: Option<&[u8]>,
    pat: Option<PatCtx<'_>>,
    dirty: Option<&mut [usize; 4]>,
    // hwB 行带一期：`buf` 行 0 对应的**帧坐标**行号（全帧调用 = 0；
    // band 调用 = band 首行）。输出行偏移 = `(y − buf_y0)`；预计算
    // 缓存（clip cov / mask 因子）仍按帧坐标 `y` 绝对索引。
    buf_y0: usize,
    // hwB：`buf` 可写行数（全帧 = 视口高；band = band 行数）。扫描
    // 行范围被钳进 `[buf_y0, buf_y0+buf_rows)`，窗口外的行不触碰。
    buf_rows: usize,
    sc: &mut Scratch,
) {
    if opacity <= 0.0 {
        return;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    // 纯色快路径：常量色 + 常量 alpha（与 M1/M2 逐字节一致）。
    // pattern 命令的 paint 为全透明占位——跳过快路径（含 a<=0 提前
    // 返回），逐像素走瓦片采样。
    let solid: Option<(Rgba8, f32)> = match paint {
        Paint::Solid(c) if pat.is_none() => {
            let a = (c.a as f32 / 255.0) * opacity;
            if a <= 0.0 {
                return;
            }
            Some((*c, a))
        }
        _ => None,
    };
    let w = vp.width;
    let h = vp.height;
    // diff（host-perf 门控字段）不在此绑定——cfg 站点经 sc.diff 独占
    // 借用（与 edges/xs/cov 的字段借用不相交，NLL 允许）
    let Scratch {
        edges,
        xs,
        cov,
        act,
        bk_first,
        bk_last,
        bk_diff,
        bk_off,
        bk_cur,
        bk_flat,
        ..
    } = sc;

    // 边收集：(y0, y1, x_at_y0, dx/dy, winding 方向)，y0 < y1 归一化
    // 同时累计环端点的全局 x 范围（x 向剔除用；水平边虽不入边表，其端点
    // 仍参与范围累计——只会更宽，保守方向安全）
    edges.clear();
    let mut x_all_finite = true;
    let mut xmin = f32::MAX;
    let mut xmax = f32::MIN;
    collect_edges(pts, edges, &mut x_all_finite, &mut xmin, &mut xmax);
    if edges.is_empty() {
        return;
    }

    let y_min = libm::floorf(edges.iter().map(|e| e.0).fold(f32::MAX, f32::min)).max(0.0) as i32;
    let y_max =
        libm::ceilf(edges.iter().map(|e| e.1).fold(f32::MIN, f32::max)).min(h as f32) as i32;
    // 裁切矩形（设备空间）收紧扫描范围与列区间
    let (cy0, cy1, cx0, cx1) = match &clip {
        Some(c) => (
            libm::ceilf(c.rect[1] - 0.5).max(y_min as f32) as i32,
            libm::ceilf(c.rect[3] - 0.5).min(y_max as f32) as i32,
            c.rect[0],
            c.rect[2],
        ),
        None => (y_min, y_max, f32::MIN, f32::MAX),
    };
    // hwB 行带一期：扫描行范围再钳进可写行窗 [buf_y0, buf_y0+buf_rows)。
    // 全帧调用（buf_y0=0、buf_rows=h）两钳均空转（cy0 ≥ 0、cy1 ≤ h
    // 恒成立），行为逐字节不变；band 调用只扫窗内行——行内计算（交点/
    // 覆盖/渐变相位）全部按帧坐标 y 进行，与全帧渲染同式同值。
    let cy0 = cy0.max(buf_y0 as i32);
    let cy1 = cy1.min((buf_y0 + buf_rows) as i32);

    // ── x 向剔除（性能）：端点全局 x 范围 ∩ 裁切/视口 → 像素列区间 [gx0,gx1)。
    // 仅当全部端点坐标与边斜率有限时启用（inf/NaN 端点的退化 span 记账
    // 可能落到任意列，保守关闭剔除走全宽）。有限输入下交点插值只可能
    // 越出端点范围几个 ulp，±1 像素放宽即覆盖；区间外像素不可能被
    // span 记账触碰（lo/hi 已按裁切收紧），行末 cov 循环跳过它们
    // ≡ 不迭代（acc 恒 0）→ 逐字节一致。空交集整体提前返回。
    let x_cull = x_all_finite
        && edges
            .iter()
            .all(|&(_y0, _y1, _x0, slope, _dir)| slope.is_finite());
    let (gx0, gx1) = if x_cull {
        let lo_f = xmin.max(cx0);
        let hi_f = xmax.min(cx1);
        let g0 = (libm::floorf(lo_f) as i32).saturating_sub(1).max(0);
        let g1 = (libm::ceilf(hi_f) as i32).saturating_add(1).min(w as i32);
        (g0, g1.max(g0))
    } else {
        (0, w as i32)
    };
    if gx0 >= gx1 {
        return; // 形状与视口/裁切在 x 向无交集
    }
    // S2 层脏区记账：着墨像素 ⊆ [gx0,gx1) × [cy0,cy1)（保守超集——
    // 行内循环实际只碰 cov 非零列，超集只多合成几个全 0 像素）。
    // hwB：y 记账转 **band-local**（band 层缓冲/主 band 切片的行 0 =
    // 帧 buf_y0 行）；全帧调用 buf_y0=0 时与原值逐位相同。
    if let Some(d) = dirty {
        let x0 = (gx0 as usize).min(w as usize);
        let x1 = (gx1 as usize).min(w as usize);
        let y0 = ((cy0.max(0) as usize).min(h as usize)).saturating_sub(buf_y0);
        let y1 = ((cy1.max(0) as usize).min(h as usize))
            .saturating_sub(buf_y0)
            .min(buf_rows);
        if x0 < x1 && y0 < y1 {
            if x0 < d[0] {
                d[0] = x0;
            }
            if y0 < d[1] {
                d[1] = y0;
            }
            if x1 > d[2] {
                d[2] = x1;
            }
            if y1 > d[3] {
                d[3] = y1;
            }
        }
    }

    // aa = 1：像素中心采样（历史路径，逐字节一致）。
    // aa = 2（RC4 解析式覆盖，legacy 档）：
    //   y 方向 4 条子扫描线（yc = y+(sub+0.5)/4，替代旧 aa×aa 子点采样），
    //   x 方向解析——交点吸附到 1/4 像素网格，span 与像素的交叠按
    //   1/4 单位 ×16 累计成 alpha（整像素 64/64/64/63 封顶，和 256→255），
    //   行末 coverage = alpha/255。
    // aa ≥ 3（车道KC 精档）：同架构，仅 span 端点浮点直通（去 1/4 网格
    //   吸附）——吸附的 ±0.125px = ±32LSB 是 24px 图标海量集 tol32 细环
    //   主误差源（8x 收敛探针实证）；浮点端点下 ov×64 部分覆盖天然细
    //   粒度，对角三角测例精确命中真值覆盖。
    let aa = vp.aa.max(1) as i32;
    let aa_on = aa > 1;
    let aa_fine = aa > 2;
    // host-perf（ADR-0027）：full-span 差分累计开关——进程级原子位，
    // 每命令读一次（不进行内热循环）；MCU 面整段机械不编译
    #[cfg(feature = "host-perf")]
    let cov_diff = coverage_opt() >= 1;
    // ADR-0027 阶段二：交点顺序复用（covopt=2；蕴含差分累计）
    #[cfg(feature = "host-perf")]
    let cov_reuse = coverage_opt() >= 2;
    // ── 车道NT：行带边桶（y 分桶）。一次构建每行活跃边表，替代逐行
    // 全边扫描（O(rows×edges) → O(edges+pairs)）；谓词校验保证桶边界
    // 与旧扫描逐字节同构，失败（超容量）回退旧扫描。桶内容按边原序，
    // stable sort 平局次序不变。
    let row_lo = cy0.max(0);
    let row_hi = cy1.max(0);
    let (hi_d, lo_d) = if aa_on {
        (0.875_f32, 0.125_f32)
    } else {
        (0.5_f32, 0.5_f32)
    };
    let bucketed = build_row_buckets(
        edges, row_lo, row_hi, hi_d, lo_d, bk_first, bk_last, bk_diff, bk_off, bk_cur, bk_flat,
    );
    // cov 复用（跨命令）：容量不足时一次分配；「行末只清脏段」保证
    // 上一命令结束时 cov 已全零，这里无需重新清零整行
    if aa_on && cov.len() != w as usize {
        cov.clear();
        cov.resize(w as usize, 0);
    }
    // （cov 脏段变量 d0/d1 在 aa>1 分支内按行声明）
    // 渐变 span 求值器（性能：行常量预计算 + 垂直轴每行常数折叠 + 停靠段
    // 索引缓存；输出与逐像素 sample_paint 逐位等价，见 gradient::SpanEval）
    // 抖动分块相位步进按视口宽选档（Chrome tile 尺寸启发式，见 DITHER8 注释）
    // feature `gradient` 关闭：Paint 恒 Solid（烘焙层不再产渐变 Paint），
    // span 求值器缺席；手写 DL 的渐变 Paint 按不描绘处理（ADR-0009）。
    #[cfg(feature = "gradient")]
    let tile_shift = if vp.width >= 512 {
        gradient::TILE_SHIFT_WIDE
    } else {
        gradient::TILE_SHIFT_TALL
    };
    #[cfg(feature = "gradient")]
    let mut span: Option<gradient::SpanEval> = if solid.is_none() && pat.is_none() {
        Some(gradient::SpanEval::new(paint, tile_shift))
    } else {
        None
    };
    #[cfg(not(feature = "gradient"))]
    let mut span: Option<NoSpanEval> = None;
    for y in cy0.max(0)..cy1.max(0) {
        if !aa_on {
            // ── aa=1：像素中心采样（历史路径，逐字节一致）──
            let yc = y as f32 + 0.5;
            if let Some(ev) = span.as_mut() {
                ev.begin_row(yc);
            }
            xs.clear();
            if bucketed {
                // NT 分桶：只扫本行桶内边（原序），谓词复检（权威语义）
                let r = (y - row_lo) as usize;
                for &ei in &bk_flat[bk_off[r] as usize..bk_off[r + 1] as usize] {
                    let &(y0, y1, x0, slope, dir) = &edges[ei as usize];
                    if y0 <= yc && yc < y1 {
                        xs.push((x0 + slope * (yc - y0), dir));
                    }
                }
            } else {
                for &(y0, y1, x0, slope, dir) in edges.iter() {
                    if y0 <= yc && yc < y1 {
                        xs.push((x0 + slope * (yc - y0), dir));
                    }
                }
            }
            if xs.len() < 2 {
                continue;
            }
            sort_scan_x(xs);

            let mut wind = 0i32;
            let mut span_start: Option<f32> = None;
            let is_in = |wind: i32| match rule {
                FillRule::NonZero => wind != 0,
                FillRule::EvenOdd => wind % 2 != 0,
            };
            for &(x, dir) in xs.iter() {
                if rule == FillRule::NonZero {
                    wind += dir;
                } else {
                    wind += if dir > 0 { 1 } else { -1 };
                }
                let now_in = is_in(wind);
                match (span_start, now_in) {
                    (None, true) => span_start = Some(x),
                    (Some(sx), false) => {
                        span_start = None;
                        // 采样点 ∈ [sx, x) ∩ [cx0, cx1)
                        let lo = sx.max(cx0);
                        let hi = x.min(cx1);
                        if hi <= lo {
                            continue;
                        }
                        // 像素中心采样：px+0.5 ∈ [lo, hi)
                        let px0 = (libm::ceilf(lo - 0.5) as i32).max(0);
                        let px1 = (libm::ceilf(hi - 0.5) as i32).min(w as i32);
                        // hwB：输出行基 band-local（缓存查找仍用绝对 y）
                        let row = (y as usize - buf_y0) * w as usize * 4;
                        for px in px0..px1 {
                            // 矢量裁切：aa=1 蒙版为 0/255 硬边，0 即裁光
                            if let Some(cm) = clip.as_ref().and_then(|c| c.cov) {
                                if cm[y as usize * w as usize + px as usize] == 0 {
                                    continue;
                                }
                            }
                            // 亮度蒙版因子（0 = 不可见；否则乘进有效 alpha）
                            let kf = match mask {
                                Some(mf) => {
                                    let k = mf[y as usize * w as usize + px as usize];
                                    if k == 0 {
                                        continue;
                                    }
                                    k as f32 / 255.0
                                }
                                None => 1.0,
                            };
                            let o = row + px as usize * 4;
                            match solid {
                                Some((c, a)) => blend_px(&mut buf[o..o + 4], c, a * kf),
                                None => {
                                    // 渐变：f32 premul 单次舍入管线（车道AT，
                                    // Chrome 模型），kf 为 aa=1 的 mask/clip 因子；
                                    // pattern：F⁻¹ 瓦片取模采样
                                    if let Some(ev) = span.as_mut() {
                                        let (pm, sa) = ev.at_premul(px as f32 + 0.5);
                                        let k = opacity * kf;
                                        if sa * k > 0.0 {
                                            blend_gradient_pm(&mut buf[o..o + 4], pm, sa, k);
                                        }
                                    } else if let Some(pc) = pat.as_ref() {
                                        let c = pc.at(px as f32 + 0.5, yc);
                                        let a = (c.a as f32 / 255.0) * opacity * kf;
                                        if a > 0.0 {
                                            blend_px(&mut buf[o..o + 4], c, a);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        // ── aa>1：解析式覆盖（4 子行 × 解析 x，resvg 对齐）──
        // 行带预筛（性能）：与 4 条子扫描线（y+0.125..=y+0.875）的并集带
        // 相交的边一次筛出（y0 ≤ 带顶 && y1 > 带底），4 条子行复用该
        // 候选集，逐边仍按原条件 y0 ≤ yc < y1 精确判定——被筛掉的边
        // 对 4 条子线的条件均不成立，不产生交点 → 逐字节一致
        let yb_lo = y as f32 + 0.125;
        let yb_hi = y as f32 + 0.875;
        // NT 分桶：直接以本行桶切片为活跃边表（免 act 拷贝）；回退时
        // 仍走旧逐行扫描填 act。两者内容+顺序逐字节同构。
        let act_slice: &[u32] = if bucketed {
            let r = (y - row_lo) as usize;
            &bk_flat[bk_off[r] as usize..bk_off[r + 1] as usize]
        } else {
            act.clear();
            for (ei, &(y0, y1, _x0, _slope, _dir)) in edges.iter().enumerate() {
                if y0 <= yb_hi && y1 > yb_lo {
                    act.push(ei as u32);
                }
            }
            act.as_slice()
        };
        // 当前行 cov 写过的列段 [d0..=d1]（usize::MAX=空）：行末只清这段
        let mut d0 = usize::MAX;
        let mut d1 = 0usize;
        // 差分累计（ADR-0027）：事件表每行清空；行末横扫合并（见循环后）
        #[cfg(feature = "host-perf")]
        if cov_diff {
            sc.diff.clear();
        }
        // 阶段二（ADR-0027）：交点复用行级存活位——NaN/预算回退只杀本行
        #[cfg(feature = "host-perf")]
        let mut reuse_alive = cov_reuse;
        // [fill-phase:isect-gen] —— 4 子扫描线交点生成（行级归因标记，
        // 见 tests/oracle/android_fill_profile.py 五段分桶）
        // 第 4 条子行的整像素贡献是 63（非 64），4 行累计 ≤255 而非 256
        for sub in 0..4i32 {
            let yc = y as f32 + (sub as f32 + 0.5) * 0.25;
            // 阶段二复用路径产出已排序 xs；false = 权威生成（含回退子行）
            #[cfg(feature = "host-perf")]
            let via_reuse = if reuse_alive {
                reuse_build_xs(
                    y,
                    sub,
                    yc,
                    act_slice,
                    edges,
                    xs,
                    &mut sc.reuse,
                    &mut sc.submask,
                    &mut reuse_alive,
                )
            } else {
                false
            };
            #[cfg(not(feature = "host-perf"))]
            let via_reuse = false;
            if !via_reuse {
                xs.clear();
                for &ei in act_slice.iter() {
                    let &(y0, y1, x0, slope, dir) = &edges[ei as usize];
                    if y0 <= yc && yc < y1 {
                        xs.push((x0 + slope * (yc - y0), dir));
                    }
                }
                if xs.len() < 2 {
                    continue;
                }
                sort_scan_x(xs);
            } else if xs.len() < 2 {
                // 复用臂：xs 已排序产出；len 检查与权威臂同语义
                continue;
            }
            // [fill-phase:span-accum] —— 绕数推进 + span 覆盖累计（sort
            // 本体样本落在 sort_scan_x 符号 = intersection-sort 段）
            let full = if sub < 3 { 64.0 } else { 63.0 };
            let mut wind = 0i32;
            let mut span_start: Option<f32> = None;
            let is_in = |wind: i32| match rule {
                FillRule::NonZero => wind != 0,
                FillRule::EvenOdd => wind % 2 != 0,
            };
            for &(x, dir) in xs.iter() {
                if rule == FillRule::NonZero {
                    wind += dir;
                } else {
                    wind += if dir > 0 { 1 } else { -1 };
                }
                let now_in = is_in(wind);
                match (span_start, now_in) {
                    (None, true) => span_start = Some(x),
                    (Some(sx), false) => {
                        // 先退出 span 状态：空区间（重合边零宽带/裁切外）也必须
                        // 重置，否则下一对交点与过期起点配对 → 两填充带跨洞合并
                        // （诊断 RC1：material 143 枚图标洞被填实的根因）
                        span_start = None;
                        // legacy 档吸附 1/4 网格；精档（车道KC）浮点直通
                        // ——与 clip_coverage 同款分支，见 Viewport.aa 文档。
                        let (lo, hi) = if aa_fine {
                            (sx.max(cx0), x.min(cx1))
                        } else {
                            (
                                libm::roundf(sx.max(cx0) * 4.0) * 0.25,
                                libm::roundf(x.min(cx1) * 4.0) * 0.25,
                            )
                        };
                        if hi <= lo {
                            continue;
                        }
                        let b0 = (libm::floorf(lo) as i32).max(0);
                        let b1 = (libm::floorf(hi) as i32).min(w as i32 - 1);
                        if b0 <= b1 {
                            // 脏段记账在 span 级做（写入列 ⊆ [b0,b1]，端点可能
                            // ov=0 但清零无害）——避免逐像素分支拖慢 4 子行
                            // 覆盖累计热循环
                            let bu0 = b0 as usize;
                            let bu1 = b1 as usize;
                            if bu0 < d0 {
                                d0 = bu0;
                            }
                            if bu1 > d1 {
                                d1 = bu1;
                            }
                        }
                        // 满段快速路径（wt/perf-raster）：整像素区间
                        // [ceil(lo), floor(hi)) 上 ov ≡ 1.0（min(pxf+1,hi) 与
                        // max(pxf,lo) 分别退化为 pxf+1/pxf，差恰 1.0），add 恒
                        // 为 full（64/63）——连续切片常量加法可被自动向量化；
                        // 两端至多各 1 个边界像素走原逐像素公式。
                        // 与原循环逐字节一致：ov≥1.0 ⟺ pxf≥lo ∧ pxf+1≤hi
                        // ⟺ ceil(lo)≤px<floor(hi)（浮点端点下同式成立，
                        // 车道KC 去吸附后不再是 1/4 网格点）。
                        // NaN/inf 端点：as i32 饱和/归零与原 b0/b1 同源，
                        // 病态区间落入 else 支支逐像素路径，行为不变。
                        let fs = (libm::ceilf(lo) as i32).max(b0);
                        let fe = (libm::floorf(hi) as i32).min(b1 + 1);
                        if fs < fe {
                            // 差分累计（ADR-0027 阶段一，host-perf）：满档区间只记
                            // 起止事件，4 子行后一次横扫合并进 cov——权威路径每
                            // 子行各扫一遍 cov（4×），这里整行一遍。字节等价证明
                            // 见 cov_apply_diff。MCU 面（host-perf 关）整段不编译，
                            // 只保留权威切片累计（T1-T4 零增量）。
                            #[cfg(feature = "host-perf")]
                            {
                                if cov_diff {
                                    let full_i: i32 = if sub < 3 { 64 } else { 63 };
                                    sc.diff.push((fs, full_i));
                                    sc.diff.push((fe, -full_i));
                                } else {
                                    let full_u = full as u8;
                                    for v in &mut cov[fs as usize..fe as usize] {
                                        *v = v.saturating_add(full_u);
                                    }
                                }
                            }
                            #[cfg(not(feature = "host-perf"))]
                            {
                                let full_u = full as u8;
                                for v in &mut cov[fs as usize..fe as usize] {
                                    *v = v.saturating_add(full_u);
                                }
                            }
                            for px in b0..fs {
                                cov_add_partial(&mut cov[..], px, lo, hi, full);
                            }
                            for px in fe..=b1 {
                                cov_add_partial(&mut cov[..], px, lo, hi, full);
                            }
                        } else {
                            // 无整像素满档（薄 span / 全边界像素）：全走原式
                            for px in b0..=b1 {
                                cov_add_partial(&mut cov[..], px, lo, hi, full);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // 差分累计合并（ADR-0027）：4 子行事件一次横扫进 cov（acc 段内
        // 恒定 → min(acc,255) 饱和切片加）。空表（无满档区间）零成本。
        #[cfg(feature = "host-perf")]
        if cov_diff && !sc.diff.is_empty() {
            cov_apply_diff(&mut cov[..], &mut sc.diff[..]);
        }
        // [fill-phase:finalize] —— 行末 coverage 结算/混合/清脏段
        // 空行早退：本行 4 条子行未产生任何 span（d0 未被触碰）→ cov 保持
        // 全零，行末混合必为空操作（跳过 begin_row 亦无观察者）。字节级等价。
        if d0 == usize::MAX {
            continue;
        }
        // 行末：累计 alpha（0..255）→ coverage 混合；满覆盖 255 与 aa=1 一致；
        // 渐变色取像素中心采样、alpha 乘 coverage。
        // 只扫 [gx0,gx1)：区间外像素不可能有累计（x 向剔除，逐字节一致）。
        //
        // wt/perf-raster 内部满覆盖段快速路径（跨度切割）：acc==255 且无
        // 逐像素蒙版（矢量裁切 cov / 亮度蒙版）时 coverage≡1.0，纯色不透明
        // （a==1.0）或渐变不透明色（c.a==255 ∧ opacity==1 → a==1.0）的
        // blend_px 数学上恒等于直写 (r,g,b,255)：
        //   out_a = 1 + da·0 = 1；mix = (sv·1 + dv·0)/1 = sv（sv 为精确
        //   整数 f32，截断无损）；α 通道 (255.0+0.5) as u8 = 255。
        // 与逐像素路径逐字节一致（等价性单测 + 全语料 sha256 对照兜底）。
        // 垂直轴线性渐变（dx==0）行内 t 恒定，色仅随 8×8 抖动列相位循环
        // → 每行 8 色（SpanEval::vertical_row_colors，与逐像素 at 逐位
        // 一致），按 32B 旋转图案直写；非不透明色退化为 8 色查表 + 逐像素
        // blend（仍省掉全部渐变求值）。
        if let Some(ev) = span.as_mut() {
            ev.begin_row(y as f32 + 0.5);
        }
        // hwB：输出行基 band-local（渐变 SpanEval 的 begin_row 已按绝对
        // 像素中心 y 取行常量，与全帧渲染同值）
        let row = (y as usize - buf_y0) * w as usize * 4;
        let g0 = gx0 as usize;
        let g1 = gx1 as usize;
        // 逐像素蒙版存在时满段直写不安全（cm/mf 逐像素可 <255）→ 全走原路径
        let no_px_mask = clip.as_ref().map_or(true, |c| c.cov.is_none()) && mask.is_none();
        if let Some((c, a)) = solid {
            // 车道OL（perf-round3 刀①b）：all-rect 链内区在本行的列区间
            //（半开 [ri0,ri1)，ri0 ≥ ri1 = 空）。内区 cm 恒 255
            //（rect_interior_of 双向证明）→ coverage ≡ acc/255，行末
            // 可按无逐像素蒙版口径满段直写；非内区行/边界带维持逐像素。
            let (ri0, ri1) = if a == 1.0 && mask.is_none() {
                match clip.as_ref().and_then(|cc| cc.rect_interior) {
                    Some(r) if y >= r.y0 && y < r.y1 => {
                        let lo = (r.x0.max(g0 as i32) as usize).min(g1);
                        let hi = (r.x1.min(g1 as i32) as usize).min(g1).max(lo);
                        (lo, hi)
                    }
                    _ => (g1, g0),
                }
            } else {
                (g1, g0)
            };
            if no_px_mask && (a == 1.0 || host_fill_run_opt()) {
                // 无逐像素蒙版：字扫定位 0/255 段。不透明段直写；
                // host-perf 下半透明满覆盖段批量混合，边界像素仍走原式。
                let mut i = g0;
                while i < g1 {
                    i = scan_nonzero(cov, i, g1);
                    if i >= g1 {
                        break;
                    }
                    let acc = cov[i];
                    if acc == 255 {
                        let j = scan_run_end(cov, i + 1, g1);
                        if a == 1.0 {
                            fill_run_opaque(&mut buf[row + i * 4..row + j * 4], c.r, c.g, c.b);
                        } else {
                            #[cfg(feature = "host-perf")]
                            blend_solid_full_run(&mut buf[row + i * 4..row + j * 4], c, a);
                        }
                        i = j;
                    } else {
                        blend_px(
                            &mut buf[row + i * 4..row + i * 4 + 4],
                            c,
                            a * (acc as f32 / 255.0),
                        );
                        i += 1;
                    }
                }
            } else if ri0 < ri1 {
                // 车道OL：内区直写恢复——coverage ≡ acc/255 与 no_px_mask
                // 直写逐位同语义；两端边界带逐像素 cm 乘（逐式同原路径）。
                blend_solid_px_range(
                    buf,
                    cov,
                    g0..ri0,
                    row,
                    c,
                    a,
                    clip.as_ref().and_then(|cc| cc.cov),
                    mask,
                    w as usize,
                    y as usize,
                );
                let mut i = ri0;
                while i < ri1 {
                    i = scan_nonzero(cov, i, ri1);
                    if i >= ri1 {
                        break;
                    }
                    let acc = cov[i];
                    if acc == 255 {
                        let j = scan_run_end(cov, i + 1, ri1);
                        fill_run_opaque(&mut buf[row + i * 4..row + j * 4], c.r, c.g, c.b);
                        i = j;
                    } else {
                        blend_px(
                            &mut buf[row + i * 4..row + i * 4 + 4],
                            c,
                            a * (acc as f32 / 255.0),
                        );
                        i += 1;
                    }
                }
                blend_solid_px_range(
                    buf,
                    cov,
                    ri1..g1,
                    row,
                    c,
                    a,
                    clip.as_ref().and_then(|cc| cc.cov),
                    mask,
                    w as usize,
                    y as usize,
                );
            } else {
                for (i, &acc) in cov[g0..g1].iter().enumerate() {
                    if acc == 0 {
                        continue;
                    }
                    let px = g0 + i;
                    let mut coverage = acc as f32 / 255.0;
                    if let Some(cm) = clip.as_ref().and_then(|c| c.cov) {
                        coverage *= cm[y as usize * w as usize + px] as f32 / 255.0;
                    }
                    if let Some(mf) = mask {
                        coverage *= mf[y as usize * w as usize + px] as f32 / 255.0;
                    }
                    if coverage <= 0.0 {
                        continue;
                    }
                    let o = row + px * 4;
                    blend_px(&mut buf[o..o + 4], c, a * coverage);
                }
            }
        } else if let Some(pc) = pat.as_ref() {
            for (i, &acc) in cov[g0..g1].iter().enumerate() {
                if acc == 0 {
                    continue;
                }
                let px = g0 + i;
                let mut coverage = acc as f32 / 255.0;
                if let Some(cm) = clip.as_ref().and_then(|c| c.cov) {
                    coverage *= cm[y as usize * w as usize + px] as f32 / 255.0;
                }
                if let Some(mf) = mask {
                    coverage *= mf[y as usize * w as usize + px] as f32 / 255.0;
                }
                if coverage <= 0.0 {
                    continue;
                }
                let c = pc.at(px as f32 + 0.5, y as f32 + 0.5);
                let a = (c.a as f32 / 255.0) * opacity * coverage;
                if a > 0.0 {
                    let o = row + px * 4;
                    blend_px(&mut buf[o..o + 4], c, a);
                }
            }
        } else if let Some(ev) = span.as_mut() {
            if no_px_mask {
                // 垂直轴渐变快路径仅保留全不透明形态（opacity==1 且 8 色
                // a==255）：直写字节与通用 f32 管线逐位一致。含 alpha 时
                // 落入通用单次舍入管线（车道AT Chrome 模型）。
                let vertical_colors = ev.vertical_row_colors();
                let vertical_opaque = vertical_colors
                    .is_some_and(|colors| opacity == 1.0 && colors.iter().all(|c| c.a == 255));
                if let (Some(colors), true) = (vertical_colors, vertical_opaque) {
                    // ── 垂直轴渐变（全不透明）：每行 8 色图案满覆盖直写 ──
                    // 8 相位 × 32B 旋转图案：rot[p] = 从相位 p 起连续 8 像素
                    let mut rot = [0u8; 8 * 32];
                    for (p, chunk) in rot.chunks_exact_mut(32).enumerate() {
                        for j in 0..8 {
                            let c = colors[(p + j) & 7];
                            chunk[j * 4] = c.r;
                            chunk[j * 4 + 1] = c.g;
                            chunk[j * 4 + 2] = c.b;
                            chunk[j * 4 + 3] = 255;
                        }
                    }
                    #[cfg(feature = "gradient")]
                    let shift = tile_shift;
                    #[cfg(not(feature = "gradient"))]
                    let shift = 8; // 不可达（span 恒 None），哑值
                    let mut i = g0;
                    while i < g1 {
                        i = scan_nonzero(cov, i, g1);
                        if i >= g1 {
                            break;
                        }
                        let acc = cov[i];
                        if acc == 255 {
                            // 满覆盖不透明段（字扫终点）：按 256/512px 抖动分块直写
                            let j = scan_run_end(cov, i + 1, g1);
                            let mut px = i;
                            while px < j {
                                let pi = px as i32;
                                let block_end = (((pi >> shift) + 1) << shift) as usize;
                                let end = j.min(block_end);
                                // 块内相位偏移 2·((px>>shift)&3) 常量
                                let mut p = ((pi + 2 * ((pi >> shift) & 3)) & 7) as usize;
                                while px < end {
                                    let n = (end - px).min(8);
                                    let o = row + px * 4;
                                    buf[o..o + n * 4].copy_from_slice(&rot[p * 32..p * 32 + n * 4]);
                                    px += n;
                                    p = (p + n) & 7;
                                }
                            }
                            i = j;
                        } else {
                            // 边界像素（coverage<1）：f32 管线按 premul 缩放
                            // （不透明色 premul ≡ 直色；coverage 折叠进 k）
                            let pi = i as i32;
                            let c = colors[((pi + 2 * ((pi >> shift) & 3)) & 7) as usize];
                            let sa = c.a as f32 / 255.0;
                            let kf = opacity * (acc as f32 / 255.0);
                            if sa * kf > 0.0 {
                                let o = row + i * 4;
                                blend_gradient_pm(
                                    &mut buf[o..o + 4],
                                    [c.r as f32 * sa, c.g as f32 * sa, c.b as f32 * sa],
                                    sa,
                                    kf,
                                );
                            }
                            i += 1;
                        }
                    }
                } else {
                    // ── 一般渐变（斜轴/径向/含 alpha 垂直轴）：f32 premul
                    // 单次舍入管线（车道AT Chrome 模型），字扫跳过零段 ──
                    let mut i = scan_nonzero(cov, g0, g1);
                    while i < g1 {
                        let acc = cov[i];
                        let px = i;
                        let coverage = acc as f32 / 255.0;
                        let (pm, sa) = ev.at_premul(px as f32 + 0.5);
                        let k = opacity * coverage;
                        if sa * k > 0.0 {
                            let o = row + px * 4;
                            blend_gradient_pm(&mut buf[o..o + 4], pm, sa, k);
                        }
                        i = scan_nonzero(cov, i + 1, g1);
                    }
                }
            } else {
                // ── 带逐像素蒙版（矢量裁切/亮度蒙版）：f32 管线（字扫零段）──
                let mut i = scan_nonzero(cov, g0, g1);
                while i < g1 {
                    let acc = cov[i];
                    let px = i;
                    let mut coverage = acc as f32 / 255.0;
                    if let Some(cm) = clip.as_ref().and_then(|c| c.cov) {
                        coverage *= cm[y as usize * w as usize + px] as f32 / 255.0;
                    }
                    if let Some(mf) = mask {
                        coverage *= mf[y as usize * w as usize + px] as f32 / 255.0;
                    }
                    let k = opacity * coverage;
                    if k > 0.0 {
                        let (pm, sa) = ev.at_premul(px as f32 + 0.5);
                        if sa * k > 0.0 {
                            let o = row + px * 4;
                            blend_gradient_pm(&mut buf[o..o + 4], pm, sa, k);
                        }
                    }
                    i = scan_nonzero(cov, i + 1, g1);
                }
            }
        }
        // 只清本行写过的脏段 [d0..=d1]：段外列未被触碰、保持全零，
        // 把「整行 memset」换成「按需清段」，全零不变式跨行/跨命令携带
        if d0 != usize::MAX {
            cov[d0..=d1].fill(0);
        }
    }
}

/// 满覆盖不透明纯色段直写：每像素写 (r,g,b,255)。
/// 与逐像素 blend_px(c, a=1.0) 逐字节一致（数学等价，见 fill_path
/// 行末注释）；16 像素栈上图案块 memcpy 展开，memset 级带宽。
/// u64 字扫：[i, end) 内首个非零 cov 字节下标（无则 end）。纯扫描加速，
/// 访问序列与逐字节等价。
#[inline]
fn scan_nonzero(cov: &[u8], mut i: usize, end: usize) -> usize {
    let mut word = [0u8; 8];
    while i + 8 <= end {
        word.copy_from_slice(&cov[i..i + 8]);
        if u64::from_le_bytes(word) != 0 {
            break;
        }
        i += 8;
    }
    while i < end && cov[i] == 0 {
        i += 1;
    }
    i
}

/// u64 字扫：[i, end) 内首个非 0xFF cov 字节下标（无则 end）——满覆盖段终点的字级检测。
#[inline]
fn scan_run_end(cov: &[u8], mut i: usize, end: usize) -> usize {
    let mut word = [0u8; 8];
    while i + 8 <= end {
        word.copy_from_slice(&cov[i..i + 8]);
        if u64::from_le_bytes(word) != u64::MAX {
            break;
        }
        i += 8;
    }
    while i < end && cov[i] == 255 {
        i += 1;
    }
    i
}

fn fill_run_opaque(dst: &mut [u8], r: u8, g: u8, b: u8) {
    const TILE: usize = 64; // 16 px × 4B
    let mut tile = [0u8; TILE];
    for px in tile.chunks_exact_mut(4) {
        px[0] = r;
        px[1] = g;
        px[2] = b;
        px[3] = 255;
    }
    let n = dst.len();
    let mut o = 0;
    while o + TILE <= n {
        dst[o..o + TILE].copy_from_slice(&tile);
        o += TILE;
    }
    while o < n {
        dst[o] = r;
        dst[o + 1] = g;
        dst[o + 2] = b;
        dst[o + 3] = 255;
        o += 4;
    }
}

/// 宿主半透明纯色满覆盖段。coverage 在调用点已证明恒为 255，
/// 因此只需执行 src-over。连续白底两像素用一个 u64 判定/写入；
/// 普通不透明底复用 [`blend_px`] 的 f32 运算顺序做专化；其余 alpha
/// 逐像素回到权威函数。白底结果由同一 `blend_px` 预计算。
#[cfg(feature = "host-perf")]
fn blend_solid_full_run(dst: &mut [u8], c: Rgba8, a: f32) {
    let mut white = [255u8; 4];
    blend_px(&mut white, c, a);
    let inv = 1.0 - a;
    let opaque_out_a = a + inv;
    let opaque_alpha = (opaque_out_a * 255.0 + 0.5) as u8;
    let word = u64::from_ne_bytes([
        white[0], white[1], white[2], white[3], white[0], white[1], white[2], white[3],
    ]);
    let blend_one = |px: &mut [u8]| {
        if px == [255, 255, 255, 255] {
            px.copy_from_slice(&white);
        } else if px[3] == 255 {
            // blend_px 的 da=1 专化；保持相同的 f32 运算顺序与截断，
            // 省去每像素 da/out_a/alpha 重算。非不透明底回权威通路。
            let mix =
                |sc: u8, dc: u8| -> u8 { ((sc as f32 * a + dc as f32 * inv) / opaque_out_a) as u8 };
            px[0] = mix(c.r, px[0]);
            px[1] = mix(c.g, px[1]);
            px[2] = mix(c.b, px[2]);
            px[3] = opaque_alpha;
        } else {
            blend_px(px, c, a);
        }
    };
    let mut pairs = dst.chunks_exact_mut(8);
    for pair in &mut pairs {
        let mut old = [0u8; 8];
        old.copy_from_slice(pair);
        if u64::from_ne_bytes(old) == u64::MAX {
            pair.copy_from_slice(&word.to_ne_bytes());
        } else {
            blend_one(&mut pair[..4]);
            blend_one(&mut pair[4..]);
        }
    }
    for px in pairs.into_remainder().chunks_exact_mut(4) {
        blend_one(px);
    }
}

/// 单个边界像素的覆盖累计（浮点端点解析式，车道KC 去 1/4 档吸附）：
/// 与满段快速路径之前的原逐像素公式逐行同式（含 f32::min/max 的 NaN
/// 跳过语义）。u8 饱和加与原 (v+add).min(255.0) 逐值一致；部分覆盖
/// 加数为 ov×64 截断（0..64 内精确整数，旧口径恒 16k 倍数——浮点端点
/// 后为任意整数，saturating_add 语义与向量化不受影响）。
#[inline]
fn cov_add_partial(cov: &mut [u8], px: i32, lo: f32, hi: f32, full: f32) {
    let pxf = px as f32;
    let ov = (pxf + 1.0).min(hi) - pxf.max(lo);
    if ov <= 0.0 {
        return;
    }
    // 整像素满档 64/63；边界像素 1/4 档 ×16（0.25→16…）
    let add = if ov >= 1.0 { full } else { ov * 64.0 };
    let c = &mut cov[px as usize];
    *c = c.saturating_add(add as u8); // 256→255（对齐参考）
}

/// coverage 差分累计的事件横扫（host-perf，ADR-0027 阶段一）。
/// 输入是本行 4 条子扫描线全部 span 的 (列, ±full) 事件：满档区间
/// [fs,fe) 只记 (fs,+full)/(fe,−full)，两端部分覆盖像素仍由
/// [`cov_add_partial`] 在子行内直写 cov（记 P）。按列排序后一次横扫：
/// 相邻事件间累计值恒定，把每列的满档总和 F=acc 以 min(F,255) 的 u8
/// 饱和切片加法合并——权威路径每条子行各扫一遍 cov（4×），这里整行
/// 只扫一遍。
///
/// 字节等价证明：权威路径对每像素按子行顺序做**非负整数**的逐次饱和
/// 加（64/63 与 ov×64 截断值），终值 = min(Σ加数, 255)——和式单调
/// 不减，中途饱和后恒 255，与加法顺序无关。差分路径把同像素全部满档
/// 加数聚成 F，min(F,255) 后与 P 做一次饱和加：
/// min(P + min(F,255), 255) = min(P+F, 255)（F≥255 时两侧均 255；
/// F<255 时直接展开）→ 逐字节一致。acc 在任一列恒等于覆盖该列的 span
/// 满档和（事件成对、横扫即前缀和），不可能为负；同列多事件在发射
/// 该列右侧线段前全部生效，顺序无关。NaN/Inf 端点的病态区间在 fs/fe
/// 计算处已与权威路径同源饱和（`as i32` 饱和语义），事件列恒在
/// [d0, d1+1] ⊆ cov 范围内。
#[cfg(feature = "host-perf")]
fn cov_apply_diff(cov: &mut [u8], events: &mut [(i32, i32)]) {
    if events.is_empty() {
        return;
    }
    // 事件量 = 2×span×4 子行（典型 ≤ 数十）：小数组不稳定排序，同列
    // 事件顺序无关（发射前全部生效）
    events.sort_unstable_by_key(|&(col, _)| col);
    let mut acc: i32 = 0;
    let mut seg = events[0].0;
    for &(col, d) in events.iter() {
        if col > seg {
            if acc > 0 {
                let v = acc.min(255) as u8;
                for c in &mut cov[seg as usize..col as usize] {
                    *c = c.saturating_add(v);
                }
            }
            seg = col;
        }
        acc += d;
    }
}

/// ADR-0027 阶段二（host-perf）：4 子扫描线交点顺序复用。
///
/// 首条子行权威生成 + (x, ei) 全序排序；后续子行对上一子行已排序表
/// **精确重算** x（同一表达式 `x0 + slope * (yc - y0)`，逐位同值——
/// 禁止 `x += slope*0.25` 式增量累计的浮点漂移）、按行级活性掩码
/// 剔除/入列，再做插入排序修复（近有序 → O(n) + 少量搬移；权威路径
/// 每子行从零 driftsort O(n log n)，MI 8 pelican isect_sort 占
/// coverage 44.3% 是第一热点——五段画像见 reports/coverage-hotpath.md）。
///
/// 序等价证明：权威路径 xs 生成序 = act 边序（ei 升序，NT 分桶保序），
/// sort_scan_x 稳定排序 ⇒ 相等 x 保持 ei 序 ⇒ 结果 ≡ 按 (x, ei) 全序
/// 排序。±Inf 在两比较器下行为一致（正常参与偏序）；NaN 与任何值比较
/// 恒 Equal，稳定排序结果依赖算法走向，与 (x, ei) 全序不保证一致 ⇒
/// 一旦出现 NaN 交点立即行级回退权威路径（计数入
/// [`COVERAGE_REUSE_FALLBACKS`]）。活性子行掩码用与逐子行生成完全
/// 相同的谓词 `y0 <= yc && yc < y1` 对 4 个 yc 求值：两谓词子式对
/// yc 均单调（yc 随 s 严格递增）⇒ 掩码是连续 1 段 ⇒ 成员只在
/// 首置位位入列一次、离列后不复入（trailing_zeros 判新成员的依据）。
#[cfg(feature = "host-perf")]
#[inline]
fn cmp_scan_x_ei(a: &(f32, i32, u32, u8), b: &(f32, i32, u32, u8)) -> core::cmp::Ordering {
    a.0.partial_cmp(&b.0)
        .unwrap_or(core::cmp::Ordering::Equal)
        .then(a.2.cmp(&b.2))
}

/// 阶段二首子行排序：与 sort_scan_x 同构（≤32 插入 / 更大稳定排序），
/// 比较器换 (x, ei) 全序（序等价证明见上）。
#[cfg(feature = "host-perf")]
fn sort_scan_x_reuse(v: &mut [(f32, i32, u32, u8)]) {
    const INSERTION_MAX: usize = 32;
    if v.len() > INSERTION_MAX {
        v.sort_by(cmp_scan_x_ei);
        return;
    }
    for i in 1..v.len() {
        let item = v[i];
        let mut j = i;
        while j > 0 && cmp_scan_x_ei(&item, &v[j - 1]) == core::cmp::Ordering::Less {
            v[j] = v[j - 1];
            j -= 1;
        }
        v[j] = item;
    }
}

/// 阶段二修复排序：插入排序整体一遍（只在 Less 时搬移，正确稳定）。
/// 近有序输入 O(n)；搬移数超预算（病态交叉/成员翻搅）返回 false →
/// 行级回退权威路径（返回时数组可能部分有序，调用方弃用）。
#[cfg(feature = "host-perf")]
fn repair_scan_reuse(v: &mut [(f32, i32, u32, u8)]) -> bool {
    let budget = v.len() * 2 + 16;
    let mut moves = 0usize;
    for i in 1..v.len() {
        let item = v[i];
        let mut j = i;
        while j > 0 && cmp_scan_x_ei(&item, &v[j - 1]) == core::cmp::Ordering::Less {
            v[j] = v[j - 1];
            j -= 1;
            moves += 1;
            if moves > budget {
                return false;
            }
        }
        v[j] = item;
    }
    true
}

/// 阶段二行级驱动：为当前子行产出已排序 xs（复用路径）。返回 false =
/// 本子行回退权威路径且 `alive` 已置 false（本行剩余子行同样回退）。
#[cfg(feature = "host-perf")]
#[allow(clippy::too_many_arguments)]
fn reuse_build_xs(
    y: i32,
    sub: i32,
    yc: f32,
    act_slice: &[u32],
    edges: &[(f32, f32, f32, f32, i32)],
    xs: &mut Vec<(f32, i32)>,
    reuse: &mut Vec<(f32, i32, u32, u8)>,
    submask: &mut Vec<(u32, u8)>,
    alive: &mut bool,
) -> bool {
    let bail = |alive: &mut bool| -> bool {
        *alive = false;
        COVERAGE_REUSE_FALLBACKS.fetch_add(1, Ordering::Relaxed);
        false
    };
    if sub == 0 {
        // 行级活性掩码（4 个 yc 与主循环逐字同式）+ 首子行权威生成
        submask.clear();
        reuse.clear();
        for &ei in act_slice.iter() {
            let &(y0, y1, x0, slope, dir) = &edges[ei as usize];
            let mut mask = 0u8;
            for s in 0..4i32 {
                let ycs = y as f32 + (s as f32 + 0.5) * 0.25;
                if y0 <= ycs && ycs < y1 {
                    mask |= 1 << s;
                }
            }
            submask.push((ei, mask));
            if mask & 1 != 0 {
                let x = x0 + slope * (yc - y0);
                if x.is_nan() {
                    return bail(alive);
                }
                reuse.push((x, dir, ei, mask));
            }
        }
        sort_scan_x_reuse(reuse);
    } else {
        // 原地重算 + 剔除（保持既有序），追加新成员后整体修复
        let sbit = 1u8 << sub;
        let mut w = 0usize;
        for r in 0..reuse.len() {
            let (_, dir, ei, mask) = reuse[r];
            if mask & sbit == 0 {
                continue;
            }
            let &(y0, _y1, x0, slope, _d) = &edges[ei as usize];
            let x = x0 + slope * (yc - y0);
            if x.is_nan() {
                return bail(alive);
            }
            reuse[w] = (x, dir, ei, mask);
            w += 1;
        }
        reuse.truncate(w);
        let mut dead = false;
        for &(ei, mask) in submask.iter() {
            if mask.trailing_zeros() as i32 == sub {
                let &(y0, _y1, x0, slope, dir) = &edges[ei as usize];
                let x = x0 + slope * (yc - y0);
                if x.is_nan() {
                    dead = true;
                    break;
                }
                reuse.push((x, dir, ei, mask));
            }
        }
        if dead || !repair_scan_reuse(reuse) {
            return bail(alive);
        }
    }
    xs.clear();
    xs.extend(reuse.iter().map(|&(x, dir, _ei, _mask)| (x, dir)));
    true
}

/// 点池 → 归一化边集（NaN 分隔环自动闭合；水平边丢弃）。
/// fill_path 与 clip_coverage 共用。
pub(crate) fn collect_edges(
    pts: &[f32],
    edges: &mut Vec<(f32, f32, f32, f32, i32)>,
    x_all_finite: &mut bool,
    xmin: &mut f32,
    xmax: &mut f32,
) {
    let mut ring_start = 0usize;
    let mut k = 0usize;
    let n = pts.len() / 2;
    while k <= n {
        let is_sep = k == n || pts[k * 2].is_nan() || pts[k * 2 + 1].is_nan();
        if is_sep {
            let ring_end = k;
            if ring_end > ring_start + 1 {
                for i in ring_start..ring_end {
                    let a = i;
                    let b = if i + 1 < ring_end { i + 1 } else { ring_start };
                    let (ax, ay) = (pts[a * 2], pts[a * 2 + 1]);
                    let (bx, by) = (pts[b * 2], pts[b * 2 + 1]);
                    if !(ax.is_finite() && ay.is_finite() && bx.is_finite() && by.is_finite()) {
                        *x_all_finite = false;
                    }
                    *xmin = xmin.min(ax).min(bx);
                    *xmax = xmax.max(ax).max(bx);
                    push_edge(
                        edges,
                        (pts[a * 2], pts[a * 2 + 1]),
                        (pts[b * 2], pts[b * 2 + 1]),
                    );
                }
            }
            ring_start = k + 1;
        }
        k += 1;
    }
}

/// 归一化边：y0 < y1，携带 y0 处 x、dx/dy 与环绕方向（原边向下为 +1）。
fn push_edge(edges: &mut Vec<(f32, f32, f32, f32, i32)>, a: (f32, f32), b: (f32, f32)) {
    let (x0, y0) = a;
    let (x1, y1) = b;
    if y0 == y1 {
        return; // 水平边不参与交点
    }
    if y0 < y1 {
        edges.push((y0, y1, x0, (x1 - x0) / (y1 - y0), 1));
    } else {
        edges.push((y1, y0, x1, (x0 - x1) / (y0 - y1), -1));
    }
}

/// 车道NT：估计用 floor/ceil（截断式，|x| < 2³¹ 精确）。只作校验前
/// 初值——精确性由随后的谓词逐行校验兜底；opt-z 下 libm floorf/ceilf
/// 是真函数调用，采样路径 32×2 次调用的固定开销主导小填充回退差。
#[inline]
fn floor_est(x: f32) -> i32 {
    let t = x as i32;
    if t as f32 > x {
        t - 1
    } else {
        t
    }
}
#[inline]
fn ceil_est(x: f32) -> i32 {
    -floor_est(-x)
}

/// 车道NT：单边活跃行区间 [first, last)（谓词与旧扫描**逐字符同式**：
/// `y0 <= y+hi_d && y1 > y+lo_d`）。floor/ceil 估计的 ±1ulp 舍入由随后的
/// 谓词逐步校验修正；越界/非有限端点走 O(1) 快路径，与旧谓词语义一致。
/// 返回 None = 恒不活跃。
#[inline]
fn edge_row_span(
    y0: f32,
    y1: f32,
    row_lo: i32,
    row_hi: i32,
    hi_d: f32,
    lo_d: f32,
) -> Option<(i32, i32)> {
    let active = |y: i32| -> bool { y0 <= y as f32 + hi_d && y1 > y as f32 + lo_d };
    // 快路径：越出行带极端值 O(1) 定界（与谓词语义一致）
    if !y0.is_finite() && y0 != f32::NEG_INFINITY {
        return None; // NaN / +inf：y0 <= y+hi_d 恒 false
    }
    if !y1.is_finite() && y1 != f32::INFINITY {
        return None; // NaN / -inf：y1 > y+lo_d 恒 false
    }
    let row_lo_f = row_lo as f32;
    let row_hi_f = row_hi as f32;
    // 起点：floor 估计（只可能 ≤ 真first），谓词向前校验
    let first = if y0 < row_lo_f - 1.0 {
        row_lo
    } else if y0 > row_hi_f + 1.0 {
        return None;
    } else {
        let est = floor_est(y0 - hi_d).clamp(row_lo, row_hi);
        let mut f = est;
        while f < row_hi && !active(f) {
            f += 1;
        }
        while f > row_lo && active(f - 1) {
            f -= 1;
        }
        f
    };
    // 终点（排他）：ceil 估计（只可能 ≥ 真 last），谓词向后校验
    let last = if y1 > row_hi_f + 1.0 {
        row_hi
    } else if y1 < row_lo_f {
        return None;
    } else {
        let est = ceil_est(y1 - lo_d).clamp(first, row_hi);
        let mut l = est;
        while l > first && !active(l - 1) {
            l -= 1;
        }
        while l < row_hi && active(l) {
            l += 1;
        }
        l
    };
    if first >= last {
        return None;
    }
    Some((first, last))
}

/// 车道NT：行带边桶构建（fill_path 逐行预筛的加速结构）。
///
/// 把「每行扫描全部边」的 O(rows×edges) 换成一次构建的每行边表：
/// 对每条边求出活跃行区间 [first, last)（见 [`edge_row_span`]），
/// 按边原序装入桶 → act/xs 的内容与顺序与旧逐行扫描完全一致，输出
/// 逐字节不变。采样预筛 + 逐边实算双效益门：估算 pairs ≥
/// rows×edges/8 时旧扫描更便宜（scatter 点簇实测 0.75x 回归教训）→
/// 回退旧逐行扫描；桶总条目超 [`BUCKET_PAIR_CAP`] 同样回退
/// （时间换内存，防病态路径）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_row_buckets(
    edges: &[(f32, f32, f32, f32, i32)],
    row_lo: i32,
    row_hi: i32,
    hi_d: f32,
    lo_d: f32,
    bk_first: &mut Vec<i32>,
    bk_last: &mut Vec<i32>,
    bk_diff: &mut Vec<i32>,
    bk_off: &mut Vec<u32>,
    bk_cur: &mut Vec<u32>,
    bk_flat: &mut Vec<u32>,
) -> bool {
    const BUCKET_PAIR_CAP: usize = 4 << 20; // 4M 条目 ≈ 16MB 上限
    let rows = (row_hi - row_lo).max(0) as usize;
    if rows < 48 || edges.is_empty() {
        // 行带极窄的填充旧扫描本来就便宜（rows×edges 常数小），分桶
        // 固定开销（逐边 span 计算）反占大头——直接回退
        return false;
    }
    let n = edges.len();
    let rows_e = rows as u64 * n as u64;
    if rows_e < 8192 {
        // 廉价扫描域：旧逐行过滤 < ~1 万次比较，分桶固定开销不划算
        return false;
    }
    // ── 采样预筛（O(32)）：回退路径不得白付逐边 pass1（scatter 点簇
    // 实测教训——2133 填充 × 301 边的跨度计算 ≈ 30 Mcyc/帧纯开销）。
    // 以 ≤32 条样本边的平均跨度外推 pairs，估算不划算直接回退。
    let stride = (n / 32).max(1);
    let mut sampled = 0usize;
    let mut span_sum = 0usize;
    let mut k = 0usize;
    while k < n {
        if let Some((f, l)) = edge_row_span(edges[k].0, edges[k].1, row_lo, row_hi, hi_d, lo_d) {
            span_sum += (l - f) as usize;
            sampled += 1;
        }
        k += stride;
    }
    if sampled == 0 {
        return false;
    }
    let est_pairs = (span_sum as u64) * (n as u64) / (sampled as u64);
    if est_pairs * 48 >= rows_e {
        return false;
    }
    bk_first.clear();
    bk_last.clear();
    bk_first.resize(n, 0);
    bk_last.resize(n, 0);
    let mut total = 0usize;
    for (i, &(y0, y1, ..)) in edges.iter().enumerate() {
        match edge_row_span(y0, y1, row_lo, row_hi, hi_d, lo_d) {
            None => bk_first[i] = i32::MAX,
            Some((first, last)) => {
                total += (last - first) as usize;
                bk_first[i] = first;
                bk_last[i] = last;
            }
        }
    }
    // 精确效益门（样本外推有误差，以逐边实算值复核）
    if (total as u64) * 48 >= rows_e {
        return false;
    }
    if total > BUCKET_PAIR_CAP {
        return false;
    }
    // 差分计数 → 每行边数前缀（off[r] = 行 r 的边数）
    bk_diff.clear();
    bk_diff.resize(rows + 1, 0);
    for i in 0..edges.len() {
        if bk_first[i] == i32::MAX {
            continue;
        }
        let f = (bk_first[i] - row_lo) as usize;
        let l = (bk_last[i] - row_lo) as usize;
        bk_diff[f] += 1;
        bk_diff[l] -= 1;
    }
    bk_off.clear();
    bk_off.resize(rows + 1, 0);
    let mut run = 0u32;
    for r in 0..=rows {
        // 差分含 -1（边结束）；u32 wrapping 加法下 +1/-1 对消精确
        // （模 2³² 同构于带符号计数），debug 构建免 overflow panic
        run = run.wrapping_add(bk_diff[r] as u32);
        bk_off[r] = run;
    }
    // 排他前缀 → 起始偏移；按边原序填桶（同行的边保持索引升序，
    // 与旧扫描 push 顺序一致 → stable sort 平局次序不变）
    bk_cur.clear();
    bk_cur.resize(rows + 1, 0);
    let mut acc = 0u32;
    for r in 0..=rows {
        bk_cur[r] = acc;
        acc += bk_off[r];
    }
    bk_flat.clear();
    bk_flat.resize(total, 0);
    for (i, &f) in bk_first.iter().enumerate() {
        if f == i32::MAX {
            continue;
        }
        // 写入活跃行区间 [first, last) 的每一行桶（cursor 顺序推进
        // 保持同行内边索引升序 = 旧扫描 push 顺序）
        let l = bk_last[i];
        for r in f..l {
            let slot = bk_cur[(r - row_lo) as usize] as usize;
            bk_flat[slot] = i as u32;
            bk_cur[(r - row_lo) as usize] += 1;
        }
    }
    // bk_cur[r] 现在指向行 r 的排他末尾 → 转成排他偏移表
    for r in (0..rows).rev() {
        bk_off[r + 1] = bk_cur[r];
    }
    bk_off[0] = 0;
    true
}

/// 位图求值（M8 [`Cmd::Image`]）：扫描范围 = 图像设备 AABB ∩ 视口 ∩
/// 裁切矩形；设备像素中心经 xf⁻¹ 映回图像空间采样，图外（meet
/// letterbox）透明跳过。采样口径（车道BB，`mipmap::` 模块文档有完整
/// 规格与拟合证据）：
///
/// - `pixelated`（image-rendering: pixelated/crisp-edges）→ **最近邻**
///   点采样（[0,w)×[0,h) 内 floor）。
/// - 其余（auto/smooth）→ **Chrome kMedium 等价**：`df` = 逆矩阵行模
///   （设备像素覆盖的源纹素数），`level = floor(log2(max(df_x,df_y)))`
///   钳 ≥ 0；mip 链按需生成（premul 1/2 box，floor 落 u8，缓存于
///   [`ImgData::mips`]）；层上 16.16 定点双轴双线性（权重 4bit 截断），
///   premul 域混合单次 floor，unpremul `c·255/a` floor。
///
/// 合成关键：**Chrome/Skia 低位整型量化 src-over（256 基）**，实证
/// 公式（M8 评估：chrome-headless-shell 148 ARM，256 级灰阶 × opacity
/// 0.3/0.5/0.7 三组共 768 探针点全吻合，无一例外）：
///
/// ```text
/// a8    = round(α·255)                                // 半进位
/// out_c = ((s_c·(a8+1)) >> 8) + ((d_c·(255−a8)) >> 8)  // d = 不透明底
/// ```
///
/// 与浮点 round((s·a8 + d·(255−a8))/255) 在 frac≥0.5 的通道上系统性
/// ±1（s10-05 半透明图初版 9.5pp 差分的根因）。实现三分支：不透明底
/// 走精确两段；**透明底按白底预折后置不透明**（SVG 页面语义——最终
/// 显示必然叠宿主背景，白底折算使预言机白底管线逐字节一致；宿主用
/// 非白背景合成时该像素 ≤1 级偏差，已知近似）；半透明底（位图叠半
/// 透明内容，语料未用）回退 [`blend_px`]。coverage（矢量裁切）与
/// 亮度蒙版因子在量化前折进 α。
/// draw_image 的 mip 层级需求（df = 逆矩阵行模；车道hwB 抽出为单一
/// 事实源——帧内链缓存预生成与采样层选择共用同一计算，与原 draw_image
/// 内联式逐位同源）。
#[cfg(feature = "image-decode")]
fn mip_need(xf: &[f32; 6]) -> u32 {
    let [a, b, c, d, ..] = *xf;
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-12 {
        return 1;
    }
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    let df_x = libm::sqrtf(ia * ia + ib * ib);
    let df_y = libm::sqrtf(ic * ic + id * id);
    crate::mipmap::level_count(df_x, df_y)
}

// 车道M3：帧内 mip 链缓存已并入 FrameCache.mips（惰性路径与 hwB
// 预计算路径共用同一 img 索引 → 链的驻留形式，`slot.len() < need`
// 时补建——原 pair 形式 `mip_chain_idx` 的「同图不同 xf 需求升级」
// 语义由同一判式覆盖）。跨帧携带条目 == 重算条目（mipmap 构链是
// 同源纯函数，precompute_frame_move_pools 同款契约）。

#[cfg(feature = "image-decode")]
#[allow(clippy::too_many_arguments)]
fn draw_image(
    data: &ImgData,
    xf: &[f32; 6],
    opacity: f32,
    pixelated: bool,
    buf: &mut [u8],
    vp: &Viewport,
    clip: Option<&ClipCtx<'_>>,
    mask: Option<&[u8]>,
    dirty: Option<&mut [usize; 4]>,
    // hwB：调用方预生成的 mip 链（帧内缓存/FrameCache；pixelated 或
    // 无链 = None 走点采样）。
    mip: Option<&[crate::mipmap::MipLevel]>,
    // hwB 行带：输出缓冲可写行窗（帧坐标），语义同 fill_path 同名参数。
    buf_y0: usize,
    buf_rows: usize,
) {
    let [a, b, c, d, e, f] = *xf;
    // 逆矩阵（退化/病态直接放弃——构建期保证可逆，此处防御手写 DL）
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-12 {
        return;
    }
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    let (ie, iff) = ((c * f - d * e) / det, (b * e - a * f) / det);
    let (iw, ih) = (data.w as f32, data.h as f32);
    // 设备 AABB（图像四角映射）
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for (cx, cy) in [(0.0, 0.0), (iw, 0.0), (0.0, ih), (iw, ih)] {
        let (px, py) = (a * cx + c * cy + e, b * cx + d * cy + f);
        if px.is_finite() && py.is_finite() {
            x0 = x0.min(px);
            x1 = x1.max(px);
            y0 = y0.min(py);
            y1 = y1.max(py);
        }
    }
    // 扫描范围：AABB ∩ 裁切矩形 ∩ 视口
    if let Some(cc) = clip {
        x0 = x0.max(cc.rect[0]);
        y0 = y0.max(cc.rect[1]);
        x1 = x1.min(cc.rect[2]);
        y1 = y1.min(cc.rect[3]);
    }
    let (vw, vh) = (vp.width as f32, vp.height as f32);
    let px0 = libm::floorf(x0.max(0.0)) as i32;
    let py0 = libm::floorf(y0.max(0.0)) as i32;
    let px1 = libm::ceilf(x1.min(vw)) as i32;
    let py1 = libm::ceilf(y1.min(vh)) as i32;
    // hwB：扫描行范围钳进 band 窗（全帧 buf_y0=0/buf_rows=h 空转）；
    // 窗外整段提前返回（与「无交集返回」同路，脏区不记账）。
    let py0 = py0.max(buf_y0 as i32);
    let py1 = py1.min((buf_y0 + buf_rows) as i32);
    if px1 <= px0 || py1 <= py0 {
        return;
    }
    let stride = vp.width as usize;
    let dw = data.w as usize;
    // 车道BB：auto 走 kMedium（mip 链+定点双线性）；pixelated 点采样
    // 维持源图直读。车道hwB：链由调用方预生成传入（ImgData 不再内嵌
    // RefCell 缓存——DL 必须是可跨 band 线程只读共享的纯数据）；采样层
    // 取链上第 need-1 级（调用方契约：链长 ≥ need；防御回落末级——
    // 全帧局部缓存链长恰为 need，band 的 FrameCache 链长为帧内最大
    // need，两者第 need-1 级逐位相同）。
    let lvl: Option<&crate::mipmap::MipLevel> = if pixelated {
        None
    } else {
        let need = mip_need(xf);
        let chain = mip.unwrap_or(&[]);
        chain
            .get((need as usize).saturating_sub(1))
            .or_else(|| chain.last())
    };
    for y in py0..py1 {
        let yc = y as f32 + 0.5;
        // hwB：缓存查找行基（全画布 clip cov / mask 因子）与输出行基
        // （band-local）分离——全帧调用两者同值。
        let row_abs = y as usize * stride;
        let row = (y as usize - buf_y0) * stride;
        for x in px0..px1 {
            let xc = x as f32 + 0.5;
            let u = ia * xc + ic * yc + ie;
            let v = ib * xc + id * yc + iff;
            // NaN/负/越界（meet letterbox）一律透明
            if !(u >= 0.0 && v >= 0.0 && u < iw && v < ih) {
                continue;
            }
            // 采样（mip 路径返回 premul 混合+unpremul 后的直色）
            let c = if let Some(lv) = lvl {
                let px = crate::mipmap::sample_level(lv, lv.w, lv.h, iw, ih, u, v);
                Rgba8 {
                    r: px[0],
                    g: px[1],
                    b: px[2],
                    a: px[3],
                }
            } else {
                let (su, sv) = (u as usize, v as usize);
                let so = (sv * dw + su) * 4;
                Rgba8 {
                    r: data.rgba[so],
                    g: data.rgba[so + 1],
                    b: data.rgba[so + 2],
                    a: data.rgba[so + 3],
                }
            };
            let mut cov = 1.0f32;
            if let Some(cm) = clip.and_then(|cc| cc.cov) {
                cov *= cm[row_abs + x as usize] as f32 / 255.0;
            }
            if let Some(mf) = mask {
                cov *= mf[row_abs + x as usize] as f32 / 255.0;
            }
            if cov <= 0.0 {
                continue;
            }
            // 源 alpha 8 位量化（半进位）后按 Chrome/Skia 低位整型量化
            // src-over（256 基；实证公式见 draw_image 文档注释）
            let a8 = ((c.a as f32 / 255.0) * opacity * cov * 255.0 + 0.5) as u8;
            if a8 == 0 {
                continue;
            }
            let o = (row + x as usize) * 4;
            let px = &mut buf[o..o + 4];
            if a8 == 255 {
                // 不透明：直写
                px[0] = c.r;
                px[1] = c.g;
                px[2] = c.b;
                px[3] = 255;
            } else {
                // Skia 低位整型量化（SkAlpha255To256：权 = a+1 / 256−a）
                let w_s = a8 as u32 + 1;
                let w_d = 256 - a8 as u32;
                // 底色：不透明底取真值；透明底按白底预折（SVG 页面语义
                // ——最终显示必然叠宿主背景，白底折算使预言机白底管线
                // 逐字节一致；宿主非白背景 ≤1 级偏差，已知近似）
                let (dr, dg, db, opaque) = if px[3] == 255 {
                    (px[0] as u32, px[1] as u32, px[2] as u32, true)
                } else if px[3] == 0 {
                    (255u32, 255, 255, false)
                } else {
                    // 半透明底（罕见：位图叠半透明内容）：回退通用混合
                    blend_px(px, c, a8 as f32 / 255.0);
                    continue;
                };
                px[0] = ((((c.r as u32 * w_s) >> 8) + ((dr * w_d) >> 8)).min(255)) as u8;
                px[1] = ((((c.g as u32 * w_s) >> 8) + ((dg * w_d) >> 8)).min(255)) as u8;
                px[2] = ((((c.b as u32 * w_s) >> 8) + ((db * w_d) >> 8)).min(255)) as u8;
                if !opaque {
                    px[3] = 255; // 白底预折产物按不透明存储
                }
            }
        }
    }
    if let Some(d) = dirty {
        // hwB：y 记账 band-local（py0/py1 已钳进 band 窗，减法不欠流）
        dirty_union_rect(
            d,
            &[
                px0 as usize,
                py0 as usize - buf_y0,
                px1 as usize,
                py1 as usize - buf_y0,
            ],
        );
    }
}

/// src-over 直 color 混合（非预乘），作用于 4 字节像素。
/// 输出 alpha 四舍五入（S2：对齐 Chrome 的 0.5·255=127.5→128——
/// 截断会令全部半透明像素对预言机系统性 -1；s2-04 右组逐元素
/// opacity 0.5 的预言机像素与 round(127.5)=128 精确一致）。
///
/// 白底臂（车道BA，ADR-0010）：dst 为不透明白时走 Chrome/Skia u8
/// 量化整数模型——`a8=round(255a)`，
/// `out=round((c·a8+255·(255−a8))/255)`（u32 半进位）。65536 例穷举
/// 实证：与旧透明管线「直存 (c,a8) + PIL 白底合成」**逐位等价**。
/// 白底初始化后「首墨落背景」全部落本臂；若不设本臂而走通用直色
/// 截断，半透明纯色/层合成对预言机有 −0.5 均值偏置（实测 s5-01
/// +4~12pp）。**仅限白底**：非白不透明底（叠已绘内容）保持历史
/// 截断路径——旧管线对它的语义是引擎内截断（非 PIL 兜底），run2
/// 实测 round 化会让 s3-03/s10-03 层叠像素 +1.8~2.2pp（历史调优
/// 基线在截断侧）。
fn blend_px(px: &mut [u8], c: Rgba8, a: f32) {
    let da = px[3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0 {
        return;
    }
    if px[3] == 255 && px[0] == 255 && px[1] == 255 && px[2] == 255 {
        let a8 = (a * 255.0 + 0.5) as u8; // a∈[0,1]，饱和转换兜 255.5
        if a8 == 0 {
            return; // 量化后无墨：白底保持（Chrome α8=0 不描绘）
        }
        if a8 < 255 {
            let w_d = 255 - a8 as u32;
            // round(x/255) = (2x+255)/510（半进位，x ≤ 255·255+255·255）
            let mix =
                |sc: u8| -> u8 { ((2 * (sc as u32 * a8 as u32 + 255 * w_d) + 255) / 510) as u8 };
            px[0] = mix(c.r);
            px[1] = mix(c.g);
            px[2] = mix(c.b);
        } else {
            px[0] = c.r;
            px[1] = c.g;
            px[2] = c.b;
        }
        return; // alpha 恒 255
    }
    let mix = |sc: u8, dc: u8| -> u8 {
        let sv = sc as f32;
        let dv = dc as f32 * da;
        ((sv * a + dv * (1.0 - a)) / out_a) as u8
    };
    px[0] = mix(c.r, px[0]);
    px[1] = mix(c.g, px[1]);
    px[2] = mix(c.b, px[2]);
    // round(x) ≡ trunc(x+0.5)（正数半进位，与 libm::roundf 同结果），
    // 免函数调用——blend_px 是逐像素热路径（frog 基准 +15% 的教训）；
    // float→int 饱和转换兜底 255.5 → 255
    px[3] = (out_a * 255.0 + 0.5) as u8;
}

/// 渐变填充的 Chrome/Skia 单次舍入合成（车道AT，f32 premul 管线）：
/// `pm` = premul RGB（已含 8×8 抖动并钳 [0,255]，来自
/// `gradient::SpanEval::at_premul`），`sa` = 直 alpha [0,1]，`k` =
/// opacity·coverage 折叠（premul 域整体缩放，与 Skia coverage/层语义
/// 一致）。对直色 u8 帧缓冲做 src-over：颜色与 alpha 各一次舍入。
///
/// 与旧路径（u8 unpremul 双重量化 + alpha 抖动 + blend_px 截断）的差异
/// 即探针 at3/at4 实证的 Chrome 语义——`round(max(c·α+d,0) + 255(1−α))`
/// 对白底逐位一致。不透明（sa==1 ∧ k==1）时 `round(clamp(c+d))` ≡ 旧
/// `round(c+d)` 直写——不透明渐变全语料字节不变。
fn blend_gradient_pm(px: &mut [u8], pm: [f32; 3], sa: f32, k: f32) {
    let sa_k = sa * k;
    let da = px[3] as f32 / 255.0;
    let out_a = sa_k + da * (1.0 - sa_k);
    if out_a <= 0.0 {
        return;
    }
    let inv = 1.0 - sa_k;
    let mix = |pm_c: f32, dc: u8| -> u8 {
        let out_pm = pm_c * k + dc as f32 * da * inv;
        let straight = out_pm / out_a;
        // 四舍五入 + 钳制（out_pm 数学上 ∈ [0, out_a·255]，除法后仍在
        // 域内；此处钳制兜 float 边界）
        let v = libm::roundf(straight);
        if v < 0.0 {
            0
        } else if v > 255.0 {
            255
        } else {
            v as u8
        }
    };
    px[0] = mix(pm[0], px[0]);
    px[1] = mix(pm[1], px[1]);
    px[2] = mix(pm[2], px[2]);
    px[3] = (out_a * 255.0 + 0.5) as u8;
}

/// 把纯色矩形写入宿主 RGBA8 帧缓冲（直写不做混合；越界自动裁剪）。
/// M0 遗留的便捷原语，保留给宿主直接画底色/清屏用。
pub fn fill_rect_rgba8(buf: &mut [u8], vp: &Viewport, x0: i32, y0: i32, w: i32, h: i32, c: Rgba8) {
    debug_assert!(matches!(vp.format, PixFmt::Rgba8));
    let stride = vp.width as usize * 4;
    let xs = x0.clamp(0, vp.width as i32);
    let ys = y0.clamp(0, vp.height as i32);
    let xe = (x0 + w).clamp(0, vp.width as i32);
    let ye = (y0 + h).clamp(0, vp.height as i32);
    for y in ys..ye {
        let row = y as usize * stride;
        for x in xs..xe {
            let o = row + x as usize * 4;
            buf[o] = c.r;
            buf[o + 1] = c.g;
            buf[o + 2] = c.b;
            buf[o + 3] = c.a;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dl::{FillRule, GradStop, LinearGradient, SpreadMethod};
    use alloc::vec;

    /// 测试采样像素偏移（4×4 RGBA 缓冲；避免 identity_op lint 的字面算式）
    fn px_off(x: usize, y: usize) -> usize {
        (y * 4 + x) * 4
    }

    fn red() -> Rgba8 {
        Rgba8 {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        }
    }

    fn blue() -> Rgba8 {
        Rgba8 {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        }
    }

    fn vp2x2() -> Viewport {
        Viewport {
            width: 2,
            height: 2,
            format: PixFmt::Rgba8,
            aa: 1,
        }
    }

    #[test]
    fn fill_full() {
        let mut buf = [0u8; 16];
        fill_rect_rgba8(&mut buf, &vp2x2(), 0, 0, 2, 2, red());
        assert!(buf.chunks(4).all(|px| px == [255, 0, 0, 255]));
    }

    /// 部分越界的矩形：裁剪到视口，只写 (1,1)。
    #[test]
    fn fill_clips_to_viewport() {
        let mut buf = [255u8; 16];
        fill_rect_rgba8(&mut buf, &vp2x2(), 1, 1, 5, 5, blue());
        assert_eq!(&buf[12..16], &[0, 0, 255, 255]);
        assert!(buf[..12].chunks(4).all(|px| px == [255; 4]));
    }

    /// 完全越界：不 panic、不写入。
    #[test]
    fn fill_fully_outside_is_noop() {
        let mut buf = [7u8; 16];
        fill_rect_rgba8(&mut buf, &vp2x2(), 10, 10, 4, 4, red());
        assert!(buf.iter().all(|&b| b == 7));
    }

    /// DisplayList → render 全链路：矩形走通用扫描线。
    #[test]
    fn render_displaylist_rect() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let mut buf = [0u8; 16];
        render(&dl, &vp2x2(), &mut buf);
        assert!(buf.chunks(4).all(|px| px == [255, 0, 0, 255]));
    }

    /// 三角形（非轴对齐）扫描线。
    #[test]
    fn render_triangle() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(4.0, 0.0), (0.0, 4.0), (8.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(blue()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 8,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = [0u8; 8 * 4 * 4];
        render(&dl, &vp, &mut buf);
        // 顶点行 (y=0..1)：只填中轴附近
        let px = |x: u32, y: u32| -> [u8; 4] {
            let o = (y * 8 + x) as usize * 4;
            [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
        };
        // 顶点行：像素中心 x+0.5 ∈ [3.5, 4.5) → 只有 px=3 被填
        assert_eq!(px(3, 0), [0, 0, 255, 255]);
        assert_eq!(px(0, 0), [0, 0, 0, 0]);
        assert_eq!(px(1, 3), [0, 0, 255, 255]);
        assert_eq!(px(6, 3), [0, 0, 255, 255]); // 底行右缘：7.5 恰在边上不填
    }

    /// evenodd：两环重叠区挖洞。
    #[test]
    fn evenodd_hole() {
        let mut dl = DisplayList::default();
        let outer: &[(f32, f32)] = &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        let inner: &[(f32, f32)] = &[(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)];
        let d = dl.push_rings(&[outer, inner]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::EvenOdd,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 8,
            height: 8,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = [0u8; 8 * 8 * 4];
        render(&dl, &vp, &mut buf);
        let px = |x: u32, y: u32| -> [u8; 4] {
            let o = (y * 8 + x) as usize * 4;
            [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
        };
        assert_eq!(px(0, 0), [255, 0, 0, 255]); // 外圈填充
        assert_eq!(px(4, 4), [0, 0, 0, 0]); // 内环挖空
    }

    /// 裁切矩形：溢出视口的填充被截断。
    #[test]
    fn clip_rect_trims() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        let d = dl.push_rings(&[ring]);
        let clip = dl.push_clip([0.0, 0.0, 4.0, 4.0]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(blue()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 8,
            height: 8,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = [0u8; 8 * 8 * 4];
        render(&dl, &vp, &mut buf);
        let px = |x: u32, y: u32| -> [u8; 4] {
            let o = (y * 8 + x) as usize * 4;
            [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
        };
        assert_eq!(px(3, 3), [0, 0, 255, 255]); // 裁切内
        assert_eq!(px(4, 3), [0, 0, 0, 0]); // 裁切外
        assert_eq!(px(7, 7), [0, 0, 0, 0]);
    }

    /// alpha 混合：半透明红叠在白底上。
    #[test]
    fn alpha_blend() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 0.5,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let mut buf = [255u8; 16]; // render 会清屏为透明，白底无效
        render(&dl, &vp2x2(), &mut buf);
        // 透明底上的半透明红：rgb 保持纯红、alpha 四舍五入（S2：
        // 127.5 → 128，对齐 Chrome——旧截断 127 对预言机系统性 -1）
        assert!(buf.chunks(4).all(|px| px == [255, 0, 0, 128]));
    }

    /// 对角三角形（RC4 解析式覆盖）：边缘出现中间 alpha 的过渡像素，
    /// 内部像素满覆盖不回归（coverage=1 时与 aa=1 混合结果一致），
    /// 且无 alpha=0 内洞。车道KC：顶点行 0 的边界像素双档钉死——
    /// legacy aa=2（1/4 网格吸附）给 96/159（端点舍入偏差 ±31.5，
    /// 图标海量集 tol32 细环的误差形态）；精档 aa≥3（浮点端点）给
    /// 128/128，恰为真值覆盖 0.5（127.5 四舍五入）。
    #[test]
    fn aa2_triangle_edge_transition() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(4.0, 0.0), (0.0, 4.0), (8.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(blue()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let render_at = |aa: u8| -> alloc::vec::Vec<u8> {
            let vp = Viewport {
                width: 8,
                height: 4,
                format: PixFmt::Rgba8,
                aa,
            };
            let mut buf = alloc::vec![0u8; 8 * 4 * 4];
            render(&dl, &vp, &mut buf);
            buf
        };
        let alpha = |buf: &[u8], x: u32, y: u32| buf[((y * 8 + x) * 4 + 3) as usize];
        let legacy = render_at(2);
        let fine = render_at(3);

        // 内部像素：解析覆盖恰为 1 → alpha 与 aa=1 相同的满覆盖 255
        for (x, y) in [(1, 3), (4, 3), (6, 3), (4, 2)] {
            assert_eq!(
                alpha(&legacy, x, y),
                255,
                "内部像素 ({x},{y}) 应回归为满覆盖"
            );
            assert_eq!(alpha(&fine, x, y), 255, "内部像素 ({x},{y}) 应回归为满覆盖");
        }
        // 边缘存在中间 alpha（过渡像素）
        let partial = |buf: &[u8]| {
            (0..8 * 4).any(|i| {
                let a = buf[i * 4 + 3];
                a > 1 && a < 254
            })
        };
        assert!(partial(&legacy), "解析覆盖应产生中间 alpha 的边缘过渡像素");
        assert!(partial(&fine), "解析覆盖应产生中间 alpha 的边缘过渡像素");
        // 顶点行 0：legacy 吸附口径 96/159（4 子行 × 1/4 档：0+16+32+48
        // 与 16+32+48+63）；精档 8+24+40+56 = 128 精确命中真值。
        // 旧 2x2 子采样只能给 3/4=191 与 1/4=63 两档。
        assert_eq!(alpha(&legacy, 3, 0), 96);
        assert_eq!(alpha(&legacy, 4, 0), 159);
        assert_eq!(alpha(&fine, 3, 0), 128);
        assert_eq!(alpha(&fine, 4, 0), 128);
        // 底行整行落在三角覆盖带内，不允许 alpha=0 内洞
        for x in 0..8 {
            assert!(alpha(&legacy, x, 3) > 0, "({x},3) 出现内洞");
            assert!(alpha(&fine, x, 3) > 0, "({x},3) 出现内洞");
        }
    }

    /// aa=4 直角三角形（斜边 x=y，真实面积 32px）：行内覆盖率从边缘到
    /// 内部单调不减；解析式覆盖下总覆盖落在真实面积的 1/16 吸附邻域
    /// （旧 aa=1 中心采样系统性高估到 36）。
    #[test]
    fn aa4_coverage_monotonic_and_sum() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let render_sum = |aa: u8| -> f32 {
            let vp = Viewport {
                width: 8,
                height: 8,
                format: PixFmt::Rgba8,
                aa,
            };
            let mut buf = [0u8; 8 * 8 * 4];
            render(&dl, &vp, &mut buf);
            buf.chunks(4).map(|p| f32::from(p[3]) / 255.0).sum()
        };

        // 行 y=1：覆盖率沿 x 单调不减（px0=0 → px1=1/2 → 其余满覆盖）
        let vp = Viewport {
            width: 8,
            height: 8,
            format: PixFmt::Rgba8,
            aa: 4,
        };
        let mut buf = [0u8; 8 * 8 * 4];
        render(&dl, &vp, &mut buf);
        let alphas: Vec<u8> = (0..8).map(|x| buf[((8 + x) * 4 + 3) as usize]).collect();
        for pair in alphas.windows(2) {
            assert!(pair[0] <= pair[1], "行 y=1 覆盖率应单调不减：{alphas:?}");
        }

        // aa=1 像素中心采样系统性高估斜边面积（Σ=36）；解析式覆盖（aa>1）
        // 与参考实现同模型，总和落在面积 32 的 1/16 吸附邻域（≈31）。
        let sum1 = render_sum(1);
        let sum4 = render_sum(4);
        assert!(
            (sum1 - 32.0).abs() > (sum4 - 32.0).abs(),
            "aa1={sum1} aa4={sum4}"
        );
        assert!(sum4 > 30.0 && sum4 < 33.0, "解析覆盖总和应≈面积 32：{sum4}");
    }

    /// RC4 解析式覆盖（1/3）：竖直边落在像素正中 → 该像素覆盖恰为 1/2
    /// （每子行 2 个 1/4 档 ×16 → alpha 128），整像素内部满覆盖 255，
    /// 未覆盖像素为 0。
    #[test]
    fn analytic_half_pixel_vertical_edge() {
        let mut dl = DisplayList::default();
        // 宽 2.5 的矩形：右缘 x=2.5 把像素 2 劈成两半
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (2.5, 0.0), (2.5, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 2,
        };
        let mut buf = [0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf);
        let alpha = |x: u32, y: u32| buf[((y * 4 + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 1), 255, "整像素内部应满覆盖");
        assert_eq!(alpha(1, 1), 255, "整像素内部应满覆盖");
        assert_eq!(alpha(2, 1), 128, "半像素覆盖 → alpha 恰为 256/2");
        assert_eq!(alpha(3, 1), 0, "未覆盖像素");
    }

    /// RC4 解析式覆盖（2/3）：整数坐标矩形的 aa=2 输出与 aa=1 逐字节一致
    /// （边在像素边界上不产生中间 alpha，coverage 恰为 1 或 0）。
    #[test]
    fn analytic_aa2_matches_aa1_on_integer_rect() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(1.0, 1.0), (6.0, 1.0), (6.0, 6.0), (1.0, 6.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(blue()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let render_at = |aa: u8| -> alloc::vec::Vec<u8> {
            let vp = Viewport {
                width: 8,
                height: 8,
                format: PixFmt::Rgba8,
                aa,
            };
            let mut buf = alloc::vec![0u8; 8 * 8 * 4];
            render(&dl, &vp, &mut buf);
            buf
        };
        assert_eq!(render_at(1), render_at(2), "整数矩形两路径应逐字节一致");
        assert_eq!(render_at(1), render_at(4), "aa 倍率不应影响解析覆盖结果");
    }

    /// RC4 解析式覆盖（3/3）：顶点位于行带内部（尖顶 y=0.2）→ 行 0 得到
    /// 解析式部分覆盖。精档 aa=3（车道KC 浮点端点）：像素 3/4 = 24+60+63
    /// = 147，对真值 146.6；legacy 档 aa=2 的 1/4 网格吸附给 159（端点
    /// 舍入偏差 +12）。旧 2x2 子采样只能量化到 0/半档/整档。
    #[test]
    fn analytic_vertex_sliver_partial_row() {
        let mut dl = DisplayList::default();
        // 尖顶 (4, 0.2) 落在行 0 的带内，底边 y=2
        let ring: &[(f32, f32)] = &[(4.0, 0.2), (0.0, 2.0), (8.0, 2.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let render_at = |aa: u8| -> alloc::vec::Vec<u8> {
            let vp = Viewport {
                width: 8,
                height: 2,
                format: PixFmt::Rgba8,
                aa,
            };
            let mut buf = alloc::vec![0u8; 8 * 2 * 4];
            render(&dl, &vp, &mut buf);
            buf
        };
        let alpha = |buf: &[u8], x: u32, y: u32| buf[((y * 8 + x) * 4 + 3) as usize];
        // 精档 aa=3：像素 2 = 0.5 档×64 = 32，像素 3/4 = 24+60+63 = 147
        let fine = render_at(3);
        assert_eq!(alpha(&fine, 0, 0), 0, "尖顶行远离顶点的像素不覆盖");
        assert_eq!(alpha(&fine, 2, 0), 32);
        assert_eq!(alpha(&fine, 3, 0), 147);
        assert_eq!(alpha(&fine, 4, 0), 147);
        assert_eq!(alpha(&fine, 5, 0), 32);
        // legacy 档 aa=2：吸附口径 32/159/159/32（历史契约保持）
        let legacy = render_at(2);
        assert_eq!(alpha(&legacy, 2, 0), 32);
        assert_eq!(alpha(&legacy, 3, 0), 159);
        assert_eq!(alpha(&legacy, 4, 0), 159);
        assert_eq!(alpha(&legacy, 5, 0), 32);
    }

    /// RC4 nonzero 挖洞边界：洞左缘 x=2.5 → 边界像素覆盖 0.5（精确扣除），
    /// 洞内像素 0，环带像素满覆盖。
    #[test]
    fn analytic_nonzero_hole_edge_half_coverage() {
        let mut dl = DisplayList::default();
        let outer: &[(f32, f32)] = &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        // 反向环绕的洞（nonzero 扣除），左缘 x=2.5
        let hole: &[(f32, f32)] = &[(2.5, 2.0), (2.5, 6.0), (6.0, 6.0), (6.0, 2.0)];
        let d = dl.push_rings(&[outer, hole]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 8,
            height: 8,
            format: PixFmt::Rgba8,
            aa: 2,
        };
        let mut buf = [0u8; 8 * 8 * 4];
        render(&dl, &vp, &mut buf);
        let alpha = |x: u32, y: u32| buf[((y * 8 + x) * 4 + 3) as usize];
        assert_eq!(alpha(1, 3), 255, "环带整像素满覆盖");
        assert_eq!(alpha(2, 3), 128, "洞边界像素半覆盖（精确扣除）");
        assert_eq!(alpha(3, 3), 0, "洞内不覆盖");
        assert_eq!(alpha(4, 3), 0, "洞内不覆盖");
        assert_eq!(alpha(6, 3), 255, "洞右缘恰在 x=6 整数边界 → 像素 6 满覆盖");
    }

    /// RC4 evenodd 区间长度覆盖：洞边界落在像素内部 → 边界像素按交集
    /// 长度 0.5 覆盖；洞外整像素满覆盖。
    #[test]
    fn analytic_evenodd_hole_boundary_half() {
        let mut dl = DisplayList::default();
        let outer: &[(f32, f32)] = &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
        let hole: &[(f32, f32)] = &[(2.5, 2.0), (6.0, 2.0), (6.0, 6.0), (2.5, 6.0)];
        let d = dl.push_rings(&[outer, hole]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::EvenOdd,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 8,
            height: 8,
            format: PixFmt::Rgba8,
            aa: 2,
        };
        let mut buf = [0u8; 8 * 8 * 4];
        render(&dl, &vp, &mut buf);
        let alpha = |x: u32, y: u32| buf[((y * 8 + x) * 4 + 3) as usize];
        assert_eq!(alpha(1, 3), 255, "洞外整像素满覆盖");
        assert_eq!(alpha(2, 3), 128, "洞边界像素交集长度 0.5");
        assert_eq!(alpha(3, 3), 0, "洞内不覆盖");
        assert_eq!(alpha(6, 3), 255, "右 span 整像素满覆盖");
    }

    /// 渐变 paint 的两条光栅化路径：aa=1 逐像素与 aa>1 coverage 混合，
    /// 内部满覆盖像素两者逐字节一致（coverage=1，色取像素中心）。
    #[test]
    fn gradient_alpha_aa_coverage_paths() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        use crate::dl::{GradStop, LinearGradient, SpreadMethod};
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::LinearGradient(LinearGradient {
                p0: (0.0, 0.0),
                p1: (0.0, 4.0), // 垂直轴：t=(y+0.5)/4
                stops: vec![
                    GradStop {
                        offset: 0.0,
                        color: Rgba8 {
                            r: 0,
                            g: 0,
                            b: 0,
                            a: 255,
                        },
                    },
                    GradStop {
                        offset: 1.0,
                        color: Rgba8 {
                            r: 0,
                            g: 0,
                            b: 0,
                            a: 0,
                        },
                    },
                ],
                spread: SpreadMethod::Pad,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let render_at = |aa: u8| -> alloc::vec::Vec<u8> {
            let vp = Viewport {
                width: 4,
                height: 4,
                format: PixFmt::Rgba8,
                aa,
            };
            let mut buf = alloc::vec![0u8; 4 * 4 * 4];
            render(&dl, &vp, &mut buf);
            buf
        };
        let buf1 = render_at(1);
        let alpha = |buf: &[u8], y: usize| buf[(y * 4 + 2) * 4 + 3];
        // t=(y+0.5)/4 → a=223/159/96/32（±1：渐变 Bayer 抖动）
        let ca = |a: u8, b: u8| (i16::from(a) - i16::from(b)).abs() <= 1;
        assert!(ca(alpha(&buf1, 0), 223));
        assert!(ca(alpha(&buf1, 1), 159));
        assert!(ca(alpha(&buf1, 2), 96));
        assert!(ca(alpha(&buf1, 3), 32));
        // aa=2：内部满覆盖 → 与 aa=1 一致（±1/通道，抖动路径同坐标应同值；
        // 垂直渐变 aa=2 子行 yc 不同 → Bayer 行号不同 → 允许 ±1）
        let buf2 = render_at(2);
        let diffs: usize = buf1
            .chunks(4)
            .zip(buf2.chunks(4))
            .map(|(p, q)| {
                p.iter()
                    .zip(q)
                    .filter(|(a, b)| (i16::from(**a) - i16::from(**b)).abs() > 1)
                    .count()
            })
            .sum();
        assert_eq!(diffs, 0, "两路径超出 ±1 抖动容差");
    }

    /// 车道AT 端到端钉：白底上的 alpha 线性渐变按 Chrome 单次舍入管线
    /// 合成——`round(max(c·α+d, 0) + 255(1−α))`，alpha 无抖动。旧管线
    /// （unpremul 双量化 + alpha 抖动 + trunc 合成）在 (1,1) 给 95，
    /// Chrome/新管线给 96。
    #[test]
    fn gradient_alpha_over_white_single_rounding() {
        use crate::gradient::{bayer_off, TILE_SHIFT_TALL};
        let mut dl = DisplayList::default();
        // 白底（不透明）
        let bg_ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let bg = dl.push_rings(&[bg_ring]);
        dl.cmds.push(Cmd::Fill {
            d: bg,
            paint: Paint::Solid(Rgba8 {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        // 蓝→透明 alpha 渐变（水平轴）
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::LinearGradient(LinearGradient {
                p0: (0.0, 0.0),
                p1: (4.0, 0.0), // t=(x+0.5)/4
                stops: vec![
                    GradStop {
                        offset: 0.0,
                        color: Rgba8 {
                            r: 0,
                            g: 0,
                            b: 255,
                            a: 255,
                        },
                    },
                    GradStop {
                        offset: 1.0,
                        color: Rgba8 {
                            r: 0,
                            g: 0,
                            b: 255,
                            a: 0,
                        },
                    },
                ],
                spread: SpreadMethod::Pad,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 2,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf);
        // (1,1)：t=0.375 α=0.625；d(1,1)=(M[1][1]−31.5)/64=−0.2421875
        let px = &buf[(4 + 1) * 4..(4 + 1) * 4 + 4];
        let d = bayer_off(1, 1, TILE_SHIFT_TALL);
        let a_f = 0.625f32;
        let want_r = (0.0f32.max(d) + 255.0 * (1.0 - a_f)).round() as u8; // clamp0 + 单次舍入
        let want_b = (255.0 * a_f).max(d) + 255.0 * (1.0 - a_f); // f32
        let want_b = libm::roundf(want_b) as u8;
        assert_eq!(px[0], want_r, "R 应为 clamp(max(d,0))+255(1-α) 单次舍入");
        assert_eq!(px[1], want_r);
        assert_eq!(px[2], want_b);
        assert_eq!(px[3], 255, "白底不透明");
    }

    // ============ ADR-0010：渲染目标初始化域（白底入口） ============

    /// 蓝渐变 alpha DL：0..4 全幅 (0,0)-(4,4) 矩形，线性蓝(alpha 1→0)，
    /// p0=(0,0) p1=(4,0)（与 AT 探针 at4/AT 端到端钉同几何）。
    fn blue_fade_dl() -> DisplayList {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::LinearGradient(LinearGradient {
                p0: (0.0, 0.0),
                p1: (4.0, 0.0),
                stops: vec![
                    GradStop {
                        offset: 0.0,
                        color: Rgba8 {
                            r: 0,
                            g: 0,
                            b: 255,
                            a: 255,
                        },
                    },
                    GradStop {
                        offset: 1.0,
                        color: Rgba8 {
                            r: 0,
                            g: 0,
                            b: 255,
                            a: 0,
                        },
                    },
                ],
                spread: SpreadMethod::Pad,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        dl
    }

    /// API 钉：`render_bg(透明)` 与缺省 `render` 逐字节一致（bg 穿线
    /// 不改变透明缺省语义；FFI/Android/ESP32 零回归的入口级保证）。
    #[test]
    fn render_bg_transparent_matches_render() {
        let dl = blue_fade_dl();
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 2,
        };
        let mut a = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut a);
        let mut b = alloc::vec![0u8; 4 * 4 * 4];
        render_bg(&dl, &vp, &mut b, BG_TRANSPARENT);
        assert_eq!(a, b, "透明 bg 必须与缺省 render 逐字节一致");
    }

    /// 初始化生效钉：空 DL + 白底 → 整幅 (255,255,255,255)——防止
    /// 「fill_init 被 render 内部清屏覆盖」的回归。
    #[test]
    fn white_bg_init_empty_dl_is_white() {
        let dl = DisplayList::default();
        let vp = Viewport {
            width: 3,
            height: 2,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 3 * 2 * 4];
        render_bg(&dl, &vp, &mut buf, BG_WHITE);
        assert!(
            buf.chunks(4).all(|px| px == [255, 255, 255, 255]),
            "空 DL 白底初始化应整幅不透明白"
        );
    }

    /// 车道BA 核心钉：白底初始化下 alpha 渐变按 Chrome 单次舍入管线
    /// 端到端出字节——`round(max(c·α+d,0) + 255(1−α))`，alpha 恒 255。
    /// (1,1)：t=0.375 α=0.625，d=−0.2421875 → R=G=96（透明域+后置合成
    /// 旧管线给 95 的判别格），B=255。与 AT 端到端钉（DL 内画白底）
    /// 互为独立验证：本测试白底来自 render_bg 初始化而非 DL 内容。
    #[test]
    fn white_bg_gradient_chrome_single_rounding() {
        let dl = blue_fade_dl();
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 2,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render_bg(&dl, &vp, &mut buf, BG_WHITE);
        let px = &buf[(4 + 1) * 4..(4 + 1) * 4 + 4];
        assert_eq!(px, &[96, 96, 255, 255], "(1,1) 应为 Chrome 单次舍入字节");
        assert!(
            buf.chunks(4).all(|p| p[3] == 255),
            "白底初始化输出 alpha 应恒 255"
        );
    }

    /// 零回归钉：不透明内容（整像素对齐轴对齐矩形，aa=1 无 AA 半覆盖）
    /// 在两初始化域下逐字节一致——透明域存储 + 恒等白合成 ≡ 白底直混
    /// （blend_px 对不透明源 mix ≡ 源色）。
    #[test]
    fn white_bg_opaque_rect_identical_across_domains() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(Rgba8 {
                r: 0,
                g: 0,
                b: 255,
                a: 255,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut a = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut a);
        let mut b = alloc::vec![0u8; 4 * 4 * 4];
        render_bg(&dl, &vp, &mut b, BG_WHITE);
        // 着墨像素跨域恒等（不透明源 mix ≡ 源色；空白像素域外本就
        // 「透明 vs 白」不同，不在恒等断言内）
        for (pa, pb) in a.chunks(4).zip(b.chunks(4)) {
            if pa[3] > 0 {
                assert_eq!(pa, pb, "着墨像素在两初始化域下必须逐字节一致");
            }
        }
        // 白底域：矩形内部 = 源色蓝不透明（像素 (1,1) = 偏移 4+1 的
        // 第 20 字节）；矩形外 = 白
        assert_eq!(&b[(4 + 1) * 4..(4 + 1) * 4 + 4], &[0, 0, 255, 255]);
        assert_eq!(&b[0..4], &[255, 255, 255, 255]);
    }

    /// 白底上的半透明纯色：白底臂整数舍入——opacity=0.5 红（f32 精确
    /// 0.5）→ R=round((255·128+255·127)/255)=255、G=B=127、A=255。
    /// Chrome 同形态（paint α8=128）：R=128+127=255、G=B=127——逐位一致。
    #[test]
    fn white_bg_solid_semitransparent_over_white() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(Rgba8 {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }),
            opacity: 0.5,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render_bg(&dl, &vp, &mut buf, BG_WHITE);
        for px in buf.chunks(4) {
            assert_eq!(px, &[255, 127, 127, 255], "半透明红对白底单次舍入");
        }
    }

    /// 车道BA run2 教训钉：**非白**不透明底上的叠层保持历史直色截断
    /// 路径（旧管线对该形态无 PIL 兜底，主语料 s3-03/s10-03 调优基线
    /// 在截断侧）。灰 103 底 + 黑 overlay opacity=0.68：
    /// 截断模型 trunc(103·(1−a)) = trunc(32.96) = **32**；若误走白底臂
    /// 整数式（a8=173）round(103·82/255) = **33**——本格即两模型分歧
    /// 判别格，钉死分支路由。
    #[test]
    fn stacked_over_nonwhite_opaque_keeps_truncation() {
        let mut dl = DisplayList::default();
        // 底：不透明灰 (103,103,103) 全幅
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(Rgba8 {
                r: 103,
                g: 103,
                b: 103,
                a: 255,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        // 顶：半透明黑 opacity 0.68
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::Solid(Rgba8 {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }),
            opacity: 0.68,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf); // 透明域即可触发（dst 已被灰填成不透明）
        for px in buf.chunks(4) {
            assert_eq!(px, &[32, 32, 32, 255], "非白不透明底叠层应保持截断语义");
        }
    }

    // ============ feComposite：linearRGB 工作域（车道V） ============

    /// 1×1 位图便捷构造（直色 sRGBA）。
    fn px1(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        alloc::vec![r, g, b, a]
    }

    fn px_of(buf: &[u8]) -> [u8; 4] {
        [buf[0], buf[1], buf[2], buf[3]]
    }

    fn close4(got: [u8; 4], want: [u8; 4]) -> bool {
        got.iter()
            .zip(want)
            .all(|(a, b)| (i16::from(*a) - i16::from(b)).abs() <= 1)
    }

    /// atop 半透明红/青柠：premul-linear 域 out=(0.25,0.25,0)@α0.5 →
    /// 直色 linear(0.5,0.5,0) → sRGB(188,188,0)。sRGB 域直算给
    /// (128,128,0)——本格即车道U a1-04 判别格的直色形态（Chrome
    /// obs_b=(94,94,0)=round(188·0.5)）。
    #[test]
    fn composite_atop_half_alpha_works_in_linear() {
        let s = px1(255, 0, 0, 128);
        let d = px1(0, 255, 0, 128);
        let out = composite_pd(&s, &d, 1, 1, FeCompositeOp::Atop, FeColorCtx::LINEAR);
        assert!(
            close4(px_of(&out), [188, 188, 0, 128]),
            "atop 半透明应 linear 域求值：{:?}",
            px_of(&out)
        );
    }

    /// 各算子 α=1 不透明原色：gamma 是恒等变换，linear 域结果与旧
    /// sRGB 域字节一致（等价性回归——既有 in/atop 语料不因换域漂移）。
    #[test]
    fn composite_operators_gamma_invariant_at_full_alpha() {
        let red = px1(255, 0, 0, 255);
        let lime = px1(0, 255, 0, 255);
        let cases = [
            (FeCompositeOp::Over, [255, 0, 0, 255]),
            (FeCompositeOp::In, [255, 0, 0, 255]),
            // Out：αd=1 → 源整体被裁掉；Xor：α=1+1−2=0
            (FeCompositeOp::Out, [0, 0, 0, 0]),
            (FeCompositeOp::Atop, [255, 0, 0, 255]),
            (FeCompositeOp::Xor, [0, 0, 0, 0]),
        ];
        for (op, want) in cases {
            let out = composite_pd(&red, &lime, 1, 1, op, FeColorCtx::LINEAR);
            assert_eq!(px_of(&out), want, "{op:?} α=1 不透明目的");
        }
        // 空目标算子语义：out（红×空=红）、atop/in（×空=透明）
        let empty = px1(0, 0, 0, 0);
        assert_eq!(
            px_of(&composite_pd(
                &red,
                &empty,
                1,
                1,
                FeCompositeOp::Out,
                FeColorCtx::LINEAR
            )),
            [255, 0, 0, 255]
        );
        assert_eq!(
            px_of(&composite_pd(
                &red,
                &empty,
                1,
                1,
                FeCompositeOp::Atop,
                FeColorCtx::LINEAR
            ))[3],
            0,
            "atop 空目标 → α=0"
        );
        assert_eq!(
            px_of(&composite_pd(
                &empty,
                &lime,
                1,
                1,
                FeCompositeOp::Atop,
                FeColorCtx::LINEAR
            )),
            [0, 255, 0, 255],
            "atop 空源 → 纯目标"
        );
    }

    /// arithmetic premul-linear 域：k 全 0.5 × α(0.5,0.5) 红/青柠
    ///（车道U a1-18 判别格，Chrome obs=(225,225,188)@α1）：
    /// v_R=v_G=0.5·0+0.5·0.5+0.5·0+0.5=0.75、v_B=0.5；α=1.125→钳 1。
    #[test]
    fn composite_arithmetic_premul_linear_replicates_a1_18() {
        let s = px1(255, 0, 0, 128);
        let d = px1(0, 255, 0, 128);
        let op = FeCompositeOp::Arithmetic {
            k1: 0.5,
            k2: 0.5,
            k3: 0.5,
            k4: 0.5,
        };
        let out = composite_pd(&s, &d, 1, 1, op, FeColorCtx::LINEAR);
        assert!(
            close4(px_of(&out), [225, 225, 188, 255]),
            "arithmetic premul-linear：{:?}",
            px_of(&out)
        );
    }

    /// arithmetic 系数分工 + clamp：k2 独占 → v=k2·i1、α=k2·αs，反预
    /// 乘比值 = 纯源直色（红 255）；k4 独占铺空区（α=k4、直色白）；
    /// 全零系数 = 全透明；k1 独占交叉项只点亮 R/G 双非零通道。
    #[test]
    fn composite_arithmetic_coefficients_and_clamp() {
        let s = px1(255, 0, 0, 128); // i1 premul-linear = (0.502, 0, 0)
        let d = px1(0, 255, 0, 128); // i2 premul-linear = (0, 0.502, 0)
                                     // k2 独占：v_R=0.4·0.502、α=0.4·0.502 → v/α=1 → 纯红直色
        let out = composite_pd(
            &s,
            &d,
            1,
            1,
            FeCompositeOp::Arithmetic {
                k1: 0.0,
                k2: 0.4,
                k3: 0.0,
                k4: 0.0,
            },
            FeColorCtx::LINEAR,
        );
        assert!(
            close4(px_of(&out), [255, 0, 0, 51]),
            "k2 独占：α=0.4·0.502·255≈51，直色=源：{:?}",
            px_of(&out)
        );
        // k4 独占铺空区：premul=k4、α=k4 → 直色 linear 1.0（全白）
        let out = composite_pd(
            &px1(0, 0, 0, 0),
            &px1(0, 0, 0, 0),
            1,
            1,
            FeCompositeOp::Arithmetic {
                k1: 0.0,
                k2: 0.0,
                k3: 0.0,
                k4: 0.5,
            },
            FeColorCtx::LINEAR,
        );
        assert!(
            close4(px_of(&out), [255, 255, 255, 128]),
            "k4 独占：premul/α=k4 → 直色白：{:?}",
            px_of(&out)
        );
        // 全零系数 = 全透明
        let out = composite_pd(
            &s,
            &d,
            1,
            1,
            FeCompositeOp::Arithmetic {
                k1: 0.0,
                k2: 0.0,
                k3: 0.0,
                k4: 0.0,
            },
            FeColorCtx::LINEAR,
        );
        assert_eq!(px_of(&out)[3], 0, "全零系数 → 全透明");
        // k1 独占交叉项（源×目的须共享通道，改白目的）：v_R=2·0.502²
        // =0.504=v/α → 红 255；G/B 交叉积为 0 保持 0；α=2·0.502²·255≈129
        let out = composite_pd(
            &s,
            &px1(255, 255, 255, 128),
            1,
            1,
            FeCompositeOp::Arithmetic {
                k1: 2.0,
                k2: 0.0,
                k3: 0.0,
                k4: 0.0,
            },
            FeColorCtx::LINEAR,
        );
        assert!(
            close4(px_of(&out), [255, 0, 0, 129]),
            "k1 独占交叉项：{:?}",
            px_of(&out)
        );
    }

    // ============ 车道X：color-interpolation-filters 工作色域 ============

    const SRGB: FeColorCtx = FeColorCtx { linear: false };

    /// atop 半透明 α(.5,.5) 在 **sRGB 声明域**：直色 (128,128,0)@α128
    ///（工作域恒等映射直算）——即车道U a1-07 判别格的直色形态
    ///（Chrome obs_b=(64,64,0)=round(128·0.5)）；linear 域给 (188,188,0)。
    /// 两域差 ≥32 LSB（非平凡）。
    #[test]
    fn composite_atop_srgb_domain_matches_a1_07() {
        let s = px1(255, 0, 0, 128);
        let d = px1(0, 255, 0, 128);
        let srgb = px_of(&composite_pd(&s, &d, 1, 1, FeCompositeOp::Atop, SRGB));
        assert!(
            close4(srgb, [128, 128, 0, 128]),
            "sRGB 域 atop 直色应 (128,128,0,128)：{:?}",
            srgb
        );
        let lin = px_of(&composite_pd(
            &s,
            &d,
            1,
            1,
            FeCompositeOp::Atop,
            FeColorCtx::LINEAR,
        ));
        assert!(
            close4(lin, [188, 188, 0, 128]),
            "linear 域 atop 直色应 (188,188,0,128)：{:?}",
            lin
        );
        let gap = (i16::from(srgb[0]) - i16::from(lin[0])).abs();
        assert!(gap >= 32, "两域输出差应非平凡（实测 {gap} LSB）");
    }

    /// feBlend Screen 中灰自混：linear 域 167（EOTF→screen→逆 EOTF）、
    /// sRGB 声明域 192（编码值直算）——域切必须可观测（≥8 LSB）。
    #[test]
    fn blend_screen_srgb_vs_linear_diverge() {
        let g = px1(128, 128, 128, 255);
        let lin = px_of(&blend_fe(
            &g,
            &g,
            1,
            1,
            FeBlendMode::Screen,
            FeColorCtx::LINEAR,
        ));
        let srgb = px_of(&blend_fe(&g, &g, 1, 1, FeBlendMode::Screen, SRGB));
        assert!(
            close4(lin, [167, 167, 167, 255]),
            "linear Screen：{:?}",
            lin
        );
        assert!(
            close4(srgb, [192, 192, 192, 255]),
            "sRGB Screen：{:?}",
            srgb
        );
        let gap = (i16::from(srgb[0]) - i16::from(lin[0])).abs();
        assert!(gap >= 8, "两域 Screen 输出差应非平凡（实测 {gap} LSB）");
    }

    /// 车道IA：半透明 flood 的 overlay 混合无通道回绕（狼形水彩根因格）。
    /// s = feFlood rgb(11,99,168)@α0.651（直色链缓冲约定），d = #35497a
    /// 不透明。CSS overlay（Cb≤0.5 全通道 → B=2·Cb·Cs）+ Co 公式：
    /// co = sa·B + (1−sa)·Cb = (21.47, 62.38, 147.23) → (21,62,147)。
    /// 旧实现把直色输入再除 α（二次反预乘）：B 通道 168/0.651=258
    /// `& 0xFF` 回绕成 2 → 绿色 (30,124,44)；且旧枚举无 overlay，
    /// mode="overlay" 静默降级 Normal。两缺陷任一复活本格即红。
    #[test]
    fn blend_fe_overlay_flood_alpha_no_wraparound() {
        let s = px1(11, 99, 168, 166);
        let d = px1(53, 73, 122, 255);
        let out = px_of(&blend_fe(&s, &d, 1, 1, FeBlendMode::Overlay, SRGB));
        assert_eq!(
            out,
            [21, 62, 147, 255],
            "overlay(flood@0.651, rect) 应逐字节命中 CSS 公式"
        );
    }

    /// 车道IA：半透明输入的 Normal 混合 = premul 域 src-over，直色输入
    /// 不得再反预乘。co = sa·Cs + (1−sa)·Cb = (25.66, 89.92, 151.94)
    /// → (26,90,152)；旧 inv_s 路径产出回绕绿 (30,124,44)。
    #[test]
    fn blend_fe_normal_semitransparent_no_double_unpremul() {
        let s = px1(11, 99, 168, 166);
        let d = px1(53, 73, 122, 255);
        let out = px_of(&blend_fe(&s, &d, 1, 1, FeBlendMode::Normal, SRGB));
        assert_eq!(
            out,
            [26, 90, 152, 255],
            "normal(flood@0.651, rect) = premul src-over 直色输出"
        );
        // 反向钉：backdrop 透明处结果 = 源直色、α 保持 sa（不升到 255）。
        let dt = px1(0, 0, 0, 0);
        let out_t = px_of(&blend_fe(&s, &dt, 1, 1, FeBlendMode::Normal, SRGB));
        assert_eq!(
            out_t,
            [11, 99, 168, 166],
            "backdrop α=0：直通源直色与源 alpha"
        );
    }

    /// feColorMatrix 0.5 缩放矩阵作用于中灰：linear 域经 EOTF（≈92）、
    /// sRGB 声明域字节直算（64）；identity 矩阵两域都字节恒等。
    #[test]
    fn color_matrix_srgb_vs_linear_diverge() {
        let mut scale = [0.0f32; 20];
        scale[0] = 0.5;
        scale[6] = 0.5;
        scale[12] = 0.5;
        scale[18] = 1.0;
        let g = px1(128, 128, 128, 255);
        let lin = px_of(&color_matrix(&g, 1, 1, &scale, FeColorCtx::LINEAR));
        let srgb = px_of(&color_matrix(&g, 1, 1, &scale, SRGB));
        assert!(
            (i16::from(lin[0]) - 92).abs() <= 2,
            "linear 域 0.5 缩放 ≈92：{:?}",
            lin
        );
        assert!(
            (i16::from(srgb[0]) - 64).abs() <= 1,
            "sRGB 域 0.5 缩放 =64：{:?}",
            srgb
        );
        let mut ident = [0.0f32; 20];
        ident[0] = 1.0;
        ident[6] = 1.0;
        ident[12] = 1.0;
        ident[18] = 1.0;
        assert_eq!(
            px_of(&color_matrix(&g, 1, 1, &ident, FeColorCtx::LINEAR)),
            [128, 128, 128, 255]
        );
        assert_eq!(
            px_of(&color_matrix(&g, 1, 1, &ident, SRGB)),
            [128, 128, 128, 255]
        );
    }

    /// feComponentTransfer feFuncR type=linear slope=0.5 作用于中灰：
    /// linear 域（EOTF→函数→逆 EOTF）≈92、sRGB 声明域直算 64；identity
    /// 函数两域都字节恒等（车道X 起 RGB 传递函数按工作色域求值——此前
    /// 恒在 sRGB 编码值上直算，与缺省 linearRGB 域的其余求值器不一致）。
    #[test]
    fn component_transfer_srgb_vs_linear_diverge() {
        let fns = [
            FeTransferFn::Linear {
                slope: 0.5,
                intercept: 0.0,
            },
            FeTransferFn::Identity,
            FeTransferFn::Identity,
            FeTransferFn::Identity,
        ];
        let g = px1(128, 128, 128, 255);
        let lin = px_of(&component_transfer(&g, 1, 1, &fns, FeColorCtx::LINEAR));
        let srgb = px_of(&component_transfer(&g, 1, 1, &fns, SRGB));
        assert!(
            (i16::from(lin[0]) - 92).abs() <= 2,
            "linear 域 slope 0.5 ≈92：{:?}",
            lin
        );
        assert!(
            (i16::from(srgb[0]) - 64).abs() <= 1,
            "sRGB 域 slope 0.5 =64：{:?}",
            srgb
        );
        assert_eq!([lin[1], lin[2], lin[3]], [128, 128, 255], "G/B/A 不动");
        assert_eq!([srgb[1], srgb[2], srgb[3]], [128, 128, 255], "G/B/A 不动");
        let id = [
            FeTransferFn::Identity,
            FeTransferFn::Identity,
            FeTransferFn::Identity,
            FeTransferFn::Identity,
        ];
        assert_eq!(
            px_of(&component_transfer(&g, 1, 1, &id, FeColorCtx::LINEAR)),
            [128, 128, 128, 255]
        );
        assert_eq!(
            px_of(&component_transfer(&g, 1, 1, &id, SRGB)),
            [128, 128, 128, 255]
        );
    }

    // ------------------------------------------------------------------
    // M7：RGB565 输出路径（软件侧车道）——与「RGBA 渲染后软件转 565」
    // 的参考逐位一致（纯色 / 渐变 / 透明度 / 组层全形态）。
    // ------------------------------------------------------------------

    /// 测试内独立实现的软件 RGBA→RGB565 参考转换（不调用被测的
    /// pack_rgb565，避免同函数自证）。
    fn rgba_to_rgb565_ref(rgba: &[u8]) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec![0u8; rgba.len() / 2];
        for i in 0..rgba.len() / 4 {
            let r5 = (rgba[i * 4] >> 3) as u16;
            let g6 = (rgba[i * 4 + 1] >> 2) as u16;
            let b5 = (rgba[i * 4 + 2] >> 3) as u16;
            let v = (r5 << 11) | (g6 << 5) | b5;
            out[i * 2] = (v & 0xFF) as u8;
            out[i * 2 + 1] = (v >> 8) as u8;
        }
        out
    }

    /// 同一 DL 双格式渲染并断言 565 输出 == RGBA 参考转换。
    fn assert_rgb565_matches_rgba(dl: &DisplayList, w: u32, h: u32, aa: u8) {
        let vp_rgba = Viewport {
            width: w,
            height: h,
            format: PixFmt::Rgba8,
            aa,
        };
        let vp_565 = Viewport {
            width: w,
            height: h,
            format: PixFmt::Rgb565,
            aa,
        };
        let mut rgba = alloc::vec![0u8; (w * h * 4) as usize];
        let mut out565 = alloc::vec![0u8; (w * h * 2) as usize];
        render(dl, &vp_rgba, &mut rgba);
        render(dl, &vp_565, &mut out565);
        let expect = rgba_to_rgb565_ref(&rgba);
        assert_eq!(out565, expect, "RGB565 输出必须与 RGBA→565 参考逐位一致");
    }

    fn fill_cmd(dl: &mut DisplayList, ring: &[(f32, f32)], paint: Paint, opacity: f32) {
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint,
            opacity,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
    }

    /// 纯色：565 打包已知色值点检 + 参考一致性。
    #[test]
    fn rgb565_pure_color() {
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 2.0), (0.0, 2.0)];
        let mut dl = DisplayList::default();
        fill_cmd(&mut dl, ring, Paint::Solid(red()), 1.0);
        // 红 #FF0000 → r5=31,g6=0,b5=0 → 0xF800（小端 00 F8）
        let vp = Viewport {
            width: 4,
            height: 2,
            format: PixFmt::Rgb565,
            aa: 1,
        };
        let mut out = alloc::vec![0u8; 16];
        render(&dl, &vp, &mut out);
        assert!(out.chunks(2).all(|px| px == [0x00, 0xF8]));
        // 参考一致性（aa=1 与 aa=4 两口径）
        assert_rgb565_matches_rgba(&dl, 4, 2, 1);
        assert_rgb565_matches_rgba(&dl, 4, 2, 4);
    }

    /// 已知非边界色点检：#DDDDDD → 27/55/27 → 0xDEFB。
    #[test]
    fn rgb565_known_mid_color() {
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let mut dl = DisplayList::default();
        fill_cmd(
            &mut dl,
            ring,
            Paint::Solid(Rgba8 {
                r: 0xDD,
                g: 0xDD,
                b: 0xDD,
                a: 255,
            }),
            1.0,
        );
        let vp = Viewport {
            width: 1,
            height: 1,
            format: PixFmt::Rgb565,
            aa: 1,
        };
        let mut out = [0u8; 2];
        render(&dl, &vp, &mut out);
        assert_eq!(out, [0xFB, 0xDE]); // 0xDEFB 小端
    }

    /// 渐变：线性渐变跨全视口，逐位一致。
    #[test]
    fn rgb565_linear_gradient() {
        let mut dl = DisplayList::default();
        let ring: &[(f32, f32)] = &[(0.0, 0.0), (8.0, 0.0), (8.0, 4.0), (0.0, 4.0)];
        let d = dl.push_rings(&[ring]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint: Paint::LinearGradient(LinearGradient {
                p0: (0.0, 0.0),
                p1: (8.0, 0.0),
                stops: vec![
                    GradStop {
                        offset: 0.0,
                        color: red(),
                    },
                    GradStop {
                        offset: 1.0,
                        color: blue(),
                    },
                ],
                spread: SpreadMethod::Pad,
            }),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        assert_rgb565_matches_rgba(&dl, 8, 4, 1);
        assert_rgb565_matches_rgba(&dl, 8, 4, 4);
    }

    /// 透明度：半透明叠半透明（白底预折 + src-over 混合路径），
    /// 含元素级 opacity 与重叠区。
    #[test]
    fn rgb565_alpha_blending() {
        let mut dl = DisplayList::default();
        let r1: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let r2: &[(f32, f32)] = &[(2.0, 0.0), (6.0, 0.0), (6.0, 4.0), (2.0, 4.0)];
        fill_cmd(&mut dl, r1, Paint::Solid(red()), 0.5);
        fill_cmd(&mut dl, r2, Paint::Solid(blue()), 0.5);
        assert_rgb565_matches_rgba(&dl, 6, 4, 1);
        assert_rgb565_matches_rgba(&dl, 6, 4, 4);
    }

    /// 组层（PushLayer/PopLayer + 组 opacity）：离屏合成路径在 565
    /// 模式下同样逐位一致。
    #[test]
    fn rgb565_group_opacity_layer() {
        let mut dl = DisplayList::default();
        dl.cmds.push(Cmd::PushLayer {
            opacity: 0.6,
            blend: BlendMode::Normal,
        });
        let r1: &[(f32, f32)] = &[(0.0, 0.0), (5.0, 0.0), (5.0, 3.0), (0.0, 3.0)];
        fill_cmd(&mut dl, r1, Paint::Solid(red()), 1.0);
        let r2: &[(f32, f32)] = &[(2.0, 0.0), (7.0, 0.0), (7.0, 3.0), (2.0, 3.0)];
        fill_cmd(&mut dl, r2, Paint::Solid(blue()), 0.5);
        dl.cmds.push(Cmd::PopLayer);
        assert_rgb565_matches_rgba(&dl, 7, 3, 1);
        assert_rgb565_matches_rgba(&dl, 7, 3, 4);
    }

    // ============ 车道AV：旋转 pattern 瓦片 SS 推导 =====
    /// SS 推导唯一事实源：轴对齐（含平移/纯缩放）恒 1（旧路径零改动）；
    /// 旋转按位图尺寸守卫 8→4→2 降档；全超限回退 1。
    #[test]
    fn pattern_supersample_selects_by_axis_alignment_and_size() {
        let ident = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
        assert_eq!(pattern_supersample(14.0, 14.0, 1.0, 1.0, &ident), 1);
        // 纯缩放仍轴对齐 → 1
        assert_eq!(pattern_supersample(14.0, 14.0, 2.5, 2.5, &ident), 1);
        // rotate(30)：非对角耦合非零 → 8
        let c = 0.866_025_4_f32;
        let s = 0.5_f32;
        let rot30 = [c, -s, s, c, 0.0, 0.0];
        assert_eq!(pattern_supersample(14.0, 14.0, 1.0, 1.0, &rot30), 8);
        // 400px 旋转瓦片：8 档 3200>2048 → 4 档 1600 ✓
        assert_eq!(pattern_supersample(400.0, 400.0, 1.0, 1.0, &rot30), 4);
        // 1000px：8/4 超限 → 2 档 2000 ✓
        assert_eq!(pattern_supersample(1000.0, 1000.0, 1.0, 1.0, &rot30), 2);
        // 3000px：全超限 → 回退 1（最近邻，行为同旧版）
        assert_eq!(pattern_supersample(3000.0, 3000.0, 1.0, 1.0, &rot30), 1);
        // 非有限 inv 防御 → 1
        let bad = [1.0, f32::NAN, 0.0, 1.0, 0.0, 0.0];
        assert_eq!(pattern_supersample(14.0, 14.0, 1.0, 1.0, &bad), 1);
    }

    // ============ 车道DA/ED：at_boxed 回归网 =====

    /// 车道ED 分数窗参考实现回归：at_boxed（条件加减回卷 + 行基外提 +
    /// 快路）对照测试内嵌的「(n+1)² 分数加权 + rem_euclid 寻址」参考，
    /// 全相位扫描逐位一致（快/慢路、负相位、跨窗界相位全覆盖）。
    #[cfg(feature = "pattern")]
    #[test]
    fn pattern_at_boxed_fractional_matches_reference() {
        // 5×3 位图，逐 texel 唯一色 + 若干部分 alpha（行使预乘平均分支）
        let (bw, bh, ss) = (5u32, 3u32, 2u32);
        let mut bmp = alloc::vec![0u8; (bw * bh * 4) as usize];
        for j in 0..bh {
            for i in 0..bw {
                let o = ((j * bw + i) * 4) as usize;
                bmp[o] = (i * 47 + 13) as u8;
                bmp[o + 1] = (j * 83 + 29) as u8;
                bmp[o + 2] = (i * 31 + j * 57 + 7) as u8;
                bmp[o + 3] = if (i + j) % 3 == 0 { 255 } else { 179 };
            }
        }
        let tile = PatternTile {
            dl: DisplayList::default(),
            tw: 5.0,
            th: 3.0,
            inv: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            sx: 1.0,
            sy: 1.0,
        };
        let ctx = PatCtx {
            tile: &tile,
            bmp: &bmp,
            bw,
            bh,
            ss,
        };
        // 参考实现 = 车道ED 分数窗口径（(n+1)² texel，边界列/行权
        // 1−f/f，SNAP+clamp 相位语义与实现相同；rem_euclid 寻址）
        let ref_at_boxed = |u: f32, v: f32| -> Rgba8 {
            let s = ss as f32;
            let n = ss as i32;
            const SNAP: f32 = 1e-3;
            let fx = u * tile.sx * s;
            let fy = v * tile.sy * s;
            let i0 = libm::floorf(fx - s * 0.5 + SNAP) as i32;
            let j0 = libm::floorf(fy - s * 0.5 + SNAP) as i32;
            let fa = (fx - s * 0.5 - i0 as f32).clamp(0.0, 1.0);
            let fb = (fy - s * 0.5 - j0 as f32).clamp(0.0, 1.0);
            let (mut pr, mut pg, mut pb, mut pa) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for dj in 0..=n {
                let wy = if dj == 0 {
                    1.0 - fb
                } else if dj == n {
                    fb
                } else {
                    1.0
                };
                let jj = (j0 + dj).rem_euclid(bh as i32) as u32;
                for di in 0..=n {
                    let wx = if di == 0 {
                        1.0 - fa
                    } else if di == n {
                        fa
                    } else {
                        1.0
                    };
                    let ii = (i0 + di).rem_euclid(bw as i32) as u32;
                    let o = ((jj * bw + ii) * 4) as usize;
                    let wt = wx * wy;
                    let a = bmp[o + 3] as f32;
                    let w = a / 255.0 * wt;
                    pr += bmp[o] as f32 * w;
                    pg += bmp[o + 1] as f32 * w;
                    pb += bmp[o + 2] as f32 * w;
                    pa += a * wt;
                }
            }
            let cnt = (n * n) as f32;
            let a = pa / cnt;
            if a < 0.5 {
                return Rgba8 {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                };
            }
            let af = a / 255.0;
            Rgba8 {
                r: libm::roundf(pr / cnt / af).clamp(0.0, 255.0) as u8,
                g: libm::roundf(pg / cnt / af).clamp(0.0, 255.0) as u8,
                b: libm::roundf(pb / cnt / af).clamp(0.0, 255.0) as u8,
                a: libm::roundf(a).clamp(0.0, 255.0) as u8,
            }
        };
        // 全相位扫描：u∈[-4, 6)、v∈[-3, 4) 以 0.37 步进——i0 覆盖
        // [-9, 11]（负相位 → 慢路回卷；域内 → 快路；含跨窗界相位）。
        // 容差 ≤1：快路按「整窗和 + 行/列带 + 角点」分解求和，与参考
        // 的逐 texel 加权序不同，f32 结合差在 u8 量化边界上可差 1。
        let mut v = -3.0f32;
        let mut cases = 0u32;
        while v < 4.0 {
            let mut u = -4.0f32;
            while u < 6.0 {
                let got = ctx.at_boxed(u, v);
                let want = ref_at_boxed(u, v);
                assert!(
                    (got.r as i32 - want.r as i32).abs() <= 1
                        && (got.g as i32 - want.g as i32).abs() <= 1
                        && (got.b as i32 - want.b as i32).abs() <= 1
                        && (got.a as i32 - want.a as i32).abs() <= 1,
                    "u={u} v={v}: got={got:?} want={want:?}"
                );
                cases += 1;
                u += 0.37;
            }
            v += 0.37;
        }
        assert!(cases > 400);
    }

    /// 车道ED 修复的行为钉：硬边缘位图上足迹相位连续移动时，采样值
    /// 必须随亚 texel 相位线性分级（旧整数窗吸附会产生 1/ss 量化台阶，
    /// 同一值平台化——da1 探针 ±0.5 texel 锯齿的根源）。
    #[cfg(feature = "pattern")]
    #[test]
    fn pattern_at_boxed_fractional_tracks_subtexel_phase() {
        // 16×8 位图：x<4 透明黑，x≥4 不透明红——texel 4 左缘 = 硬边
        let (bw, bh, ss) = (16u32, 8u32, 8u32);
        let mut bmp = alloc::vec![0u8; (bw * bh * 4) as usize];
        for j in 0..bh {
            for i in 0..bw {
                let o = ((j * bw + i) * 4) as usize;
                if i >= 4 {
                    bmp[o] = 200;
                    bmp[o + 3] = 255;
                }
            }
        }
        let tile = PatternTile {
            dl: DisplayList::default(),
            tw: 16.0,
            th: 8.0,
            inv: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            sx: 1.0,
            sy: 1.0,
        };
        let ctx = PatCtx {
            tile: &tile,
            bmp: &bmp,
            bw,
            bh,
            ss,
        };
        // 足迹中心 fx 从 7.0 走到 9.0（窗 [fx−4, fx+4) 扫过 x=4 的边）：
        // 覆盖率 = (fx+4−4)/8 = fx/8，采样 alpha 应随 fx/8 线性走满
        let mut prev = 0u8;
        let mut steps = 0u32;
        let mut fx = 7.0f32;
        while fx <= 9.0 {
            let px = ctx.at_boxed(fx / ss as f32, 4.0 / ss as f32);
            let want = libm::roundf(fx / 8.0 * 255.0).clamp(0.0, 255.0) as u8;
            assert!(
                (px.a as i32 - want as i32).abs() <= 1,
                "fx={fx}: a={} want≈{want}",
                px.a
            );
            assert!(
                px.a >= prev,
                "量化台阶回归：fx={fx} a={} < prev={prev}",
                px.a
            );
            prev = px.a;
            steps += 1;
            fx += 0.125; // 1/8 texel 步进（旧码 0.125 texel 内恒平台）
        }
        assert!(steps >= 16);
        // 旧整数窗在 0.125-texel 步进下至少 8 步输出不变（平台指纹），
        // 新实现不允许出现
    }

    /// 常色位图不变量：任意相位下窗权和恒 = 窗面积（n²），采样输出
    /// 与相位无关（分数窗分解的自洽性哨兵）。
    #[cfg(feature = "pattern")]
    #[test]
    fn pattern_at_boxed_constant_color_phase_invariant() {
        let (bw, bh, ss) = (9u32, 9u32, 8u32);
        let mut bmp = alloc::vec![180u8; (bw * bh * 4) as usize]; // 全灰
        for o in (0..bmp.len()).step_by(4) {
            bmp[o + 3] = 255; // 不透明
        }
        let tile = PatternTile {
            dl: DisplayList::default(),
            tw: 9.0,
            th: 9.0,
            inv: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            sx: 1.0,
            sy: 1.0,
        };
        let ctx = PatCtx {
            tile: &tile,
            bmp: &bmp,
            bw,
            bh,
            ss,
        };
        let mut u = 0.0f32;
        while u < 9.0 {
            let px = ctx.at_boxed(u, u * 0.5 + 0.25);
            assert_eq!((px.r, px.g, px.b, px.a), (180, 180, 180, 255), "u={u}");
            u += 0.113; // 无理相位步进
        }
    }

    /// turbulence_bitmap 内圈重写（车道DA：f32 梯度表 + 行不变量外提 +
    /// turb_channel 寄存器驻留）与改写前逐像素内圈的逐位一致：双 type ×
    /// stitch 开关 × 非零 origin 小画布全帧对照（测试内嵌旧内圈参考实现）。
    #[cfg(feature = "filters")]
    #[test]
    fn turbulence_bitmap_matches_preopt_inner_loop_reference() {
        let reference = |ty: FeTurbType,
                         bfx: f32,
                         bfy: f32,
                         octaves: u32,
                         seed: i32,
                         stitch: bool,
                         origin: [f32; 2],
                         tile: [u32; 4],
                         w: usize,
                         h: usize,
                         cs: FeColorCtx|
         -> Vec<u8> {
            // —— 以下为车道DA 改写前的原实现（u16 时代的 turb_grad 在
            // 采样点展开；f32 表值 = turb_grad(u16) 逐位一致）——
            let mut out = alloc::vec![0u8; w * h * 4];
            let (sx0, sy0, sx1, sy1) = (
                tile[0] as usize,
                tile[1] as usize,
                tile[2] as usize,
                tile[3] as usize,
            );
            let (mut bfx, mut bfy) = (bfx, bfy);
            let (mut sw, mut sh) = (0.0f32, 0.0f32);
            if stitch {
                let tw = tile[2].saturating_sub(tile[0]) as f32;
                let th = tile[3].saturating_sub(tile[1]) as f32;
                if tw > 0.0 && bfx != 0.0 {
                    let lo = libm::floorf(tw * bfx) / tw;
                    let hi = libm::ceilf(tw * bfx) / tw;
                    bfx = if bfx / lo < hi / bfx { lo } else { hi };
                }
                if th > 0.0 && bfy != 0.0 {
                    let lo = libm::floorf(th * bfy) / th;
                    let hi = libm::ceilf(th * bfy) / th;
                    bfy = if bfy / lo < hi / bfy { lo } else { hi };
                }
                sw = libm::roundf(tw * bfx);
                sh = libm::roundf(th * bfy);
            }
            let tab = turbulence_table(seed);
            for y in sy0..sy1.min(h) {
                for x in sx0..sx1.min(w) {
                    let mut nx = (origin[0] + x as f32 + 1.0) * bfx;
                    let mut ny = (origin[1] + y as f32 + 1.0) * bfy;
                    let mut sum = [0.0f32; 4];
                    let mut ratio = 1.0f32;
                    let mut osx = sw;
                    let mut osy = sh;
                    for _ in 0..octaves {
                        let fx = libm::floorf(nx);
                        let fy = libm::floorf(ny);
                        let frx = nx - fx;
                        let fry = ny - fy;
                        let (mut flx, mut fly) = (fx, fy);
                        let (mut clx, mut cly) = (fx + 1.0, fy + 1.0);
                        if stitch {
                            if flx >= osx {
                                flx -= osx;
                            }
                            if fly >= osy {
                                fly -= osy;
                            }
                            if clx >= osx {
                                clx -= osx;
                            }
                            if cly >= osy {
                                cly -= osy;
                            }
                        }
                        let lx = tab.selector[((flx as i32) & 0xFF) as usize] as f32;
                        let ly = tab.selector[((clx as i32) & 0xFF) as usize] as f32;
                        let b00 = (((lx + fly) as i32) & 0xFF) as usize;
                        let b10 = (((ly + fly) as i32) & 0xFF) as usize;
                        let b01 = (((lx + cly) as i32) & 0xFF) as usize;
                        let b11 = (((ly + cly) as i32) & 0xFF) as usize;
                        let smx = frx * frx * (3.0 - 2.0 * frx);
                        let smy = fry * fry * (3.0 - 2.0 * fry);
                        for (ch, s) in sum.iter_mut().enumerate() {
                            let g = &tab.grad[ch];
                            let dot =
                                |b: usize, vx: f32, vy: f32| -> f32 { g[b][0] * vx + g[b][1] * vy };
                            let u = dot(b00, frx, fry);
                            let v = dot(b10, frx - 1.0, fry);
                            let a = u + smx * (v - u);
                            let u = dot(b01, frx, fry - 1.0);
                            let v = dot(b11, frx - 1.0, fry - 1.0);
                            let b = u + smx * (v - u);
                            let mut c = a + smy * (b - a);
                            if ty == FeTurbType::Turbulence {
                                c = c.abs();
                            }
                            *s += c * ratio;
                        }
                        nx *= 2.0;
                        ny *= 2.0;
                        osx *= 2.0;
                        osy *= 2.0;
                        ratio *= 0.5;
                    }
                    let (vr, vg, vb, va) = if ty == FeTurbType::FractalNoise {
                        (
                            sum[0] * 0.5 + 0.5,
                            sum[1] * 0.5 + 0.5,
                            sum[2] * 0.5 + 0.5,
                            sum[3] * 0.5 + 0.5,
                        )
                    } else {
                        (sum[0], sum[1], sum[2], sum[3])
                    };
                    let o = (y * w + x) * 4;
                    let a = va.clamp(0.0, 1.0);
                    let a_byte = (a * 255.0 + 0.5) as u8;
                    let q = |v: f32| -> u8 {
                        if a_byte == 0 {
                            return 0;
                        }
                        let pm = (v.clamp(0.0, 1.0) * a_byte as f32 + 0.5) as u32;
                        cs.to_byte(pm as f32 / a_byte as f32)
                    };
                    out[o] = q(vr);
                    out[o + 1] = q(vg);
                    out[o + 2] = q(vb);
                    out[o + 3] = a_byte;
                }
            }
            out
        };
        let cs = FeColorCtx { linear: false };
        let (w, h) = (17usize, 11usize);
        type TurbCase = (FeTurbType, f32, f32, u32, i32, bool, [f32; 2]);
        let cases: [TurbCase; 6] = [
            (
                FeTurbType::FractalNoise,
                0.043,
                0.067,
                4,
                7,
                false,
                [0.0, 0.0],
            ),
            (FeTurbType::Turbulence, 0.11, 0.05, 3, -5, false, [0.0, 0.0]),
            (
                FeTurbType::FractalNoise,
                0.05,
                0.05,
                2,
                1,
                true,
                [13.5, -7.25],
            ),
            (
                FeTurbType::Turbulence,
                0.08,
                0.09,
                4,
                2_147_483_000,
                true,
                [-3.0, 4.5],
            ),
            (
                FeTurbType::FractalNoise,
                0.21,
                0.13,
                1,
                0,
                false,
                [100.0, 100.0],
            ),
            (
                FeTurbType::Turbulence,
                0.03,
                0.03,
                6,
                12345,
                true,
                [0.0, 0.0],
            ),
        ];
        for (i, (ty, bfx, bfy, oct, seed, stitch, origin)) in cases.into_iter().enumerate() {
            let got = turbulence_bitmap(
                w,
                h,
                ty,
                (bfx, bfy),
                oct,
                seed,
                stitch,
                origin,
                [0, 0, w as u32, h as u32],
                cs,
            );
            let want = reference(
                ty,
                bfx,
                bfy,
                oct,
                seed,
                stitch,
                origin,
                [0, 0, w as u32, h as u32],
                w,
                h,
                cs,
            );
            assert_eq!(got, want, "case #{i} mismatch");
        }
    }

    /// 车道EF：SIMD 通道臂 `turb_channels_vec` vs scalar `turb_channel`
    /// 逐 lane **位型**等价（to_bits 断言，比 assert_eq 更强——连 −0.0
    /// 与 0.0 的符号位差都判红）。扫描覆盖：lattice 组合（含 0/255 边界
    /// 邻域）× 亚像素分数 × smoothstep/原始 s 值 × ratio 倍减 × acc 起点
    /// × abs_mode 两态。
    #[cfg(all(
        feature = "filters",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    #[test]
    fn turb_vec_channels_bit_exact_vs_scalar() {
        let tab = turbulence_table(977);
        let fracs = [0.0f32, 0.043, 0.17, 0.5, 0.83, 0.99];
        let ratios = [1.0f32, 0.5, 0.1171875];
        let accs = [0.0f32, -1.75, 3.25];
        // Knuth LCG 混合出 lattice 组合（可复现；>256 时 &0xFF 取模回绕）
        let mix = |n: usize| (n.wrapping_mul(2654435761) >> 24) & 0xFF;
        let mut cases = 0u32;
        for ci in 0..64usize {
            let (b00, b10, b01, b11) = (mix(ci), mix(ci + 64), mix(ci + 128), mix(ci + 192));
            for &frx in &fracs {
                for &fry in &fracs {
                    // s 值既取生产 smoothstep 公式，也夹一组原始值
                    //（0/0.5/1 与半格 0.25·0.87）覆盖 lerp 两端与中点。
                    for &(smx, smy) in &[
                        (frx * frx * (3.0 - 2.0 * frx), fry * fry * (3.0 - 2.0 * fry)),
                        (0.0, 0.5),
                        (0.87, 1.0),
                    ] {
                        for &ratio in &ratios {
                            for &acc0 in &accs {
                                for &abs_mode in &[false, true] {
                                    let got = svgine_simd::turb_channels_vec(
                                        &tab.grad_v,
                                        b00,
                                        b10,
                                        b01,
                                        b11,
                                        frx,
                                        fry,
                                        smx,
                                        smy,
                                        svgine_simd::V4::splat(acc0),
                                        ratio,
                                        abs_mode,
                                    )
                                    .to_array();
                                    let mut want = [acc0; 4];
                                    for ch in 0..4 {
                                        want[ch] = turb_channel(
                                            &tab.grad[ch],
                                            b00,
                                            b10,
                                            b01,
                                            b11,
                                            frx,
                                            fry,
                                            smx,
                                            smy,
                                            acc0,
                                            ratio,
                                            abs_mode,
                                        );
                                        assert_eq!(
                                            got[ch].to_bits(),
                                            want[ch].to_bits(),
                                            "lane {ch}: b=({b00},{b10},{b01},{b11}) \
                                             frx={frx} fry={fry} smx={smx} smy={smy} \
                                             ratio={ratio} acc0={acc0} abs={abs_mode}"
                                        );
                                    }
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(cases > 10_000, "扫描量不足：{cases}");
    }

    /// 车道EF：生产 `turbulence_bitmap`（桌面 = SIMD 臂）全帧对照 scalar
    /// 参考逐字节一致。大画布（256×128 = 32768 px 触发 encode memo 表）
    /// × linear 工作域（memo 生效域）× stitch/origin/octaves/种子变化；
    /// 小画布（<32768）补 linear 非 memo 路径。参考内圈 = `turb_channel`
    /// scalar 链 + 直算 to_byte（memo 表值 = 同一 to_byte 算式的结果，
    /// 两者逐位同值）；其与 preopt 原文的逐位一致由 DA 车道参考测试钉死。
    #[cfg(all(
        feature = "filters",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    #[test]
    fn turbulence_bitmap_simd_matches_scalar_reference_fullframe() {
        let reference = |ty: FeTurbType,
                         bfx: f32,
                         bfy: f32,
                         octaves: u32,
                         seed: i32,
                         stitch: bool,
                         origin: [f32; 2],
                         w: usize,
                         h: usize,
                         cs: FeColorCtx|
         -> Vec<u8> {
            let mut out = alloc::vec![0u8; w * h * 4];
            let tab = turbulence_table(seed);
            let (mut bfx, mut bfy) = (bfx, bfy);
            let (mut sw, mut sh) = (0.0f32, 0.0f32);
            if stitch {
                let (tw, th) = (w as f32, h as f32);
                if tw > 0.0 && bfx != 0.0 {
                    let lo = libm::floorf(tw * bfx) / tw;
                    let hi = libm::ceilf(tw * bfx) / tw;
                    bfx = if bfx / lo < hi / bfx { lo } else { hi };
                }
                if th > 0.0 && bfy != 0.0 {
                    let lo = libm::floorf(th * bfy) / th;
                    let hi = libm::ceilf(th * bfy) / th;
                    bfy = if bfy / lo < hi / bfy { lo } else { hi };
                }
                sw = libm::roundf(tw * bfx);
                sh = libm::roundf(th * bfy);
            }
            for y in 0..h {
                for x in 0..w {
                    let mut nx = (origin[0] + x as f32 + 1.0) * bfx;
                    let mut ny = (origin[1] + y as f32 + 1.0) * bfy;
                    let mut sum = [0.0f32; 4];
                    let mut ratio = 1.0f32;
                    let mut osx = sw;
                    let mut osy = sh;
                    for _ in 0..octaves {
                        let fx = libm::floorf(nx);
                        let fy = libm::floorf(ny);
                        let frx = nx - fx;
                        let fry = ny - fy;
                        let (mut flx, mut fly) = (fx, fy);
                        let (mut clx, mut cly) = (fx + 1.0, fy + 1.0);
                        if stitch {
                            if flx >= osx {
                                flx -= osx;
                            }
                            if fly >= osy {
                                fly -= osy;
                            }
                            if clx >= osx {
                                clx -= osx;
                            }
                            if cly >= osy {
                                cly -= osy;
                            }
                        }
                        let lx = tab.selector[((flx as i32) & 0xFF) as usize] as f32;
                        let ly = tab.selector[((clx as i32) & 0xFF) as usize] as f32;
                        let b00 = (((lx + fly) as i32) & 0xFF) as usize;
                        let b10 = (((ly + fly) as i32) & 0xFF) as usize;
                        let b01 = (((lx + cly) as i32) & 0xFF) as usize;
                        let b11 = (((ly + cly) as i32) & 0xFF) as usize;
                        let smx = frx * frx * (3.0 - 2.0 * frx);
                        let smy = fry * fry * (3.0 - 2.0 * fry);
                        for (ch, s) in sum.iter_mut().enumerate() {
                            *s = turb_channel(
                                &tab.grad[ch],
                                b00,
                                b10,
                                b01,
                                b11,
                                frx,
                                fry,
                                smx,
                                smy,
                                *s,
                                ratio,
                                ty == FeTurbType::Turbulence,
                            );
                        }
                        nx *= 2.0;
                        ny *= 2.0;
                        osx *= 2.0;
                        osy *= 2.0;
                        ratio *= 0.5;
                    }
                    let (vr, vg, vb, va) = if ty == FeTurbType::FractalNoise {
                        (
                            sum[0] * 0.5 + 0.5,
                            sum[1] * 0.5 + 0.5,
                            sum[2] * 0.5 + 0.5,
                            sum[3] * 0.5 + 0.5,
                        )
                    } else {
                        (sum[0], sum[1], sum[2], sum[3])
                    };
                    let o = (y * w + x) * 4;
                    let a = va.clamp(0.0, 1.0);
                    let a_byte = (a * 255.0 + 0.5) as u8;
                    let q = |v: f32| -> u8 {
                        if a_byte == 0 {
                            return 0;
                        }
                        let pm = (v.clamp(0.0, 1.0) * a_byte as f32 + 0.5) as u32;
                        cs.to_byte(pm as f32 / a_byte as f32)
                    };
                    out[o] = q(vr);
                    out[o + 1] = q(vg);
                    out[o + 2] = q(vb);
                    out[o + 3] = a_byte;
                }
            }
            out
        };
        // 256×128 = 32768 px = memo 启用阈值的整定边界（含）。
        type FullCase = (
            FeTurbType,
            f32,
            f32,
            u32,
            i32,
            bool,
            [f32; 2],
            usize,
            usize,
            bool,
        );
        let cases: [FullCase; 5] = [
            // memo ON（linear + 阈值边界画布）
            (
                FeTurbType::FractalNoise,
                0.021,
                0.017,
                4,
                7,
                false,
                [0.0, 0.0],
                256,
                128,
                true,
            ),
            // memo ON + stitch + 非零 origin + 负种子
            (
                FeTurbType::Turbulence,
                0.031,
                0.023,
                3,
                -5,
                true,
                [13.5, -7.25],
                256,
                128,
                true,
            ),
            // linear 但低于 memo 阈值（128×128）
            (
                FeTurbType::Turbulence,
                0.11,
                0.13,
                5,
                2147483000,
                false,
                [100.0, 100.0],
                128,
                128,
                true,
            ),
            // sRGB 域 + stitch + 高 octave
            (
                FeTurbType::FractalNoise,
                0.009,
                0.011,
                6,
                12345,
                true,
                [-3.0, 4.5],
                256,
                128,
                false,
            ),
            // 阈值下一像素（memo OFF 边界，200×163 = 32600）
            (
                FeTurbType::Turbulence,
                0.05,
                0.07,
                2,
                42,
                true,
                [0.0, 0.0],
                200,
                163,
                true,
            ),
        ];
        for (i, (ty, bfx, bfy, oct, seed, stitch, origin, w, h, linear)) in
            cases.into_iter().enumerate()
        {
            let cs = FeColorCtx { linear };
            let got = turbulence_bitmap(
                w,
                h,
                ty,
                (bfx, bfy),
                oct,
                seed,
                stitch,
                origin,
                [0, 0, w as u32, h as u32],
                cs,
            );
            let want = reference(ty, bfx, bfy, oct, seed, stitch, origin, w, h, cs);
            assert_eq!(got, want, "fullframe case #{i} mismatch");
        }
    }

    /// 车道EF：`turb_floor`（aarch64 = frintm）vs `libm::floorf` 全域
    /// 位型扫描——±0 符号保持、±inf、quiet-nan（含负号位）、2^23 边界、
    /// 大值域、均匀撒点；sNaN 两家行为不同但 engine 全程由算术产生值
    /// 不可达（见 svgine_simd::floor_exact 文档）。
    #[cfg(all(feature = "filters", target_arch = "aarch64"))]
    #[test]
    fn turb_floor_matches_libm_bit_exact() {
        let mut cases: alloc::vec::Vec<f32> = alloc::vec![
            0.0,
            -0.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            -f32::NAN,
            1.0,
            -1.0,
            0.5,
            -0.5,
            0.9999999,
            1.0000001,
            8388607.5,
            8388608.0,
            8388609.0,
            123.456,
            -123.456,
            1e7,
            -1e7,
            2.1474836e9,
            -2.3e9,
            3.4e38,
            1e-30,
            -1e-30,
        ];
        let mut v = 0.173f32;
        for _ in 0..20_000 {
            cases.push(v);
            cases.push(-v);
            cases.push(v * 1.0e9);
            v = v * 1.0007 + 0.0131;
            if v > 1.0e9 {
                v = 0.173;
            }
        }
        for &x in &cases {
            assert_eq!(
                turb_floor(x).to_bits(),
                libm::floorf(x).to_bits(),
                "x={x:?}"
            );
        }
    }

    /// 半透明单绘 alpha 量化：0.5·255=127.5 → 128（S2 语义，对齐
    /// Chrome 的 alpha u8 往返；截断会令全部半透明像素 alpha -1）。
    #[test]
    fn au_blend_alpha_half_rounds_up() {
        let mut px = [0u8; 4];
        blend_px(&mut px, red(), 0.5);
        assert_eq!(px, [255, 0, 0, 128]);
    }

    /// 不透明绘制存直色精确值（coverage=1 时 mix=sv，无量化损失）。
    #[test]
    fn au_blend_opaque_stores_exact_straight() {
        let mut px = [9u8; 4];
        blend_px(&mut px, red(), 1.0);
        assert_eq!(px, [255, 0, 0, 255]);
    }

    /// host-perf 的满覆盖批处理必须只是执行形态优化：对白底、透明底、
    /// 已绘不透明底、奇偶像素数以及低/高 alpha，逐字节等于逐像素
    /// `blend_px` 权威路径。
    #[cfg(feature = "host-perf")]
    #[test]
    fn blend_solid_full_run_matches_scalar() {
        let colors = [
            Rgba8 {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            Rgba8 {
                r: 17,
                g: 129,
                b: 250,
                a: 255,
            },
            Rgba8 {
                r: 255,
                g: 0,
                b: 255,
                a: 255,
            },
        ];
        let alphas = [1.0 / 255.0, 0.25, 0.5, 254.0 / 255.0, 1.0];
        let seeds: &[&[u8]] = &[
            &[255, 255, 255, 255],
            &[0, 0, 0, 0],
            &[11, 22, 33, 255],
            &[255, 255, 255, 255, 255, 255, 255, 255],
            &[255, 255, 255, 255, 12, 34, 56, 255, 255, 255, 255, 255],
            &[
                255, 255, 255, 255, 12, 34, 56, 255, 255, 255, 255, 255, 9, 8, 7, 128, 255, 255,
                255, 255,
            ],
        ];
        for &c in &colors {
            for &a in &alphas {
                for &seed in seeds {
                    let mut want = seed.to_vec();
                    for px in want.chunks_exact_mut(4) {
                        blend_px(px, c, a);
                    }
                    let mut got = seed.to_vec();
                    blend_solid_full_run(&mut got, c, a);
                    assert_eq!(got, want, "c={c:?} a={a} seed_len={}", seed.len());
                }
            }
        }
    }

    /// 两枚 opacity 0.5 叠加（s2-04 右组语义）：引擎现行直色 u8 模型
    /// 的字节。曾实验色通道 mix 四舍五入（贴 Chrome 预乘公式的进位，
    /// s2-04 重叠区 Chrome=(192,132,160) vs 我方 (191,131,159)），但同
    /// 一改动令 s3-03 描边 AA 带 +869px 回归（Chrome 对 coverage<1 的
    /// 混合走截断类 SIMD 路径，两类无标量规则可同时命中），语料门禁
    /// 否决——本测试把现行截断语义钉死，防后续无意识漂移。归因与
    /// 噪声地板证据见 tests/oracle/reports/opacity-attr.md。
    #[test]
    fn au_elem_overlap_two_half_draws() {
        let mut dl = DisplayList::default();
        let full: &[(f32, f32)] = &[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        fill_cmd(&mut dl, full, Paint::Solid(blue()), 0.5);
        fill_cmd(&mut dl, full, Paint::Solid(red()), 0.5);
        let vp = Viewport {
            width: 2,
            height: 2,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 16];
        render(&dl, &vp, &mut buf);
        for px in buf.chunks(4) {
            assert_eq!(px, [169, 0, 85, 192]);
        }
    }

    /// 组 opacity 单次合成：重叠区 = 层内容整体 ×组 opacity 一次
    /// src-over，不逐层重复 premul——重叠区字节 == 同色单矩形
    /// @组 opacity 直绘（s2-04 左组「重叠区无加深」语义回归锚）。
    #[test]
    fn au_group_opacity_compose_once() {
        // 像素 0=红独占、1=重叠（后绘蓝胜）、2=蓝独占、3=空。
        let mut layered = DisplayList::default();
        layered.cmds.push(Cmd::PushLayer {
            opacity: 0.5,
            blend: BlendMode::Normal,
        });
        let left: &[(f32, f32)] = &[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        fill_cmd(&mut layered, left, Paint::Solid(red()), 1.0);
        let right: &[(f32, f32)] = &[(1.0, 0.0), (3.0, 0.0), (3.0, 2.0), (1.0, 2.0)];
        fill_cmd(&mut layered, right, Paint::Solid(blue()), 1.0);
        layered.cmds.push(Cmd::PopLayer);
        // 基准：与重叠区同色的单矩形直绘 @0.5。
        let mut plain = DisplayList::default();
        fill_cmd(&mut plain, right, Paint::Solid(blue()), 0.5);
        let vp = Viewport {
            width: 4,
            height: 2,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut a = alloc::vec![0u8; 32];
        render(&layered, &vp, &mut a);
        let mut b = alloc::vec![0u8; 32];
        render(&plain, &vp, &mut b);
        // 重叠区（像素 1）：组合成一次到位，与直绘逐位一致。
        assert_eq!(&a[4..8], &b[4..8]);
        // 独占区非零、层外空白（像素 3）不受沾染。
        assert_eq!(&a[0..4], &[255, 0, 0, 128]);
        assert_eq!(&a[12..16], &[0, 0, 0, 0]);
    }

    /// 车道FD（EI 登记）：layer_pool 病态防御上限——深嵌套层（> CAP）
    /// 触顶淘汰最旧后，池长钳在 CAP、带缓存渲染（首帧建池 + 次帧
    /// 复用出池）仍与无缓存 render 逐位一致（池只是缓存，淘汰只损失
    /// 复用、不改语义）。
    #[test]
    fn layer_pool_cap_eviction_semantics_unchanged() {
        let vp = Viewport {
            width: 4,
            height: 2,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        // DEPTH 层嵌套（> CAP=64 必触顶），每层内垫一枚不透明矩形
        // （行交错，令各行都有脏区、入池归零路径全量走到）。
        const DEPTH: usize = 70;
        let mut dl = DisplayList::default();
        for i in 0..DEPTH {
            dl.cmds.push(Cmd::PushLayer {
                opacity: 0.5,
                blend: BlendMode::Normal,
            });
            let y = (i % 2) as f32;
            let ring: &[(f32, f32)] = &[(0.0, y), (4.0, y), (4.0, y + 1.0), (0.0, y + 1.0)];
            let c = if i % 2 == 0 { red() } else { blue() };
            fill_cmd(&mut dl, ring, Paint::Solid(c), 1.0);
        }
        for _ in 0..DEPTH {
            dl.cmds.push(Cmd::PopLayer);
        }
        // 基准：无缓存 render（内部同样走带 CAP 的新池）。
        let mut plain = alloc::vec![0u8; 32];
        render(&dl, &vp, &mut plain);
        assert!(plain.iter().any(|&b| b != 0), "DL 必须真的着墨");
        // 带缓存跨帧复用：帧 1 建池触顶淘汰、帧 2 出池复用再触顶。
        let mut cache = RenderCache::default();
        let mut f1 = alloc::vec![0u8; 32];
        render_with_cache(&dl, &vp, &mut f1, &mut cache);
        let mut f2 = alloc::vec![0u8; 32];
        render_with_cache(&dl, &vp, &mut f2, &mut cache);
        // 触顶证据：DEPTH=70 > CAP，帧末池长必被钳在 CAP（无上限会是 70）。
        assert_eq!(cache.layer_pool.len(), LAYER_POOL_CAP);
        // 语义不变式：带缓存两帧与无缓存基准逐位一致。
        assert_eq!(f1, plain);
        assert_eq!(f2, plain);
    }

    // ============ 车道BL：mix-blend-mode / isolation ============

    /// 混合模式层合成的期望值表（CSS Compositing 规范数学，sRGB 直色域）。
    /// 源 #C08040 × 底 #4080C0——**逐值经 live Chrome 探针矩阵实证 ±1**
    /// （tests/oracle/blend_probe.py + chrome/probes/bl-modes-*.png）。
    #[cfg(feature = "css")]
    fn blend_expected(mode: BlendMode, alpha: f32) -> [u8; 3] {
        let (cs, cb) = (
            [192.0 / 255.0, 128.0 / 255.0, 64.0 / 255.0],
            [64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0],
        );
        let da = 1.0;
        let ao = alpha + da * (1.0 - alpha);
        let b = blend_css(cb, cs, mode);
        let mut out = [0u8; 3];
        for ch in 0..3 {
            let pm = alpha * (1.0 - da) * cs[ch] + alpha * da * b[ch] + (1.0 - alpha) * da * cb[ch];
            out[ch] = (pm / ao * 255.0 + 0.5) as u8;
        }
        out
    }

    /// 单模式层合成往返：backdrop 矩形 + 混合层内源矩形，采样重叠中心。
    /// `src_alpha` 经命令 opacity 进层（αs = coverage × opacity 语义）。
    #[cfg(feature = "css")]
    fn blend_pixel(mode: BlendMode, src_alpha: f32) -> [u8; 3] {
        let mut dl = DisplayList::default();
        let bd: &[(f32, f32)] = &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        fill_cmd(
            &mut dl,
            bd,
            Paint::Solid(Rgba8 {
                r: 64,
                g: 128,
                b: 192,
                a: 255,
            }),
            1.0,
        );
        dl.cmds.push(Cmd::PushLayer {
            opacity: 1.0,
            blend: mode,
        });
        // 源矩形 [1,3)²（重叠区含像素 (1,1)/(2,2)，独占区 (0,*)/(3,*)）
        let src: &[(f32, f32)] = &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
        fill_cmd(
            &mut dl,
            src,
            Paint::Solid(Rgba8 {
                r: 192,
                g: 128,
                b: 64,
                a: 255,
            }),
            src_alpha,
        );
        dl.cmds.push(Cmd::PopLayer);
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf);
        let o = px_off(1, 1); // 像素 (1,1)：重叠区中心
        [buf[o], buf[o + 1], buf[o + 2]]
    }

    /// 16 模式 × {不透明源, α=.5 源} 的色值断言（±1 量化容差——
    /// Chrome 侧 exclusion 有 ±1 的 f16 级噪声，引擎对规范数学应逐位）。
    #[test]
    #[cfg(feature = "css")]
    fn blend_layer_all_modes_match_css_compositing() {
        const MODES: [BlendMode; 16] = [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ];
        for mode in MODES {
            for (alpha, tag) in [(1.0, "opaque"), (0.5, "alpha.5")] {
                let got = blend_pixel(mode, alpha);
                let want = blend_expected(mode, alpha);
                let d = (0..3).fold(0i32, |acc, i| {
                    acc.max((got[i] as i32 - want[i] as i32).abs())
                });
                assert!(d <= 1, "{tag} {mode:?}: got {got:?} want {want:?} (d={d})");
            }
        }
    }

    /// 混合层独占区：αs 覆盖在非混合语义（B 项为 0）下退化为 src-over；
    /// 不透明源独占区 = 源色原样。
    #[test]
    #[cfg(feature = "css")]
    fn blend_layer_exclusive_region_is_src_over() {
        let mut dl = DisplayList::default();
        // 底 [0,3)²（像素列 0-2）、源 [1,4)²（列 1-3）：列 0 纯底、
        // 列 3 源独占、列 1-2 重叠
        let bd: &[(f32, f32)] = &[(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)];
        fill_cmd(
            &mut dl,
            bd,
            Paint::Solid(Rgba8 {
                r: 64,
                g: 128,
                b: 192,
                a: 255,
            }),
            1.0,
        );
        dl.cmds.push(Cmd::PushLayer {
            opacity: 1.0,
            blend: BlendMode::Multiply,
        });
        let src: &[(f32, f32)] = &[(1.0, 0.0), (4.0, 0.0), (4.0, 4.0), (1.0, 4.0)];
        fill_cmd(
            &mut dl,
            src,
            Paint::Solid(Rgba8 {
                r: 192,
                g: 128,
                b: 64,
                a: 255,
            }),
            1.0,
        );
        dl.cmds.push(Cmd::PopLayer);
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf);
        // 像素 (0,0)：层透明 → backdrop 原样（B 不参与，αb=0 守卫）
        assert_eq!(&buf[0..4], &[64, 128, 192, 255]);
        // 像素 (1,1)：multiply (48,64,48)（Chrome 实证值）
        let o = px_off(1, 1);
        assert_eq!(&buf[o..o + 3], &[48, 64, 48]);
        // 像素 (3,1)：源独占（αb=0，B 项系数 0）→ 源色
        let o = px_off(3, 1);
        assert_eq!(&buf[o..o + 3], &[192, 128, 64]);
    }

    /// isolation 截断（blending-002 语义）：混合层 backdrop = **所在层
    /// 缓冲**而非主缓冲——隔离层内的 screen 蓝看不见层外的红。
    #[test]
    #[cfg(feature = "css")]
    fn isolation_layer_truncates_blend_backdrop() {
        // red (1,1)-(3,3) 主缓冲；隔离层：lime (2.1,2.1)-(2.9,2.9)（像素
        // (2,2) 全覆盖、(1,1) 零覆盖——采样点避开边界）+ blue screen 层
        // (1,1)-(3,3)。(1,1)（红∩蓝、lime 外）：隔离生效 → 蓝对隔离层
        // 透明底 = 纯蓝；若穿透主缓冲 → screen(红,蓝) = 品红。
        let mut dl = DisplayList::default();
        let red_r: &[(f32, f32)] = &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
        fill_cmd(&mut dl, red_r, Paint::Solid(red()), 1.0);
        dl.cmds.push(Cmd::PushLayer {
            opacity: 1.0,
            blend: BlendMode::Normal, // isolation 层（Normal，仅截断）
        });
        let lime_r: &[(f32, f32)] = &[(2.1, 2.1), (2.9, 2.1), (2.9, 2.9), (2.1, 2.9)];
        fill_cmd(
            &mut dl,
            lime_r,
            Paint::Solid(Rgba8 {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }),
            1.0,
        );
        dl.cmds.push(Cmd::PushLayer {
            opacity: 1.0,
            blend: BlendMode::Screen,
        });
        let blue_r: &[(f32, f32)] = &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
        fill_cmd(&mut dl, blue_r, Paint::Solid(blue()), 1.0);
        dl.cmds.push(Cmd::PopLayer);
        dl.cmds.push(Cmd::PopLayer);
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf);
        // (1,1)：蓝对隔离层透明底 → 蓝（截断生效；品红 = 穿透信号）
        let o = px_off(1, 1);
        assert_eq!(&buf[o..o + 3], &[0, 0, 255]);
        // (2,2)：蓝对隔离层 lime → screen(lime, blue) = cyan
        let o = px_off(2, 2);
        assert_eq!(&buf[o..o + 3], &[0, 255, 255]);
        // (0,0)：层外空白不受沾染
        assert_eq!(&buf[0..4], &[0, 0, 0, 0]);
    }

    /// 对照组：无隔离层时同一混合层直接对主缓冲 backdrop 混合——
    /// screen(红, 蓝) = 品红（证明上一测试的截断来自隔离层而非巧合）。
    #[test]
    #[cfg(feature = "css")]
    fn blend_layer_without_isolation_sees_backdrop() {
        let mut dl = DisplayList::default();
        let red_r: &[(f32, f32)] = &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
        fill_cmd(&mut dl, red_r, Paint::Solid(red()), 1.0);
        dl.cmds.push(Cmd::PushLayer {
            opacity: 1.0,
            blend: BlendMode::Screen,
        });
        let blue_r: &[(f32, f32)] = &[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)];
        fill_cmd(&mut dl, blue_r, Paint::Solid(blue()), 1.0);
        dl.cmds.push(Cmd::PopLayer);
        let vp = Viewport {
            width: 4,
            height: 4,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = alloc::vec![0u8; 4 * 4 * 4];
        render(&dl, &vp, &mut buf);
        let o = px_off(1, 1);
        assert_eq!(&buf[o..o + 3], &[255, 0, 255]);
    }

    /// 车道NT：行带边桶 ≡ 逐行谓词扫描（逐行内容+顺序全等）。
    /// 随机边集（含水平边、跨全行带边、越界边、行带边界精确值）上
    /// 与暴力扫描逐行比对——分桶是纯加速结构，任何偏差都是字节回归。
    #[test]
    fn row_buckets_match_per_row_scan() {
        let mut edges: Vec<(f32, f32, f32, f32, i32)> = alloc::vec![];
        // 确定性伪随机（LCG）
        let mut s: u32 = 0x1234_5678;
        let mut rnd = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1 << 24) as f32
        };
        for i in 0..600 {
            let y0 = rnd() * 60.0;
            let y1 = y0 + rnd() * 2.0; // 短跨度（分桶的赢面形态）
            let x = rnd() * 30.0 - 5.0;
            let slope = (rnd() - 0.5) * 4.0;
            let dir = if i % 2 == 0 { 1 } else { -1 };
            // push_edge 同款归一化
            if y0 == y1 {
                continue;
            }
            if y0 < y1 {
                edges.push((y0, y1, x, slope, dir));
            } else {
                edges.push((y1, y0, x, slope, -dir));
            }
        }
        // 边界精确值：整行、±0.125/±0.875 邻域
        for &gy in &[2.0f32, 2.125, 2.875, 3.0, 10.5] {
            edges.push((gy, 60.0, 1.0, 0.0, 1));
            edges.push((0.0, gy, 2.0, 0.0, -1));
        }
        for &(hi_d, lo_d) in &[(0.875f32, 0.125f32), (0.5f32, 0.5f32)] {
            let row_lo = 0i32;
            let row_hi = 256i32;
            let mut bk_first = alloc::vec![];
            let mut bk_last = alloc::vec![];
            let mut bk_diff = alloc::vec![];
            let mut bk_off = alloc::vec![];
            let mut bk_cur = alloc::vec![];
            let mut bk_flat = alloc::vec![];
            let ok = build_row_buckets(
                &edges,
                row_lo,
                row_hi,
                hi_d,
                lo_d,
                &mut bk_first,
                &mut bk_last,
                &mut bk_diff,
                &mut bk_off,
                &mut bk_cur,
                &mut bk_flat,
            );
            // 效益门可合法拒绝（回退旧扫描同语义）；本数据形态应放行
            assert!(ok, "expected bucketing to pass the benefit gate");
            let active = |y0: f32, y1: f32, y: i32| -> bool {
                y0 <= y as f32 + hi_d && y1 > y as f32 + lo_d
            };
            for y in row_lo..row_hi {
                let r = (y - row_lo) as usize;
                let bucket: Vec<u32> = bk_flat[bk_off[r] as usize..bk_off[r + 1] as usize].to_vec();
                let brute: Vec<u32> = edges
                    .iter()
                    .enumerate()
                    .filter(|(_, &(y0, y1, _, _, _))| active(y0, y1, y))
                    .map(|(ei, _)| ei as u32)
                    .collect();
                assert_eq!(
                    bucket, brute,
                    "row {y} hi_d={hi_d} lo_d={lo_d}: bucket != brute scan"
                );
            }
        }
    }

    // ---- 车道NO（blur-perf）：盒模糊重排/魔数除法/逆 EOTF 阈值表的
    // 字节级同源校验。参考实现 = 旧算式原样（u32 逐像素除法 + 列主序
    // 逐列滑窗），新实现逐字节必须全等。

    /// 确定性伪随机填充（LCG，覆盖 0..=65535 全值域）。
    fn no_fill_u16(buf: &mut [u16], seed: u32) {
        let mut s = seed | 1;
        for v in buf.iter_mut() {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *v = (s >> 11) as u16;
        }
    }

    /// 旧 box_blur_axis 算式参考（列主序逐列滑窗 + u32 逐像素除法）。
    fn no_box_blur_axis_legacy(
        src: &[u16],
        dst: &mut [u16],
        w: usize,
        h: usize,
        start: i32,
        width: u32,
        horizontal: bool,
    ) {
        let win = width;
        let len = if horizontal { w } else { h };
        let lines = if horizontal { h } else { w };
        if len == 0 || win == 0 {
            return;
        }
        let win_lo = start;
        let win_hi = start + win as i32;
        for line in 0..lines {
            let base = if horizontal { line * w * 4 } else { line * 4 };
            let stride = if horizontal { 4 } else { w * 4 };
            let mut sums = [0u32; 4];
            let lo = win_lo.max(0);
            let hi = win_hi.min(len as i32);
            for k in lo..hi.max(lo) {
                for ch in 0..4 {
                    sums[ch] += src[base + k as usize * stride + ch] as u32;
                }
            }
            for x in 0..len {
                for ch in 0..4 {
                    dst[base + x * stride + ch] =
                        ((sums[ch] * 2 + win) / (2 * win)).min(u16::MAX as u32) as u16;
                }
                let add = x as i32 + win_hi;
                if add >= 0 && add < len as i32 {
                    for ch in 0..4 {
                        sums[ch] += src[base + add as usize * stride + ch] as u32;
                    }
                }
                let sub = x as i32 + win_lo;
                if sub >= 0 && sub < len as i32 {
                    for ch in 0..4 {
                        sums[ch] =
                            sums[ch].saturating_sub(src[base + sub as usize * stride + ch] as u32);
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "filters")]
    fn box_blur_axis_byte_equal_legacy_4ch() {
        let sizes = [(1usize, 7usize), (5, 4), (33, 17), (64, 64)];
        let wins = [1u32, 2, 3, 4, 15, 16, 17, 255, 256, 4096, 4097];
        for &(w, h) in &sizes {
            for &win in &wins {
                for &start in &[
                    -(win as i32),
                    -(win as i32 / 2),
                    -1,
                    0,
                    1,
                    7,
                    -(win as i32 + 3),
                ] {
                    let mut src = alloc::vec![0u16; w * h * 4];
                    no_fill_u16(&mut src, (w * 31 + h * 7 + win as usize) as u32);
                    let mut a = alloc::vec![0u16; w * h * 4];
                    let mut b = alloc::vec![0u16; w * h * 4];
                    no_box_blur_axis_legacy(&src, &mut a, w, h, start, win, true);
                    box_blur_axis(&src, &mut b, w, h, start, win, true);
                    assert_eq!(a, b, "horizontal w={w} h={h} win={win} start={start}");
                    no_box_blur_axis_legacy(&src, &mut a, w, h, start, win, false);
                    box_blur_axis(&src, &mut b, w, h, start, win, false);
                    assert_eq!(a, b, "vertical w={w} h={h} win={win} start={start}");
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "filters")]
    fn box_blur_axis_1ch_byte_equal_legacy() {
        let sizes = [(1usize, 9usize), (7, 5), (41, 23)];
        let wins = [1u32, 2, 3, 8, 63, 64, 65, 4096, 4097];
        for &(w, h) in &sizes {
            for &win in &wins {
                for &start in &[-(win as i32), -3, 0, 2, win as i32] {
                    let mut src = alloc::vec![0u16; w * h];
                    no_fill_u16(&mut src, (w * 13 + h * 5 + win as usize) as u32);
                    let mut a = alloc::vec![0u16; w * h];
                    let mut b = alloc::vec![0u16; w * h];
                    // 1ch 旧式参考（就地展开）
                    let run = |src: &[u16], dst: &mut [u16], horiz: bool| {
                        let len = if horiz { w } else { h };
                        let lines = if horiz { h } else { w };
                        let win_lo = start;
                        let win_hi = start + win as i32;
                        for line in 0..lines {
                            let base = if horiz { line * w } else { line };
                            let stride = if horiz { 1 } else { w };
                            let mut sum = 0u32;
                            let lo = win_lo.max(0);
                            let hi = win_hi.min(len as i32);
                            for k in lo..hi.max(lo) {
                                sum += src[base + k as usize * stride] as u32;
                            }
                            for x in 0..len {
                                dst[base + x * stride] =
                                    ((sum * 2 + win) / (2 * win)).min(u16::MAX as u32) as u16;
                                let add = x as i32 + win_hi;
                                if add >= 0 && add < len as i32 {
                                    sum += src[base + add as usize * stride] as u32;
                                }
                                let sub = x as i32 + win_lo;
                                if sub >= 0 && sub < len as i32 {
                                    sum = sum
                                        .saturating_sub(src[base + sub as usize * stride] as u32);
                                }
                            }
                        }
                    };
                    run(&src, &mut a, true);
                    box_blur_axis_1ch(&src, &mut b, w, h, start, win, true);
                    assert_eq!(a, b, "1ch horizontal w={w} h={h} win={win} start={start}");
                    run(&src, &mut a, false);
                    box_blur_axis_1ch(&src, &mut b, w, h, start, win, false);
                    assert_eq!(a, b, "1ch vertical w={w} h={h} win={win} start={start}");
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "filters")]
    fn box_blur_axis_byte_equal_vite_dims() {
        let wins = [3u32, 5];
        let starts = [-3i32, -2, -1, 0, 1];
        for win in wins {
            for &start in &starts {
                for w in [267usize, 268, 271, 201, 303, 265, 266, 251] {
                    for h in [99usize, 173, 157, 140] {
                        let mut src = alloc::vec![0u16; w * h * 4];
                        no_fill_u16(&mut src, (w * 7 + h * 3 + win as usize) as u32);
                        let mut a = alloc::vec![0u16; w * h * 4];
                        let mut b = alloc::vec![0u16; w * h * 4];
                        no_box_blur_axis_legacy(&src, &mut a, w, h, start, win, true);
                        box_blur_axis(&src, &mut b, w, h, start, win, true);
                        if a != b {
                            panic!("H w={w} h={h} win={win} start={start}");
                        }
                        no_box_blur_axis_legacy(&src, &mut a, w, h, start, win, false);
                        box_blur_axis(&src, &mut b, w, h, start, win, false);
                        if a != b {
                            panic!("V w={w} h={h} win={win} start={start}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "filters")]
    fn boxdiv_round_matches_legacy_expr() {
        // win 全域抽样（含 2 的幂边界、sigma_box_passes 上限 4097）；
        // s 覆盖 0、小值、65535·win 邻域（累加器值域上界）。
        let mut wins: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
        for win in 1..=97 {
            wins.push(win);
        }
        for &k in &[
            1u32, 2, 4, 8, 16, 32, 64, 128, 255, 256, 510, 1023, 4096, 4097,
        ] {
            wins.push(k);
        }
        let mut seed = 12_345u32;
        for win in wins {
            let div = BoxDiv::new(win);
            let smax = 65535u64 * win as u64 + win as u64 / 2;
            let check = |s: u64| {
                if s > u32::MAX as u64 {
                    return;
                }
                let s = s as u32;
                let legacy = (s * 2 + win) / (2 * win);
                assert_eq!(
                    div.round(s).min(u16::MAX as u32),
                    legacy.min(u16::MAX as u32),
                    "win={win} s={s}"
                );
            };
            check(0);
            for s in 1..=2000u64 {
                check(s);
            }
            for j in 0..200u64 {
                check(smax.saturating_sub(j));
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                check((seed as u64) % (smax + 1));
            }
        }
    }

    #[test]
    #[cfg(feature = "filters")]
    fn lin2srgb_lut_byte_equal_powf() {
        let t = lin2srgb_thresholds();
        // ① 分层可达域：l = 255p/(257·a16)，a16 抽样 × p 全域
        for &a16 in &[1u32, 2, 3, 127, 128, 255, 1000, 8191, 32768, 65025] {
            for p in 0..=65535u32 {
                let l = p as f32 * 255.0 / (a16 as f32 * 257.0);
                assert_eq!(
                    lin2srgb_lut_byte(&t, l),
                    lin2srgb_u8(l),
                    "a16={a16} p={p} l={l}"
                );
            }
        }
        // ② 每个阈值 ±4ULP 边界带（判别最敏感区）× a16 抽样
        for b in 1..=255usize {
            let tb = t[b];
            if !tb.is_finite() {
                continue;
            }
            for d in -4i32..=4 {
                let bits = (tb.to_bits() as i64 + d as i64).max(0) as u32;
                let l = f32::from_bits(bits);
                assert_eq!(lin2srgb_lut_byte(&t, l), lin2srgb_u8(l), "b={b} ulp={d}");
            }
        }
        // ③ 20 万确定性伪随机 (p, a16) 对
        let mut s = 9_876_543u32;
        for _ in 0..200_000 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let p = (s >> 17) % 65536;
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let a16 = (s >> 17) % 65025 + 1;
            let l = p as f32 * 255.0 / (a16 as f32 * 257.0);
            assert_eq!(lin2srgb_lut_byte(&t, l), lin2srgb_u8(l));
        }
        // ④ 退化输入：负值 / NaN / ≥1
        for l in [-1.0f32, -1e-9, 0.0, 1.0, 2.0, f32::INFINITY, f32::NAN] {
            assert_eq!(lin2srgb_lut_byte(&t, l), lin2srgb_u8(l), "l={l}");
        }
    }

    // ============ 车道OL（perf-round3）：两刀字节契约单测 ============

    /// 刀①（ensure_chain 矩形限域）渲染 harness：同矩形以
    /// 「纯 ClipDef（bounds，path=None）」/「PathClip（单轴对齐矩形
    /// 环）」两种形态挂链（path_form 选择），大形状满铺 + 小形状
    /// 居内（frac 选择整数网格/分数边缘），aa 档位渲染返回全帧。
    #[cfg(feature = "clip-path")]
    fn ol_render_clipped(
        rect: [f32; 4],
        path_form: bool,
        frac: bool,
        aa: u8,
    ) -> alloc::vec::Vec<u8> {
        let mut dl = DisplayList::default();
        let clip = if path_form {
            let ring = [
                (rect[0], rect[1]),
                (rect[2], rect[1]),
                (rect[2], rect[3]),
                (rect[0], rect[3]),
                (rect[0], rect[1]),
            ];
            let d = dl.push_rings(&[&ring]);
            dl.push_clip_def(ClipDef {
                bounds: rect,
                path: Some(PathClip {
                    d,
                    rule: FillRule::NonZero,
                }),
                parent: NO_CLIP,
            })
        } else {
            dl.push_clip(rect)
        };
        let big: &[(f32, f32)] = &[(0.0, 0.0), (16.0, 0.0), (16.0, 12.0), (0.0, 12.0)];
        let d1 = dl.push_rings(&[big]);
        dl.cmds.push(Cmd::Fill {
            d: d1,
            paint: Paint::Solid(red()),
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let small: &[(f32, f32)] = if frac {
            &[(2.4, 2.6), (5.8, 2.6), (5.8, 6.2), (2.4, 6.2)]
        } else {
            &[(2.0, 2.0), (5.0, 2.0), (5.0, 5.0), (2.0, 5.0)]
        };
        let d2 = dl.push_rings(&[small]);
        dl.cmds.push(Cmd::Fill {
            d: d2,
            paint: Paint::Solid(blue()),
            opacity: 0.5,
            rule: FillRule::NonZero,
            clip,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        let vp = Viewport {
            width: 12,
            height: 10,
            format: PixFmt::Rgba8,
            aa,
        };
        let mut out = alloc::vec![0u8; 12 * 10 * 4];
        render(&dl, &vp, &mut out);
        out
    }

    /// 刀①契约（aa=1，刀①a）：PathClip 矩形 ≡ rect 语义（逐字节
    /// 一致）——两侧同为像素中心采样谓词。覆盖整数网格 / 分数边缘 /
    /// 裁切大于视口 / 裁切在视口外（全透明）/ 零面积 / 右缘 1/4 网格
    /// 非整数点六种形态。
    /// 注意：aa>1 时 PathClip 与 push_clip **本来就不字节等价**（span
    /// 钳制先按 bounds 吸附剪裁 shape 覆盖、再乘 cm 的既有双计语义，
    /// 见 fill_path legacy span 钳制注释）——跨形态断言在 aa>1 是伪
    /// 契约，刀①b 的真契约由下方内区谓词暴力等价测试承载。
    #[test]
    #[cfg(feature = "clip-path")]
    fn ol_clip_rect_pathclip_equals_rect_semantics() {
        let cases: [([f32; 4], bool); 6] = [
            ([1.0, 1.0, 9.0, 7.0], false),
            ([1.3, 0.6, 8.7, 5.4], true),
            ([0.0, 0.0, 20.0, 20.0], false),
            ([14.0, 2.0, 18.0, 6.0], false),
            ([3.0, 3.0, 3.0, 6.0], false),
            // 右缘恰落 1/4 网格非整数点（aa=2 吸附后 H'=8.75）：x1 取
            // ceil 会把 ov<1 的 px=8 列误判进内区（首跑 3 文件 DIFF 根因）
            ([1.25, 0.5, 8.75, 6.5], true),
        ];
        for (rect, frac) in cases {
            let a = ol_render_clipped(rect, false, frac, 1);
            let b = ol_render_clipped(rect, true, frac, 1);
            assert_eq!(
                a, b,
                "rect={rect:?} frac={frac}: aa=1 PathClip 矩形必须与 rect 语义逐字节一致"
            );
        }
    }

    /// 刀①契约（aa>1，刀①b）：内区谓词暴力等价——`cm == 255` 当且
    /// 仅当像素 ∈ `rect_interior_of(bounds)`。对 aa∈{2,3,4} × 九种
    /// 矩形（整数/离网分数/1/4 网格非整数点/溢出视口/零面积/细条）
    /// 逐像素双向断言。这是内区直写恢复快路径依赖的唯一不变量；
    /// x1 的 floor 边界（非 ceil）由此测试钉死。
    #[test]
    #[cfg(feature = "clip-path")]
    fn ol_rect_interior_predicate_matches_coverage() {
        let rects: [[f32; 4]; 9] = [
            [1.0, 1.0, 9.0, 7.0],       // 整数网格
            [1.3, 0.6, 8.7, 5.4],       // 离网分数（aa=2 吸附后四边非整）
            [1.25, 0.5, 8.75, 6.5],     // 1/4 网格非整数右/下缘
            [0.0, 0.0, 20.0, 20.0],     // 溢出视口
            [14.0, 2.0, 18.0, 6.0],     // 视口外（右溢）
            [3.0, 3.0, 3.0, 6.0],       // 零面积
            [2.4, 2.9, 2.9, 5.0],       // 细条：吸附后无内区列
            [0.35, 0.35, 11.65, 9.65],  // 近满幅离网
            [4.125, 1.875, 7.875, 4.5], // 1/8 网格（aa=2 吸附到 1/4）
        ];
        for aa in [2u8, 3, 4] {
            for rect in rects {
                let mut dl = DisplayList::default();
                let ring = [
                    (rect[0], rect[1]),
                    (rect[2], rect[1]),
                    (rect[2], rect[3]),
                    (rect[0], rect[3]),
                    (rect[0], rect[1]),
                ];
                let d = dl.push_rings(&[&ring]);
                let pc = PathClip {
                    d,
                    rule: FillRule::NonZero,
                };
                let vp = Viewport {
                    width: 12,
                    height: 10,
                    format: PixFmt::Rgba8,
                    aa,
                };
                let cm = clip_coverage(&dl, &pc, &vp);
                let ri = rect_interior_of(rect, &vp);
                for y in 0..10usize {
                    for px in 0..12usize {
                        let in_ri = px >= ri.x0 as usize
                            && px < ri.x1 as usize
                            && y >= ri.y0 as usize
                            && y < ri.y1 as usize;
                        let cm_full = cm[y * 12 + px] == 255;
                        assert_eq!(
                            in_ri, cm_full,
                            "aa={aa} rect={rect:?} px=({px},{y}): 内区谓词必须 ⟺ cm==255"
                        );
                    }
                }
            }
        }
    }

    /// 刀①检测器：矩形环（含旋转后仍正交的形态）命中；圆环、斜边
    /// 四边形、双环、零宽退化环一律拒绝。
    #[test]
    #[cfg(feature = "clip-path")]
    fn ol_clip_detector_rect_hit_and_miss() {
        let mut dl = DisplayList::default();
        let mk = |dl: &mut DisplayList, pts: &[(f32, f32)]| PathClip {
            d: dl.push_rings(&[pts]),
            rule: FillRule::NonZero,
        };
        // 正例 1：poly_points 烘焙口径（4 角 + 闭合补点）
        let p1 = mk(
            &mut dl,
            &[(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0), (0.0, 0.0)],
        );
        assert!(clip_is_single_rect(&dl, &p1));
        // 正例 2：90° 旋转语境（设备空间仍轴对齐，遍历方向纵向起步）
        let p2 = mk(
            &mut dl,
            &[(1.0, 2.0), (1.0, 7.0), (5.0, 7.0), (5.0, 2.0), (1.0, 2.0)],
        );
        assert!(clip_is_single_rect(&dl, &p2));
        // 负例 1：圆（path::circle 产物）
        let circ = crate::path::circle(4.0, 4.0, 3.0, crate::geom::Mat2x3::IDENTITY);
        let pc_circle = PathClip {
            d: dl.push_rings(
                &circ
                    .subs
                    .iter()
                    .map(|s| s.pts.as_slice())
                    .collect::<alloc::vec::Vec<_>>(),
            ),
            rule: FillRule::NonZero,
        };
        assert!(!clip_is_single_rect(&dl, &pc_circle));
        // 负例 2：斜边四边形
        let p3 = mk(
            &mut dl,
            &[(0.0, 0.0), (6.0, 0.0), (8.0, 5.0), (2.0, 5.0), (0.0, 0.0)],
        );
        assert!(!clip_is_single_rect(&dl, &p3));
        // 负例 3：双环（NaN 分隔）
        let two: alloc::vec::Vec<&[(f32, f32)]> = vec![
            &[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0), (0.0, 0.0)],
            &[(4.0, 0.0), (6.0, 0.0), (6.0, 2.0), (4.0, 2.0), (4.0, 0.0)],
        ];
        let pc_two = PathClip {
            d: dl.push_rings(&two),
            rule: FillRule::NonZero,
        };
        assert!(!clip_is_single_rect(&dl, &pc_two));
        // 负例 4：零宽矩形退化（首边零长）
        let p4 = mk(
            &mut dl,
            &[(3.0, 1.0), (3.0, 1.0), (3.0, 5.0), (3.0, 5.0), (3.0, 1.0)],
        );
        assert!(!clip_is_single_rect(&dl, &p4));
    }

    /// 刀②契约：对全部 a16 ∈ [1, 65535] × 每枚 5 个结构位 + 3 个确定
    /// 性伪随机 p，unpremul_to_direct 的 sRGB 分支输出必须与原式
    /// `((510p+a16)/(2a16)).min(255)` 逐位一致（单像素缓冲直测生产
    /// 函数；三通道取不同 p 覆盖通道位）。
    #[test]
    #[cfg(feature = "filters")]
    fn ol_unpremul_magic_matches_direct_division() {
        let div255 = BoxDiv::new(255);
        let cs = FeColorCtx { linear: false };
        let mut s = 0x5EED_2211u32;
        for a16 in 1u32..=65535 {
            let mut p_set = [0u32; 8];
            p_set[1] = 1;
            p_set[2] = a16 / 2;
            p_set[3] = a16.saturating_sub(1);
            p_set[4] = a16.min(65535);
            p_set[5] = 65535; // 越出预乘不变式的 p（公式仍须同值）
            for pk in p_set.iter_mut().skip(6) {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *pk = (s >> 16) % 65536;
            }
            for p in p_set {
                let pc = [p as u16, (p ^ 0xA5) as u16, (p >> 3) as u16, a16 as u16];
                let out = unpremul_to_direct(&pc, 1, cs, &div255, None);
                let expect = |v: u32| (((v * 510 + a16) / (2 * a16)).min(255)) as u8;
                assert_eq!(
                    (out[0], out[1], out[2]),
                    (expect(p), expect(p ^ 0xA5), expect(p >> 3)),
                    "a16={a16} p={p}"
                );
                // alpha 通道同函数产出，与原式互证
                assert_eq!(out[3], (div255.round(a16).min(255)) as u8);
            }
        }
    }

    // -----------------------------------------------------------------------
    // 车道M3：跨帧缓存字节等值红线（CrossFrameCache vs 逐帧 render）。
    // 全部走真实 builder 管线（parse_document + build_display_list_at）
    // ——池烘焙口径与宿主一致，比手写 DL 更能抓指纹漏判。
    // -----------------------------------------------------------------------

    /// 解析 SVG 并建 t 时刻 DL（测试专用薄封装）。
    fn cf_dl_at(svg: &str, t_ms: f64, vw: f32, vh: f32) -> DisplayList {
        let doc = crate::builder::parse_document(svg).expect("测试 SVG 必然可解析");
        crate::builder::build_display_list_at(&doc, vw, vh, t_ms)
    }

    fn cf_vp(w: u32, h: u32) -> Viewport {
        Viewport {
            width: w,
            height: h,
            format: PixFmt::Rgba8,
            aa: 2,
        }
    }

    /// 全特性探针：pattern 平铺 + 矢量裁切链 + 渐变 + 亮度蒙版 + 滤镜,
    /// 再叠 SMIL 几何动画（矩形 x 动画——池不变、命令几何逐帧变，命中
    /// 形态的主力）。
    const CF_SVG_ANIM_GEOM: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="36">
      <defs>
        <linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
        <pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="yellow"/></pattern>
        <clipPath id="c"><rect x="4" y="4" width="20" height="20"/><circle cx="10" cy="10" r="6"/></clipPath>
        <mask id="m"><rect x="8" y="8" width="30" height="24" fill="white"/></mask>
        <filter id="f" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="1.5"/></filter>
      </defs>
      <rect x="0" y="0" width="48" height="36" fill="url(#p)"/>
      <g clip-path="url(#c)"><rect x="0" y="0" width="30" height="30" fill="url(#g)"/></g>
      <rect x="2" y="2" width="20" height="20" fill="green" mask="url(#m)"/>
      <rect x="10" y="10" width="26" height="20" fill="red" filter="url(#f)"/>
      <rect x="6" y="6" width="30" height="8" fill="#FF00FF">
        <animate attributeName="x" values="2;18" dur="1s" fill="freeze"/>
      </rect>
    </svg>"##;

    /// 动画池探针：mask 内容 rect 的 x 被 SMIL 驱动——mask 子 DL 逐帧
    /// 变（masks 池指纹必失效），覆盖「动画池不得假命中」的失效面
    /// （mask 子列表经 anim-aware walk 烘焙，池逐帧真变）。
    const CF_SVG_ANIM_CLIP: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="36">
      <defs>
        <mask id="m"><rect x="0" y="0" width="24" height="36" fill="white">
          <animate attributeName="x" values="0;20" dur="1s" fill="freeze"/>
        </rect></mask>
      </defs>
      <rect x="0" y="0" width="48" height="36" fill="#00AAFF" mask="url(#m)"/>
    </svg>"##;

    #[test]
    fn crossframe_equals_render_across_frames() {
        let (w, h) = (48, 36);
        let vp = cf_vp(w, h);
        let mut cf = CrossFrameCache::new();
        let mut plain = alloc::vec![0u8; w as usize * h as usize * 4];
        let mut cf_buf = alloc::vec![0u8; w as usize * h as usize * 4];
        for (i, t) in [0.0f64, 100.0, 250.0, 500.0, 750.0, 999.0]
            .iter()
            .enumerate()
        {
            let dl = cf_dl_at(CF_SVG_ANIM_GEOM, *t, w as f32, h as f32);
            render(&dl, &vp, &mut plain);
            cf.render(&dl, &vp, &mut cf_buf);
            assert_eq!(
                cf_buf, plain,
                "帧 {i} (t={t}): 跨帧缓存输出 != 无缓存 render"
            );
        }
        let st = cf.stats();
        assert_eq!((st.misses, st.hits), (1, 5), "首帧 miss、其余全命中");
    }

    #[test]
    fn crossframe_bg_equals_render_bg_across_frames() {
        let (w, h) = (48, 36);
        let vp = cf_vp(w, h);
        let bg = BG_WHITE;
        let mut cf = CrossFrameCache::new();
        let mut plain = alloc::vec![0u8; w as usize * h as usize * 4];
        let mut cf_buf = alloc::vec![0u8; w as usize * h as usize * 4];
        for t in [0.0f64, 400.0, 900.0] {
            let dl = cf_dl_at(CF_SVG_ANIM_GEOM, t, w as f32, h as f32);
            render_bg(&dl, &vp, &mut plain, bg);
            cf.render_bg(&dl, &vp, &mut cf_buf, bg);
            assert_eq!(cf_buf, plain, "t={t}: 白底直出跨帧缓存 != render_bg");
        }
        assert_eq!(cf.stats().hits, 2);
    }

    #[test]
    fn crossframe_animated_pool_misses_every_frame_still_equal() {
        let (w, h) = (48, 36);
        let vp = cf_vp(w, h);
        let mut cf = CrossFrameCache::new();
        let mut plain = alloc::vec![0u8; w as usize * h as usize * 4];
        let mut cf_buf = alloc::vec![0u8; w as usize * h as usize * 4];
        for t in [0.0f64, 300.0, 600.0, 999.0] {
            let dl = cf_dl_at(CF_SVG_ANIM_CLIP, t, w as f32, h as f32);
            render(&dl, &vp, &mut plain);
            cf.render(&dl, &vp, &mut cf_buf);
            assert_eq!(cf_buf, plain, "t={t}: 动画裁切池帧输出漂移");
        }
        let st = cf.stats();
        assert_eq!(st.misses, 4, "动画池逐帧失效（不得假命中）");
        assert_eq!(st.hits, 0);
    }

    #[test]
    fn crossframe_viewport_change_invalidates() {
        let svg = CF_SVG_ANIM_GEOM;
        let mut cf = CrossFrameCache::new();
        let dl48 = cf_dl_at(svg, 0.0, 48.0, 36.0);
        let mut buf48 = alloc::vec![0u8; 48 * 36 * 4];
        cf.render(&dl48, &cf_vp(48, 36), &mut buf48);
        // 同视口第二帧命中
        let dl48b = cf_dl_at(svg, 500.0, 48.0, 36.0);
        cf.render(&dl48b, &cf_vp(48, 36), &mut buf48);
        assert_eq!(cf.stats().hits, 1);
        // 换视口：必须失效且输出与该视口 render 逐字节一致
        let dl32 = cf_dl_at(svg, 500.0, 32.0, 32.0);
        let mut plain = alloc::vec![0u8; 32 * 32 * 4];
        let mut buf32 = alloc::vec![0u8; 32 * 32 * 4];
        render(&dl32, &cf_vp(32, 32), &mut plain);
        cf.render(&dl32, &cf_vp(32, 32), &mut buf32);
        assert_eq!(buf32, plain);
        assert_eq!(cf.stats().misses, 2, "视口变化必须失效");
    }

    #[test]
    fn crossframe_invalidate_forces_full_recompute() {
        let (w, h) = (48, 36);
        let vp = cf_vp(w, h);
        let mut cf = CrossFrameCache::new();
        let mut buf = alloc::vec![0u8; w as usize * h as usize * 4];
        let dl = cf_dl_at(CF_SVG_ANIM_GEOM, 0.0, w as f32, h as f32);
        cf.render(&dl, &vp, &mut buf);
        cf.render(&dl, &vp, &mut buf);
        assert_eq!(cf.stats().hits, 1);
        cf.invalidate();
        // 统计是会话累计口径（与 RetainedStats 同哲学）：invalidate 只
        // 丢驻留不清账。
        let mut plain = alloc::vec![0u8; w as usize * h as usize * 4];
        render(&dl, &vp, &mut plain);
        cf.render(&dl, &vp, &mut buf);
        assert_eq!(buf, plain);
        assert_eq!(cf.stats().misses, 2, "invalidate 后首帧全量重算");
        assert_eq!(cf.stats().hits, 1);
    }

    #[test]
    fn crossframe_is_doc_agnostic_by_fingerprint() {
        // 跨文档复用同一 cache（指纹含五池全量——防错引上一文档条目）。
        let (w, h) = (48, 36);
        let vp = cf_vp(w, h);
        let mut cf = CrossFrameCache::new();
        let mut buf = alloc::vec![0u8; w as usize * h as usize * 4];
        let mut plain = alloc::vec![0u8; w as usize * h as usize * 4];
        let a = cf_dl_at(CF_SVG_ANIM_GEOM, 0.0, w as f32, h as f32);
        cf.render(&a, &vp, &mut buf);
        let b_svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="36">
          <defs><mask id="m"><rect x="0" y="0" width="40" height="30" fill="white"/></mask></defs>
          <rect x="0" y="0" width="48" height="36" fill="teal" mask="url(#m)"/>
        </svg>"##;
        let b = cf_dl_at(b_svg, 0.0, w as f32, h as f32);
        render(&b, &vp, &mut plain);
        cf.render(&b, &vp, &mut buf);
        assert_eq!(buf, plain, "跨文档复用不得污染输出");
        assert_eq!(cf.stats().misses, 2);
    }

    #[test]
    fn scan_intersection_sort_matches_reference_order_and_membership() {
        let cases: alloc::vec::Vec<alloc::vec::Vec<(f32, i32)>> = alloc::vec![
            alloc::vec![],
            alloc::vec![(2.0, 1)],
            alloc::vec![(2.0, 1), (-1.0, -1), (2.0, 7), (0.0, 3)],
            (0..32)
                .map(|i| (((i * 17) % 23) as f32 - 11.0, i))
                .collect(),
            (0..33)
                .map(|i| (((i * 19) % 29) as f32 - 14.0, i))
                .collect(),
            (0..257)
                .map(|i| (((i * 7919) % 101) as f32 - 50.0, i))
                .collect(),
            (0..96)
                .map(|i| (if i % 2 == 0 { -0.0 } else { 0.0 }, i))
                .collect(),
            alloc::vec![(2.0, 1), (f32::NAN, 2), (-1.0, 3)],
        ];
        for mut got in cases {
            let original = got.clone();
            let mut want = got.clone();
            want.sort_by(cmp_scan_x);
            sort_scan_x(&mut got);
            assert_eq!(got.len(), want.len());
            for pair in got.windows(2) {
                assert_ne!(cmp_scan_x(&pair[0], &pair[1]), core::cmp::Ordering::Greater);
            }
            let mut got_members = got.clone();
            let mut want_members = original;
            let bits_then_dir = |a: &(f32, i32), b: &(f32, i32)| {
                a.0.to_bits().cmp(&b.0.to_bits()).then(a.1.cmp(&b.1))
            };
            got_members.sort_unstable_by(bits_then_dir);
            want_members.sort_unstable_by(bits_then_dir);
            for (a, b) in got_members.iter().zip(&want_members) {
                assert_eq!(a.0.to_bits(), b.0.to_bits());
                assert_eq!(a.1, b.1);
            }
            for (a, b) in got.iter().zip(&want) {
                assert_eq!(a.0.to_bits(), b.0.to_bits());
                assert_eq!(a.1, b.1, "快路必须保持稳定次序");
            }
        }
    }

    // ── ADR-0027：coverage 差分累计字节等价硬门 ─────────────────────

    // 等值三测串行锁：coverage_reuse_fallbacks 是进程级计数器，病态
    // 样本测试的 NaN 回退会并发污染 10k 存活性哨兵的读数窗口。
    static COV_TEST_LOCK: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);

    fn cov_tests_lock() {
        while COV_TEST_LOCK
            .compare_exchange(
                false,
                true,
                core::sync::atomic::Ordering::Acquire,
                core::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
        {}
    }

    fn cov_tests_unlock() {
        COV_TEST_LOCK.store(false, core::sync::atomic::Ordering::Release);
    }

    /// 同一 pts/参数在 coverage_opt 0/1/2 三臂下渲染，逐字节对照。
    /// 字节等价与开关值无关（证明见 cov_apply_diff 文档注释）。
    #[allow(clippy::too_many_arguments)]
    fn covopt_arms_byte_equal(
        pts: &[f32],
        w: u32,
        h: u32,
        aa: u8,
        rule: FillRule,
        paint: &Paint,
        opacity: f32,
        clip: Option<ClipCtx>,
        mask: Option<&[u8]>,
    ) {
        let vp = Viewport {
            width: w,
            height: h,
            format: PixFmt::Rgba8,
            aa,
        };
        let mut base: Option<alloc::vec::Vec<u8>> = None;
        for mode in [0u8, 1, 2] {
            set_coverage_opt(mode);
            let mut buf = alloc::vec![0u8; (w as usize) * (h as usize) * 4];
            let mut sc = Scratch::default();
            fill_path(
                pts, &mut buf, &vp, paint, opacity, rule, clip, mask, None, None, 0, h as usize,
                &mut sc,
            );
            match base {
                None => base = Some(buf),
                Some(ref b) => {
                    assert!(buf == *b, "covopt={mode} 与权威臂（0）逐字节不一致");
                }
            }
        }
        set_coverage_opt(0); // 恢复产品缺省（权威路径，ADR-0027 裁决）
    }

    /// 引理直证：随机 span 集（多子行、可重叠）上，差分横扫与
    /// 「逐 span 顺序饱和加」终值逐字节一致（min(Σ,255) 顺序无关性）。
    #[cfg(all(test, feature = "host-perf"))]
    #[test]
    fn cov_apply_diff_matches_sequential_saturation() {
        let mut seed: u64 = 0x5356_4949_4e45_a11c;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let w = 96usize;
        for _case in 0..2000usize {
            let mut auth = [0u8; 96];
            let mut diff: alloc::vec::Vec<(i32, i32)> = alloc::vec::Vec::new();
            for _sub in 0..4usize {
                let full: i32 = if _sub < 3 { 64 } else { 63 };
                let spans = (rng() % 5) as usize;
                for _s in 0..spans {
                    let a = (rng() as usize) % (w + 1);
                    let b = (rng() as usize) % (w + 1);
                    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                    if lo == hi {
                        continue;
                    }
                    for v in &mut auth[lo..hi] {
                        *v = v.saturating_add(full as u8);
                    }
                    diff.push((lo as i32, full));
                    diff.push((hi as i32, -full));
                }
            }
            let mut cand = [0u8; 96];
            cov_apply_diff(&mut cand[..], &mut diff[..]);
            assert_eq!(cand, auth, "差分横扫与顺序饱和加终值不一致");
        }
    }

    /// 固定病态样本：NaN/Inf 端点、巨坐标（as i32 饱和）、零面积、
    /// 亚像素薄片、完全重合环（F>255 饱和）、水平/垂直边、画布外。
    #[test]
    fn coverage_diff_equiv_pathological_fixtures() {
        cov_tests_lock();
        let ring_dup: alloc::vec::Vec<f32> = {
            let mut v: alloc::vec::Vec<f32> =
                alloc::vec![8.0, 8.0, 88.0, 9.0, 80.0, 64.0, 12.0, 60.0];
            v.push(f32::NAN);
            v.push(f32::NAN);
            v.extend_from_slice(&[8.0, 8.0, 88.0, 9.0, 80.0, 64.0, 12.0, 60.0]);
            v
        };
        let cases: &[&[f32]] = &[
            &[0.0, 0.0, 90.0, 0.0, 90.0, 70.0, 0.0, 70.0], // 轴对齐矩形（宽 span）
            &[f32::NAN, f32::NAN, 0.0, 0.0, 90.0, 0.0, 90.0, 70.0], // 前置 NaN 分隔
            &[
                f32::INFINITY,
                0.0,
                90.0,
                f32::INFINITY,
                90.0,
                70.0,
                f32::NEG_INFINITY,
                35.0,
            ], // Inf 端点
            &[f32::NAN, 0.0, 90.0, f32::NAN, 90.0, 70.0, 0.0, 70.0], // 单坐标 NaN（全环退化）
            &[1e30, 1e30, -1e30, 1e30, 0.0, 0.0, 5.0, 70.0], // 巨坐标（i32 饱和）
            &[5.0, 5.0, 5.1, 5.0],                         // 两点（零面积）
            &[5.0, 5.0, 5.05, 5.0, 5.05, 70.0, 5.0, 70.0], // 0.05px 薄片
            &[50.0, 50.0, 51.0, 50.2, 50.8, 51.0, 50.0, 50.9], // 单像素级
            &[20.0, 30.0, 20.0, 40.0, 20.0, 50.0, 30.0, 30.0], // 全垂直/水平边
            &[f32::NAN, f32::NAN],                         // 空
            &[],                                           // 空
            &ring_dup[..],                                 // 完全重合双环（饱和）
        ];
        let mask: alloc::vec::Vec<u8> = (0..96 * 72).map(|i| ((i * 37) % 256) as u8).collect();
        for &pts in cases {
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                for aa in [2u8, 3] {
                    for a in [255u8, 160] {
                        let paint = Paint::Solid(Rgba8 {
                            r: 40,
                            g: 120,
                            b: 200,
                            a,
                        });
                        covopt_arms_byte_equal(pts, 96, 72, aa, rule, &paint, 1.0, None, None);
                        covopt_arms_byte_equal(pts, 96, 72, aa, rule, &paint, 0.6, None, None);
                        // 矩形裁切 / 矢量裁切 / 亮度蒙版（逐像素乘法消费点）
                        covopt_arms_byte_equal(
                            pts,
                            96,
                            72,
                            aa,
                            rule,
                            &paint,
                            1.0,
                            Some(ClipCtx {
                                rect: [10.0, 5.0, 85.0, 68.0],
                                cov: None,
                                rect_interior: None,
                            }),
                            None,
                        );
                        covopt_arms_byte_equal(
                            pts,
                            96,
                            72,
                            aa,
                            rule,
                            &paint,
                            1.0,
                            Some(ClipCtx {
                                rect: [0.0, 0.0, 96.0, 72.0],
                                cov: Some(&mask),
                                rect_interior: None,
                            }),
                            None,
                        );
                        covopt_arms_byte_equal(
                            pts,
                            96,
                            72,
                            aa,
                            rule,
                            &paint,
                            1.0,
                            None,
                            Some(&mask),
                        );
                    }
                }
            }
        }
        cov_tests_unlock();
    }

    /// 10,000 组确定性 LCG 等值：混合环（交叉/洞/重合边/同 x 列/退化顶点/
    /// 画布外坐标）× 规则 × aa 档 × 纯色不透明/半透明 × 裁切三态 × 蒙版，
    /// 三臂逐字节对照。ADR-0027 硬门。
    #[test]
    fn coverage_diff_equiv_deterministic_10k() {
        let mut seed: u64 = 0x5356_4949_4e45_2026;
        let mut rng = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // 阶段二存活性哨兵：串行窗内复位计数器，跑完读差——证明复用
        // 路径真实执行（若逐行全回退，等值测试会因「退化为权威路径」
        // 平凡通过，测不出复用本体的字节等价）
        cov_tests_lock();
        reset_coverage_reuse_fallbacks();
        let mask: alloc::vec::Vec<u8> = (0..96 * 72).map(|i| ((i * 149) % 256) as u8).collect();
        for it in 0..10_000usize {
            let pick = rng();
            // 画布三档：极小（行带分桶回退域）/ 中 / 常规（分桶命中域）
            let (w, h) = match it % 5 {
                0 => (12u32, 9u32),
                1 => (48, 40),
                _ => (96, 72),
            };
            let n_rings = 1 + (pick % 3) as usize;
            let mut pts: alloc::vec::Vec<f32> = alloc::vec::Vec::new();
            for r in 0..n_rings {
                let n = 3 + ((rng() >> 8) % 12) as usize;
                let cx = ((rng() >> 16) % (w as u64 * 4)) as f32 / 4.0;
                let cy = ((rng() >> 24) % (h as u64 * 4)) as f32 / 4.0;
                let rad = 2.0 + ((rng() >> 32) % 110) as f32 / 4.0;
                let ring_start = pts.len();
                for k in 0..n {
                    let ang = (k as f32) * 0.7 + ((rng() >> 40) % 64) as f32 / 32.0;
                    let jit = ((rng() >> 48) % 16) as f32 / 8.0;
                    let mut x = cx + libm::cosf(ang) * (rad + jit);
                    let y = cy + libm::sinf(ang) * (rad + jit) * 0.8;
                    if k % 5 == 0 {
                        x = libm::roundf(x); // 整数列 → 同 x 垂直边
                    }
                    if pick % 17 == 0 && k == 1 {
                        // 偶发重复顶点（退化水平边丢弃路径）
                        let (px, py) = (pts[pts.len() - 2], pts[pts.len() - 1]);
                        pts.push(px);
                        pts.push(py);
                    }
                    // 偶发画布外坐标（x 剔除/钳制路径）
                    if pick % 23 == 0 && k == 2 {
                        x -= (w as f32) * 0.6;
                    }
                    pts.push(x);
                    pts.push(y);
                }
                if pick % 13 == 0 {
                    // 完全重合环（重合边 + F 超饱和）
                    let dup = pts[ring_start..].to_vec();
                    pts.extend_from_slice(&dup);
                }
                pts.push(f32::NAN); // 环分隔 → 下一环（洞/交叠形态）
                pts.push(f32::NAN);
                let _ = r;
            }
            let rule = if (pick >> 60) & 1 == 1 {
                FillRule::EvenOdd
            } else {
                FillRule::NonZero
            };
            let aa: u8 = if (pick >> 61) & 1 == 1 { 3 } else { 2 };
            let a: u8 = if (pick >> 62) & 1 == 1 { 160 } else { 255 };
            let paint = Paint::Solid(Rgba8 {
                r: (pick & 0xff) as u8,
                g: ((pick >> 4) & 0xff) as u8,
                b: ((pick >> 12) & 0xff) as u8,
                a,
            });
            let opacity = if (pick >> 63) & 1 == 1 { 0.7 } else { 1.0 };
            // 裁切/蒙版组合按位轮换（None / 矩形 / 矢量 / 蒙版）
            let cm_full = (w * h) as usize;
            match it % 4 {
                0 => covopt_arms_byte_equal(&pts, w, h, aa, rule, &paint, opacity, None, None),
                1 => covopt_arms_byte_equal(
                    &pts,
                    w,
                    h,
                    aa,
                    rule,
                    &paint,
                    opacity,
                    Some(ClipCtx {
                        rect: [2.0, 1.0, (w - 2) as f32, (h - 1) as f32],
                        cov: None,
                        rect_interior: None,
                    }),
                    None,
                ),
                2 => covopt_arms_byte_equal(
                    &pts,
                    w,
                    h,
                    aa,
                    rule,
                    &paint,
                    opacity,
                    Some(ClipCtx {
                        rect: [0.0, 0.0, w as f32, h as f32],
                        cov: Some(&mask[..cm_full]),
                        rect_interior: None,
                    }),
                    None,
                ),
                _ => covopt_arms_byte_equal(
                    &pts,
                    w,
                    h,
                    aa,
                    rule,
                    &paint,
                    opacity,
                    None,
                    Some(&mask[..cm_full]),
                ),
            }
        }
        // 存活性哨兵：随机环坐标全有限 → NaN 回退应为 0；预算回退
        // （成员翻搅超 2n+16）在 3-14 边小环上不可能触发。先解锁再断言
        // （panic 不持锁）。
        let fb_delta = coverage_reuse_fallbacks();
        cov_tests_unlock();
        assert!(
            fb_delta < 100,
            "复用路径疑似逐行回退（fallbacks Δ={fb_delta}），等值测试退化为平凡通过"
        );
    }
}
