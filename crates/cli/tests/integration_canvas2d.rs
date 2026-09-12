//! M94 阶段 1：canvas 2D 真像素渲染集成测试。
//!
//! 验收（docs/assessments/M94-canvas-fidelity-feasibility.md §四）：
//! - toDataURL 输出真实 PNG（非 1x1 常量）
//! - 同一序列两次运行字节级一致（canvasFingerprint 自一致性前提）
//! - getImageData 读回真实像素（fillRect #f60 → 255,102,0,255）
//! - VM 检测序列（fillText + arc + evenodd + multiply）完整执行不抛

use assert_cmd::Command;

#[test]
fn canvas2d_real_pixels_end_to_end() {
    let page = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/canvas/fingerprint_sequence.html"
    );
    let out = Command::cargo_bin("browser")
        .expect("browser binary")
        .args(["render-file"])
        .arg(page)
        .args(["--width", "500"])
        .output()
        .expect("run render-file");
    let text = String::from_utf8_lossy(&out.stdout);
    let start = text.find("E2E[").expect("marker start") + 4;
    let end = text[start..].find("]E2EEND").expect("marker end") + start;
    let marks = &text[start..end];

    assert!(
        marks.contains("png=true"),
        "toDataURL must be real PNG: {marks}"
    );
    assert!(
        marks.contains("big=true"),
        "PNG must be substantial: {marks}"
    );
    assert!(
        marks.contains("stable=true"),
        "two runs must be byte-identical: {marks}"
    );
    assert!(
        marks.contains("px=255,102,0,255"),
        "pixel readback: {marks}"
    );
    assert!(
        marks.contains("hole=0"),
        "evenodd hole must be empty: {marks}"
    );
    assert!(
        marks.contains("ring=255"),
        "outer ring must be opaque: {marks}"
    );
}
