//! M62: ES6+ 语言特性覆盖测试。
//!
//! 验证 boa 0.21 引擎对现代 JS 语法的支持。每项一个最小用例。
//! 这是 JS-COVERAGE.md 矩阵的入库回归测试，防止引擎升级后退化。

use assert_cmd::Command;
use predicates::prelude::*;
fn bin() -> Command {
    Command::cargo_bin("browser").expect("browser binary not found")
}

#[test]
fn es6_let_const() {
    // 用 fixture 文件而非内联，因为 assert_cmd 的 stdout 断言需要这样。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
let a = 1; const b = 2;
document.getElementById('out').textContent = 'LET_CONST_OK ' + (a + b);
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("LET_CONST_OK 3"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_arrow_function() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var add = (x, y) => x + y;
document.getElementById('out').textContent = 'ARROW_OK ' + add(2, 3);
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ARROW_OK 5"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_template_string() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var name = 'World';
var s = `Hello ${name}!`;
document.getElementById('out').textContent = 'TEMPLATE_OK ' + s;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("TEMPLATE_OK Hello World!"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_destructuring() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var {x, y} = {x: 10, y: 20};
var [a, b] = [1, 2];
document.getElementById('out').textContent = 'DESTRUCTURE_OK ' + x + y + a + b;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DESTRUCTURE_OK 102012"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_class_syntax() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
class Animal {
  constructor(name) { this.name = name; }
  speak() { return this.name + ' speaks'; }
}
class Dog extends Animal {
  speak() { return this.name + ' barks'; }
}
var d = new Dog('Rex');
document.getElementById('out').textContent = 'CLASS_OK ' + d.speak();
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CLASS_OK Rex barks"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_symbol() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var sym = Symbol('test');
var obj = {};
obj[sym] = 'hidden';
document.getElementById('out').textContent = 'SYMBOL_OK ' + typeof sym + ' ' + obj[sym];
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SYMBOL_OK symbol hidden"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_map_set() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var m = new Map();
m.set('key', 'value');
var s = new Set([1, 2, 2, 3]);
document.getElementById('out').textContent = 'MAPSET_OK ' + m.get('key') + ' ' + s.size;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("MAPSET_OK value 3"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_proxy() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var p = new Proxy({base: 'real'}, {
  get: function(target, prop) {
    return prop in target ? target[prop] : 'intercepted_' + prop;
  }
});
document.getElementById('out').textContent = 'PROXY_OK ' + p.base + ' ' + p.missing;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "PROXY_OK real intercepted_missing",
        ));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es6_spread_forof() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var arr = [1, 2, 3];
var arr2 = [...arr, 4];
var sum = 0;
for (var v of arr2) { sum += v; }
document.getElementById('out').textContent = 'SPREAD_OK ' + arr2.length + ' sum=' + sum;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SPREAD_OK 4 sum=10"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es2017_async_await() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">PENDING</div>
<script>
async function fetchData() {
  await new Promise(function(resolve) { setTimeout(resolve, 20); });
  return 'ASYNC_RESULT';
}
fetchData().then(function(v) {
  document.getElementById('out').textContent = 'ASYNC_AWAIT_OK ' + v;
});
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ASYNC_AWAIT_OK ASYNC_RESULT"))
        // PENDING 占位应被替换
        .stdout(predicate::str::contains("PENDING").not());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es5_json_parse_stringify() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var o = JSON.parse('{"a":1,"b":"x","c":[3,4]}');
var s = JSON.stringify({n: 42, tag: "rust"});
document.getElementById('out').textContent = 'JSON_OK ' + o.a + o.b + o.c[1] + ' ' + (s.indexOf('42') >= 0);
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("JSON_OK 1x4 true"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es5_regexp() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var m = 'hello2026world'.match(/(\d+)/);
var rep = 'a-b-c'.replace(/-/g, '_');
var parts = '2026-06-17'.split('-');
document.getElementById('out').textContent = 'REGEXP_OK ' + m[1] + ' ' + rep + ' ' + parts.length;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("REGEXP_OK 2026 a_b_c 3"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_base64_atob_btoa() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var enc = btoa('hello');
var dec = atob('aGVsbG8=');
var roundtrip = atob(btoa('test123'));
document.getElementById('out').textContent = 'BASE64_OK ' + enc + ' ' + dec + ' ' + roundtrip;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("BASE64_OK aGVsbG8= hello test123"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_event_constructors() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var ev = new Event('click', { bubbles: true });
var cev = new CustomEvent('custom', { detail: { v: 99 } });
document.getElementById('out').textContent = 'EVENT_OK ' + ev.type + ' ' + ev.bubbles + ' ' + cev.detail.v;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EVENT_OK click true 99"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_dom_content_loaded_auto_dispatched() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
document.addEventListener('DOMContentLoaded', function() {
  document.getElementById('out').textContent = 'DCL_FIRED';
});
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DCL_FIRED"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_element_dispatch_custom_event() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<button id="btn">Click</button>
<script>
var btn = document.getElementById('btn');
btn.addEventListener('action', function(e) {
  document.getElementById('out').textContent = 'EVT_' + e.detail.amount;
});
btn.dispatchEvent(new CustomEvent('action', { detail: { amount: 100 } }));
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EVT_100"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_query_selector_all_returns_all() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<div class="item">A</div>
<div class="item">B</div>
<div class="item">C</div>
<script>
var items = document.querySelectorAll('.item');
document.getElementById('out').textContent = 'QSA_' + items.length;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("QSA_3"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_queue_microtask() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var order = ['sync'];
queueMicrotask(function() {
  order.push('micro');
  document.getElementById('out').textContent = order.join(',');
});
order.push('queued');
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sync,queued,micro"));
    let _ = std::fs::remove_file(&path);
}

/// 辅助：写临时 HTML 文件，返回路径。用计数器保证并发安全（不依赖纳秒时间戳）。
fn write_tmp(html: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("m62_es6_{pid}_{id}.html"));
    std::fs::write(&path, html).unwrap();
    path.to_str().unwrap().to_string()
}
