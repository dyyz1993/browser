//! 车道hwB（行带并行一期）：band 拼接 vs 整帧渲染**逐字节等值**断言。
//!
//! 口径：同一 Document/t/aa/bg 下，`render_bg` 整帧 vs `precompute_frame`
//! 加 `render_band` 分带拼接。band 缓冲以 0xAA 哨兵预填，render_band
//! 必须覆写窗口内每个字节，未写残留会直接炸出。覆盖面：纯色、描边
//! dash、渐变 SpanEval 与抖动相位、clip/mask、嵌套组层（层栈）、
//! pattern 超采样瓦片、滤镜 blur、文本、SMIL 动画 t>0。
//! band 划分：均匀 2/4/7/16 带，以及奇数行边界带（1/2 行宽的边缘带
//! 最能抓行间状态泄漏）。

use svgine_core::builder::{build_display_list_at_with_font, parse_document, Document};
use svgine_core::dl::Rgba8;
use svgine_core::raster::{
    precompute_frame, render_band, render_bg, PixFmt, RenderCache, Viewport,
};

const NS: &str = "http://www.w3.org/2000/svg";

fn svg(body: &str) -> String {
    format!(
        r##"<svg xmlns="{NS}" width="{W}" height="{H}" viewBox="0 0 {W} {H}">{body}</svg>"##,
        W = 97,
        H = 61,
    )
}

fn frame(doc: &Document, w: u32, h: u32, t: f64) -> svgine_core::dl::DisplayList {
    build_display_list_at_with_font(doc, w as f32, h as f32, t, None)
}

/// 整帧渲染基线（bg 指定底色，与 CLI 白底管线同口径）。
fn full_frame(dl: &svgine_core::dl::DisplayList, vp: &Viewport, bg: Rgba8) -> Vec<u8> {
    let mut out = vec![0u8; vp.width as usize * vp.height as usize * 4];
    render_bg(dl, vp, &mut out, bg);
    out
}

/// band 拼接渲染：`cuts` = 递增的 band 边界（帧坐标，均 ∈ (0,h)）；
/// band 缓冲 0xAA 哨兵预填（必须被 render_band 全窗覆写）。
fn band_stitched(
    dl: &svgine_core::dl::DisplayList,
    vp: &Viewport,
    bg: Rgba8,
    cuts: &[usize],
) -> Vec<u8> {
    let w = vp.width as usize;
    let h = vp.height as usize;
    let shared = precompute_frame(dl, vp);
    let mut out = vec![0u8; w * h * 4];
    let mut y = 0usize;
    for &cut in cuts {
        assert!(cut > y && cut < h, "bad cut {cut} (y={y} h={h})");
        let rows = cut - y;
        let mut band = vec![0xAAu8; rows * w * 4];
        let mut cache = RenderCache::default();
        render_band(dl, vp, &mut band, y, cut, bg, &shared, &mut cache);
        out[y * w * 4..cut * w * 4].copy_from_slice(&band);
        y = cut;
    }
    if y < h {
        let rows = h - y;
        let mut band = vec![0xAAu8; rows * w * 4];
        let mut cache = RenderCache::default();
        render_band(dl, vp, &mut band, y, h, bg, &shared, &mut cache);
        out[y * w * 4..].copy_from_slice(&band);
    }
    out
}

/// 均匀 n 带 + 奇数边界带两组划分的等值断言（白底 + 透明底双底色）。
fn assert_band_equal(doc: &Document, t: f64, aa: u8, label: &str) {
    let (w, h) = (97u32, 61u32);
    let dl = frame(doc, w, h, t);
    let vp = Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa,
    };
    // 奇数边界带：1/2 行宽的边缘带 + 中部奇位切口（最能抓行间状态泄漏）
    let odd: Vec<usize> = vec![
        1,
        2,
        5,
        17,
        30,
        31,
        h as usize - 5,
        h as usize - 2,
        h as usize - 1,
    ];
    let mut partitions: Vec<Vec<usize>> = vec![odd];
    for n in [2usize, 4, 7, 16] {
        let p: Vec<usize> = (1..n).map(|i| i * h as usize / n).collect();
        partitions.push(p);
    }
    for bg in [
        Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        },
    ] {
        let base = full_frame(&dl, &vp, bg);
        for cuts in &partitions {
            let got = band_stitched(&dl, &vp, bg, cuts);
            assert_eq!(got.len(), base.len(), "[{label} aa={aa}] 拼接帧长度不一致");
            for (i, (a, b)) in got.iter().zip(base.iter()).enumerate() {
                assert_eq!(
                    a,
                    b,
                    "[{label} aa={aa} bg] 字节差异 @byte {i} (px {} ch {})",
                    i / 4,
                    i % 4
                );
            }
        }
    }
}

fn parse_or_panic(src: &str) -> Document {
    parse_document(src).unwrap_or_else(|e| panic!("parse failed: {e:?}"))
}

/// 纯色形状 + 描边 + dash（AA 覆盖边界带 / dash 相位）。
#[test]
fn band_eq_shapes_stroke_dash() {
    let src = svg(
        r##"<rect x="3.5" y="2.25" width="40" height="30.75" fill="#c04" stroke="#210" stroke-width="2.5"/>
         <circle cx="70" cy="20" r="14.5" fill="#08a" fill-opacity="0.7"/>
         <path d="M 5 50 Q 30 38 55 52 T 92 48" fill="none" stroke="#333" stroke-width="3" stroke-dasharray="6 3 1.5 4" stroke-dashoffset="2.2"/>
         <ellipse cx="48" cy="18" rx="12" ry="7.5" fill="none" stroke="#903" stroke-width="1.25"/>"##,
    );
    for aa in [1u8, 2, 3] {
        assert_band_equal(&parse_or_panic(&src), 0.0, aa, "shapes-dash");
    }
}

/// 渐变（linear/radial + gradientTransform + 抖动相位）。
#[test]
fn band_eq_gradients() {
    let src = svg(r##"<defs>
         <linearGradient id="lg" x1="0" y1="0" x2="1" y2="1">
           <stop offset="0" stop-color="#ff8800"/><stop offset="0.5" stop-color="#cc2266"/><stop offset="1" stop-color="#1133aa"/>
         </linearGradient>
         <radialGradient id="rg" cx="0.7" cy="0.3" r="0.8" gradientTransform="rotate(30 0.5 0.5)">
           <stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#004400" stop-opacity="0.4"/>
         </radialGradient>
       </defs>
       <rect width="97" height="30" fill="url(#lg)"/>
       <rect y="31" width="97" height="30" fill="url(#rg)"/>"##);
    for aa in [1u8, 2, 3] {
        assert_band_equal(&parse_or_panic(&src), 0.0, aa, "gradients");
    }
}

/// 嵌套组层（PushLayer/PopLayer 栈 + 组 opacity 乘链 + 混合域）。
#[test]
fn band_eq_nested_layers() {
    let src = svg(r##"<rect width="97" height="61" fill="#ddd"/>
         <g opacity="0.5">
           <rect x="4" y="4" width="50" height="30" fill="#e33"/>
           <g opacity="0.6" transform="translate(20,10)">
             <circle cx="20" cy="20" r="12" fill="#06c" fill-opacity="0.8"/>
             <rect x="25" y="12" width="30" height="18" fill="#3a3" opacity="0.7"/>
           </g>
         </g>
         <g opacity="0.9"><path d="M60 40 L90 40 L75 58 Z" fill="#fc0" opacity="0.65"/></g>"##);
    for aa in [1u8, 2, 3] {
        assert_band_equal(&parse_or_panic(&src), 0.0, aa, "nested-layers");
    }
}

/// clipPath（矢量 + 矩形链）+ 亮度 mask（全画布耦合预计算消费）。
#[test]
fn band_eq_clip_mask() {
    let src = svg(r##"<defs>
         <clipPath id="cp"><path d="M10 5 Q 50 40 90 8 L 70 55 L 20 50 Z"/></clipPath>
         <mask id="m">
           <rect x="0" y="0" width="97" height="61" fill="white"/>
           <circle cx="30" cy="30" r="18" fill="black"/>
           <rect x="50" y="10" width="30" height="40" fill="#808080"/>
         </mask>
       </defs>
       <g clip-path="url(#cp)">
         <rect x="5" y="3" width="87" height="55" fill="#26a"/>
         <circle cx="60" cy="30" r="20" fill="#fa0"/>
       </g>
       <g mask="url(#m)"><rect x="10" y="8" width="77" height="45" fill="#0a5" opacity="0.85"/></g>"##);
    for aa in [1u8, 2, 3] {
        assert_band_equal(&parse_or_panic(&src), 0.0, aa, "clip-mask");
    }
}

/// pattern（旋转瓦片 = 超采样盒式滤波域）+ 滤镜 blur（跨行核，
/// 预计算后 band 安全的关键证据枚）。
#[test]
fn band_eq_pattern_filter() {
    let src = svg(r##"<defs>
         <pattern id="pt" width="14" height="10" patternTransform="rotate(30)" patternUnits="userSpaceOnUse">
           <rect width="14" height="10" fill="#eee"/>
           <circle cx="7" cy="5" r="3.5" fill="#c33"/>
         </pattern>
         <filter id="fb" x="-20%" y="-20%" width="140%" height="140%">
           <feGaussianBlur stdDeviation="3.2"/>
         </filter>
       </defs>
       <rect width="97" height="30" fill="url(#pt)"/>
       <g filter="url(#fb)">
         <rect x="20" y="36" width="50" height="18" fill="#08f"/>
         <circle cx="75" cy="48" r="9" fill="#f80"/>
       </g>"##);
    for aa in [1u8, 2, 3] {
        assert_band_equal(&parse_or_panic(&src), 0.0, aa, "pattern-filter");
    }
}

/// 文本（Hershey 烘焙路径）+ use 引用 + SMIL 动画 t>0 采样。
#[test]
fn band_eq_text_anim() {
    let src = svg(
        r##"<defs><rect id="rr" x="0" y="0" width="18" height="9" fill="#5a5"/></defs>
         <rect width="97" height="61" fill="#f4f4f4"/>
         <use href="#rr" x="6" y="6"/>
         <text x="6" y="30" font-size="11" fill="#222">band Ab1 {(g)}"</text>
         <circle cx="70" cy="45" r="12" fill="#06c">
           <animate attributeName="opacity" values="1;0.2;1" dur="3s" repeatCount="indefinite"/>
         </circle>
         <rect x="8" y="38" width="34" height="18" fill="#c33">
           <animateTransform attributeName="transform" type="translate" values="0 0; 20 8" dur="3s" repeatCount="indefinite"/>
         </rect>"##,
    );
    let doc = parse_or_panic(&src);
    // 静止 + 动画中途两时刻（t>0 走 SMIL 属性覆盖表 → DL 重建路径）
    for t in [0.0f64, 1234.5] {
        for aa in [1u8, 2, 3] {
            assert_band_equal(&doc, t, aa, "text-anim");
        }
    }
}

/// 边界防御：空窗 / 越界窗无操作不 panic；整帧两 band（0/h 切一刀）
/// 与单 band 全窗等值（退化 = 全帧）。
#[test]
fn band_edge_contract() {
    let src = svg(r##"<rect width="97" height="61" fill="#246"/>"##);
    let doc = parse_or_panic(&src);
    let (w, h) = (97u32, 61u32);
    let dl = frame(&doc, w, h, 0.0);
    let vp = Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa: 2,
    };
    let shared = precompute_frame(&dl, &vp);
    let base = full_frame(
        &dl,
        &vp,
        Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
    );

    // 越界钳位：y1 > h 收进 h；y0 ≥ y1 空窗
    let mut band = vec![0xAAu8; h as usize * w as usize * 4];
    let mut cache = RenderCache::default();
    render_band(
        &dl,
        &vp,
        &mut band,
        0,
        h as usize + 100,
        Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        &shared,
        &mut cache,
    );
    assert_eq!(band, base, "越界钳位后单 band 全窗应等于整帧");

    // 空窗：缓冲保持哨兵（无写入）
    let mut band2 = vec![0xAAu8; w as usize * 4];
    render_band(
        &dl,
        &vp,
        &mut band2,
        40,
        40,
        Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        &shared,
        &mut cache,
    );
    assert!(band2.iter().all(|&b| b == 0xAA), "空窗不得写入任何字节");

    // 565 格式：一期 band 面外，无操作
    let vp565 = Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgb565,
        aa: 2,
    };
    let mut band3 = vec![0xAAu8; w as usize * 4];
    render_band(
        &dl,
        &vp565,
        &mut band3,
        0,
        h as usize,
        Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        &shared,
        &mut cache,
    );
    assert!(band3.iter().all(|&b| b == 0xAA), "565 下 band 应无操作");
}

// ---------------------------------------------------------------------------
// 车道hwB2：命令→行区间索引。除拼接等值（既有 helper 复用）外，用
// band_index_stats 钉扎「跳过语义」本身——区间无交的命令必须被跳、
// PushLayer/PopLayer 哨兵恒不跳。
// ---------------------------------------------------------------------------

use svgine_core::dl::{Cmd, DisplayList, ImgData, Paint, PathRef, NO_CLIP, NO_MASK, NO_PATTERN};
use svgine_core::raster::{balanced_band_cuts, band_index_stats};

fn white() -> Rgba8 {
    Rgba8 {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    }
}

fn vp_rgba(w: u32, h: u32, aa: u8) -> Viewport {
    Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa,
    }
}

/// 垂直不相交形状：上圆 + 下矩形/线，中段空窗。stats 钉扎「区间无交
/// 必跳」，拼接等值钉扎「跳错即缺角炸出」。
#[test]
fn band_idx_vertical_disjoint_and_stats() {
    let src = svg(r##"<circle cx="48" cy="10" r="8" fill="#08a"/>
         <rect x="10" y="48" width="60" height="10" fill="#c04"/>
         <path d="M 5 55 L 90 58" stroke="#333" stroke-width="2" fill="none"/>"##);
    let doc = parse_or_panic(&src);
    let (w, h) = (97u32, 61u32);
    let dl = frame(&doc, w, h, 0.0);
    let vp = vp_rgba(w, h, 2);
    // cuts=[20,40] → band0 [0,20) 只圆相交；band1 [20,40) 全跳；
    // band2 [40,61) 矩形+线相交。cmds = 3 Fill（无层命令）。
    let stats = band_index_stats(&dl, &vp, &[20, 40]);
    assert_eq!(stats.len(), 3);
    assert_eq!(stats[0], (1, 2), "band0 应只处理圆、跳过矩形与线");
    assert_eq!(stats[1], (0, 3), "band1 空窗应全跳");
    assert_eq!(stats[2], (2, 1), "band2 应处理矩形与线、跳过圆");
    // 单带全帧：零跳
    assert_eq!(band_index_stats(&dl, &vp, &[]), vec![(3, 0)]);
    // 拼接等值（索引在环上）
    for aa in [1u8, 2, 3] {
        assert_band_equal(&doc, 0.0, aa, "idx-vertical-disjoint");
    }
}

/// 形状恰落在 band 切口行上/下（半开区间边界）+ 跨切口大形状：
/// floor/ceil 口径的边界钉扎。
#[test]
fn band_idx_boundary_exact_rows() {
    let src = svg(r##"<rect x="5" y="19" width="40" height="2" fill="#08a"/>
         <rect x="5" y="21" width="40" height="2" fill="#c04"/>
         <rect x="5" y="29.5" width="40" height="1" fill="#0a0"/>
         <circle cx="70" cy="20" r="14" fill="none" stroke="#903" stroke-width="1.5"/>
         <rect x="2" y="10" width="93" height="41" fill="none" stroke="#246" stroke-width="0.5"/>"##);
    let doc = parse_or_panic(&src);
    for aa in [1u8, 2, 3] {
        assert_band_equal(&doc, 0.0, aa, "idx-boundary-exact");
    }
}

/// 滤镜 blur 区域横跨切口（blur 核有界于区域位图——索引按区域行窗
/// 跳过时跨界核的带必须保留该命令）+ 层命令夹在其中（哨兵恒不跳、
/// 栈不错位）。cmds = [Push, Filter, Pop, Filter]。
#[test]
fn band_idx_filter_region_crossing() {
    let src = svg(
        r##"<defs><filter id="fb" x="-20%" y="-20%" width="140%" height="140%">
           <feGaussianBlur stdDeviation="3"/>
         </filter></defs>
       <g opacity="0.8">
         <rect x="20" y="18" width="57" height="24" fill="#08a" filter="url(#fb)"/>
       </g>
       <rect x="5" y="40" width="40" height="12" fill="#c04" filter="url(#fb)"/>"##,
    );
    let doc = parse_or_panic(&src);
    for aa in [1u8, 2, 3] {
        assert_band_equal(&doc, 0.0, aa, "idx-filter-crossing");
    }
    // stats 钉扎：区域1 y≈[13,47)、区域2 y≈[37,55)（含 PAD）
    let (w, h) = (97u32, 61u32);
    let dl = frame(&doc, w, h, 0.0);
    let vp = vp_rgba(w, h, 2);
    let stats = band_index_stats(&dl, &vp, &[10, 50]);
    assert_eq!(
        stats[0],
        (2, 2),
        "band0 [0,10) 只 Push/Pop 不跳、两条 Filter 全跳"
    );
    assert_eq!(stats[1], (4, 0), "band1 [10,50) 两区域均相交全保留");
    assert_eq!(stats[2], (3, 1), "band2 [50,61) 只剩 Push/Pop + 区域2");
}

/// 手写 DL 的 Cmd::Image（绕过解码器注入）：斜切 xf 的位图在帧顶 +
/// 病态池项（w=0）与 opacity 0（索引空区间恒跳）+ 空路径 Fill 同帧。
#[cfg(feature = "image-decode")]
#[test]
fn band_idx_image_manual_dl() {
    let mut dl = DisplayList::default();
    let mut rgba = Vec::new();
    for y in 0..4u32 {
        for x in 0..4u32 {
            let v = if (x + y) % 2 == 0 { 200u8 } else { 40 };
            rgba.extend_from_slice(&[v, 255 - v, v, 255]);
        }
    }
    let img0 = dl.push_image_def(ImgData {
        w: 4,
        h: 4,
        rgba: rgba.clone(),
    });
    let img_bad = dl.push_image_def(ImgData {
        w: 0,
        h: 0,
        rgba: Vec::new(),
    });
    // 斜切仿射（y' = x + y + 20，非旋转正交也覆盖）：y AABB [20,28]
    dl.cmds.push(Cmd::Image {
        img: img0,
        xf: [1.0, 1.0, -1.0, 1.0, 40.0, 20.0],
        opacity: 1.0,
        clip: NO_CLIP,
        mask: NO_MASK,
        pixelated: false,
    });
    dl.cmds.push(Cmd::Image {
        img: img_bad,
        xf: [10.0, 0.0, 0.0, 10.0, 5.0, 5.0],
        opacity: 1.0,
        clip: NO_CLIP,
        mask: NO_MASK,
        pixelated: true,
    });
    dl.cmds.push(Cmd::Image {
        img: img0,
        xf: [8.0, 0.0, 0.0, 8.0, 30.0, 2.0],
        opacity: 0.0,
        clip: NO_CLIP,
        mask: NO_MASK,
        pixelated: false,
    });
    dl.cmds.push(Cmd::Fill {
        d: PathRef { first: 0, count: 0 },
        paint: Paint::Solid(white()),
        opacity: 1.0,
        rule: svgine_core::dl::FillRule::NonZero,
        clip: NO_CLIP,
        mask: NO_MASK,
        pattern: NO_PATTERN,
    });
    let (w, h) = (97u32, 61u32);
    let vp = vp_rgba(w, h, 3);
    let base = full_frame(&dl, &vp, white());
    let cuts = [1usize, 2, 17, 30, 31, 45];
    let got = band_stitched(&dl, &vp, white(), &cuts);
    assert_eq!(got, base, "手写 DL 位图带拼接应与整帧逐字节一致");
    // 病态池项 / opacity 0 / 空路径 = 空区间恒跳；band0 只斜切位图
    let stats = band_index_stats(&dl, &vp, &[30]);
    assert_eq!(stats[0], (1, 3), "band0 [0,30) 应只处理斜切位图");
    assert_eq!(stats[1], (0, 4), "band1 [30,61) 应全跳（位图 y AABB ≤ 30）");
}

/// 均衡切分（车道hwB2）：切口合法性（严格递增、∈ (0,h)、数量正确）
/// 与均衡切口下的拼接等值（切分策略不进渲染语义）。垂直偏斜语料上
/// 均衡切口必须与均匀切口不同（否则权重模型失效）。
#[test]
fn band_idx_balanced_cuts() {
    let src = svg(r##"<circle cx="48" cy="10" r="8" fill="#08a"/>
         <rect x="10" y="48" width="60" height="10" fill="#c04"/>"##);
    let doc = parse_or_panic(&src);
    let (w, h) = (97u32, 61u32);
    let dl = frame(&doc, w, h, 0.0);
    let vp = vp_rgba(w, h, 2);
    for bands in [2u32, 4, 8] {
        let cuts = balanced_band_cuts(&dl, &vp, bands);
        assert_eq!(cuts.len(), (bands - 1) as usize, "切口数 = bands-1");
        let mut prev = 0usize;
        for &c in &cuts {
            assert!(c > prev && c < h as usize, "切口须严格递增且 ∈ (0,h)：{c}");
            prev = c;
        }
        // 拼接等值（均衡切分只是换切线，不换语义）
        let base = full_frame(&dl, &vp, white());
        let got = band_stitched(&dl, &vp, white(), &cuts);
        assert_eq!(got, base, "均衡切分拼接应与整帧逐字节一致（bands={bands}）");
    }
    // 偏斜语料：2 带均衡切口 ≠ 均匀切口 h/2（权重模型有效性钉扎——
    // 圆面积小、矩形面积大，等分点应被压向矩形侧）
    let cuts2 = balanced_band_cuts(&dl, &vp, 2);
    assert_ne!(
        cuts2,
        vec![h as usize / 2],
        "偏斜语料均衡切口不应等于均匀切"
    );
    // 退化输入：bands ≤ 1 → 空（整帧原路径）；空 DL → 均匀回退不 panic
    assert!(balanced_band_cuts(&dl, &vp, 1).is_empty());
    assert_eq!(balanced_band_cuts(&DisplayList::default(), &vp, 4).len(), 3);
}
