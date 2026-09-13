//! M96.2 冒烟：V8 桥管线（libtest harness 与 V8 静态初始化冲突，
//! 用 example 直跑——与 v8eval 同型）。
fn main() {
    // 桥走 arena DOM thread_local——先装空树（QuickJS 管线同款前置）。
    // 此前挂起的根因：无树时 with_tree panic 在 V8 C++ 回调内 = UB 挂死。
    let _guard = browser_js_runtime::bridge::install_current(browser_dom::Tree::new());
    let mut e = browser_js_runtime::engine_v8::V8Engine::new().expect("init");
    assert!(e.install_core_bridges(), "bridges");
    let out = e.eval_string(
        "(function(){ var el = __createEl('div'); __setAttr(el,'id','v8t'); return typeof el+':'+el; })()",
    );
    println!("bridge: {:?}", out);
    assert_eq!(out.as_deref(), Some("number:1"));
    let tz = e.eval_string("__sysTimezone()");
    println!("tz: {:?}", tz);
    assert_eq!(tz.as_deref(), Some("Asia/Shanghai"));
    println!("V8_BRIDGES_OK");
}
