fn main() {
    let tree = browser_html_parser::parse("<html><body></body></html>");
    let _guard = browser_js_runtime::bridge::install_current(tree);
    browser_js_runtime::bridge::install_navigation(browser_navigation::new_navigation(
        "about:blank",
    ));
    browser_js_runtime::bridge::install_storage(browser_storage::new_storage());
    let mut e = browser_js_runtime::engine_v8::V8Engine::new().expect("init");
    assert!(e.install_core_bridges());
    println!("hwCores: {:?}", e.eval_string("__hwCores()"));
    println!(
        "navigator created: {:?}",
        e.eval_string(
            "(function(){
        var segments = browser_js_runtime_shim_segments();
        return 'ok';
    })()"
        )
        .is_some()
    );
    // 装完 shim 再测 navigator.hardwareConcurrency
    let segs = browser_js_runtime::scripts::shim_segments_for_v8();
    for (_, js) in &segs {
        let _ = e.eval_install(js);
    }
    println!(
        "after shim hw: {:?}",
        e.eval_string("navigator.hardwareConcurrency")
    );
}
