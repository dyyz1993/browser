#!/usr/bin/env python3
"""批量生成 A 类（JS Core）25 个 fixture。
每个 fixture 往 #out 写 TESTNAME:PASS/FAIL，Chrome 和我们跑后逐行对比。
"""
import os
from pathlib import Path

HERE = Path(__file__).resolve().parent  # bug-hunt/
OUT = HERE / "categories/A-js-core"
OUT.mkdir(parents=True, exist_ok=True)

# 模板：每个测试点是一个函数，push 一行到 out
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

# 测试点定义：(id, 描述, JS body)
TESTS = [
("A01-promise-allSettled", "Promise.allSettled",
 """var p = Promise.allSettled([Promise.resolve(1), Promise.reject('x')]);
 p.then(function(r){ assert('allSettled-len', r.length===2);
   assert('allSettled-0', r[0].status==='fulfilled' && r[0].value===1);
   assert('allSettled-1', r[1].status==='rejected' && r[1].reason==='x');
 });"""),
("A02-promise-any", "Promise.any",
 """Promise.any([Promise.reject('a'), Promise.resolve(42)]).then(function(v){
   assert('any-value', v===42);
 }, function(e){ assert('any-value', false, 'rejected: '+e); });"""),
("A03-promise-finally", "Promise.finally",
 """Promise.resolve(1).finally(function(){ assert('finally-called', true); });"""),
("A04-bigint-basic", "BigInt 基本运算",
 """assert('bigint-lit', typeof 9007199254740993n === 'bigint');
 assert('bigint-add', (1n+2n)===3n);
 assert('bigint-mul', (10n*10n)===100n);
 assert('bigint-toStr', (255n).toString(16)==='ff');
 assert('bigint-parse', BigInt('999')===999n);"""),
("A05-optional-chaining", "Optional chaining (?.)",
 """var obj = {{a:{{b:1}}}};
 assert('oc-deep', obj?.a?.b === 1);
 assert('oc-null', obj?.x?.y === undefined);
 assert('oc-call', obj?.a?.b?.toFixed?.(0) === '1');
 assert('oc-arr', [1,2,3]?.[1] === 2);"""),
("A06-nullish-coalescing", "Nullish coalescing (??)",
 """assert('nc-null', (null ?? 'd') === 'd');
 assert('nc-undef', (undefined ?? 'd') === 'd');
 assert('nc-zero', (0 ?? 'd') === 0);
 assert('nc-empty', ('' ?? 'd') === '');"""),
("A07-globalThis", "globalThis",
 """assert('gt-defined', typeof globalThis === 'object');
 globalThis.__test72 = 42;
 assert('gt-assign', __test72 === 42);
 assert('gt-window', globalThis.globalThis === globalThis);"""),
("A08-reflect", "Reflect API",
 """var o = {{x:1}};
 assert('ref-get', Reflect.get(o,'x')===1);
 assert('ref-set', Reflect.set(o,'y',2)===true && o.y===2);
 assert('ref-has', Reflect.has(o,'x')===true);
 assert('ref-ownKeys', Reflect.ownKeys(o).length===2);
 assert('ref-apply', Reflect.apply(Math.max, null, [1,5,3])===5);"""),
("A09-proxy-basic", "Proxy 基础",
 """var t = {{x:1}};
 var p = new Proxy(t, {{ get:function(t,k){{ return k in t ? t[k] : 'MISS'; }} }});
 assert('proxy-get', p.x===1);
 assert('proxy-miss', p.y==='MISS');"""),
("A10-proxy-revocable", "Proxy revoke",
 """var o={{a:1}};
 var {{proxy, revoke}} = Proxy.revocable(o, {{}});
 assert('revoc-pre', proxy.a===1);
 revoke();
 var threw = false;
 try{{ proxy.a; }}catch(e){{ threw = true; }}
 assert('revoc-post', threw);"""),
("A11-symbol-iterator", "Symbol.iterator",
 """var arr = [1,2,3];
 var it = arr[Symbol.iterator]();
 assert('sym-next1', it.next().value===1);
 assert('sym-next2', it.next().value===2);
 assert('sym-done', it.next().done===false && it.next().done===true);"""),
("A12-symbol-toPrimitive", "Symbol.toPrimitive",
 """var o = {{ [Symbol.toPrimitive](hint){{ return hint==='number'?42:'s'; }} }};
 assert('symtp-num', +o===42);
 assert('symtp-str', ''+o==='s');"""),
("A13-array-flat", "Array.flat/flatMap",
 """assert('flat-1', [1,[2,[3]]].flat().length===3);
 assert('flat-2', [1,[2,[3]]].flat(2).length===3);
 assert('flatMap', [1,2].flatMap(function(x){{ return [x,x*10]; }}).join(',')==='1,10,2,20');"""),
("A14-array-from-entries", "Array.from/keys/values",
 """assert('from', Array.from('ab').join()==='a,b');
 assert('from-set', Array.from(new Set([1,1,2])).join()==='1,2');
 assert('keys', [10,20].keys ? Array.from([10,20].keys()).join(',')==='0,1' : false);
 assert('at-neg', [1,2,3].at(-1)===3);"""),
("A15-object-fromEntries", "Object.fromEntries",
 """var o = Object.fromEntries([['a',1],['b',2]]);
 assert('ofe-a', o.a===1);
 assert('ofe-b', o.b===2);
 assert('ofe-keys', Object.keys(o).length===2);"""),
("A16-object-is-assign", "Object.is/assign",
 """assert('is-nan', Object.is(NaN, NaN));
 assert('is-0n', Object.is(-0, -0));
 assert('is-0p0n', !Object.is(-0, 0));
 var dst={{}}; Object.assign(dst, {{a:1}}, {{b:2}});
 assert('assign', dst.a===1 && dst.b===2);"""),
("A17-string-pad-matchall", "String pad/matchAll/replaceAll",
 """assert('pad', 'x'.padStart(3,'ab')==='abx');
 assert('padEnd', 'x'.padEnd(3,'0')==='x00');
 var ms = 'a1b2'.matchAll(/([a-z])(\\d)/g);
 assert('matchAll', Array.from(ms).length===2);
 assert('replaceAll', 'a-a'.replaceAll('a','x')==='x-x');"""),
("A18-regex-named-lookbehind", "正则 named groups/lookbehind/dotAll",
 """var m = /(?<y>\\d{4})-(?<m>\\d{2})/.exec('2024-03');
 assert('rg-name', m.groups.y==='2024' && m.groups.m==='03');
 var lb = /(?<=\\$)\\d+/.exec('price $100');
 assert('rg-lb', lb && lb[0]==='100');
 var ds = /a.b/s.test('a\\nb');
 assert('rg-dotAll', ds);"""),
("A19-destructuring", "解构嵌套/默认/rest/交换",
 """var {{a, b=5, ...r}} = {{a:1, c:3, d:4}};
 assert('dc-vals', a===1 && b===5);
 assert('dc-rest', r.c===3 && r.d===4);
 var [x, , z] = [1,2,3];
 assert('dc-arr', x===1 && z===3);
 var [p,q] = [10,20]; [p,q]=[q,p];
 assert('dc-swap', p===20 && q===10);"""),
("A20-generator", "生成器/迭代器协议",
 """function* gen(){{ yield 1; yield 2; return 3; }}
 var g = gen();
 assert('gen-1', g.next().value===1);
 assert('gen-2', g.next().value===2);
 var last = g.next();
 assert('gen-done', last.done===true && last.value===3);
 function* range(n){{ for(var i=0;i<n;i++) yield i; }}
 assert('gen-spread', [...range(3)].join()==='0,1,2');"""),
("A21-import-meta-dynamic", "动态 import（本地无模块文件，测语法不抛错）",
 """// 静态测：import.meta 引用不崩，import() 返回 Promise
 assert('im-ref', safe(function(){{ return typeof import.meta; }}).indexOf('__THREW__')<0);
 var ip = import('./nonexistent.mjs');
 assert('im-promise', ip instanceof Promise);
 ip.catch(function(){{ assert('im-reject', true); }});"""),
("A22-class-private-static", "类 private field / static / extends",
 """class Base{{ static x = 10; #v = 5; get v(){{ return this.#v; }} }}
 class Sub extends Base{{ #s = 1; get s(){{ return this.#s; }} }}
 var s = new Sub();
 assert('cls-static', Sub.x===10);
 assert('cls-priv', s.v===5);
 assert('cls-sub-priv', s.s===1);"""),
("A23-try-nobinding", "try/catch 无参数绑定",
 """var got = false;
 try {{ throw new Error('e'); }} catch {{ got = true; }}
 assert('try-nobind', got);"""),
("A24-template-tagged", "模板字符串标签/raw",
 """function tag(strs, ...vals){{ return strs.raw.join('|') + '/' + vals.join(','); }}
 assert('tpl-tag', tag`a${1}b${2}c`==='a|b|c/1,2');
 assert('tpl-multiline', `line1\\nline2`.includes('\\n'));"""),
("A25-map-set-iter", "Map/Set 迭代与构造",
 """var m = new Map([['k',1]]);
 m.set('x', 2);
 assert('map-get', m.get('k')===1 && m.get('x')===2);
 assert('map-size', m.size===2);
 assert('map-iter', [...m.keys()].join()==='k,x');
 var s = new Set([1,1,2,3,3]);
 assert('set-unique', s.size===3);
 assert('set-has', s.has(2) && !s.has(4));"""),
]

def main():
    for tid, title, body in TESTS:
        html = TEMPLATE.format(title=title, body=body)
        path = OUT / f"{tid}.html"
        with open(path, "w") as f:
            f.write(html)
    print(f"生成 {len(TESTS)} 个 A 类 fixture → {OUT}")
    for tid, _, _ in TESTS:
        print(f"  {tid}")

if __name__ == "__main__":
    main()
