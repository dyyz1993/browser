//! 车道CR：Tier C 伪粗体合成集成测试（真实 TTF fixture，
//! fixture-noto-arial.ttf 与 tc_font.rs 同源）。
//! 覆盖：加宽几何量（Skia kStdFakeBoldInterp 插值表逐档 + 对称性）/
//! advance 不变 / 触发条件经 builder 全链（weight ≥600 才合成）/
//! 展开环绕向对齐（绕向错会抵消原填充——墨量反而变小的回归锚）。

use svgine_core::builder::{build_display_list_at_with_font, parse_document};
use svgine_core::font::Font;
use svgine_core::geom::Mat2x3;
use svgine_core::path::PathGeom;
use svgine_core::raster::{render, PixFmt, Viewport};
use svgine_core::text::emit_run_font_adj;

const ARIAL: &[u8] = include_bytes!("fixture-noto-arial.ttf");

/// 'l' 字形 bbox（用户空间；emit_run_font_adj 的几何输出）。
fn l_bbox(font: &Font, fs: f32, bold: bool) -> (f32, f32, f32, f32) {
    let mut g = PathGeom::default();
    let adv = emit_run_font_adj(
        "l",
        10.0,
        40.0,
        fs,
        0.0,
        0.0,
        0.0,
        0.0,
        Mat2x3::IDENTITY,
        &mut g,
        font,
        bold,
    );
    assert!(!g.subs.is_empty(), "l 应有轮廓");
    let mut bb = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for sp in &g.subs {
        for &(x, y) in &sp.pts {
            bb.0 = bb.0.min(x);
            bb.1 = bb.1.min(y);
            bb.2 = bb.2.max(x);
            bb.3 = bb.3.max(y);
        }
    }
    // advance 不受伪粗体影响（Chrome fake bold 掩膜级膨胀，advance 不变）
    let want = font
        .advance_em(font.glyph_id(0x6C).expect("l gid"))
        .expect("l adv")
        * fs;
    assert!(
        (adv - want).abs() < 1e-3,
        "advance 应 = 字体度量（不受 bold 影响）：{adv} vs {want}"
    );
    bb
}

/// 插值表逐档：加宽总量 = fs·interp(fs)（Skia kStdFakeBoldInterp 移植）。
/// bbox 每侧 ±w/2；'l' 竖杆矩形 + miter 90° 角不越出带边（对角伸出
/// = hw·√2·cos45° = hw，与带缘平齐）。
#[test]
fn bold_widening_matches_skia_interp_table() {
    let font = Font::parse(ARIAL).expect("parse");
    // (fs, 预期总加宽 w)：9→1/24 钳位；18→插值中段；36→1/32 钳位；72→1/32
    for (fs, want) in [
        (9.0, 9.0 / 24.0),
        (
            18.0,
            18.0 * (1.0 / 24.0 + 9.0 * (1.0 / 32.0 - 1.0 / 24.0) / 27.0),
        ),
        (36.0, 36.0 / 32.0),
        (72.0, 72.0 / 32.0),
    ] {
        let r = l_bbox(&font, fs, false);
        let b = l_bbox(&font, fs, true);
        let dw = (b.2 - b.0) - (r.2 - r.0);
        assert!(
            (dw - want).abs() < 1e-3,
            "fs={fs} 宽度增量 {dw} 应 = {want:.4}"
        );
        let dl = r.0 - b.0;
        let dr = b.2 - r.2;
        assert!(
            (dl - want / 2.0).abs() < 1e-3 && (dr - want / 2.0).abs() < 1e-3,
            "fs={fs} 应对称膨胀 ±w/2：dl={dl} dr={dr} w/2={:.4}",
            want / 2.0
        );
    }
}

/// 触发条件经 builder 全链：weight 600/700 合成（ink 严格多于 400），
/// 500 与 400 渲染逐字节相同（不合成）。
#[test]
fn bold_triggers_only_at_weight_600() {
    let font = Font::parse(ARIAL).expect("parse");
    let buf_of = |svg: &str| -> Vec<u8> {
        let doc = parse_document(svg).expect("parse");
        let dl = build_display_list_at_with_font(&doc, 60.0, 60.0, 0.0, Some(&font));
        let vp = Viewport {
            width: 60,
            height: 60,
            format: PixFmt::Rgba8,
            aa: 1,
        };
        let mut buf = vec![0u8; 60 * 60 * 4];
        render(&dl, &vp, &mut buf);
        buf
    };
    let svg = |w: &str| {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60"><text x="10" y="45" font-size="36" font-weight="{w}" fill="#000">l</text></svg>"##
        )
    };
    let ink = |buf: &[u8]| buf.chunks(4).filter(|p| p[3] > 0).count();
    let b400 = buf_of(&svg("400"));
    let b500 = buf_of(&svg("500"));
    let b600 = buf_of(&svg("600"));
    let b700 = buf_of(&svg("700"));
    assert_eq!(b400, b500, "500 不应合成（与 400 逐字节相同）");
    assert!(
        ink(&b600) > ink(&b400),
        "600 应合成：{} > {}",
        ink(&b600),
        ink(&b400)
    );
    assert!(
        ink(&b700) > ink(&b400),
        "700 应合成：{} > {}",
        ink(&b700),
        ink(&b400)
    );
}

/// 绕向对齐回归锚：展开环若与源轮廓反向，nonzero 并集把带内原墨
/// 抵成洞（几何覆盖面积反而缩水）——cr-bold 探针的坏绕向指纹。
/// Arial 'g'（多轮廓含反向 counter）上断言 bold 几何覆盖面积严格
/// 大于 regular（覆盖面积 = 子路径 |shoelace| 之和的近似）。
#[test]
fn bold_ink_monotonically_increases_despite_winding() {
    let font = Font::parse(ARIAL).expect("parse");
    let geom_area = |bold: bool| -> f32 {
        let mut g = PathGeom::default();
        emit_run_font_adj(
            "g",
            10.0,
            50.0,
            48.0,
            0.0,
            0.0,
            0.0,
            0.0,
            Mat2x3::IDENTITY,
            &mut g,
            &font,
            bold,
        );
        let mut area = 0.0f32;
        for sp in &g.subs {
            let n = sp.pts.len();
            let mut s = 0.0;
            for i in 0..n {
                let j = (i + 1) % n;
                s += sp.pts[i].0 * sp.pts[j].1 - sp.pts[j].0 * sp.pts[i].1;
            }
            area += s.abs() * 0.5;
        }
        area
    };
    let regular = geom_area(false);
    let bold = geom_area(true);
    assert!(
        bold > regular * 1.05,
        "bold 并集覆盖应明显大于 regular（绕向错时反缩水）：{bold} vs {regular}"
    );
}
