//! retained —— 保留式渲染计划（车道RL）：帧级缓存 + 脏带增量渲染。
//!
//! 动机（用户架构分析第一优先级）：动画播放时 ~95% 像素与上一帧相同，
//! 但 [`crate::raster::render`] 每帧全量重算。本模块在 DL 之上驻留
//! 「上一帧 DL + 着墨区间 + 预计算缓存」（帧像素驻留调用方缓冲），
//! 按帧差分只重画脏行带：
//!
//! - **字节等值红线**：`render` 的输出序列与逐帧调用
//!   [`crate::raster::render`] 逐字节一致。依据两条已证定理：
//!   ①行带定理（车道hwB，`render_band` + band_sweep 3645/3645）——
//!   任意行窗的 band 渲染 == 整帧渲染的对应行；②确定性（ADR-0003
//!   纯函数渲染）——DL 逐字节相同 ⇒ 光栅产物逐字节相同。脏行推导的
//!   保守性由 [`crate::raster::band_index_spans`] 的零贡献定理保证
//!   （区间与行窗无交 ⟺ 命令对窗内像素零贡献）。
//! - **瓦片账本**：`RETAINED_TILE` 网格只做脏记账/统计（瓦片是 2D 的，
//!   而字节等值的渲染单元是**全宽行带**——渐变档位按视口宽选档、
//!   bayer 抖动相位按绝对 (x,y)，列切分不在等值定理覆盖面）。
//! - **池携带**：clip/mask/pattern/filter/image 五池定义不变时，
//!   [`crate::raster::precompute_frame_move_pools`] 携带上一帧缓存条目
//!   （同参纯函数 ⇒ 携带 == 重算），mask/滤镜类文档的增量帧免重算。
//! - **保守降级**：任何不可判情形（池变化 / 命令数漂移 / NaN 泄漏）
//!   一律全帧重画——最坏情形退化为无缓存的 [`render`]，绝不错报
//!   「未变」。
//!
//! 增量写契约：[`RetainedPlan::render`] 只写 `out` 的脏行带，其余行
//! 保持调用方缓冲原样（必须等于上一帧内容——宿主复用同一缓冲且不
//! 中途改写；改写后先 [`RetainedPlan::invalidate`]）。全命中（帧与上
//! 帧逐字节相同）时零写入零重算。
//!
//! 本模块 `no_std + alloc` 纯新增；未链接时对既有渲染路径零影响
//! （gc-sections 剥离，T1 体积预算不变）。

use alloc::vec;
use alloc::vec::Vec;

use crate::dl::{DisplayList, Rgba8};
use crate::raster::{
    band_index_spans, precompute_frame, precompute_frame_move_pools, render_band, FrameCache,
    RenderCache, Viewport, BG_TRANSPARENT,
};

/// 瓦片边长（设备像素）。只作脏记账/统计粒度（渲染单元是全宽行带，
/// 见模块注释「瓦片账本」）。
pub const RETAINED_TILE: u32 = 128;

/// 脏行带合并间隙（行）：间隔 ≤ 此值的脏行并进同一条带，压每带固定
/// 成本（命令流重放的 per-band 开销）与带宽度的平衡。L4e8 像素差分
/// 通道同款量级（4）；语义差分脏区更紧凑，取 8。
const BAND_GAP: usize = 8;

/// 脏行推导的外扩护栏（行）。span 行域与 band 跳过判式同式精确，+1 行
/// 是对「索引口径与光栅口径未来漂移」的冗余护栏（与
/// `BAND_IDX_PAD` 同哲学：宁可少省不可错漏）。
const ROW_PAD: f32 = 1.0;

/// 保留式会话的累计/当帧统计（`svgine_retained_stats` 镜像源）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetainedStats {
    /// 瓦片网格总数（`cols × rows`，构造期定）。
    pub tiles_total: u32,
    /// 最近一次 [`RetainedPlan::render`] 的脏瓦片数（全命中 = 0）。
    pub tiles_dirty: u32,
    /// 累计全命中次数（帧与上帧逐字节相同、零重算零写入）。
    pub cache_hits: u32,
}

/// 帧差分产物（任务契约命名）：哪些命令变了、是否整帧未变。
#[derive(Clone, Debug)]
pub struct FrameDiff {
    /// 变化命令的下标（按升序）。池变化等不可判情形 = 全体下标
    /// （保守全帧）。
    pub changed_cmd_indices: Vec<usize>,
    /// true = 本帧与上帧**逐字节相同**（全命中：零重算零写入）。
    pub all_static: bool,
    /// true = 五池任一定义变化（clip/mask/pattern/filter/image）——
    /// 变化的影响面不经命令几何表达（如动画 clipPath），保守全帧。
    pub pools_changed: bool,
}

/// 上一帧的驻留产物：DL（差分基准）+ 命令着墨区间（脏行推导基准）
/// + 预计算缓存（池携带源）。
struct PrevFrame {
    dl: DisplayList,
    spans: Vec<[f32; 4]>,
    cache: FrameCache,
}

/// 保留式渲染计划：持有画布光栅 + 上一帧 DL，`render` 逐帧差分增量。
///
/// 生命周期：`new` → 逐帧 [`Self::render`]（同一调用方缓冲）→ 可选
/// [`Self::invalidate`]。线程契约：`&mut self` 独占写（单线程入口，
/// 引擎核心零线程纪律不变）。
pub struct RetainedPlan {
    w: u32,
    h: u32,
    cols: u32,
    rows: u32,
    prev: Option<PrevFrame>,
    /// 前前帧的顶层 DisplayList allocation。当前帧构建前由宿主取走，
    /// render 完成后再把淘汰的上一帧 DL 放回，形成双缓冲容量轮换。
    recycle: Option<DisplayList>,
    tiles: Vec<bool>,
    stats: RetainedStats,
}

impl RetainedPlan {
    /// 建计划（`w×h` 设备像素，RGBA8）。尺寸溢出返回 `None`。
    /// 引擎不持有帧缓冲——帧内容驻留在调用方缓冲（增量写契约），
    /// 双重存储省一次整帧拷贝与 2.4MB 级驻留。
    pub fn new(w: u32, h: u32) -> Option<Self> {
        let _ = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
        let cols = w.div_ceil(RETAINED_TILE);
        let rows = h.div_ceil(RETAINED_TILE);
        Some(Self {
            w,
            h,
            cols,
            rows,
            prev: None,
            recycle: None,
            tiles: vec![false; (cols as usize) * (rows as usize)],
            stats: RetainedStats {
                tiles_total: cols * rows,
                tiles_dirty: 0,
                cache_hits: 0,
            },
        })
    }

    /// 视口尺寸（设备像素）。
    pub fn dimensions(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// 统计快照。
    pub fn stats(&self) -> RetainedStats {
        self.stats
    }

    /// 是否已驻留上一帧（全命中判定的前提）。
    pub fn has_prev(&self) -> bool {
        self.prev.is_some()
    }

    /// 取出可供下一帧 builder 清空复用的 DisplayList。首帧/失效后可能
    /// 返回默认空表；调用方应把返回值交给容量复用 builder，再把产物
    /// 传回 [`Self::render_with_bands_bg`]，render 会自动回收淘汰帧。
    pub fn take_recycled_display_list(&mut self) -> DisplayList {
        self.recycle.take().unwrap_or_default()
    }

    /// 记账一次「宿主侧全命中」：调用方（FFI 快路径）在跳过 DL 重建
    /// 直接判按时调用——与 [`Self::render`] 全命中路径同口径
    /// （`cache_hits += 1`、`tiles_dirty = 0`）。
    pub fn note_full_hit(&mut self) {
        self.stats.cache_hits += 1;
        self.stats.tiles_dirty = 0;
        self.tiles.fill(false);
    }

    /// 作废全部缓存：下一次 [`Self::render`] 全帧重画并**全帧写** `out`
    /// （宿主缓冲被改写/更换后的恢复通道）。
    pub fn invalidate(&mut self) {
        if let Some(prev) = self.prev.take() {
            self.recycle = Some(prev.dl);
        }
    }

    /// 渲染一帧：与上一帧 DL 差分 → 只重画脏行带（全命中零写入）。
    /// `vp` 必须与构造尺寸一致且 `Rgba8`（违约防御 = 全帧重画）。
    /// 返回本帧写出的行数（全命中 0；调用方可据此做上传调度）。
    ///
    /// 增量写契约（模块注释）：`out` 在两次调用间必须保持上帧内容；
    /// 违约先 [`Self::invalidate`]。
    pub fn render(&mut self, dl: DisplayList, vp: &Viewport, out: &mut [u8]) -> usize {
        let mut bands = Vec::new();
        self.render_with_bands_bg(dl, vp, out, BG_TRANSPARENT, &mut bands)
    }

    /// 渲染一帧并导出本次实际写入的全宽行带。
    ///
    /// `bands_out` 会先清空；全命中时为空，首帧/保守全帧时为
    /// `[(0, h)]`。导出的区间与内部写入区间完全相同，宿主可以据此
    /// 只同步对应的纹理行，避免再次做像素 diff。
    pub fn render_with_bands(
        &mut self,
        dl: DisplayList,
        vp: &Viewport,
        out: &mut [u8],
        bands_out: &mut Vec<(usize, usize)>,
    ) -> usize {
        self.render_with_bands_bg(dl, vp, out, BG_TRANSPARENT, bands_out)
    }

    /// `render_with_bands` 的指定主缓冲底色版本。
    ///
    /// Android 混合路径的 base 是白底直出，而 iOS/FFI 默认是透明底；
    /// retained 的差分与底色无关，只有实际行带光栅化需要消费该参数。
    pub fn render_with_bands_bg(
        &mut self,
        dl: DisplayList,
        vp: &Viewport,
        out: &mut [u8],
        bg: Rgba8,
        bands_out: &mut Vec<(usize, usize)>,
    ) -> usize {
        bands_out.clear();
        // 防御：视口违约（宿主改尺寸/格式）→ 重置计划按新视口全帧。
        if vp.width != self.w || vp.height != self.h || vp.format != crate::raster::PixFmt::Rgba8 {
            let (w, h) = (vp.width, vp.height);
            if w == 0 || h == 0 {
                return 0;
            }
            *self = match Self::new(w, h) {
                Some(mut p) => {
                    p.stats.cache_hits = self.stats.cache_hits;
                    p
                }
                None => return 0,
            };
        }
        let n = self.w as usize * self.h as usize * 4;
        debug_assert!(out.len() >= n);
        let mut prev = match self.prev.take() {
            Some(p) => p,
            None => return self.full_render(dl, vp, out, bg, bands_out),
        };
        let diff = diff_display_list(&prev.dl, &dl);
        if diff.all_static {
            self.recycle = Some(dl);
            self.prev = Some(prev);
            self.stats.cache_hits += 1;
            self.stats.tiles_dirty = 0;
            self.tiles.fill(false);
            return 0;
        }
        let new_spans = band_index_spans(&dl);
        // 脏行带：池变化 = 全帧保守；否则按变化命令的新旧 span 并集。
        let bands = if diff.pools_changed {
            vec![(0usize, self.h as usize)]
        } else {
            merge_dirty_bands(
                &diff.changed_cmd_indices,
                &prev.spans,
                &new_spans,
                self.h as usize,
            )
        };
        bands_out.extend_from_slice(&bands);
        // 帧缓存：池不变 → 携带上帧条目（mask/滤镜免重算）；池变 →
        // 全新预计算。
        let cache = if diff.pools_changed {
            precompute_frame(&dl, vp)
        } else {
            precompute_frame_move_pools(&dl, vp, &mut prev.cache)
        };
        let wb = self.w as usize * 4;
        let mut rows_written = 0usize;
        for &(y0, y1) in &bands {
            // 直接写调用方缓冲的行带（无引擎侧规范画布二次拷贝）。
            let band = &mut out[y0 * wb..y1 * wb];
            let mut rc = RenderCache::default();
            render_band(&dl, vp, band, y0, y1, bg, &cache, &mut rc);
            rows_written += y1 - y0;
        }
        self.mark_tiles(
            &diff.changed_cmd_indices,
            &prev.spans,
            &new_spans,
            diff.pools_changed,
        );
        self.recycle = Some(prev.dl);
        self.prev = Some(PrevFrame {
            dl,
            spans: new_spans,
            cache,
        });
        rows_written
    }

    /// 全帧渲染（首帧 / invalidate 后 / 视口违约重置）：整帧 band 渲染
    /// （全画布单带 == [`crate::raster::render`] 逐字节，band 定理）
    /// 直写 `out`。
    fn full_render(
        &mut self,
        dl: DisplayList,
        vp: &Viewport,
        out: &mut [u8],
        bg: Rgba8,
        bands_out: &mut Vec<(usize, usize)>,
    ) -> usize {
        let cache = precompute_frame(&dl, vp);
        let h = self.h as usize;
        let mut rc = RenderCache::default();
        render_band(&dl, vp, out, 0, h, bg, &cache, &mut rc);
        bands_out.push((0, h));
        let spans = band_index_spans(&dl);
        self.mark_tiles_all();
        self.prev = Some(PrevFrame { dl, spans, cache });
        h
    }

    fn mark_tiles_all(&mut self) {
        self.tiles.fill(true);
        self.stats.tiles_dirty = self.stats.tiles_total;
    }

    /// 瓦片账本：变化命令的新旧 span（并集）→ 命中瓦片标脏。
    /// 池变化/哨兵 span = 全部标脏。统计口径（`tiles_dirty`）。
    fn mark_tiles(
        &mut self,
        changed: &[usize],
        prev_spans: &[[f32; 4]],
        new_spans: &[[f32; 4]],
        pools_changed: bool,
    ) {
        if pools_changed {
            self.mark_tiles_all();
            return;
        }
        self.tiles.fill(false);
        let (tw, th) = (RETAINED_TILE as f32, RETAINED_TILE as f32);
        let gw = self.cols as f32;
        let gh = self.rows as f32;
        let full_w = self.w as f32;
        let full_h = self.h as f32;
        for &i in changed {
            for sp in [prev_spans.get(i), new_spans.get(i)].into_iter().flatten() {
                // 退化/哨兵 span（非有限）→ 全格标脏。
                let (x0, y0, x1, y1) = (sp[0], sp[1], sp[2], sp[3]);
                if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
                    self.mark_tiles_all();
                    return;
                }
                // 行/列 → 瓦片格区间（clamp 到画布，空 span 零标记；
                // no_std 纪律：floor 走 libm）。
                let fl = |v: f32| libm::floorf(v);
                let ry0 = fl(y0.max(0.0).min(full_h) / th).min(gh - 1.0);
                let ry1 = fl(y1.max(0.0).min(full_h) / th).min(gh - 1.0);
                let cx0 = fl(x0.max(0.0).min(full_w) / tw).min(gw - 1.0);
                let cx1 = fl(x1.max(0.0).min(full_w) / tw).min(gw - 1.0);
                if ry1 < ry0 || cx1 < cx0 {
                    continue; // 空区间（EMPTY span 或越界回缩）
                }
                for gy in (ry0 as u32)..=(ry1 as u32) {
                    for gx in (cx0 as u32)..=(cx1 as u32) {
                        let idx = gy as usize * self.cols as usize + gx as usize;
                        self.tiles[idx] = true;
                    }
                }
            }
        }
        self.stats.tiles_dirty = self.tiles.iter().filter(|t| **t).count() as u32;
    }
}

/// f32 逐位等值（NaN 安全；`f32::eq` 对 NaN 恒 false 不可用于含哨兵
/// NaN 的点池比较）。
#[inline]
fn bits_eq(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

/// 文档时不变判定：无任何时变内容（SMIL/CSS 动画绑定 + keyframes）
/// ⇒ 任意 `t` 的 `build_display_list_at` 产物逐字节相同。宿主（FFI
/// retained begin 期）据此对静态文档走「任意 t 全命中」快路径——
/// anim 特性关闭的构建恒时不变（能力位降级语义，动画元素解析期
/// 跳过）。
pub fn doc_is_time_invariant(doc: &crate::builder::Document) -> bool {
    #[cfg(feature = "anim")]
    {
        doc.anims.is_empty() && doc.css_anims.is_empty() && doc.keyframes.is_empty()
    }
    #[cfg(not(feature = "anim"))]
    {
        let _ = doc;
        true
    }
}

/// 点池全池逐位比较。
fn points_bits_eq(pa: &[f32], pb: &[f32]) -> bool {
    pa.len() == pb.len() && pa.iter().zip(pb.iter()).all(|(a, b)| bits_eq(*a, *b))
}

/// 命令对的几何内容等值：`Cmd` 的 derive 等值只覆盖 `PathRef` **索引**，
/// 被指点的内容须回查点池（同下标切片 bits 比较）。调用方保证 `a == b`
/// 已成立（索引同 ⇒ 切片同位）。`points_same` = 全池已证等值（快路径
/// 免逐命令回查）。
fn geometry_content_eq(a: &crate::dl::Cmd, b: &crate::dl::Cmd, pa: &[f32], pb: &[f32]) -> bool {
    use crate::dl::{Cmd, PathRef};
    fn slice<'a>(p: &'a [f32], d: &PathRef) -> &'a [f32] {
        let s = d.first as usize;
        let e = s + d.count as usize * 2;
        if e > p.len() {
            return &[];
        }
        &p[s..e]
    }
    match (a, b) {
        (Cmd::Fill { d: da, .. }, Cmd::Fill { d: db, .. }) => {
            debug_assert_eq!(da, db);
            points_bits_eq(slice(pa, da), slice(pb, db))
        }
        _ => true, // Image/PushLayer/PopLayer/Filter 无点池引用
    }
}

/// 五池深比较（车道RL）：任一池定义变化即保守全帧（影响面不经命令
/// 几何表达）。子 DL 递归经 `DisplayList` 的手写 `PartialEq`（NaN 安全）。
fn pools_eq(a: &DisplayList, b: &DisplayList) -> bool {
    a.clips == b.clips
        && a.masks == b.masks
        && a.patterns == b.patterns
        && a.filters == b.filters
        && a.images == b.images
}

/// 帧差分（Phase 1 脏标记系统）：比较相邻两帧 DL，产出
/// [`FrameDiff`]。结构漂移（命令/池条数变化）直接保守全帧；否则逐
/// 命令 `PartialEq` + 被指点内容回查。
pub fn diff_display_list(prev: &DisplayList, next: &DisplayList) -> FrameDiff {
    let n = next.cmds.len();
    if prev.cmds.len() != n
        || prev.points.len() != next.points.len()
        || prev.clips.len() != next.clips.len()
        || prev.masks.len() != next.masks.len()
        || prev.patterns.len() != next.patterns.len()
        || prev.filters.len() != next.filters.len()
        || prev.images.len() != next.images.len()
    {
        return FrameDiff {
            changed_cmd_indices: (0..n).collect(),
            all_static: false,
            pools_changed: true,
        };
    }
    let pools_changed = !pools_eq(prev, next);
    let points_same = points_bits_eq(&prev.points, &next.points);
    let mut changed = Vec::new();
    for (i, (a, b)) in prev.cmds.iter().zip(next.cmds.iter()).enumerate() {
        let eq = a == b && (points_same || geometry_content_eq(a, b, &prev.points, &next.points));
        if !eq {
            changed.push(i);
        }
    }
    let all_static = changed.is_empty() && !pools_changed;
    if pools_changed {
        // 池变化（动画 clip/mask/pattern/filter/image 定义）：影响面
        // 不经命令几何表达，保守全帧。
        changed.clear();
        changed.extend(0..n);
    }
    FrameDiff {
        changed_cmd_indices: changed,
        all_static,
        pools_changed,
    }
}

/// 脏行 → 行带（间隙 [`BAND_GAP`] 合并）。变化命令的**新旧 span 并集**
/// 都是脏行（几何移动 = 旧位擦除 + 新位绘制，行带整行重画天然覆盖）。
/// 非有限 span（层哨兵）→ 全帧。
fn merge_dirty_bands(
    changed: &[usize],
    prev_spans: &[[f32; 4]],
    new_spans: &[[f32; 4]],
    h: usize,
) -> Vec<(usize, usize)> {
    let mut dirty = vec![false; h];
    let mut all = false;
    for &i in changed {
        for sp in [prev_spans.get(i), new_spans.get(i)].into_iter().flatten() {
            let (y0, y1) = (sp[0], sp[1]);
            if !(y0.is_finite() && y1.is_finite()) {
                all = true;
                break;
            }
            // 行域整数化（no_std 纪律：floor/ceil 走 libm；cast 饱和 +
            // clamp 双保险。span 本身已按 BAND_IDX_PAD 外扩 ±1 行——
            // band 跳过定理的同一保守性，这里再叠 ROW_PAD 冗余护栏）。
            let r0 = if y0 <= ROW_PAD {
                0usize
            } else {
                (libm::floorf(y0 - ROW_PAD) as i64).clamp(0, h as i64) as usize
            };
            let r1 = if y1 >= h as f32 - ROW_PAD {
                h
            } else {
                (libm::ceilf(y1 + ROW_PAD) as i64).clamp(0, h as i64) as usize
            };
            for r in dirty.get_mut(r0..r1).into_iter().flatten() {
                *r = true;
            }
        }
        if all {
            break;
        }
    }
    if all {
        return vec![(0, h)];
    }
    let mut bands = Vec::new();
    let mut y = 0usize;
    while y < h {
        if !dirty[y] {
            y += 1;
            continue;
        }
        let start = y;
        let mut last = y;
        y += 1;
        while y < h {
            if dirty[y] {
                last = y;
                y += 1;
            } else if y + BAND_GAP <= h && dirty[y..y + BAND_GAP].iter().any(|d| *d) {
                y += 1; // 间隙内还有脏行 → 并入同带继续扫
            } else {
                break;
            }
        }
        bands.push((start, last + 1));
    }
    bands
}

// ---------------------------------------------------------------------------
// 测试：差分正确性 + 字节等值红线（合成 DL 序列逐帧 vs 全量 render）。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::dl::{Cmd, FillRule, Rgba8, NO_CLIP, NO_MASK, NO_PATTERN};

    fn solid(r: u8, g: u8, b: u8) -> crate::dl::Paint {
        crate::dl::Paint::Solid(Rgba8 { r, g, b, a: 255 })
    }

    /// 单矩形 DL（设备空间整数坐标点）。
    fn rect_dl(x: f32, y: f32, w: f32, h: f32, paint: crate::dl::Paint) -> DisplayList {
        let mut dl = DisplayList::default();
        let d = dl.push_rings(&[&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)]]);
        dl.cmds.push(Cmd::Fill {
            d,
            paint,
            opacity: 1.0,
            rule: FillRule::NonZero,
            clip: NO_CLIP,
            mask: NO_MASK,
            pattern: NO_PATTERN,
        });
        dl
    }

    fn vp(w: u32, h: u32) -> Viewport {
        Viewport {
            width: w,
            height: h,
            format: crate::raster::PixFmt::Rgba8,
            aa: 2,
        }
    }

    #[test]
    fn diff_static_and_moved() {
        let a = rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0));
        let b = rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0));
        let d = diff_display_list(&a, &b);
        assert!(d.all_static);
        assert!(d.changed_cmd_indices.is_empty());
        assert!(!d.pools_changed);

        // 同形状挪位置：命令 0 变、池不变。
        let c = rect_dl(12.0, 10.0, 40.0, 30.0, solid(255, 0, 0));
        let d = diff_display_list(&a, &c);
        assert!(!d.all_static);
        assert_eq!(d.changed_cmd_indices, vec![0]);

        // 同几何换色：命令 0 变。
        let e = rect_dl(10.0, 10.0, 40.0, 30.0, solid(0, 255, 0));
        let d = diff_display_list(&a, &e);
        assert_eq!(d.changed_cmd_indices, vec![0]);
    }

    #[test]
    fn diff_structure_drift_conservative() {
        let mut a = rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0));
        let mut b = rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0));
        // b 多一条裁切池条目 + 引用它：结构漂移 → 保守全帧。
        let clip = b.push_clip([0.0, 0.0, 50.0, 50.0]);
        if let Cmd::Fill { clip: c, .. } = &mut b.cmds[0] {
            *c = clip;
        }
        let d = diff_display_list(&a, &b);
        assert!(!d.all_static);
        assert!(d.pools_changed);
        assert_eq!(d.changed_cmd_indices.len(), b.cmds.len());
        // 池同长但内容不同（动画 clipPath 形态）：仍判池变化 → 保守全帧。
        let clip_a = a.push_clip([0.0, 0.0, 50.0, 50.0]);
        if let Cmd::Fill { clip: c, .. } = &mut a.cmds[0] {
            *c = clip_a;
        }
        b.clips[0].bounds = [0.0, 0.0, 60.0, 50.0];
        let d = diff_display_list(&a, &b);
        assert!(d.pools_changed, "池条目内容不同应判变化");
        assert_eq!(d.changed_cmd_indices.len(), b.cmds.len());
    }

    /// 字节等值红线（合成序列）：帧序列过 RetainedPlan，逐帧与全量
    /// [`crate::raster::render`] 逐字节比较。覆盖：全命中 / 几何移动 /
    /// 换色 / 命令数漂移 / invalidate。
    #[test]
    fn retained_equals_full_render_byte_for_byte() {
        let (w, h) = (96, 64);
        let viewport = vp(w, h);
        let mut plan = RetainedPlan::new(w, h).expect("测试视口必然可建计划");
        let mut out = vec![0u8; (w * h * 4) as usize];

        let frames = vec![
            rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0)),
            rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0)), // 全命中
            rect_dl(30.0, 20.0, 40.0, 30.0, solid(255, 0, 0)), // 几何移动
            rect_dl(30.0, 20.0, 40.0, 30.0, solid(0, 0, 255)), // 换色
            rect_dl(0.0, 0.0, 96.0, 64.0, solid(0, 255, 0)),   // 大面积重画
        ];
        for (i, dl) in frames.into_iter().enumerate() {
            let expect = {
                let mut buf = vec![0u8; (w * h * 4) as usize];
                crate::raster::render(&dl, &viewport, &mut buf);
                buf
            };
            if i == 2 {
                plan.invalidate(); // 中途作废：恢复通道走全帧
                for byte in out.iter_mut() {
                    *byte = 0xAA; // 毒化：全帧写必须覆盖
                }
            }
            plan.render(dl, &viewport, &mut out);
            assert_eq!(out, expect, "帧 {i}: retained 增量输出 != 全量 render");
        }
        assert_eq!(plan.stats().cache_hits, 1);
    }

    #[test]
    fn bands_cover_only_dirty_rows() {
        let (w, h) = (128, 200);
        let viewport = vp(w, h);
        let mut plan = RetainedPlan::new(w, h).expect("测试视口必然可建计划");
        let mut out = vec![0u8; (w * h * 4) as usize];
        let a = rect_dl(10.0, 10.0, 40.0, 30.0, solid(255, 0, 0));
        plan.render(a, &viewport, &mut out);
        // 移动到远处：写出行数应远小于全帧（脏带 ≈ 新旧位置并集）。
        let b = rect_dl(10.0, 150.0, 40.0, 30.0, solid(255, 0, 0));
        let expect = {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            crate::raster::render(&b, &viewport, &mut buf);
            buf
        };
        let rows = plan.render(b, &viewport, &mut out);
        assert!(rows < h as usize / 2, "rows={rows}");
        assert_eq!(out, expect);
    }

    #[test]
    fn retained_exports_exact_bands_and_preserves_background_semantics() {
        let (w, h) = (96, 80);
        let viewport = vp(w, h);
        let mut plan = RetainedPlan::new(w, h).expect("测试视口必然可建计划");
        let mut out = vec![0u8; (w * h * 4) as usize];
        let mut bands = Vec::new();
        let first = rect_dl(8.0, 12.0, 20.0, 16.0, solid(255, 0, 0));
        let rows = plan.render_with_bands_bg(
            first.clone(),
            &viewport,
            &mut out,
            crate::raster::BG_WHITE,
            &mut bands,
        );
        assert_eq!(rows, h as usize);
        assert_eq!(bands, vec![(0, h as usize)]);
        let mut expect = vec![0u8; (w * h * 4) as usize];
        crate::raster::render_bg(&first, &viewport, &mut expect, crate::raster::BG_WHITE);
        assert_eq!(out, expect);

        let rows = plan.render_with_bands_bg(
            first,
            &viewport,
            &mut out,
            crate::raster::BG_WHITE,
            &mut bands,
        );
        assert_eq!(rows, 0);
        assert!(bands.is_empty());

        let moved = rect_dl(8.0, 56.0, 20.0, 16.0, solid(255, 0, 0));
        let rows = plan.render_with_bands_bg(
            moved.clone(),
            &viewport,
            &mut out,
            crate::raster::BG_WHITE,
            &mut bands,
        );
        assert!(rows < h as usize);
        assert!(!bands.is_empty());
        let mut expect = vec![0u8; (w * h * 4) as usize];
        crate::raster::render_bg(&moved, &viewport, &mut expect, crate::raster::BG_WHITE);
        assert_eq!(out, expect);
    }

    #[test]
    fn layer_opacity_change_conservative() {
        let (w, h) = (64, 64);
        let viewport = vp(w, h);
        let mk = |op: f32| {
            let mut dl = rect_dl(10.0, 10.0, 20.0, 20.0, solid(9, 9, 9));
            dl.cmds.insert(
                0,
                Cmd::PushLayer {
                    opacity: op,
                    blend: crate::dl::BlendMode::Normal,
                },
            );
            dl.cmds.push(Cmd::PopLayer);
            dl
        };
        let a = mk(1.0);
        let b = mk(0.5);
        let d = diff_display_list(&a, &b);
        assert!(!d.all_static);
        // PushLayer 是哨兵 span → 全帧行带。
        let expect = {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            crate::raster::render(&b, &viewport, &mut buf);
            buf
        };
        let mut plan = RetainedPlan::new(w, h).expect("测试视口必然可建计划");
        let mut out = vec![0u8; (w * h * 4) as usize];
        plan.render(a, &viewport, &mut out);
        let rows = plan.render(b, &viewport, &mut out);
        assert_eq!(rows, h as usize, "层透明度变化应全帧重画");
        assert_eq!(out, expect);
    }
}
