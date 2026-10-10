//! M8 验收：`<filter>` 滤镜四件套端到端像素断言（feGaussianBlur /
//! feDropShadow / feColorMatrix / feOffset + feFlood + feComposite +
//! feMerge + 容错回退）。
//!
//! 每个用例都是最小 SVG → parse → DisplayList → render 的全链路
//! 断言（aa=1 像素中心采样，值可手算）；差分级验证（预言机逐像素
//! 对齐）见 docs/assessments 的 M8 评估报告。

use svgine_core::builder::{build_display_list, parse_document};
use svgine_core::raster::{render, PixFmt, Viewport};

/// aa=1 视口（w×h）。
fn vp(w: u32, h: u32) -> Viewport {
    Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa: 1,
    }
}

/// 取 (x,y) 像素 RGBA。
fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let o = (y * w + x) as usize * 4;
    [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
}

/// SVG → 像素缓冲（aa=1）。
fn render_svg(svg: &str, w: u32, h: u32) -> Vec<u8> {
    let doc = parse_document(svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let dl = build_display_list(&doc, w as f32, h as f32);
    let viewport = vp(w, h);
    let mut buf = alloc_zero(w, h);
    render(&dl, &viewport, &mut buf);
    buf
}

fn alloc_zero(w: u32, h: u32) -> Vec<u8> {
    use std::vec;
    vec![0u8; (w * h * 4) as usize]
}

/// feGaussianBlur σ=5（W3C 三盒 d=9）：2px 白线中心 alpha 从 255 降至
/// ~42（三盒扩散），裙边延展到线缘 +12px（3×4 taps），之外清零。
#[test]
fn gaussian_blur_softens_and_spreads() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 40 20">
  <defs><filter id="b" filterUnits="userSpaceOnUse" x="0" y="0" width="40" height="20">
    <feGaussianBlur stdDeviation="5"/>
  </filter></defs>
  <rect x="19" y="0" width="2" height="20" fill="#ffffff" filter="url(#b)"/>
</svg>"##;
    let buf = render_svg(svg, 40, 20);
    // 中心列（线本体）：显著软化（模糊后 alpha 远低于 255）
    let center = px(&buf, 40, 19, 10)[3];
    assert!(
        (30..=60).contains(&center),
        "线中心 alpha 应落在 [30,60]（实测 {center}）"
    );
    // 裙边：线缘（x=21）外 4px / 9px 仍有能量、且单调衰减
    let a25 = px(&buf, 40, 25, 10)[3];
    let a30 = px(&buf, 40, 30, 10)[3];
    assert!(a25 >= 15, "x=25 裙边 alpha≥15（实测 {a25}）");
    assert!(a30 >= 1 && a30 < a25, "x=30 衰减（实测 {a30} < {a25}）");
    // 三盒 reach = 3×4 taps = 12px：线缘 21+12=33，34 起清零
    assert_eq!(px(&buf, 40, 34, 10), [0, 0, 0, 0], "reach 外应清零");
    assert_eq!(px(&buf, 40, 38, 10), [0, 0, 0, 0]);
    // 白线模糊后色通道保持白（预乘往返不偏色）
    let c = px(&buf, 40, 19, 10);
    assert!(c[0] >= c[2] && c[0] >= c[1], "白色分量占优（实测 {c:?}）");
}

/// feDropShadow dx=dy=3、σ=0（无模糊）：阴影 = SourceAlpha 平移着
/// flood 色；前景原图叠上。
#[test]
fn drop_shadow_offset_and_color() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="30" viewBox="0 0 30 30">
  <defs><filter id="s" filterUnits="userSpaceOnUse" x="0" y="0" width="30" height="30">
    <feDropShadow dx="3" dy="3" stdDeviation="0" flood-color="#ff0000" flood-opacity="1"/>
  </filter></defs>
  <rect x="5" y="5" width="10" height="10" fill="#0000ff" filter="url(#s)"/>
</svg>"##;
    let buf = render_svg(svg, 30, 30);
    // 前景保持蓝
    assert_eq!(px(&buf, 30, 10, 10), [0, 0, 255, 255], "前景原色");
    // 阴影矩形 = [8,18)²：(16,16) 纯阴影红、(17,17) 同
    assert_eq!(px(&buf, 30, 16, 16), [255, 0, 0, 255], "偏移 (3,3) 阴影色");
    assert_eq!(px(&buf, 30, 17, 17), [255, 0, 0, 255]);
    // 阴影水平外（x<8）/垂直外（y<8）清零
    assert_eq!(px(&buf, 30, 16, 7), [0, 0, 0, 0], "y<8 无阴影");
    assert_eq!(px(&buf, 30, 7, 16), [0, 0, 0, 0], "x<8 无阴影");
    assert_eq!(px(&buf, 30, 2, 2), [0, 0, 0, 0], "远处透明");
}

/// feColorMatrix 交换 R/B 通道：红矩形变蓝（linearRGB 域乘单位矩阵
/// 行交换——饱和端点值精确往返）。
#[test]
fn color_matrix_swaps_red_blue() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20">
  <defs><filter id="m">
    <feColorMatrix type="matrix" values="0 0 1 0 0  0 1 0 0 0  1 0 0 0 0  0 0 0 1 0"/>
  </filter></defs>
  <rect x="4" y="4" width="12" height="12" fill="#ff0000" filter="url(#m)"/>
</svg>"##;
    let buf = render_svg(svg, 20, 20);
    assert_eq!(px(&buf, 20, 10, 10), [0, 0, 255, 255], "红→蓝");
    assert_eq!(px(&buf, 20, 2, 2), [0, 0, 0, 0], "矩形外不受滤镜影响");
}

/// feComposite operator="in"：feFlood 全区域黄色 ∩ SourceGraphic（圆）
/// → 交集外清零、交集内纯 flood 色（Porter-Duff in 的裁切语义）。
#[test]
fn composite_in_clips_to_source() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20">
  <defs><filter id="c" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
    <feFlood flood-color="#ffff00" flood-opacity="1" result="f"/>
    <feComposite in="f" in2="SourceGraphic" operator="in"/>
  </filter></defs>
  <circle cx="10" cy="10" r="5" fill="#00ff00" filter="url(#c)"/>
</svg>"##;
    let buf = render_svg(svg, 20, 20);
    // 圆内：flood 黄（源色被 in 完全替换）
    assert_eq!(px(&buf, 20, 10, 10), [255, 255, 0, 255], "交集内 flood 色");
    assert_eq!(px(&buf, 20, 10, 14), [255, 255, 0, 255]);
    // 圆外（含近角）：flood 被 in 裁掉 → 透明
    assert_eq!(px(&buf, 20, 16, 16), [0, 0, 0, 0], "交集外清零");
    assert_eq!(px(&buf, 20, 19, 19), [0, 0, 0, 0], "区域角落清零");
}

/// feOffset + feFlood + feComposite(in) + feMerge 完整链（s7-02 的
/// fOffsetMerge 模式）：命名 result=/in= 解析、偏移阴影着色、原图
/// 叠上层。
#[test]
fn offset_composite_merge_chain() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="30" viewBox="0 0 30 30">
  <defs><filter id="om" filterUnits="userSpaceOnUse" x="0" y="0" width="30" height="30">
    <feOffset in="SourceGraphic" dx="6" dy="6" result="off"/>
    <feFlood flood-color="#8e44ad" flood-opacity="1" result="col"/>
    <feComposite in="col" in2="off" operator="in" result="sh"/>
    <feMerge><feMergeNode in="sh"/><feMergeNode in="SourceGraphic"/></feMerge>
  </filter></defs>
  <circle cx="10" cy="10" r="5" fill="#ff0000" filter="url(#om)"/>
</svg>"##;
    let buf = render_svg(svg, 30, 30);
    // 原图：红圆心
    assert_eq!(px(&buf, 30, 8, 8), [255, 0, 0, 255], "SourceGraphic 上层");
    // 偏移阴影：圆心 (16,16) 处纯 #8E44AD
    assert_eq!(px(&buf, 30, 14, 14), [142, 68, 173, 255], "偏移 (6,6) 阴影");
    // 阴影圆外/区域外透明
    assert_eq!(px(&buf, 30, 3, 3), [0, 0, 0, 0]);
    assert_eq!(px(&buf, 30, 25, 25), [0, 0, 0, 0]);
}

/// 未知 filter id：宽容回退为无滤镜绘制（不吞元素），计 1 告警。
#[test]
fn unknown_filter_id_falls_back() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12">
  <rect x="2" y="2" width="8" height="8" fill="#00ff00" filter="url(#nope)"/>
</svg>"##;
    let doc = parse_document(svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    assert_eq!(doc.warnings, 1, "未知 filter 引用计告警");
    let dl = build_display_list(&doc, 12.0, 12.0);
    let viewport = vp(12, 12);
    let mut buf = alloc_zero(12, 12);
    render(&dl, &viewport, &mut buf);
    assert_eq!(px(&buf, 12, 6, 6), [0, 255, 0, 255], "回退为无滤镜原色");
    assert_eq!(px(&buf, 12, 1, 1), [0, 0, 0, 0]);
}

/// 空 primitive 链（<filter/> 无子元素）：原图直通（Chrome 同款语义）。
#[test]
fn empty_filter_chain_passthrough() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12">
  <defs><filter id="e"/></defs>
  <rect x="2" y="2" width="8" height="8" fill="#ff8800" filter="url(#e)"/>
</svg>"##;
    let buf = render_svg(svg, 12, 12);
    assert_eq!(px(&buf, 12, 6, 6), [255, 136, 0, 255], "空链直通原色");
    assert_eq!(px(&buf, 12, 1, 1), [0, 0, 0, 0]);
}
