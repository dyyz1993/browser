//! 车道PJ（viewbox-viewport）：根无 width/height 的文档尺寸回落与命中域
//! 一致化钉扎（集成测试——公开 API 面：parse_document / build_display_list
//! / hit_test）。

use svgine_core::builder::{build_display_list, parse_document, Document};
use svgine_core::hit_test::hit_test;

fn pj_size(svg: &str) -> (Option<f32>, Option<f32>) {
    let d: Document = parse_document(svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    (d.width, d.height)
}

fn pj_hit(svg: &str, w: f32, h: f32, x: f32, y: f32) -> Option<svgine_core::tree::NodeId> {
    let d: Document = parse_document(svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    hit_test(&d, w, h, x, y)
}

fn pj_clip(svg: &str, w: f32, h: f32) -> [f32; 4] {
    let d: Document = parse_document(svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let dl = build_display_list(&d, w, h);
    dl.clips.first().expect("root clip exists").bounds
}

const NS: &str = "http://www.w3.org/2000/svg";

/// 文档尺寸回落矩阵：无 w/h 回落 viewBox；显式优先；缺轴补轴；
/// 无 viewBox / 退化 viewBox 保持 None（宿主兜底不变）。
#[test]
fn pj_doc_size_fallback_matrix() {
    let s1 = format!(
        "<svg xmlns=\"{NS}\" viewBox=\"0 0 960 600\"><rect width=\"960\" height=\"600\"/></svg>"
    );
    assert_eq!(
        pj_size(&s1),
        (Some(960.0), Some(600.0)),
        "no-w/h falls back to viewbox"
    );
    let s2 = format!("<svg xmlns=\"{NS}\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\"/>");
    assert_eq!(pj_size(&s2), (Some(24.0), Some(24.0)), "explicit wins");
    let s3 = format!("<svg xmlns=\"{NS}\" width=\"192\" viewBox=\"0 0 480 360\"/>");
    assert_eq!(
        pj_size(&s3),
        (Some(192.0), Some(360.0)),
        "partial axis fills from viewbox"
    );
    let s4 = format!("<svg xmlns=\"{NS}\"><rect width=\"10\" height=\"10\"/></svg>");
    assert_eq!(
        pj_size(&s4),
        (None, None),
        "no viewbox keeps None (host 100x100 fallback)"
    );
    let s5 = format!("<svg xmlns=\"{NS}\" viewBox=\"0 0 0 100\"/>");
    assert_eq!(pj_size(&s5), (None, None), "degenerate viewbox keeps None");
}

/// 命中域：viewBox-only 文档在 doc 尺寸域（960x600）逐点正确；
/// 旧 100x100 缺省域下同点落空（错位域反例钉死，修复前 CLI 无参形态）。
#[test]
fn pj_hit_domain_viewbox_only() {
    let svg = format!("<svg xmlns=\"{NS}\" viewBox=\"0 0 960 600\"><rect id=\"b1\" x=\"620\" y=\"130\" width=\"280\" height=\"86\" fill=\"#0f3460\"/></svg>");
    let doc: Document = parse_document(&svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let b1 = doc.ids["b1"];
    assert_eq!(
        pj_hit(&svg, 960.0, 600.0, 770.0, 175.0),
        Some(b1),
        "button center hits in viewbox domain"
    );
    assert_eq!(
        pj_hit(&svg, 960.0, 600.0, 625.0, 135.0),
        Some(b1),
        "button corner hits"
    );
    assert_eq!(
        pj_hit(&svg, 960.0, 600.0, 500.0, 300.0),
        None,
        "outside misses"
    );
    assert_eq!(
        pj_hit(&svg, 100.0, 100.0, 770.0, 175.0),
        None,
        "wrong domain misses (pre-fix shape)"
    );
    // 渲染域一致：根裁切 = 全视口（viewBox 域 == 设备域，无 letterbox）。
    assert_eq!(
        pj_clip(&svg, 960.0, 600.0),
        [0.0, 0.0, 960.0, 600.0],
        "root clip = full viewport"
    );
}
