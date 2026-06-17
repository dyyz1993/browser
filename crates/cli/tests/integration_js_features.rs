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

/// 辅助：写临时 HTML 文件，返回路径。
fn write_tmp(html: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "m62_es6_{}.html",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, html).unwrap();
    path.to_str().unwrap().to_string()
}
