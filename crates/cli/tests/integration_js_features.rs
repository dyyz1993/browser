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

#[test]
fn es6_generators() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
function* gen() { yield 1; yield 2; yield 3; }
var sum = 0;
for (var v of gen()) { sum += v; }
document.getElementById('out').textContent = 'GEN_OK ' + sum;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("GEN_OK 6"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es2020_optional_chaining_nullish() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var obj = { a: { b: 42 } };
var val = obj?.a?.b ?? 0;
var x = null;
var y = x ?? 'fallback';
document.getElementById('out').textContent = 'CHAIN_OK ' + val + ' ' + y;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CHAIN_OK 42 fallback"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es2020_logical_assignment() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var a = false; a ||= true;
var b = true; b &&= false;
var c = null; c ??= 'set';
document.getElementById('out').textContent = 'LOGIC_OK ' + a + ' ' + b + ' ' + c;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("LOGIC_OK true false set"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es2020_bigint() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var big = 9007199254740993n;
var sum = big + 1n;
document.getElementById('out').textContent = 'BIGINT_OK ' + typeof big + ' ' + (sum > big);
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("BIGINT_OK bigint true"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es2021_weakref() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var obj = { data: 42 };
var wr = new WeakRef(obj);
var derefed = wr.deref();
document.getElementById('out').textContent = 'WEAKREF_OK ' + derefed.data;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("WEAKREF_OK 42"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn es2021_numeric_separators() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var n = 1_000_000;
var hex = 0xFF_FF;
document.getElementById('out').textContent = 'NUMSEP_OK ' + n + ' ' + hex;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NUMSEP_OK 1000000 65535"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_url_and_search_params() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var u = new URL('https://x.com:8080/a/b?c=1&d=2#hash');
var sp = new URLSearchParams('x=10&y=20');
document.getElementById('out').textContent = 'URL_OK ' + u.host + ' ' + u.pathname + ' ' + sp.get('y');
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("URL_OK x.com:8080 /a/b 20"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_structured_clone() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var orig = { items: [1, 2, 3], nested: { val: 42 } };
var copy = structuredClone(orig);
copy.items.push(4);
document.getElementById('out').textContent = 'CLONE_OK ' + orig.items.length + ' ' + copy.items.length + ' ' + copy.nested.val;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CLONE_OK 3 4 42"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_text_encoder_decoder() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var te = new TextEncoder();
var enc = te.encode('hello');
var td = new TextDecoder();
var dec = td.decode(enc);
document.getElementById('out').textContent = 'TEXTENC_OK ' + enc.length + ' ' + dec;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("TEXTENC_OK 5 hello"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_headers_formdata_blob() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var h = new Headers({ 'Content-Type': 'application/json' });
h.append('X-Test', '1');
var fd = new FormData();
fd.append('name', 'alice');
var blob = new Blob(['data'], { type: 'text/plain' });
document.getElementById('out').textContent = 'HFB_OK ' + h.get('content-type') + ' ' + fd.get('name') + ' ' + blob.size;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HFB_OK application/json alice 4"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_framework_dom_checks() {
    // React/Vue 框架 DOM 元素验证的核心属性。
    let html = r#"<!DOCTYPE html><html><body>
<div id="root"></div>
<div id="out">FAIL</div>
<script>
var el = document.getElementById('root');
var frag = document.createDocumentFragment();
var comment = document.createComment('anchor');
var r = [];
r.push('nodeType:' + el.nodeType);
r.push('ELEMENT_NODE:' + Node.ELEMENT_NODE);
r.push('isElement:' + (el.nodeType === Node.ELEMENT_NODE));
r.push('fragment:' + (typeof frag.appendChild));
r.push('comment:' + (typeof comment.tagName !== 'undefined'));
document.getElementById('out').textContent = r.join(',');
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nodeType:1"))
        .stdout(predicate::str::contains("isElement:true"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_mutation_observer_and_match_media() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var mo = new MutationObserver(function() {});
mo.observe(document.body, { childList: true });
mo.disconnect();
var mql = window.matchMedia('(min-width: 800px)');
document.getElementById('out').textContent = 'OK ' + mql.media + ' ' + mo.__observing;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK (min-width: 800px)"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_class_list_real_implementation() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<div id="t" class="foo bar"></div>
<script>
var el = document.getElementById('t');
var r = [];
r.push('contains_foo:' + el.classList.contains('foo'));
r.push('contains_missing:' + el.classList.contains('missing'));
el.classList.add('baz');
r.push('after_add:' + el.classList.contains('baz'));
el.classList.remove('foo');
r.push('after_remove:' + el.classList.contains('foo'));
document.getElementById('out').textContent = r.join(',');
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("contains_foo:true"))
        .stdout(predicate::str::contains("after_add:true"))
        .stdout(predicate::str::contains("after_remove:false"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_dataset_dynamic_read() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<div id="el" data-id="42" data-url="/api/x"></div>
<script>
var el = document.getElementById('el');
var r = [];
r.push('id:' + el.dataset.id);
r.push('url:' + el.dataset.url);
el.dataset.value = 'set';
r.push('set_value:' + el.dataset.value);
document.getElementById('out').textContent = r.join(',');
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("id:42"))
        .stdout(predicate::str::contains("url:/api/x"))
        .stdout(predicate::str::contains("set_value:set"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_get_elements_by_tag_name_all() {
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<div>a</div><div>b</div><div>c</div>
<script>
var divs = document.getElementsByTagName('div');
document.getElementById('out').textContent = 'COUNT_' + divs.length;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("COUNT_4"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_element_query_selector() {
    // M62: Element.prototype.querySelector（Vue/React createElement 后查找子元素）
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var el = document.createElement('div');
el.innerHTML = '<span class="x">hello</span><p>world</p>';
var span = el.querySelector('span');
document.getElementById('out').textContent = 'QS_' + (span ? span.textContent : 'null');
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("QS_hello"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_create_element_ns() {
    // M62: document.createElementNS（Vue/React SVG/MathML 元素创建）
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
document.getElementById('out').textContent = 'NS_' + svg.tagName;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NS_SVG"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_escape_unescape() {
    // M62: escape/unescape（deprecated 但 builder.io 等第三方依赖）
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var e = escape('<test>&');
document.getElementById('out').textContent = 'ESC_' + e;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ESC_%3Ctest%3E%26"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_text_encoder_stream() {
    // M62: TextEncoderStream/TextDecoderStream（Stream API，builder.io）
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var te = new TextEncoderStream();
document.getElementById('out').textContent = 'TES_' + te.encoding;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("TES_utf-8"));
    let _ = std::fs::remove_file(&path);
}

// ──────────────────────────────────────────────────────────────────────────
// M63: 真实站点报错驱动的回归测试（bark/svelte/nextjs 自愈循环）
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn web_api_element_append_node() {
    // M63: Element.prototype.append（svelte.dev inline script: document.body.append(div)）
    // 报错「not a callable function」。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var div = document.createElement('div');
div.id = 'appended';
document.body.append(div);
var found = document.getElementById('appended');
document.getElementById('out').textContent = found ? 'APPEND_OK' : 'APPEND_FAIL';
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("APPEND_OK"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_element_append_string() {
    // M63: append 接受字符串参数（自动转文本节点）。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var div = document.createElement('div');
div.id = 'txt';
document.body.append(div);
document.getElementById('txt').append('hello', ' ', 'world');
document.getElementById('out').textContent = 'APPEND_STR_' + document.getElementById('txt').textContent;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("APPEND_STR_hello world"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_url_with_location_base() {
    // M63: new URL(".", location)（svelte.dev SvelteKit bootstrap）
    // 报错「cannot convert null/undefined to object」（URL.href getter 无限递归）。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var u = new URL(".", "https://example.com/path/");
document.getElementById('out').textContent = 'URL_' + u.protocol + '|' + u.pathname;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("URL_https:|/path/."));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_anchor_href_reflected() {
    // M63: a.href 反射属性（docsify sidebar sort: b.href.length - a.href.length）
    // 报错「cannot convert null/undefined to object in sort」。
    let html = r##"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<nav id="nav">
<a href="#/short">S</a>
<a href="#/longer-path">L</a>
</nav>
<script>
var nav = document.getElementById('nav');
var links = [].slice.call(nav.querySelectorAll('a'));
// docsify 模式：按 href 长度排序（降序）
links.sort(function(a, b) { return b.href.length - a.href.length; });
document.getElementById('out').textContent = 'HREF_' + links[0].href + '|' + links[1].href;
</script></body></html>"##;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("HREF_#/longer-path|#/short"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_get_bounding_client_rect() {
    // M63: Element.prototype.getBoundingClientRect（docsify K() scroll handler）
    // 报错「not a callable function」。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<div id="target">x</div>
<script>
var rect = document.getElementById('target').getBoundingClientRect();
var ok = typeof rect === 'object' && typeof rect.height === 'number' && typeof rect.top === 'number';
document.getElementById('out').textContent = ok ? 'RECT_OK' : 'RECT_FAIL';
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("RECT_OK"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_xhr_load_listener_this_binding() {
    // M63: XHR addEventListener('load', cb) 回调内 this 应为 XHR 实例（docsify）
    // 修复前 cb 内 this.status === undefined。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
var xhr = new XMLHttpRequest();
xhr.addEventListener('load', function(ev) {
  // this 应绑定到 xhr（规范），能读 status
  var ok = (this === xhr) && (typeof this.status === 'number');
  document.getElementById('out').textContent = ok ? 'XHR_THIS_OK' : 'XHR_THIS_FAIL_' + typeof this;
});
xhr.open('GET', '/nonexistent');
xhr.send();
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("XHR_THIS_OK"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn web_api_null_event_listener_ignored() {
    // M64: Vue/React 用 addEventListener('test', null, {get passive(){...}})
    // 检测 passive 事件支持。null listener 应静默忽略，不抛 "cannot convert null to object"。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
try {
  // Vue passive 检测模式：null listener + 带 getter 的 options
  var detected = false;
  var opts = { get passive() { detected = true; return false; } };
  window.addEventListener('testPassive', null, opts);
  window.removeEventListener('testPassive', null, opts);
  // Element 上也一样
  document.body.addEventListener('testEl', null);
  document.getElementById('out').textContent = 'NULL_LISTENER_OK';
} catch(e) {
  document.getElementById('out').textContent = 'NULL_LISTENER_ERR_' + e.message;
}
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NULL_LISTENER_OK"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn quickjs_bare_global_assignment_cross_script() {
    // M71.4 回归测试：SvelteKit/Nuxt 等框架用裸全局赋值（`__x = {}`，无 var/window.）
    // 在多个 <script> 间共享 hydration 数据。rquickjs 默认 strict 模式会抛
    // ReferenceError 中断整段 script。修复（eval_user_script 用 strict:false）后
    // 裸赋值应自动创建 globalThis 属性，并能跨 script 读取。
    let html = r#"<!DOCTYPE html><html><body>
<div id="out">FAIL</div>
<script>
  // 模拟 SvelteKit hydration 数据注入（裸全局赋值）
  __sveltekit_test = { val: 42, routes: ['/', '/about'] };
</script>
<script>
  // 模拟 SvelteKit 入口读取 hydration 数据
  var data = window.__sveltekit_test;
  var bare = (typeof __sveltekit_test !== 'undefined');
  document.getElementById('out').textContent =
    'BARE_GLOBAL_' + (data && data.val) + '_' + bare;
</script></body></html>"#;
    let path = write_tmp(html);
    bin()
        .args(["render-script", &path, "--width", "120"])
        .assert()
        .success()
        .stdout(predicate::str::contains("BARE_GLOBAL_42_true"));
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
