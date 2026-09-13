//! M96.2 冒烟：V8 桥管线（libtest harness 与 V8 静态初始化冲突，
//! 用 example 直跑——与 v8eval 同型）。
fn main() {
    // 桥走 arena DOM thread_local——先装解析树（含 html/body）。空树的
    // Tree::root panic 会跨 V8 C++ 回调 unwind = UB 挂死（M96.2 多轮
    // 调试的真凶；QuickJS 管线天然有解析树所以从未触发）。
    let tree = browser_html_parser::parse("<html><body></body></html>");
    let _guard = browser_js_runtime::bridge::install_current(tree);
    let mut e = browser_js_runtime::engine_v8::V8Engine::new().expect("init");
    assert!(e.install_core_bridges(), "bridges");
    let out = e.eval_string(
        "(function(){ var el = __createEl('div'); __setAttr(el,'id','v8t'); return typeof el+':'+el; })()",
    );
    println!("bridge: {:?}", out);
    // 解析树已有 document/html/body 节点——NodeId 为正数即成功
    let ok = out
        .as_deref()
        .and_then(|s| s.strip_prefix("number:"))
        .and_then(|n| n.parse::<i64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false);
    assert!(ok, "bridge should return positive NodeId, got {out:?}");
    let tz = e.eval_string("__sysTimezone()");
    println!("tz: {:?}", tz);
    assert_eq!(tz.as_deref(), Some("Asia/Shanghai"));
    println!("V8_BRIDGES_OK");
}
