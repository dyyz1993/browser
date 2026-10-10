//! 车道AQ 验收：feTurbulence / feDisplacementMap / feDiffuseLighting /
//! feSpecularLighting / feImage(dataURI) + Merge 工作域合成，端到端像素
//! 断言（SVG → parse → DisplayList → render，aa=1 像素中心采样）。
//!
//! 数值锚点来自 Chrome 探针（tests/oracle/chrome/probes/ltg-y|z、turb-*）
//! 反推的 Chrome/Skia 口径；涉及非 gamma 不变量的断言给 ±1 容差
//! （f32 trig/查表路径），gamma 不变端点（0/255）断精确。

use svgine_core::builder::{build_display_list, parse_document};
use svgine_core::raster::{render, PixFmt, Viewport};

fn vp(w: u32, h: u32) -> Viewport {
    Viewport {
        width: w,
        height: h,
        format: PixFmt::Rgba8,
        aa: 1,
    }
}

fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let o = (y * w + x) as usize * 4;
    [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
}

fn render_svg(svg: &str, w: u32, h: u32) -> Vec<u8> {
    let doc = parse_document(svg).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let dl = build_display_list(&doc, w as f32, h as f32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    render(&dl, &vp(w, h), &mut buf);
    buf
}

// ── feTurbulence ─────────────────────────────────────────────────────────

/// numOctaves=0 的 fractalNoise 折叠为常数 0.5 灰（工作域线性 → sRGB
/// 编码后统一灰值），alpha = 128（半透明，无 gamma）。
#[test]
fn turbulence_oct0_fractal_is_constant_half() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <filter id="t" filterUnits="userSpaceOnUse" x="0" y="0" width="8" height="8">
    <feTurbulence type="fractalNoise" baseFrequency="0.2" numOctaves="0"/>
  </filter>
  <rect width="8" height="8" filter="url(#t)"/>
</svg>"##;
    let buf = render_svg(svg, 8, 8);
    let first = px(&buf, 8, 0, 0);
    assert_eq!(first[3], 128, "oct0 fractal alpha = 128");
    assert_eq!(first[0], first[1], "灰：r==g");
    assert_eq!(first[1], first[2], "灰：g==b");
    for i in 0..64 {
        assert_eq!(px(&buf, 8, i % 8, i / 8), first, "常数场（像素 {i}）");
    }
}

/// 同 seed 确定性逐位复现；异 seed（大概率）噪声场不同。
#[test]
fn turbulence_seed_determinism() {
    let mk = |seed: &str| {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
  <filter id="t" filterUnits="userSpaceOnUse" x="0" y="0" width="16" height="16">
    <feTurbulence type="turbulence" baseFrequency="0.1" numOctaves="2" seed="{seed}"/>
  </filter>
  <rect width="16" height="16" filter="url(#t)"/>
</svg>"##
        )
    };
    let a1 = render_svg(&mk("7"), 16, 16);
    let a2 = render_svg(&mk("7"), 16, 16);
    assert_eq!(a1, a2, "同 seed 必须逐位一致");
    let b = render_svg(&mk("8"), 16, 16);
    assert_ne!(a1, b, "异 seed 噪声场应不同");
}

/// stitchTiles="stitch"：lattice 索引按瓦片像素宽回卷。y 轴频率用 1.0
/// （th·bfy = 1 → lo=hi 频率不受修正），隔离 x 轴效应：瓦片内采样不变、
/// +x 角格撞到瓦片边界（clx == sw）的像素回卷取值改变。
#[test]
fn turbulence_stitch_wraps_lattice_at_tile_edge() {
    let mk = |stitch: &str| {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <filter id="t" filterUnits="userSpaceOnUse" x="0" y="0" width="8" height="8">
    <feTurbulence type="fractalNoise" baseFrequency="0.25" numOctaves="1" seed="3" stitchTiles="{stitch}"/>
  </filter>
  <rect width="8" height="8" filter="url(#t)"/>
</svg>"##
        )
    };
    let s = render_svg(&mk("stitch"), 8, 8);
    let n = render_svg(&mk("noStitch"), 8, 8);
    // 瓦片宽 sw = round(8·0.25) = 2：x=6 → nx=1.75，flx=1=sw−1、
    // clx=2 ≥ sw 回卷 → stitch 与 noStitch 分叉
    assert_ne!(
        px(&s, 8, 6, 0),
        px(&n, 8, 6, 0),
        "x=6 的 +x 角格跨瓦片边界：stitch 应改变取值"
    );
    // 界内像素（flx=0、clx=1）两种模式完全一致
    assert_eq!(px(&s, 8, 0, 0), px(&n, 8, 0, 0), "瓦片内部不受 stitch 影响");
    assert_eq!(px(&s, 8, 1, 0), px(&n, 8, 1, 0), "瓦片内部不受 stitch 影响");
}

// ── feDisplacementMap ────────────────────────────────────────────────────

/// scale=0（缺省）：位移归零 → SourceGraphic 原样透传。
#[test]
fn displacement_zero_scale_is_identity() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
  <filter id="d" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
    <feDisplacementMap/>
  </filter>
  <rect x="5" y="5" width="10" height="10" fill="#ff0000" filter="url(#d)"/>
</svg>"##;
    let buf = render_svg(svg, 20, 20);
    assert_eq!(px(&buf, 20, 10, 10), [255, 0, 0, 255], "原样");
    assert_eq!(px(&buf, 20, 2, 2), [0, 0, 0, 0], "矩形外不变");
}

/// A 通道 map（SourceAlpha）：位移在**目标像素**读 map——矩形内 c=1
/// → 采样 in(x+5,y+5)，矩形外 c=0 → 采样 in(x−5,y−5)（scale=10、
/// nearest 取整）。半平面分裂正是 map 逐像素语义的钉子。
#[test]
fn displacement_alpha_channel_shifts() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="20">
  <filter id="d" filterUnits="userSpaceOnUse" x="0" y="0" width="30" height="20">
    <feDisplacementMap in="SourceGraphic" in2="SourceAlpha" scale="10" xChannelSelector="A" yChannelSelector="A"/>
  </filter>
  <rect x="10" y="5" width="10" height="10" fill="#ff0000" filter="url(#d)"/>
</svg>"##;
    let buf = render_svg(svg, 30, 20);
    // 目标在矩形内（c=1）：in(+5,+5)。 (12,7) → in(17,12) 矩形内红；
    // (17,12) → in(22,17) 出矩形透明。
    assert_eq!(px(&buf, 30, 12, 7), [255, 0, 0, 255], "c=1 → +5 采样红");
    assert_eq!(px(&buf, 30, 17, 12), [0, 0, 0, 0], "c=1 → +5 采样出界透明");
    // 目标在矩形外（c=0）：in(−5,−5)。(22,12) → in(17,7) 矩形内红；
    // (2,2) → in(−3,−3) 越界透明。
    assert_eq!(px(&buf, 30, 22, 12), [255, 0, 0, 255], "c=0 → −5 采样红");
    assert_eq!(px(&buf, 30, 2, 2), [0, 0, 0, 0], "c=0 → −5 采样越界透明");
}

// ── feDiffuseLighting ────────────────────────────────────────────────────

/// 平坦表面 + 仰角 90° 平行光：N=L=ẑ → 漫反射满值白、alpha=255（gamma
/// 不变端点，linear 域 1.0 编码后仍 255）。
#[test]
fn diffuse_flat_surface_top_light() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <filter id="l" filterUnits="userSpaceOnUse" x="0" y="0" width="8" height="8">
    <feDiffuseLighting surfaceScale="4" diffuseConstant="1" lighting-color="#ffffff">
      <feDistantLight azimuth="0" elevation="90"/>
    </feDiffuseLighting>
  </filter>
  <rect width="8" height="8" fill="#000000" filter="url(#l)"/>
</svg>"##;
    let buf = render_svg(svg, 8, 8);
    assert_eq!(px(&buf, 8, 4, 4), [255, 255, 255, 255], "N·L=1 满值白");
    assert_eq!(px(&buf, 8, 0, 0), [255, 255, 255, 255], "均匀照亮");
}

/// 平坦表面 + 水平光（仰角 0）：N·L=0 → 全黑不透明（漫反射无高光项）。
#[test]
fn diffuse_flat_surface_horizontal_light_is_black() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <filter id="l" filterUnits="userSpaceOnUse" x="0" y="0" width="8" height="8">
    <feDiffuseLighting surfaceScale="4" diffuseConstant="1" lighting-color="#ffffff">
      <feDistantLight azimuth="0" elevation="0"/>
    </feDiffuseLighting>
  </filter>
  <rect width="8" height="8" fill="#ffffff" filter="url(#l)"/>
</svg>"##;
    let buf = render_svg(svg, 8, 8);
    assert_eq!(px(&buf, 8, 4, 4), [0, 0, 0, 255], "N·L=0 全黑");
}

/// feSpotLight 锥外无光：光源 (20,20,20) 垂直照准 (20,20,0)，锥角 20°
/// → 正照准点满亮、z=0 面上距轴 18px 处（半角 ≈ 43°）锥外全黑（漫反
/// 射 alpha 恒 255）。
#[test]
fn diffuse_spotlight_cone_clips() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
  <filter id="l" filterUnits="userSpaceOnUse" x="0" y="0" width="40" height="40">
    <feDiffuseLighting surfaceScale="1" diffuseConstant="1" lighting-color="#ffffff">
      <feSpotLight x="20" y="20" z="20" pointsAtX="20" pointsAtY="20" pointsAtZ="0" specularExponent="1" limitingConeAngle="20"/>
    </feDiffuseLighting>
  </filter>
  <rect width="40" height="40" fill="#000000" filter="url(#l)"/>
</svg>"##;
    let buf = render_svg(svg, 40, 40);
    let off = px(&buf, 40, 20, 38);
    assert_eq!(off, [0, 0, 0, 255], "锥外（z=0 面上距轴 18px）无光");
    let on = px(&buf, 40, 20, 20);
    assert_eq!(on, [255, 255, 255, 255], "正照准点 N·L=att=1 满值白");
}

// ── feSpecularLighting ───────────────────────────────────────────────────

/// Chrome/Skia 数值口径（ltg-y 矩阵探针定律）：k 与字节量化后的光色前置
/// 相乘，alpha = coeff·lc（无 gamma），premul==alpha ⇒ 直色 = 白。
/// ss=6、ks=0.9、exp=12、lc=#bbbbbb、az=280/el=55 → a_byte = 65。
#[test]
fn specular_flat_matches_chrome_law() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12">
  <filter id="s" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
    <feSpecularLighting in="SourceAlpha" surfaceScale="6" specularConstant="0.9" specularExponent="12" lighting-color="#bbbbbb">
      <feDistantLight azimuth="280" elevation="55"/>
    </feSpecularLighting>
  </filter>
  <rect width="12" height="12" fill="#00ff00" filter="url(#s)"/>
</svg>"##;
    let buf = render_svg(svg, 12, 12);
    let p = px(&buf, 12, 6, 6);
    assert!(
        (63..=67).contains(&p[3]),
        "alpha 字节 = round(0.9·(N·H)^12·(127/255)·255) = 65（实测 {}）",
        p[3]
    );
    assert!(
        (254..=255).contains(&p[0]),
        "premul==alpha ⇒ 直色白（linear 域 encode(1.0) 舍入 254/255）"
    );
    assert_eq!(p[0], p[1], "三通道同");
    assert_eq!(p[1], p[2], "三通道同");
    // 平坦区域逐像素一致
    assert_eq!(px(&buf, 12, 1, 1), px(&buf, 12, 10, 10), "平坦场均匀");
}

/// 半值锚点：ks=0.5、lc=white、exp=1、az=0/el=0 → N·H = 1/√2，
/// alpha = round(0.5·0.70711·255) = 90；直色仍白。
#[test]
fn specular_half_reflectance_anchor() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <filter id="s" filterUnits="userSpaceOnUse" x="0" y="0" width="8" height="8">
    <feSpecularLighting in="SourceAlpha" surfaceScale="2" specularConstant="0.5" specularExponent="1" lighting-color="#ffffff">
      <feDistantLight azimuth="0" elevation="0"/>
    </feSpecularLighting>
  </filter>
  <rect width="8" height="8" fill="#000000" filter="url(#s)"/>
</svg>"##;
    let buf = render_svg(svg, 8, 8);
    let p = px(&buf, 8, 4, 4);
    assert_eq!(p[3], 90, "alpha = round(0.5/√2·255) = 90");
    assert_eq!(p[0], 255, "直色白");
}

// ── feImage ──────────────────────────────────────────────────────────────

/// data URI PNG（2x2 四色块）1:1 落位：解码像素原样呈现（nearest 同
/// 尺寸无重采样）。
#[test]
fn fe_image_data_uri_pixels() {
    const PNG_2X2: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEklEQVR4nGP4z8DwHwyBNBgAAEnICff5q7YNAAAAAElFTkSuQmCC";
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
  <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="16" height="16">
    <feImage href="data:image/png;base64,{PNG_2X2}" x="4" y="4" width="2" height="2"/>
  </filter>
  <rect width="16" height="16" filter="url(#f)"/>
</svg>"##
    );
    let buf = render_svg(&svg, 16, 16);
    assert_eq!(px(&buf, 16, 4, 4), [255, 0, 0, 255], "左上红");
    assert_eq!(px(&buf, 16, 5, 4), [0, 255, 0, 255], "右上绿");
    assert_eq!(px(&buf, 16, 4, 5), [0, 0, 255, 255], "左下蓝");
    assert_eq!(px(&buf, 16, 5, 5), [255, 255, 255, 255], "右下白");
    assert_eq!(px(&buf, 16, 0, 0), [0, 0, 0, 0], "区域外不受影响");
}

// ── feMerge 工作域合成 ───────────────────────────────────────────────────

/// 半透明红叠不透明灰（linearRGB 工作域）：out_r = encode(0.502·1 +
/// 0.498·s2l(128)) = 205；旧字节域合成给 192 —— 钉死线性域口径。
#[test]
fn merge_blends_in_working_domain() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
  <filter id="m" filterUnits="userSpaceOnUse" x="0" y="0" width="10" height="10">
    <feFlood flood-color="#ff0000" flood-opacity="0.5" result="r"/>
    <feFlood flood-color="#808080" result="g"/>
    <feMerge>
      <feMergeNode in="g"/>
      <feMergeNode in="r"/>
    </feMerge>
  </filter>
  <rect width="10" height="10" filter="url(#m)"/>
</svg>"##;
    let buf = render_svg(svg, 10, 10);
    let p = px(&buf, 10, 5, 5);
    assert_eq!(p[3], 255, "叠加后不透明");
    assert!(
        (204..=206).contains(&p[0]),
        "linear 域 over：r = encode(0.502 + 0.498·0.2158) ≈ 205（实测 {}）",
        p[0]
    );
    assert!(
        (91..=93).contains(&p[1]),
        "g = encode(0.498·0.2158) ≈ 92（实测 {}；字节域合成给 64）",
        p[1]
    );
    assert_eq!(p[2], p[1], "灰底 b==g");
}
