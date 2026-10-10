//! M0 验收 golden：SVG 字节 → parse → DisplayList → render 的端到端
//! 逐字节像素断言（M0 门禁，见 docs/ROADMAP.md）。

use svgine_core::builder::{build_display_list, parse_document};
use svgine_core::raster::{render, PixFmt, Viewport};

fn vp8() -> Viewport {
    Viewport {
        width: 8,
        height: 8,
        format: PixFmt::Rgba8,
        aa: 1,
    }
}

fn px(buf: &[u8], vp: &Viewport, x: u32, y: u32) -> [u8; 4] {
    let o = (y * vp.width + x) as usize * 4;
    [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
}

const SVG: &str = r##"<?xml version="1.0"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <!-- M0 golden -->
  <rect x="1" y="1" width="3" height="2" fill="#ff0000"/>
  <rect x="5" y="5" width="10" height="10"/>
  <rect x="0" y="0" width="2" height="2" fill="none"/>
  <rect x="0" y="5" width="2" height="2" fill="&#35;00ff00"/>
</svg>"##;

#[test]
fn m0_rect_golden_bytes() {
    let doc = parse_document(SVG).unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let dl = build_display_list(&doc, 8.0, 8.0);
    let vp = vp8();
    let mut buf = [0u8; 8 * 8 * 4];
    render(&dl, &vp, &mut buf);

    // 1) 红色 rect (1,1,3,2)：覆盖 x∈[1,3] y∈[1,2]
    for (x, y) in [(1, 1), (2, 1), (3, 1), (1, 2), (3, 2)] {
        assert_eq!(px(&buf, &vp, x, y), [255, 0, 0, 255], "at ({x},{y})");
    }
    // 边界外不受影响（4,2 透明）
    assert_eq!(px(&buf, &vp, 4, 2), [0, 0, 0, 0]);

    // 2) 默认黑 rect (5,5,10,10)：越界裁剪到 x∈[5,7] y∈[5,7]，alpha=255 区分于透明
    for x in 5..8 {
        for y in 5..8 {
            assert_eq!(px(&buf, &vp, x, y), [0, 0, 0, 255], "at ({x},{y})");
        }
    }
    assert_eq!(px(&buf, &vp, 4, 4), [0, 0, 0, 0]);

    // 3) fill="none"：不描绘，(0,0) 保持透明
    assert_eq!(px(&buf, &vp, 0, 0), [0, 0, 0, 0]);

    // 4) 属性实体解码：&#35;00ff00 == #00ff00，绿色 rect (0,5,2,2)
    for x in 0..2 {
        for y in 5..7 {
            assert_eq!(px(&buf, &vp, x, y), [0, 255, 0, 255], "at ({x},{y})");
        }
    }
}

/// 非正尺寸的 rect 按 SVG 规范不渲染（属性错误）。
#[test]
fn negative_size_rect_is_skipped() {
    let doc =
        parse_document(r##"<svg><rect x="1" y="1" width="-5" height="2" fill="#ffffff"/></svg>"##)
            .unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let dl = build_display_list(&doc, 8.0, 8.0);
    assert!(dl.cmds.is_empty());
}

/// 无 <svg> 包裹的片段同样可渲染（宽容挂到合成根）。
#[test]
fn fragment_renders() {
    let doc = parse_document(r##"<rect x="0" y="0" width="4" height="4" fill="#0000ff"/>"##)
        .unwrap_or_else(|e| panic!("parse failed: {e:?}"));
    let dl = build_display_list(&doc, 4.0, 4.0);
    let vp = Viewport {
        width: 4,
        height: 4,
        format: PixFmt::Rgba8,
        aa: 1,
    };
    let mut buf = [0u8; 4 * 4 * 4];
    render(&dl, &vp, &mut buf);
    assert!(buf.chunks(4).all(|p| p == [0, 0, 255, 255]));
}
