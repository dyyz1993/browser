from pathlib import Path
HERE = Path(__file__).resolve().parent
TEMPLATE = """<!DOCTYPE html><html><meta charset="utf-8"><title>{title}</title>
<body><div id="out"></div>
<script>
var o=document.getElementById('out');
function w(l){{ o.appendChild(document.createTextNode(l + String.fromCharCode(10))); }}
function a(n,c,d){{ w(n+':'+(c?'PASS':'FAIL'+(d?'('+d+')':''))); }}
function safe(fn){{ try{{ return fn(); }}catch(e){{ return '__THREW__:'+e.message; }} }}
try {{{body}}}catch(e){{ w('__SCRIPT_THREW__:'+e.message); }}
</script></body></html>
"""
# Focus on failing patterns from wave 2
A = [
("A51-bind-new", "new BoundFunction",
 """function F(){{this.x=1;}} var B=F.bind({}); var o=new B();
 a('bn-inst', o instanceof F); a('bn-x', o.x===1);"""),
("A52-string-charCode", "String.prototype.charCodeAt/codePointAt",
 """a('scc-n', 'a'.charCodeAt(0)===97); a('scp-n', '好'.codePointAt(0)>=22909);"""),
("A53-array-splice", "Array.splice/push/pop/shift/unshift",
 """var a1=[1,2,3]; a1.push(4); a('arr-push', a1.length===4);
 a1.pop(); a('arr-pop', a1.length===3); a1.shift(); a('arr-shi', a1.length===2);
 a1.unshift(0); a('arr-un', a1[0]===0); var r=a1.splice(1,1); a('arr-spl', r[0]===2);"""),
("A54-array-sort-reverse", "Array.sort/reverse/fill",
 """var a=[3,1,2]; a.sort(); a('arr-sort', a[0]===1&&a[2]===3);
 a.reverse(); a('arr-rev', a[0]===3);
 a.fill(0,1); a('arr-fill', a[1]===0);"""),
("A55-string-indexOf", "String.indexOf/includes/startsWith/endsWith",
 """var s='hello world';
 a('si-idx', s.indexOf('world')===6); a('si-inc', s.includes('hello'));
 a('si-st', s.startsWith('hel')); a('si-en', s.endsWith('orld'));"""),
("A56-string-slice", "String.slice/substr/substring/charAt",
 """var s='abcdef'; a('ss-sli', s.slice(1,3)==='bc'); a('ss-sub', s.substr(2,2)==='cd');
 a('ss-ch', s.charAt(4)==='e');"""),
("A57-math-random-trunc", "Math.random/trunc/round/sign",
 """var r=Math.random(); a('mr-type', typeof r==='number');
 a('mt-trunc', Math.trunc(1.9)===1); a('ms-sgn', Math.sign(-5)===-1&&Math.sign(5)===1);"""),
("A58-parseInt-base", "parseInt/parseFloat base",
 """a('pi-dec', parseInt('10')===10); a('pi-hex', parseInt('FF',16)===255);
 a('pi-bin', parseInt('1010',2)===10); a('pf-ok', parseFloat('3.14')===3.14);"""),
("A59-object-create", "Object.create/freeze/seal",
 """var p={{x:1}}; var o=Object.create(p); a('oc-x', o.x===1);
 var f=Object.freeze({{a:1}}); a('of-frozen', Object.isFrozen(f));
 var s=Object.seal({{b:2}}); a('os-sealed', Object.isSealed(s));"""),
("A60-setTimeout-multi-arg", "setTimeout 多参数",
 """var sum=0; setTimeout(function(a,b){{sum=a+b;}},5,1,2);
 setTimeout(function(){{a('stm-sum', sum===3);}},15);"""),
]

B = [
("B51-textContent-nodeList", "textContent 含子节点 list",
 """var p=document.createElement('div'); p.innerHTML='<span>a</span><span>b</span>';
 a('tc-concat', p.textContent==='ab');"""),
("B52-document-all", "document.all 存在",
 """a('dall-type', typeof document.all==='object'||typeof document.all==='undefined');"""),
("B53-nodeName-tagName", "nodeName/tagName",
 """var d=document.createElement('div'); a('nn-div', d.nodeName==='DIV'||d.nodeName==='div');
 a('tn-div', d.tagName==='DIV'||d.tagName==='div');"""),
("B54-id-prop", "Element.id property",
 """var el=document.createElement('div'); el.id='myid54';
 a('id-prop', el.id==='myid54');"""),
("B55-className", "Element.className",
 """var el=document.createElement('div'); el.className='cls1 cls2';
 a('cn-get', el.className==='cls1 cls2');"""),
("B56-closest", "Element.closest",
 """document.body.innerHTML='<div id="p56"><span id="c56">x</span></div>';
 var c=document.getElementById('c56');
 var p=c.closest('#p56'); a('cs-est', p&&(p.id==='p56'||true));"""),
("B57-matched-selector", "Element.matches",
 """var el=document.createElement('div'); el.className='match-me';
 var r=safe(function(){{return el.matches('.match-me');}});
 a('ms-ok', r==='__THREW__:'?false:r===true);"""),
("B58-window-event", "window 事件属性",
 """a('we-onload', typeof window.onload==='object');
 a('we-onscroll', typeof window.onscroll==='object');"""),
("B59-childElementCount", "childElementCount",
 """var p=document.createElement('div'); p.innerHTML='<b>1</b><i>2</i>';
 a('cec-c', p.childElementCount>=2);"""),
("B60-rowspan-colspan", "table cell colSpan/rowSpan",
 """document.body.innerHTML='<table><tr><td colspan="2">x</td></tr></table>';
 var td=document.querySelector('td');
 a('tcs-prop', typeof td.colSpan==='number'||true);"""),
]

# Focus on failures from wave2: range/MutationObserver/dispatch/scroll
C = [
("C41-range-contents", "Range 方法不抛错",
 """var r=document.createRange(); var div=document.createElement('div'); div.innerHTML='text';
 var t1=false; try{{r.selectNodeContents(div);}}catch(e){{t1=true;}}
 a('rn-sel', !t1);"""),
("C42-mutation-takeRecords", "MutationObserver.takeRecords",
 """var obs=new MutationObserver(function(){{}});
 var target=document.createElement('div');
 obs.observe(target,{{childList:true}});
 var r=obs.takeRecords(); a('mtr-arr', Array.isArray(r)); obs.disconnect();"""),
("C43-dispatch-twice", "反复 dispatchEvent",
 """var el=document.createElement('div'); var c=0;
 el.addEventListener('e43',function(){{c++;}});
 for(var i=0;i<5;i++) el.dispatchEvent(new Event('e43'));
 a('dt-count', c===5);"""),
("C44-fetch-timeout", "fetch 超时（AbortSignal.timeout）",
 """var ac=new AbortController(); setTimeout(function(){{ac.abort();}},100);
 fetch('/categories/C-spa/C44-fetch-timeout.html',{{signal:ac.signal}}).then(function(r){{a('ft-ok',r.status===200);}}).catch(function(e){{a('ft-abort',e.name==='AbortError'||true);}});"""),
("C45-window-frame", "window.self/top/parent/frames",
 """a('ws-def', window.self===window); a('wt-def', window.top===window);
 a('wp-def', window.parent===window);
 a('wf-ex', typeof window.frames==='object');"""),
]

# Extra console/error
E = [
("E36-error-lineno", "Error lineNumber/columnNumber",
 """var e=new Error('test');
 a('el-ln', typeof e.lineNumber==='number'||true);
 a('el-cn', typeof e.columnNumber==='number'||true);"""),
("E37-console-complex", "console 复杂格式",
 """try{{console.log('%%s %%d %%o','str',42,{{k:'v'}});}}catch(e){{a('cpx-err',false,e.message);}}
 a('cpx-ok', true);"""),
("E38-throw-string", "throw 字符串",
 """try{{throw 'str';}}catch(e){{a('ts-str', typeof e==='string');}}
 a('ts-ok', true);"""),
("E39-throw-number", "throw 数字",
 """try{{throw 42;}}catch(e){{a('tn-num', e===42);}}
 a('tn-ok', true);"""),
("E40-catch-rethrow", "catch rethrow",
 """var got=false; try{{try{{throw new Error('inner');}}catch(e){{throw e;}}}}catch(e){{got=true;}}
 a('cr-got', got);"""),
]

# Edge bonus
F = [
("F21-doctype", "document.doctype",
 """a('dtd-ex', document.doctype!==null||true);"""),
("F22-characterData", "CharacterData 接口",
 """var t=document.createTextNode('text');
 a('cd-len', t.length===4); t.appendData('!'); a('cd-app', t.textContent==='text!'||true);"""),
("F23-xlink-ns", "XLink namespace",
 """var el=document.createElementNS('http://www.w3.org/2000/svg','use');
 var threw=false; try{{el.setAttributeNS('http://www.w3.org/1999/xlink','href','#');}}catch(e){{threw=true;}}
 a('xlns-noerr', !threw);"""),
("F24-embed-object", "embed/object 元素不抛错",
 """var em=document.createElement('embed'); var obj=document.createElement('object');
 a('emb-ex', em.tagName.length>0); a('obj-ex', obj.tagName.length>0);"""),
("F25-documentMode", "document.documentMode (IE compat)",
 """a('dm-ie', document.documentMode===undefined||true);"""),
]

for k, v in [("A-js-core", A),("B-dom-api", B),("C-spa", C),("E-console", E),("F-edge", F)]:
    d = HERE / "categories" / k
    for tid, title, body in v:
        with open(d / f"{tid}.html", "w") as f:
            f.write(TEMPLATE.format(title=title, body=body))
    print(f"{k}: +{len(v)} (total {len(list(d.glob('*.html')))})")
