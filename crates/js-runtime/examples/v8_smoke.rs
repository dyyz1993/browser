//! M96.3 冒烟：全量桥 + shim globals 段接入。
fn main() {
    let tree = browser_html_parser::parse("<html><body></body></html>");
    let _guard = browser_js_runtime::bridge::install_current(tree);
    // navigation 桥（location_href 需要——shim globals 段构造期就调）
    browser_js_runtime::bridge::install_navigation(browser_navigation::new_navigation(
        "about:blank",
    ));
    // storage 桥（localStorage 等）
    browser_js_runtime::bridge::install_storage(browser_storage::new_storage());
    let mut e = browser_js_runtime::engine_v8::V8Engine::new().expect("init");
    assert!(e.install_core_bridges(), "bridges");

    // 接入 shim（get_all_shim_js 的全部段）
    let segments = browser_js_runtime::scripts::shim_segments_for_v8();
    let mut installed = 0;
    for (name, js) in &segments {
        eprintln!("installing segment: {} ({} bytes)", name, js.len());
        if e.eval_install(js) {
            installed += 1;
        } else {
            eprintln!("SHIM SEGMENT FAILED: {}", name);
        }
    }
    eprintln!("shim installed {}/{} segments", installed, segments.len());

    // shim 装好后验证：window/document/navigator 存在
    let w = e.eval_string("typeof window + '/' + typeof document + '/' + typeof navigator");
    println!("globals: {:?}", w);

    // DOM 操作经 shim（document.createElement）
    let d = e.eval_string(
        "(function(){ try { var el = document.createElement('div'); el.id = 'shim-test'; el.textContent = 'hello-shim'; return el.textContent + '/' + el.id; } catch(e) { return 'ERR:' + e.message; } })()",
    );
    println!("shim dom: {:?}", d);
    println!("V8_SHIM_SMOKE_DONE");
}
