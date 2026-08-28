//! M79 回归测试：solidjs.com 渲染修复链。
//!
//! 真实站点视觉扫描发现 solidjs.com（Vite + solid-router，纯 CSR）整页空白，
//! 自愈循环定位出 4 个缺口（按触发顺序）：
//!
//! 1. `URLSearchParams` 缺 `forEach` —— solid-router `parsePath` 后调
//!    `searchParams.forEach(...)` 转对象 → TypeError "not a function" →
//!    module eval 中断。
//! 2. QuickJS 默认 JS 栈 1MB 太小 —— solid 初始化的深递归（嵌套 effect +
//!    错误传播）撞限 → "Maximum call stack size exceeded"（engine_quickjs
//!    new() 提到 2MB，此处用 module 求值验证深度可控）。
//! 3. `cloneNode(true)` 只克隆元素子节点 —— solid 模板克隆依赖注释/文本
//!    占位符保留，`z.firstChild.nextSibling` 链缺环 → TypeError。
//! 4. `createComment` 返回 div —— 注释占位符身份丢失。
//!
//! 测试模式同 integration_settimeout：累积变量 + 组合标记避免源码字面量
//! 假阳性。

use browser_html_parser::parse as parse_html;
use browser_js_runtime::{bridge::body_text_content, run_scripts_with_base};

fn run_body(html: &str) -> String {
    let tree = parse_html(html);
    let (shared, _) = run_scripts_with_base(tree, None);
    let borrowed = shared.borrow();
    body_text_content(&borrowed)
}

// ---------- 根因 1：URLSearchParams WHATWG 子集 ----------

#[test]
fn usp_for_each_iterates_all_pairs() {
    // solid-router: t.searchParams.forEach((v, k) => { o[k] = v })
    let body = run_body(
        r#"<html><body><p>init</p>
<script>
var out = '';
var sp = new URLSearchParams('?a=1&b=2&a=3');
sp.forEach(function(v, k) { out += k + '=' + v + ';'; });
__setBody('OUT:' + out);
</script>
</body></html>"#,
    );
    // 多值键按顺序逐对出现（get 只取首个，forEach 遍历全部）
    assert!(
        body.contains("OUT:a=1;b=2;a=3;"),
        "forEach must visit all pairs incl. duplicates. body={body:?}"
    );
}

#[test]
fn usp_get_get_all_has_set_delete_append() {
    let body = run_body(
        r#"<html><body><p>init</p>
<script>
var sp = new URLSearchParams('k=first&k=second&x=9');
var out = '';
out += sp.get('k') + '|';          // first（多值取首）
out += sp.getAll('k').join(',') + '|'; // first,second
out += sp.has('x') + '|';          // true
out += sp.has('nope') + '|';       // false
sp.append('k', 'third');           // 追加
out += sp.getAll('k').length + '|'; // 3
sp.set('k', 'only');               // 替换首个并移除其余
out += sp.getAll('k').join(',') + '|'; // only
sp.delete('x');
out += sp.has('x');                // false
__setBody('OUT:' + out);
</script>
</body></html>"#,
    );
    assert!(
        body.contains("OUT:first|first,second|true|false|3|only|false"),
        "URLSearchParams core methods must follow WHATWG semantics. body={body:?}"
    );
}

#[test]
fn usp_entries_keys_values_and_to_string() {
    let body = run_body(
        r#"<html><body><p>init</p>
<script>
var sp = new URLSearchParams('b=2&a=hello world');
var out = '';
for (var e of sp.entries()) { out += e[0] + ':' + e[1] + ';'; }
for (var k of sp.keys()) { out += '<' + k + '>'; }
sp.sort();
out += '|' + sp.toString();
__setBody('OUT:' + out);
</script>
</body></html>"#,
    );
    // '+' 解码为空格；序列化空格编码回 '+'；sort 后 a 在 b 前
    assert!(
        body.contains("OUT:b:2;a:hello world;<b><a>|a=hello+world&b=2"),
        "entries/keys/sort/toString must work. body={body:?}"
    );
}

// ---------- 根因 2：模块深递归不撞栈（快速深递归探针） ----------

#[test]
fn deep_recursion_within_raised_stack_limit() {
    // 实测：QuickJS 解释帧开销差异极大——release ~800B/帧（2MB ≈ 2500 层，
    // CLI render-file 探针），debug -O0 ~9KB/帧（2MB ≈ 230 帧，本测试环境，
    // 指数+二分探针 diag_stack_depth 实测）。取 150 层：2MB-debug（230）通过、
    // 1MB 默认-debug（~115）失败——防止上限被误改回默认。
    let body = run_body(
        r#"<html><body><p>init</p>
<script type="module">
function down(n) { return n <= 0 ? 0 : 1 + down(n - 1); }
var r = 'ERR';
try { r = String(down(150)); } catch (e) { r = 'RANGE'; }
__setBody('DEPTH:' + r);
</script>
</body></html>"#,
    );
    assert!(
        body.contains("DEPTH:150"),
        "150-deep recursion must succeed with raised stack limit. body={body:?}"
    );
}

// ---------- 根因 3+4：cloneNode 保留注释/文本占位符 + 真 Comment ----------

#[test]
fn clone_node_keeps_comment_and_text_chain() {
    // solid 编译模板形态：节点链 z.firstChild → .nextSibling → .nextSibling
    // 中间环是注释/文本占位符；cloneNode(true) 后链必须完整。
    let body = run_body(
        r#"<html><body><p>init</p>
<script>
var host = document.createElement('div');
host.innerHTML = '<ul><li>lead<!--#-->mid<span>tail</span></li></ul>';
var li = host.firstChild.firstChild;      // <li>
var clone = li.cloneNode(true);
var z = clone.firstChild;                 // text 'lead'
var k = z.nextSibling;                    // comment <!--#-->
var nt = k ? k.nextSibling : null;        // text 'mid'
var out = '';
out += (z.nodeType === 3 ? 'text' : '?') + '|';
out += (k && k.nodeType === 8 ? 'comment' : (k ? 'other' : 'null')) + '|';
out += (nt && nt.nodeType === 3 ? 'text' : (nt ? 'other' : 'null')) + '|';
out += (nt && nt.nextSibling && nt.nextSibling.tagName === 'SPAN' ? 'span' : 'null');
__setBody('OUT:' + out);
</script>
</body></html>"#,
    );
    assert!(
        body.contains("OUT:text|comment|text|span"),
        "cloneNode(true) must keep text/comment placeholders in node chain. body={body:?}"
    );
}

#[test]
fn create_comment_makes_real_comment_node() {
    // 旧版 createComment 返回 div——nodeType 必须 8，且注释数据不污染 textContent。
    let body = run_body(
        r#"<html><body><p>init</p>
<script>
var marker = document.createComment('#');
var holder = document.createElement('div');
holder.appendChild(marker);
var out = '';
out += marker.nodeType + '|';        // 8
out += holder.textContent.length;    // 注释不计入 textContent（0）
__setBody('OUT:' + out);
</script>
</body></html>"#,
    );
    assert!(
        body.contains("OUT:8|0"),
        "createComment must yield nodeType 8 without textContent pollution. body={body:?}"
    );
}

// ---------- import.meta.env（Vite 生产语义） ----------

#[test]
fn import_meta_env_production_defaults_in_module() {
    // 入口模块路径（scripts.rs preamble）注入完整 Vite 生产缺省：
    // MODE='production', DEV=false, PROD=true, SSR=false, BASE_URL='/'
    // 未定义键（VITE_*）读 undefined 不抛错。
    let body = run_body(
        r#"<html><body><p>init</p>
<script type="module">
var env = import.meta.env;
var out = env.MODE + ',' + env.DEV + ',' + env.PROD + ',' + env.SSR + ',' + env.BASE_URL;
out += ',' + (env.VITE_ANYTHING === undefined);
__setBody('ENV:' + out);
</script>
</body></html>"#,
    );
    assert!(
        body.contains("ENV:production,false,true,false,/,true"),
        "import.meta.env must carry Vite production defaults; VITE_* reads undefined. body={body:?}"
    );
}
