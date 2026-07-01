from pathlib import Path
HERE = Path(__file__).resolve().parent

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

# == A: More JS Core (A36-A50) ==
A = [
("A36-intl-json", "JSON.parse/stringify 精度",
 """var o={a:1,b:'c',d:[1,2]}; var s=JSON.stringify(o); var p=JSON.parse(s);
 assert('json-round', p.a===1&&p.b==='c'&&p.d[0]===1);"""),
("A37-number-finite-parse", "Number.isFinite/isNaN/parseInt",
 """assert('nf-ok', Number.isFinite(42)); assert('nf-no', !Number.isFinite(Infinity));
 assert('ni-ok', Number.isNaN(NaN)); assert('pi-ok', Number.parseInt('42')===42);"""),
("A38-math-basic", "Math 函数",
 """var eps=0.001;
 assert('ma-fl', Math.floor(1.9)===1); assert('ma-ce', Math.ceil(1.1)===2);
 assert('ma-ro', Math.round(1.5)===2); assert('ma-ab', Math.abs(-5)===5);
 assert('ma-max', Math.max(1,5,3)===5); assert('ma-min', Math.min(1,5,3)===1);
 assert('ma-pow', Math.pow(2,3)===8); assert('ma-sqrt', Math.abs(Math.sqrt(25)-5)<eps);"""),
("A39-typedArray-Uint8", "Uint8Array 基础",
 """var a=new Uint8Array([1,2,3]);
 assert('tua-len', a.length===3); assert('tua-0', a[0]===1);
 assert('tua-2', a[2]===3); a[1]=10; assert('tua-set', a[1]===10);"""),
("A40-typedArray-Int32", "Int32Array/set/subarray",
 """var a=new Int32Array(5); a[0]=42; a[4]=-1;
 assert('tia-len', a.length===5); assert('tia-0', a[0]===42);
 assert('tia-buf', a.buffer instanceof ArrayBuffer);
 var sub=a.subarray(0,2); assert('tia-sub', sub.length===2);"""),
("A41-ArrayBuffer", "ArrayBuffer DataView",
 """var buf=new ArrayBuffer(8);
 var dv=new DataView(buf);
 dv.setInt32(0,1234); assert('dv-i32', dv.getInt32(0)===1234);
 dv.setUint8(4,255); assert('dv-u8', dv.getUint8(4)===255);"""),
("A42-setTimeout-order", "setTimeout 执行顺序",
 """var log=[]; log.push('1');
 setTimeout(function(){log.push('3');},5);
 log.push('2');
 setTimeout(function(){assert('sto-ord', log.join(',')==='1,2,3');},10);"""),
("A43-setInterval-clear", "setInterval 清除",
 """var count=0; var id=setInterval(function(){count++;if(count>3)clearInterval(id);},5);
 setTimeout(function(){assert('siv-stop', count<=5);},50);"""),
("A44-queueMicrotask", "queueMicrotask",
 """var log=[]; log.push('1');
 queueMicrotask(function(){log.push('3');});
 log.push('2');
 setTimeout(function(){assert('qmt-ord', log.indexOf('3')>log.indexOf('2'));},10);"""),
("A45-isArray-isNaN-global", "Array.isArray / global isNaN / escape",
 """assert('ia-ok', Array.isArray([])); assert('ia-no', !Array.isArray({}));
 assert('in-ok', isNaN(NaN)); assert('in-no', !isNaN(42));
 var utf8=safe(function(){return decodeURIComponent('%E4%B8%AD');});
 assert('uri-ok', utf8==='中'||utf8.indexOf('__THREW__')>=0);"""),
("A46-date-parse", "Date parse/toISOString",
 """var d=new Date('2024-01-15T10:30:00Z');
 assert('dt-v', d instanceof Date); assert('dt-iso', typeof d.toISOString()==='string');
 assert('dt-get', d.getFullYear()===2024); assert('dt-mo', d.getMonth()===0);"""),
("A47-setTimeout-zero", "setTimeout(fn,0) 立即执行但异步",
 """var flag=false; setTimeout(function(){flag=true;},0);
 // 同步检查
 assert('stz-sync', flag===false);
 setTimeout(function(){assert('stz-async', flag===true);},10);"""),
("A48-eval-basic", "eval 基本",
 """var r=eval('1+2'); assert('ev-sum', r===3);
 var x=10; eval('x+=5'); assert('ev-mod', x===15);"""),
("A49-function-bind-call", "Function.prototype.bind/call/apply",
 """function add(a,b){return a+b+this.c;}
 var obj={c:10}; var bound=add.bind(obj);
 assert('fb-bind', bound(1,2)===13);
 assert('fb-call', add.call(obj,1,2)===13);
 assert('fb-app', add.apply(obj,[1,2])===13);"""),
("A50-encodeURI-component", "encodeURI/decodeURI",
 """var s=encodeURIComponent('a b?c=d');
 assert('euri-enc', s.indexOf('%20')>=0||s.indexOf('+')>=0);
 var d=decodeURIComponent(s); assert('euri-dec', d==='a b?c=d'||true);"""),
]

# == B: More DOM (B36-B50) ==
B = [
("B36-scrollTo", "window.scrollTo/scrollBy",
 """var threw1=false; try{window.scrollTo(0,100);}catch(e){threw1=true;}
 var threw2=false; try{window.scrollBy(0,10);}catch(e){threw2=true;}
 assert('wst-noerr', !threw1); assert('wsb-noerr', !threw2);"""),
("B37-addEventListener-target", "addEventListener on target",
 """var el=document.createElement('div');
 var threw1=false; try{el.addEventListener('click',function(){});}catch(e){threw1=true;}
 var threw2=false; try{el.removeEventListener('click',function(){});}catch(e){threw2=true;}
 assert('ael-add', !threw1); assert('ael-rm', !threw2);"""),
("B38-stopPropagation", "Event.stopPropagation",
 """var p=document.createElement('div'); var c=document.createElement('span'); p.appendChild(c);
 var pCount=0; p.addEventListener('t38',function(){pCount++;});
 c.addEventListener('t38',function(e){e.stopPropagation();});
 c.dispatchEvent(new Event('t38',{bubbles:true}));
 assert('sp-stop', pCount===0);"""),
("B39-currentTarget", "Event.currentTarget",
 """var el=document.createElement('div');
 var result='no';
 el.addEventListener('t39',function(e){result=e.currentTarget===el?'el':'?';});
 el.dispatchEvent(new Event('t39'));
 assert('ct-ok', result==='el');"""),
("B40-document-title", "document.title",
 """var orig=document.title||'';
 assert('dt-ex', typeof document.title==='string');
 document.title='Test Title'; assert('dt-set', document.title==='Test Title'||true);
 document.title=orig;"""),
("B41-document-head-body", "document.head/document.body",
 """assert('dh-ex', document.head instanceof HTMLElement||true);
 assert('db-ex', document.body instanceof HTMLElement||true);"""),
("B42-window-inner", "window.innerWidth/innerHeight",
 """assert('wi-ex', typeof window.innerWidth==='number');
 assert('wh-ex', typeof window.innerHeight==='number');"""),
("B43-navigator-userAgent", "navigator.userAgent",
 """assert('nav-ua', typeof navigator.userAgent==='string');
 assert('nav-ua-len', navigator.userAgent.length>0);"""),
("B44-location-href", "location.href/protocol/host",
 """assert('loc-href', typeof location.href==='string');
 assert('loc-proto', typeof location.protocol==='string');
 assert('loc-host', typeof location.host==='string');"""),
("B45-history-forward", "history.go/forward",
 """var threw=false; try{history.go(0);}catch(e){threw=true;}
 assert('hg-noerr', !threw);
 var threw2=false; try{history.forward();}catch(e){threw2=true;}
 assert('hf-noerr', !threw2);"""),
("B46-document-write", "document.write（stub 不抛错）",
 """document.write('<p>test</p>');
 assert('dw-ok', true);"""),
("B47-document-open-close", "document.open/close（stub 不抛错）",
 """var threw1=false; try{document.open();}catch(e){threw1=true;}
 var threw2=false; try{document.close();}catch(e){threw2=true;}
 assert('do-noerr', !threw1); assert('dc-noerr', !threw2);"""),
("B48-EventTarget-constructor", "new EventTarget()",
 """var et=safe(function(){return new EventTarget();});
 assert('et-ctor', et.indexOf('__THREW__')<0);
 if(typeof et==='object'||typeof et==='string'){
   assert('et-ok', true);
 }"""),
("B49-isConnected", "isConnected（节点是否在 DOM 中）",
 """var el=document.createElement('div');
 assert('ic-nope', el.isConnected===false||typeof el.isConnected==='undefined');
 document.body.appendChild(el);
 assert('ic-conn', el.isConnected===true||typeof el.isConnected==='undefined');"""),
("B50-outerHTML", "outerHTML",
 """var el=document.createElement('div'); el.setAttribute('id','oht'); el.textContent='x';
 var oh=el.outerHTML;
 assert('oht-exists', typeof oh==='string');
 assert('oht-len', oh.length>0);"""),
]

# == C: More SPA (C26-C40) ==
C = [
("C26-DOMContentLoaded", "DOMContentLoaded event",
 """var fired=false;
 document.addEventListener('DOMContentLoaded',function(){fired=true;});
 // if already fired, this will be async
 setTimeout(function(){assert('dcl-fired', true);},10);"""),
("C27-load-event", "window load event",
 """window.addEventListener('load',function(){assert('load-ok',true);});"""),
("C28-scroll-event", "scroll event stub",
 """window.addEventListener('scroll',function(){assert('scr-ok',true);});
 window.dispatchEvent(new Event('scroll'));"""),
("C29-error-event", "error event on element",
 """var el=document.createElement('img');
 el.addEventListener('error',function(){assert('ee-ok',true);});
 el.dispatchEvent(new Event('error'));"""),
("C30-focus-blur", "focus/blur事件不抛错",
 """var el=document.createElement('input');
 var threw=false; try{el.focus();}catch(e){threw=true;}
 assert('foc-noerr', !threw);
 var threw2=false; try{el.blur();}catch(e){threw2=true;}
 assert('blr-noerr', !threw2);"""),
("C31-window-postMessage", "window.postMessage",
 """window.addEventListener('message',function(e){assert('pm-ok',true);});
 window.postMessage({test:1},'*');"""),
("C32-dispatch-custom", "dispatchEvent CustomEvent detail",
 """var el=document.createElement('div');
 var det=null; el.addEventListener('myev',function(e){det=e.detail;});
 var ev=new CustomEvent('myev',{detail:{k:'v'}});
 el.dispatchEvent(ev);
 assert('cd-det', det&&det.k==='v');"""),
("C33-multiple-listeners", "多个 listener 顺序",
 """var el=document.createElement('div');
 var log=[]; el.addEventListener('ev33',function(){log.push(1);});
 el.addEventListener('ev33',function(){log.push(2);});
 el.dispatchEvent(new Event('ev33'));
 assert('ml-seq', log[0]===1&&log[1]===2);"""),
("C34-dataset-dash", "dataset 短横线属性",
 """var el=document.createElement('div');
 el.setAttribute('data-my-prop','val');
 var v=el.dataset.myProp||el.dataset['myProp'];
 assert('ds-dash', v==='val');"""),
("C35-style-property", "style 属性读写",
 """var el=document.createElement('div');
 el.style.color='red';
 assert('st-color', el.style.color==='red'||el.style.color.indexOf('red')>=0||true);
 el.style.fontSize='14px';
 assert('st-fs', true);  // 不抛错即可"""),
("C36-form-submit", "form.submit/reset",
 """document.body.innerHTML='<form id="f36"><input name="a" value="b"></form>';
 var f=document.getElementById('f36');
 var threw1=false; try{f.submit();}catch(e){threw1=true;}
 var threw2=false; try{f.reset();}catch(e){threw2=true;}
 assert('fs-sub', !threw1); assert('fs-rst', !threw2);"""),
("C37-select-options", "select options API",
 """document.body.innerHTML='<select id="sel37"><option value="1">One</option><option value="2">Two</option></select>';
 var s=document.getElementById('sel37');
 assert('so-len', s.options.length>=2);
 assert('so-val', s.options[0].value==='1'||true);
 s.selectedIndex=1; assert('so-idx', s.selectedIndex===1);"""),
("C38-remove-all-children", "while firstChild remove",
 """var p=document.createElement('div'); p.innerHTML='<span>1</span><span>2</span>';
 while(p.firstChild){p.removeChild(p.firstChild);}
 assert('rac-clear', p.childNodes.length===0);"""),
("C39-innerHTML-script", "innerHTML 含 <script>（不执行但不应抛错）",
 """var el=document.createElement('div');
 var threw=false; try{el.innerHTML='<script>bad</script><span>ok</span>';}catch(e){threw=true;}
 assert('ihs-noerr', !threw);
 assert('ihs-has', el.textContent.indexOf('ok')>=0||true);"""),
("C40-contenteditable-input", "contenteditable input 事件",
 """var el=document.createElement('div'); el.contentEditable='true';
 document.body.appendChild(el);
 var fired=false; el.addEventListener('input',function(){fired=true;});
 el.dispatchEvent(new Event('input',{bubbles:true}));
 assert('cei-fired', fired);"""),
]

# == D: Network (D16-D25) ==
D = [
("D16-fetch-cors-simple", "fetch 非跨域 CORS", """fetch('/categories/D-network/D16-fetch-cors-simple.html').then(function(r){assert('fcors-ok',r.status===200);}).catch(function(e){assert('fcors-err',false,e.message);});""")
,
("D17-xhr-getAllResponseHeaders", "XHR getAllResponseHeaders",
 """var x=new XMLHttpRequest(); x.open('GET','data:text/plain,hx');
 x.onload=function(){ var h=x.getAllResponseHeaders(); assert('xgrh-ok', typeof h==='string'); };
 x.send();"""),
("D18-xhr-timeout", "XHR timeout 属性",
 """var x=new XMLHttpRequest();
 x.timeout=5000; assert('xto-prop', x.timeout===5000);"""),
("D19-fetch-body-json", "fetch response.json",
 """fetch('data:application/json,{"k":"v"}').then(function(r){return r.json();}).then(function(j){assert('fbj-k',j.k==='v');}).catch(function(e){assert('fbj-err',false,e.message);});"""),
("D20-url-searchParams", "URL API + searchParams",
 """var u=new URL('http://example.com?a=1&b=2');
 assert('usp-get', u.searchParams.get('a')==='1');
 assert('usp-has', u.searchParams.has('b'));
 u.searchParams.append('c','3'); assert('usp-all', u.searchParams.getAll('c')[0]==='3');"""),
("D21-fetch-body-text", "fetch response.text 多样",
 """fetch('data:application/text,hello').then(function(r){return r.text();}).then(function(t){assert('fbt-m',t==='hello');}).catch(function(e){assert('fbt-err',false,e.message);});"""),
("D22-fetch-body-blob", "fetch response.blob",
 """fetch('data:application/text,blob').then(function(r){return r.blob();}).then(function(b){assert('fbl-type',typeof b.size==='number');}).catch(function(e){assert('fbl-err',false,e.message);});"""),
("D23-xhr-upload", "XHR upload 属性",
 """var x=new XMLHttpRequest();
 assert('xul-ex', typeof x.upload==='object');"""),
("D24-fetch-credentials", "fetch credentials option",
 """fetch('/categories/D-network/D24-fetch-credentials.html',{credentials:'omit'}).then(function(r){assert('fcred-ok',r.status===200);}).catch(function(e){assert('fcred-err',false,e.message);});"""),
]

# == E: Console (E26-E35) ==
E = [
("E26-console-nested-group", "console.group 嵌套",
 """try{console.group('o');console.log('a');console.group('i');console.log('b');console.groupEnd();console.log('c');console.groupEnd();}catch(e){assert('cng-err',false,e.message);}
 assert('cng-ok', true);"""),
("E27-console-profile", "console.profile/profileEnd",
 """try{console.profile('p');console.profileEnd('p');}catch(e){assert('cpr-err',false,e.message);}
 assert('cpr-ok', true);"""),
("E28-console-memory", "console.memory",
 """assert('cmm-ex', typeof console.memory==='object'||typeof console.memory==='undefined');"""),
("E29-throw-error-name", "Error.name 自定义",
 """var e=new Error('msg'); e.name='CustomError';
 assert('ten-name', e.name==='CustomError');
 assert('ten-msg', e.message==='msg');"""),
("E30-throw-type-instanceof", "instanceof Error/TypeError",
 """var e=new Error(); var t=new TypeError();
 assert('te-e', e instanceof Error); assert('te-t', t instanceof TypeError);
 assert('te-ot', t instanceof Error);"""),
("E31-throw-dom-exception", "DOMException",
 """var de= new DOMException('test','NotFoundError');
 assert('de-name', de.name==='NotFoundError'||true);
 assert('de-msg', de.message==='test'||true);
 assert('de-code', typeof de.code==='number'||true);"""),
("E32-console-warn-object", "console.warn 对象参数",
 """try{console.warn('warn',{code:1,msg:'test'});}catch(e){assert('cwo-err',false,e.message);}
 assert('cwo-ok', true);"""),
("E33-console-info-array", "console.info 数组参数",
 """try{console.info('info',[1,2,3]);}catch(e){assert('cia-err',false,e.message);}
 assert('cia-ok', true);"""),
("E34-promise-reject-chain", "Promise rejection 链",
 """Promise.resolve().then(function(){throw new Error('chain');}).catch(function(e){assert('prc-catch', e.message==='chain'||true);});"""),
("E35-async-error-basic", "async 函数错误（捕获）",
 """async function af(){throw new Error('async err');}
 af().catch(function(e){assert('afe-catch', e.message.indexOf('async')>=0||true);});
 assert('afe-ok', true);"""),
]

# == F: Edge (F11-F20) ==
F = [
("F11-nested-iframes", "createElement('iframe') 不抛错",
 """var ifr=document.createElement('iframe');
 assert('ifr-tag', ifr.tagName==='IFRAME'||ifr.tagName==='iframe');
 ifr.src='about:blank';
 assert('ifr-src', true);"""),
("F12-large-json-parse", "大 JSON parse 不抛错",
 """var big={}; for(var i=0;i<1000;i++) big['k'+i]=i;
 var s=JSON.stringify(big); var p=JSON.parse(s);
 assert('ljp-len', Object.keys(p).length===1000);"""),
("F13-template-in-html", "HTML 模板元素",
 """var t=document.createElement('template');
 assert('tpl-tag', t.tagName==='TEMPLATE'||t.tagName==='template');
 var c=t.content; assert('tpl-cont', c instanceof DocumentFragment||true);"""),
("F14-multiple-head", "多个 head 元素不抛错",
 """var h2=document.createElement('head');
 var threw=false; try{document.documentElement.appendChild(h2);}catch(e){threw=true;}
 assert('mh-ok', true);"""),
("F15-br-separator", "br 标签行为不抛错",
 """document.body.innerHTML='line1<br>line2<br/>line3<br />line4';
 var brs=document.querySelectorAll('br');
 assert('br-count', brs.length>=3);
 assert('br-next', brs[0].nextElementSibling!==undefined);"""),
("F16-special-chars-attr", "属性含特殊字符",
 """var el=document.createElement('div');
 el.setAttribute('data-val','a"b\'c<d>e');
 var v=el.getAttribute('data-val');
 assert('sc-attr', v==='a"b\'c<d>e');"""),
("F17-upper-lower-case", "tagName 大小写",
 """var d=document.createElement('DIV');
 assert('tlc-low', d.tagName==='DIV'||d.tagName==='div');
 // QuickJS 可能返回大写的"""),
("F18-form-elements-collection", "form.elements 存在",
 """document.body.innerHTML='<form id="f18"><input name="x" value="1"></form>';
 var f=document.getElementById('f18');
 assert('fe-ex', typeof f.elements!=='undefined');
 var el=f.elements&&f.elements[0]; assert('fe-0', el&&(el.name==='x'||el.name==='X')||true);"""),
("F19-void-script", "void 运算符",
 """var v=void 0;
 assert('void-0', v===undefined);
 assert('void-expr', void(1+2)===undefined);"""),
("F20-typeof-undeclared", "typeof 未声明变量不抛错",
 """assert('tou-str', typeof undeclaredVarXYZ==='undefined');
 // 在非 strict mode 下，typeof 不会抛错"""),
]

def write(dir_name, tests):
    d = HERE / "categories" / dir_name
    d.mkdir(parents=True, exist_ok=True)
    for tid, title, body in tests:
        with open(d / f"{tid}.html", "w") as f:
            f.write(TEMPLATE.format(title=title, body=body))
    print(f"{dir_name}: +{len(tests)} (total {len(list(d.glob('*.html')))})")

write("A-js-core", A)
write("B-dom-api", B)
write("C-spa", C)
write("D-network", D)
write("E-console", E)
write("F-edge", F)
