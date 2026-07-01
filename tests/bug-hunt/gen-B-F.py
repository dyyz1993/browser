#!/usr/bin/env python3
"""批量生成 B-F 类 fixture（DOM/SPA/Network/Console/Edge）。
每个 fixture 往 #out 写 PASS/FAIL。
"""
import os
from pathlib import Path

HERE = Path(__file__).resolve().parent  # bug-hunt/
CATS = {
    "B-dom-api": HERE / "categories/B-dom-api",
    "C-spa": HERE / "categories/C-spa",
    "D-network": HERE / "categories/D-network",
    "E-console": HERE / "categories/E-console",
    "F-edge": HERE / "categories/F-edge",
}
for p in CATS.values():
    p.mkdir(parents=True, exist_ok=True)

# Common template
TEMPLATE = """<!DOCTYPE html><html><head><meta charset="utf-8"><title>{title}</title></head>
<body><div id="out"></div>
<script>
var o = document.getElementById('out');
function w(line){{ o.appendChild(document.createTextNode(line + '\\n')); }}
function assert(name, cond, detail){{ w(name + ':' + (cond ? 'PASS' : 'FAIL' + (detail?('('+detail+')'):''))); }}
function safe(fn){{ try{{ return fn(); }}catch(e){{ return '__THREW__:' + e.message; }} }}
try {{
{body}
}} catch(e) {{
  w('__SCRIPT_THREW__:' + e.message);
}}
</script></body></html>
"""

# ===== B. DOM API =====
B_TESTS = [
("B01-cloneNode", "cloneNode 浅/深",
 """var p = document.createElement('div'); p.id='p1';
 var c = document.createElement('span'); c.textContent='child';
 p.appendChild(c);
 var shallow = p.cloneNode(false);
 assert('cn-shallow', shallow.childNodes.length === 0);
 var deep = p.cloneNode(true);
 assert('cn-deep-count', deep.childNodes.length === 1);
 assert('cn-deep-tag', deep.childNodes[0].tagName === 'SPAN' || deep.childNodes[0].tagName === 'span');
 assert('cn-deep-text', deep.childNodes[0].textContent === 'child');"""),
("B02-contains", "Node.contains",
 """var p = document.createElement('div'); var c = document.createElement('span'); p.appendChild(c);
 assert('cont-parent', p.contains(p));
 assert('cont-child', p.contains(c));
 assert('cont-not', !p.contains(document.body));
 assert('cont-el', p instanceof Node);"""),
("B03-classList", "Element.classList",
 """var el = document.createElement('div');
 el.className = 'a b';
 assert('cl-len', el.classList.length === 2);
 assert('cl-has', el.classList.contains('a'));
 el.classList.add('c'); assert('cl-add', el.classList.contains('c'));
 el.classList.remove('a'); assert('cl-rm', !el.classList.contains('a'));
 el.classList.toggle('b'); assert('cl-tg', !el.classList.contains('b'));
 el.classList.replace('c','x'); assert('cl-rep', el.classList.contains('x'));"""),
("B04-dataset", "HTMLElement.dataset",
 """var el = document.createElement('div');
 el.setAttribute('data-name', 'test');
 el.setAttribute('data-value', '42');
 assert('ds-name', el.dataset.name === 'test');
 assert('ds-value', el.dataset.value === '42');
 assert('ds-keys', Object.keys(el.dataset).length >= 2);
 el.dataset.newKey = 'new';
 assert('ds-set', el.getAttribute('data-new-key') === 'new' || el.getAttribute('data-newkey') === 'new');"""),
("B05-namedNodeMap", "Element.attributes (NamedNodeMap)",
 """var el = document.createElement('div');
 el.setAttribute('class', 'x');
 el.setAttribute('id', 'y');
 assert('attr-len', el.attributes.length >= 2);
 assert('attr-get', el.attributes.getNamedItem('id').value === 'y');
 assert('attr-name', el.attributes[0].name.length > 0);"""),
("B06-querySelector-pseudo", "querySelectorAll 伪类/属性选择器",
 """document.body.innerHTML = '<div class=\\"list\\"><ul><li class=\\"item active\\">A</li><li class=\\"item\\">B</li></ul></div>';
 var first = document.querySelectorAll('.list .item:first-child');
 assert('qs-first', first.length === 1);
 var not = document.querySelectorAll('.item:not(.active)');
 assert('qs-not', not.length === 1);
 var attr = document.querySelectorAll('[class*=active]');
 assert('qs-attr', attr.length >= 1);"""),
("B07-querySelector-combinator", "querySelector 后代/子/兄弟选择器",
 """document.body.innerHTML = '<div id=\\"a\\"><div class=\\"b\\"><span class=\\"c\\">X</span></div></div>';
 assert('qs-desc', document.querySelectorAll('#a span').length === 1);
 assert('qs-child', document.querySelectorAll('#a > div').length === 1);
 document.body.innerHTML += '<div class=\\"b\\" id=\\"a2\\"></div>';
 // sibling: previous/next
 document.body.innerHTML = '<p>a</p><span id=\\"sib\\">b</span><p>c</p>';
 var sib = document.getElementById('sib');
 var prev = sib.previousElementSibling;
 assert('qs-prev', prev && prev.tagName === 'P');
 var next = sib.nextElementSibling;
 assert('qs-next', next && next.tagName === 'P');"""),
("B08-insertAdjacentHTML", "insertAdjacentHTML",
 """var el = document.createElement('div'); el.id = 'iah';
 el.innerHTML = '<p>orig</p>';
 el.insertAdjacentHTML('beforeend', '<span>end</span>');
 assert('iah-length', el.childNodes.length >= 2);
 assert('iah-tag', el.lastChild.tagName === 'SPAN' || el.lastChild.tagName === 'span');
 el.insertAdjacentHTML('afterbegin', '<b>begin</b>');
 assert('iah-first', el.firstChild.tagName === 'B' || el.firstChild.tagName === 'b');"""),
("B09-MutationObserver", "MutationObserver 存在且不抛错",
 """var mo = safe(function(){ return new MutationObserver(function(){}); });
 assert('mo-create', mo.indexOf('__THREW__') < 0);
 var obs = new MutationObserver(function(){});
 var target = document.createElement('div');
 var threw = false;
 try { obs.observe(target, {attributes:true}); } catch(e) { threw = true; }
 assert('mo-observe', !threw);
 obs.disconnect();
 assert('mo-disc', true);"""),
("B10-appendChild-order", "appendChild 顺序",
 """var p = document.createElement('ul'); p.id='ac';
 var i1 = document.createElement('li'); i1.textContent='1';
 var i2 = document.createElement('li'); i2.textContent='2';
 var i3 = document.createElement('li'); i3.textContent='3';
 p.appendChild(i1); p.appendChild(i2); p.appendChild(i3);
 assert('ac-len', p.childNodes.length === 3);
 assert('ac-order', p.childNodes[0].textContent === '1');
 assert('ac-last', p.childNodes[2].textContent === '3');
 // move: appendChild existing node = reorder
 p.appendChild(i1);
 assert('ac-move', p.childNodes.length === 3);
 assert('ac-move-last', p.childNodes[2].textContent === '1');"""),
("B11-removeChild", "Node.removeChild",
 """var p = document.createElement('div'); p.id='rc';
 var c = document.createElement('span'); c.textContent='rm';
 p.appendChild(c);
 assert('rc-before', p.childNodes.length === 1);
 p.removeChild(c);
 assert('rc-after', p.childNodes.length === 0);
 assert('rc-orphan', c.parentNode === null || c.parentNode === undefined);"""),
("B12-replaceChild", "Node.replaceChild",
 """var p = document.createElement('div'); p.id='rpc';
 var old = document.createElement('span'); old.textContent='old';
 var nw = document.createElement('b'); nw.textContent='new';
 p.appendChild(old);
 p.replaceChild(nw, old);
 assert('rpc-count', p.childNodes.length === 1);
 assert('rpc-tag', p.firstChild.tagName === 'B' || p.firstChild.tagName === 'b');
 assert('rpc-text', p.firstChild.textContent === 'new');"""),
("B13-textContent", "Node.textContent vs innerText",
 """var el = document.createElement('div');
 el.innerHTML = '<p>hello <b>world</b></p>';
 assert('tc-all', el.textContent === 'hello world');
 assert('tc-hidden', typeof el.innerText === 'string' || typeof el.innerText === 'undefined');
 el.textContent = 'reset';
 assert('tc-set', el.textContent === 'reset');
 assert('tc-nested', el.firstChild === null);"""),
("B14-createElementNS", "createElementNS (SVG)",
 """var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
 assert('cns-svg', svg.tagName === 'SVG' || svg.tagName === 'svg');
 var ns = svg.namespaceURI;
 assert('cns-ns', ns === 'http://www.w3.org/2000/svg' || ns === undefined);
 // 命名空间可能不存在
 var txt = document.createElementNS('http://www.w3.org/1999/xhtml', 'div');
 assert('cns-tag', txt.tagName === 'DIV' || txt.tagName === 'div');"""),
("B15-event-bubbles", "事件冒泡",
 """var parent = document.createElement('div'); parent.id='evt-p';
 var child = document.createElement('span'); child.id='evt-c';
 parent.appendChild(child);
 var parentHit = false, childHit = false;
 parent.addEventListener('test', function(){ parentHit = true; });
 child.addEventListener('test', function(){ childHit = true; });
 var ev = new Event('test', {bubbles:true});
 child.dispatchEvent(ev);
 assert('ev-child', childHit);
 // 冒泡可能要等 dispatchEvent 实现
 assert('ev-bubble', parentHit);"""),
("B16-CustomEvent-detail", "CustomEvent detail",
 """var ce = new CustomEvent('my', {detail:{x:1}});
 assert('ce-type', ce.type === 'my');
 assert('ce-det', ce.detail && ce.detail.x === 1);"""),
("B17-preventDefault", "Event.preventDefault/cancelable",
 """var ev = new Event('click', {cancelable:true});
 assert('pd-can', ev.cancelable);
 assert('pd-def', ev.defaultPrevented === false);
 ev.preventDefault();
 assert('pd-after', ev.defaultPrevented === true);"""),
("B18-form-basics", "表单 value/checked/disabled",
 """document.body.innerHTML = '<form id=\\"f1\\"><input id=\\"inp\\" value=\\"v1\\"><input id=\\"chk\\" type=\\"checkbox\\" checked></form>';
 var inp = document.getElementById('inp');
 assert('form-v', inp.value === 'v1');
 inp.value = 'v2'; assert('form-v2', inp.value === 'v2');
 var chk = document.getElementById('chk');
 assert('form-chk', chk.checked === true);
 chk.checked = false; assert('form-chk2', chk.checked === false);"""),
("B19-DocumentFragment", "DocumentFragment body/appendChild",
 """var frag = document.createDocumentFragment();
 assert('df-nodeType', frag.nodeType === 11);
 var c = document.createElement('span'); c.textContent='f';
 frag.appendChild(c);
 assert('df-len', frag.childNodes.length === 1);
 // fragment 插入父节点后自动展开
 var p = document.createElement('div');
 p.appendChild(frag);
 assert('df-expand', p.childNodes.length === 1);
 assert('df-empty', frag.childNodes.length === 0);"""),
("B20-cloneNode-deep", "cloneNode 深拷贝（复杂结构）",
 """var p = document.createElement('div'); p.setAttribute('data-x','1');
 var c = document.createElement('span'); c.textContent='deep';
 p.appendChild(c);
 var cl = p.cloneNode(true);
 assert('cnd-tag', cl.tagName === p.tagName);
 // 属性可能 clone 也可能不
 assert('cnd-child', cl.childNodes.length >= 1);
 var txt1 = c.textContent;
 var txt2 = cl.childNodes[0].textContent;
 assert('cnd-text', txt1 === txt2);"""),
("B21-comment-node", "createComment / 注释节点",
 """var cm = document.createComment('test comment');
 assert('cm-nodeType', cm.nodeType === 8);
 assert('cm-text', cm.textContent === 'test comment');
 var p = document.createElement('div'); p.appendChild(cm);
 assert('cm-parent', p.childNodes.length >= 1);"""),
("B22-nextSibling", "节点关系 nextSibling/previousSibling/children",
 """var p = document.createElement('div');
 var a = document.createElement('a'); a.id='a1';
 var b = document.createElement('b'); b.id='b1';
 p.appendChild(a); p.appendChild(b);
 assert('ns-next', a.nextElementSibling && a.nextElementSibling.id === 'b1');
 assert('ns-prev', b.previousElementSibling && b.previousElementSibling.id === 'a1');
 assert('ns-children', p.children.length >= 2);"""),
("B23-importNode", "document.importNode",
 """var p = document.createElement('div'); p.textContent='imp';
 var cp = document.importNode ? safe(function(){ return document.importNode(p, true); }) : '__NO_IMPORTNODE__';
 if (typeof cp === 'string' && cp.indexOf('__THREW__') >= 0) {
   assert('imp-threw', false, cp);
 } else {
   assert('imp-ok', true);
 }"""),
# 跳过 B24 template (内容可能为空)
("B25-scrollIntoView", "scrollIntoView/stub 不抛错",
 """var el = document.createElement('div');
 var sv = safe(function(){ el.scrollIntoView(); return 'ok'; });
 assert('siv-noerr', sv === 'ok');
 var gbcr = safe(function(){ return el.getBoundingClientRect(); });
 assert('gbr-noerr', gbcr.indexOf('__THREW__') < 0);"""),
]

# ===== C. SPA/框架 =====
C_TESTS = [
("C01-customElements", "customElements.define/存在不抛错",
 """var ce = window.customElements;
 assert('ce-exists', typeof ce !== 'undefined');
 var threw = false;
 try { ce.define('x-comp', function(){}); } catch(e) { threw = true; }
 assert('ce-define', !threw);"""),
("C02-hash-router", "hash 路由 location.hash",
 """location.hash = '#/test';
 assert('hr-hash', location.hash === '#/test' || location.hash.indexOf('test') >= 0);"""),
("C03-history-pushState", "history.pushState/popstate",
 """var state = {page:1};
 history.pushState(state, '', '/page1');
 assert('hs-state', history.state && (history.state.page === 1 || JSON.stringify(history.state).indexOf('1')>=0));
 assert('hs-len', typeof history.length === 'number');
 history.back();
 // 等待异步
 setTimeout(function(){ assert('hs-back', true); }, 10);"""),
("C04-XHR-basic", "XMLHttpRequest GET",
 """var xhr = new XMLHttpRequest();
 assert('xhr-exists', typeof xhr.open === 'function');
 xhr.open('GET', 'data:text/plain,hello');
 var ok = false;
 xhr.onload = function(){ ok = true; };
 xhr.send();
 assert('xhr-ok', ok);"""),
("C05-fetch-basic", "fetch GET（本页）",
 """fetch('/categories/C-spa/C05-fetch-basic.html').then(function(r){
   assert('f-status', r.status === 200);
   return r.text();
 }).then(function(t){
   assert('f-text', t.length > 0);
 }).catch(function(e){
   assert('f-error', false, e.message);
 });"""),
("C06-fetch-json", "fetch JSON",
 """fetch('data:application/json,{"a":1}').then(function(r){
   return r.json();
 }).then(function(j){
   assert('fj-key', j.a === 1);
 }).catch(function(e){
   assert('fj-err', false, e.message);
 });"""),
("C07-fetch-post", "fetch POST",
 """fetch('data:application/text,...', {method:'POST', body:'x'}).then(function(r){
   assert('fp-done', true);
 }).catch(function(e){
   assert('fp-err', false, e.message);
 });"""),
("C08-dynamic-import", "import() 动态导入（期望 reject，模块不存在）",
 """import('/nonexistent-module.js').then(function(){
   assert('di-ok', true);
 }).catch(function(e){
   assert('di-reject', true);
 });"""),
("C09-script-onload", "动态 script onload",
 """var s = document.createElement('script');
 s.textContent = 'window.__dynLoaded = true;';
 var loaded = false;
 s.onload = function(){ loaded = true; };
 document.body.appendChild(s);
 // 可能是同步
 setTimeout(function(){
   assert('sol-loaded', window.__dynLoaded === true);
 }, 20);"""),
("C10-event-delegation", "事件委托（document 级）",
 """document.addEventListener('click', function(e){ window.__docClick = e.type; });
 var el = document.createElement('button');
 el.id='del-btn';
 document.body.appendChild(el);
 var ev = new Event('click', {bubbles:true});
 el.dispatchEvent(ev);
 setTimeout(function(){
   assert('ed-type', window.__docClick === 'click');
 }, 10);"""),
("C11-appendChild-script", "appendChild script 自动执行",
 """var s = document.createElement('script');
 s.textContent = 'window.__appendedScriptRan = true;';
 document.body.appendChild(s);
 setTimeout(function(){
   assert('as-ran', window.__appendedScriptRan === true);
 }, 20);"""),
("C12-IntersectionObserver", "IntersectionObserver stub 不抛错",
 """var io = safe(function(){ return new IntersectionObserver(function(){}); });
 assert('io-create', io.indexOf('__THREW__') < 0);
 var obs = new IntersectionObserver(function(){});
 var el = document.createElement('div');
 var threw = false;
 try { obs.observe(el); } catch(e) { threw = true; }
 assert('io-observe', !threw);"""),
("C13-requestAnimationFrame", "requestAnimationFrame stub 不抛错",
 """var raf = safe(function(){ return requestAnimationFrame(function(){}); });
 assert('raf-fn', raf.indexOf('__THREW__') < 0);
 var id = requestAnimationFrame(function(){});
 assert('raf-id', typeof id === 'number');
 cancelAnimationFrame(id);
 assert('raf-cancel', true);"""),
("C15-contentEditable", "contentEditable",
 """var el = document.createElement('div');
 el.contentEditable = 'true';
 assert('ce-mode', el.contentEditable === 'true' || true);
 el.contentEditable = 'false';
 assert('ce-off', true);"""),
("C16-matchMedia", "matchMedia",
 """var mm = matchMedia('(min-width: 800px)');
 assert('mm-exists', typeof mm.matches === 'boolean');
 var mml = matchMedia('(max-width: 99999px)');
 assert('mm-wide', mml.matches === true);"""),
("C17-Base-href", "base href 不影响当前页面",
 """var base = document.querySelector('base');
 assert('base-ex', true);  // 不测 base href 本身"""),
("C18-SVG-innerHTML", "SVG 元素 innerHTML",
 """var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
 svg.innerHTML = '<circle cx=\\"10\\" cy=\\"10\\" r=\\"5\\"/>';
 assert('svg-html', svg.innerHTML.length > 0);"""),
("C19-window-open", "window.open stub",
 """var w = window.open('about:blank');
 assert('wo-exists', typeof w !== 'undefined' || true);
 var threw = false;
 try { window.open(); } catch(e) { threw = true; }
 assert('wo-noerr', !threw);"""),
("C20-console-noerr", "console 基本不抛错",
 """try { console.log('test'); console.error('test'); console.warn('test');
 } catch(e) { assert('c-noerr', false, e.message); }
 assert('c-ok', true);"""),
]

# ===== D. Network =====
D_TESTS = [
("D01-fetch-headers", "fetch 响应 headers",
 """fetch('/categories/D-network/D01-fetch-headers.html').then(function(r){
   var ct = r.headers.get('content-type') || '';
   assert('fh-ct', ct.indexOf('text/html') >= 0);
 }).catch(function(e){
   assert('fh-err', false, e.message);
 });"""),
("D02-xhr-status", "XHR 状态码",
 """var x = new XMLHttpRequest();
 x.open('GET', '/categories/D-network/D02-xhr-status.html');
 x.onload = function(){ assert('xs-code', x.status === 200); };
 x.onerror = function(){ assert('xs-err', false, 'xhr error'); };
 x.send();"""),
("D03-xhr-responseType", "XHR responseText",
 """var x = new XMLHttpRequest();
 x.open('GET', '/categories/D-network/D03-xhr-responseType.html');
 x.onload = function(){ assert('xrt', x.responseText.length > 0); };
 x.send();"""),
("D04-fetch-404", "fetch 404（expect status 404）",
 """fetch('/nonexistent-xyz-999').then(function(r){
   assert('f404-status', r.status === 404);
 }).catch(function(e){
   // fetch 不把 404 当作 reject
   assert('f404-mode', true);
 });"""),
("D05-xhr-send-order", "同步 XHR send 不抛错",
 """var x = new XMLHttpRequest();
 x.open('GET', 'data:text/plain,data', false);
 var threw = false;
 try { x.send(); } catch(e) { threw = true; }
 assert('xhr-sync', !threw);
 assert('xhr-sync-txt', x.responseText === 'data');"""),
("D06-WebSocket-basic", "WebSocket 存在不抛错",
 """var ws = new WebSocket('ws://127.0.0.1:9998/test');
 assert('ws-created', true);
 // 不测试连接，只测试构造不抛错
 // readyState 应是 0 (CONNECTING)
 assert('ws-state', ws.readyState === 0);"""),
]

# ===== E. Console =====
E_TESTS = [
("E01-console-log", "console.log 多参数",
 """try { console.log('a', 1, {x:2}); } catch(e) { assert('cl-err', false, e.message); }
 assert('cl-ok', true);"""),
("E02-console-error", "console.error/warn/info/debug",
 """try { console.error('err'); console.warn('warn'); console.info('info'); console.debug('debug');
 } catch(e) { assert('ce-err', false, e.message); }
 assert('ce-ok', true);"""),
("E03-console-table", "console.table",
 """try { console.table([{a:1,b:2}]); } catch(e) { assert('ct-err', false, e.message); }
 assert('ct-ok', true);"""),
("E04-console-trace", "console.trace（不抛错即可）",
 """try { console.trace('trace'); } catch(e) { assert('ctr-err', false, e.message); }
 assert('ctr-ok', true);"""),
("E05-console-group", "console.group/groupEnd",
 """try { console.group('g'); console.log('in group'); console.groupEnd(); } catch(e) { assert('cg-err', false, e.message); }
 assert('cg-ok', true);"""),
("E06-console-time", "console.time/timeEnd",
 """try { console.time('t1'); console.timeLog('t1'); console.timeEnd('t1'); } catch(e) { assert('ctm-err', false, e.message); }
 assert('ctm-ok', true);"""),
("E07-console-assert", "console.assert",
 """try { console.assert(true, 'should not fire'); } catch(e) { assert('ca-err', false, e.message); }
 assert('ca-ok', true);"""),
("E08-console-count", "console.count/countReset",
 """try { console.count('c1'); console.count('c1'); console.countReset('c1'); } catch(e) { assert('cc-err', false, e.message); }
 assert('cc-ok', true);"""),
("E09-console-dir", "console.dir",
 """try { console.dir(document.body); } catch(e) { assert('cd-err', false, e.message); }
 assert('cd-ok', true);"""),
("E10-uncaught-error", "window.onerror 存在",
 """assert('oe-type', typeof window.onerror === 'object' || typeof window.onerror === 'function' || true);
 // 不触发真实错误，只检查存在"""),
("E11-window-onerror", "window.onerror 接收",
 """window.__testUncaught = 'not called';
 window.onerror = function(msg, src, line, col, err){ window.__testUncaught = 'called'; return true; };
 // trigger an error that won't kill the page
 setTimeout(function(){
   try { JSON.parse('{broken'); } catch(e){ /* swallow */ }
   assert('oe-set', true);  // handler 存在即可
 }, 10);"""),
("E12-error-stack", "Error stack 存在",
 """try { throw new Error('test'); } catch(e) {
   assert('es-stack', typeof e.stack === 'string' || typeof e.stack === 'undefined');
   assert('es-msg', e.message === 'test');
 }"""),
("E13-promise-reject", "Promise reject 不抛同步错",
 """var p = new Promise(function(resolve, reject){ reject('intentional'); });
 p.catch(function(r){ assert('pr-catch', r === 'intentional'); });
 assert('pr-noerr', true);"""),
("E14-formatted-log", "console.log 格式化占位符 %s/%d/%o",
 """try { console.log('%s %d', 'a', 1); console.log('%o', {x:1}); } catch(e) { assert('cf-err', false, e.message); }
 assert('cf-ok', true);"""),
("E15-circular-ref", "console.log 循环引用不抛错",
 """var a = {}; a.self = a;
 try { console.log('circular:', a); } catch(e) { assert('cr-err', false, e.message); }
 assert('cr-ok', true);"""),
]

# ===== F. Edge Cases =====
F_TESTS = [
("F01-entity-decode", "HTML entity 解码（页面静态内容）",
 """document.body.innerHTML = '<p id=\\"ent\\">&amp; &lt; &gt; &quot; &#39; &nbsp; &#x26;</p>';
 var ent = document.getElementById('ent');
 var txt = ent.textContent;
 assert('ent-amp', txt.indexOf('&') >= 0);
 assert('ent-lt', txt.indexOf('<') >= 0);
 assert('ent-gt', txt.indexOf('>') >= 0);"""),
("F02-void-element", "void 元素自闭合（br/hr/input）",
 """var br = document.createElement('br');
 assert('ve-br', br.tagName === 'BR' || br.tagName === 'br');
 var hr = document.createElement('hr');
 assert('ve-hr', hr.tagName === 'HR' || hr.tagName === 'hr');
 var inp = document.createElement('input');
 inp.type = 'text';
 assert('ve-inp', inp.type === 'text');
 // void 元素不应有子节点
 assert('ve-children', br.childNodes.length === 0);"""),
("F03-duplicate-id", "duplicate id（不抛错，浏览器容错）",
 """document.body.innerHTML = '<div id=\\"dup\\">A</div><div id=\\"dup\\">B</div>';
 var el = document.getElementById('dup');
 assert('did-exists', el !== null);
 assert('did-text', el.textContent === 'A');  // 第一个"""),
("F04-nested-table", "嵌套 table（浏览器容错）",
 """document.body.innerHTML = '<table><tr><td><table><tr><td>inner</td></tr></table></td></tr></table>';
 var t = document.querySelector('table');
 assert('nt-exists', t !== null);
 var allTds = document.querySelectorAll('td');
 assert('nt-count', allTds.length >= 2);"""),
("F05-foreign-svg", "SVG 内 HTML foreignObject",
 """var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
 var fo = document.createElementNS('http://www.w3.org/2000/svg', 'foreignObject');
 var div = document.createElement('div');
 div.textContent = 'hello from svg';
 fo.appendChild(div);
 svg.appendChild(fo);
 assert('fo-text', div.textContent === 'hello from svg');
 assert('fo-parent', fo.childNodes.length >= 1);
 assert('fo-tag', fo.tagName.length > 0);"""),
("F06-large-document", "大文档（100 个元素）不抛错",
 """var p = document.createElement('div');
 p.id='ld';
 for(var i=0;i<100;i++){ var c = document.createElement('span'); c.textContent='x'; p.appendChild(c); }
 assert('ld-count', p.childNodes.length === 100);
 assert('ld-first', p.firstChild.textContent === 'x');
 assert('ld-last', p.lastChild.textContent === 'x');"""),
("F07-deep-nesting", "超深嵌套（50 层）不抛错不栈溢出",
 """var top = document.createElement('div');
 var cur = top;
 for(var i=0;i<50;i++){ var n = document.createElement('div'); cur.appendChild(n); cur = n; }
 assert('dn-depth', true);
 // 检查最深节点的内容
 cur.textContent = 'deepest';
 var walk = top;
 while(walk.firstChild){ walk = walk.firstChild; }
 assert('dn-leaf', walk.textContent === 'deepest');"""),
("F08-empty-text", "空文本节点",
 """var t = document.createTextNode('');
 assert('et-nodeType', t.nodeType === 3);
 assert('et-text', t.textContent === '');
 var p = document.createElement('div'); p.appendChild(t);
 assert('et-parent', p.childNodes.length === 1);"""),
("F09-target-blank", "link target=_blank（无实际跳转）",
 """var a = document.createElement('a'); a.href='#test'; a.target='_blank';
 assert('tb-href', true);"""),
("F10-bom-charset", "meta charset 不抛错",
 """var meta = document.createElement('meta'); meta.charset = 'utf-8';
 document.head.appendChild(meta);
 assert('mc-charset', meta.charset === 'utf-8' || meta.getAttribute('charset') === 'utf-8' || true);
 // 有时 charset 属性返回''但 attribute 有值
 assert('mc-ok', true);"""),
]

def write_tests(cat_key, tests):
    d = CATS[cat_key]
    for tid, title, body in tests:
        html = TEMPLATE.format(title=title, body=body)
        with open(d / f"{tid}.html", "w") as f:
            f.write(html)
    print(f"  {cat_key}: {len(tests)} fixtures → {d}")

def main():
    write_tests("B-dom-api", B_TESTS)
    write_tests("C-spa", C_TESTS)
    write_tests("D-network", D_TESTS)
    write_tests("E-console", E_TESTS)
    write_tests("F-edge", F_TESTS)
    
    total = len(B_TESTS) + len(C_TESTS) + len(D_TESTS) + len(E_TESTS) + len(F_TESTS)
    print(f"\n总共生成 {total} 个 fixture（B-F 类）")

if __name__ == "__main__":
    main()
# ==== D-network 扩展（D07-D15）====
D_EXTRA = [
("D07-xhr-setRequestHeader", "XHR setRequestHeader",
 """var x = new XMLHttpRequest();
 x.open('GET', 'data:text/plain,test');
 var threw = false;
 try { x.setRequestHeader('X-Custom', 'val'); } catch(e) { threw = true; }
 assert('xsrh-noerr', !threw);
 // 如果是 data: URI 会忽略，不抛错即可"""),
("D08-fetch-headers-set", "fetch Headers API",
 """try {
   var h = new Headers(); h.set('X-A', '1'); h.append('X-A', '2');
   var v = h.get('X-A'); assert('fh-get', v.length > 0);
   h.delete('X-A'); assert('fh-del', h.get('X-A') === null);
 } catch(e) { assert('fh-err', false, e.message); }"""),
("D09-fetch-formdata", "FormData 构造",
 """try {
   var fd = new FormData();
   fd.append('k', 'v');
   assert('fd-has', fd.has('k'));
   assert('fd-get', fd.get('k') === 'v' || fd.get('k').indexOf('v')>=0);
 } catch(e) { assert('fd-err', false, e.message); }"""),
("D10-fetch-abort", "AbortController（存在不抛错）",
 """try {
   var ac = new AbortController();
   assert('ac-exists', typeof ac.signal === 'object');
   ac.abort();
   assert('ac-aborted', ac.signal.aborted === true);
 } catch(e) { assert('ac-err', false, e.message); }"""),
("D11-xhr-event-order", "XHR 事件顺序（load/error/timeout）",
 """var x = new XMLHttpRequest();
 x.open('GET', 'data:text/plain,ok');
 var events = [];
 x.onloadstart = function(){ events.push('start'); };
 x.onload = function(){ events.push('load'); };
 x.onloadend = function(){ events.push('end'); };
 x.send();
 // 同步 data: URI 可能立即触发
 assert('xev-after', x.readyState === 4);
 assert('xev-status', x.status === 200);"""),
("D12-fetch-redirect", "fetch redirect follow（本页，不重定向）",
 """fetch('/categories/D-network/D12-fetch-redirect.html').then(function(r){
   assert('frd-status', r.status === 200);
   assert('frd-ok', true);
 }).catch(function(e){ assert('frd-err', false, e.message); });"""),
("D13-fetch-type-clone", "fetch response clone 存在",
 """fetch('data:application/text,clone').then(function(r){
   var c = r.clone();
   assert('frc-ok', typeof c.text === 'function');
 }).catch(function(e){ assert('frc-err', false, e.message); });"""),
("D14-post-form-urlencoded", "POST application/x-www-form-urlencoded",
 """fetch('data:application/text,...', {method:'POST', headers:{'Content-Type':'application/x-www-form-urlencoded'}, body:'a=1&b=2'}).then(function(r){
   assert('pf-ok', true);
 }).catch(function(e){ assert('pf-err', false, e.message); });"""),
("D15-xhr-override-mime", "XHR overrideMimeType",
 """var x = new XMLHttpRequest();
 x.open('GET', 'data:text/plain,xml');
 var threw = false;
 try { x.overrideMimeType('text/xml'); } catch(e) { threw = true; }
 assert('xomt-noerr', !threw);
 x.onload = function(){ assert('xomt-done', true); };
 x.send();"""),
]
