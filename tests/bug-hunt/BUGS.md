# M72 Bug 狩猎报告

生成时间: Wed Jul  1 12:46:53 2026

总览: ✅ 142 passed / 🐛 110 bugs / 252 fixtures

| 类别 | 状态 | 详情 |
|------|------|------|
| A-js-core/A05-optional-chaining | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A08-reflect | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A09-proxy-basic | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A10-proxy-revocable | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A12-symbol-toPrimitive | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A15-object-fromEntries | 💥 | SCRIPT_THREW: not a function |
| A-js-core/A16-object-is-assign | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A19-destructuring | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A21-import-meta-dynamic | ⚠️ | JS_ERR: [js] [quickjs] Error: import.meta only valid in module code |
| A-js-core/A22-class-private-static | ⚠️ | JS_ERR: [js] [quickjs] Error: invalid property name |
| A-js-core/A34-getter-setter | 💥 | SCRIPT_THREW: not a function |
| A-js-core/A36-intl-json | 💥 | SCRIPT_THREW: not a function |
| A-js-core/A42-setTimeout-order | 🐛 | FAIL: sto-ord |
| A-js-core/A51-bind-new | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A52-string-charCode | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A53-array-splice | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A54-array-sort-reverse | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A55-string-indexOf | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A56-string-slice | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A57-math-random-trunc | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A58-parseInt-base | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A59-object-create | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| A-js-core/A60-setTimeout-multi-arg | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B01-cloneNode | 🐛 | FAIL: cn-deep-tag, cn-deep-text |
| B-dom-api/B02-contains | 💥 | SCRIPT_THREW: invalid 'instanceof' right operand |
| B-dom-api/B03-classList | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B04-dataset | 🐛 | FAIL: ds-keys |
| B-dom-api/B05-namedNodeMap | 💥 | SCRIPT_THREW: cannot read property 'length' of undefined |
| B-dom-api/B08-insertAdjacentHTML | 🐛 | FAIL: iah-length, iah-tag, iah-first |
| B-dom-api/B09-MutationObserver | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B10-appendChild-order | 🐛 | FAIL: ac-order, ac-last, ac-move-last |
| B-dom-api/B11-removeChild | 🐛 | FAIL: rc-orphan |
| B-dom-api/B12-replaceChild | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B13-textContent | 🐛 | FAIL: tc-nested |
| B-dom-api/B15-event-bubbles | 🐛 | FAIL: ev-bubble |
| B-dom-api/B17-preventDefault | 🐛 | FAIL: pd-after |
| B-dom-api/B20-cloneNode-deep | 🐛 | FAIL: cnd-text |
| B-dom-api/B21-comment-node | 🐛 | FAIL: cm-nodeType, cm-text |
| B-dom-api/B22-nextSibling | 🐛 | FAIL: ns-next, ns-prev |
| B-dom-api/B25-scrollIntoView | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B26-dispatchEvent-once | 🐛 | FAIL: deo-count |
| B-dom-api/B28-getElementsByTagName | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B30-range-basic | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B33-parentNode | 🐛 | FAIL: pn-p, pe-p |
| B-dom-api/B34-setAttribute | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B35-hasAttribute | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B36-scrollTo | 🐛 | FAIL: wsb-noerr |
| B-dom-api/B46-document-write | 💥 | SCRIPT_THREW: not a function |
| B-dom-api/B47-document-open-close | 🐛 | FAIL: do-noerr, dc-noerr |
| B-dom-api/B48-EventTarget-constructor | 🐛 | FAIL: et-ctor |
| B-dom-api/B51-textContent-nodeList | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B52-document-all | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B53-nodeName-tagName | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B54-id-prop | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B55-className | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B56-closest | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B57-matched-selector | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B58-window-event | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B59-childElementCount | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| B-dom-api/B60-rowspan-colspan | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| C-spa/C03-history-pushState | 🐛 | FAIL: hs-len |
| C-spa/C04-XHR-basic | 🐛 | FAIL: xhr-ok |
| C-spa/C06-fetch-json | 🐛 | FAIL: fj-err |
| C-spa/C07-fetch-post | 🐛 | FAIL: fp-err |
| C-spa/C10-event-delegation | 🐛 | FAIL: ed-type |
| C-spa/C12-IntersectionObserver | 💥 | SCRIPT_THREW: not a function |
| C-spa/C13-requestAnimationFrame | 💥 | SCRIPT_THREW: not a function |
| C-spa/C19-window-open | 💥 | SCRIPT_THREW: not a function |
| C-spa/C21-mutation-callback | 🐛 | FAIL: moc-called |
| C-spa/C39-innerHTML-script | 💥 | SCRIPT_THREW: ' + e.message); |
| C-spa/C41-range-contents | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| C-spa/C42-mutation-takeRecords | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| C-spa/C43-dispatch-twice | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| C-spa/C44-fetch-timeout | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| C-spa/C45-window-frame | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| D-network/D01-fetch-headers | 🐛 | FAIL: fh-ct |
| D-network/D02-xhr-status | ⚠️ | JS_ERR: [js] [xhr] send /categories/D-network/D02-xhr-status.html → 740 bytes |
| D-network/D03-xhr-responseType | ⚠️ | JS_ERR: [js] [xhr] send /categories/D-network/D03-xhr-responseType.html → 694 bytes |
| D-network/D05-xhr-send-order | 🐛 | FAIL: xhr-sync-txt |
| D-network/D06-WebSocket-basic | 💥 | SCRIPT_THREW: WebSocket is not defined |
| D-network/D10-fetch-abort | 🐛 | FAIL: ac-err |
| D-network/D11-xhr-event-order | 🐛 | FAIL: xev-status |
| D-network/D13-fetch-clone | 🐛 | FAIL: frc-err |
| D-network/D14-post-form | 🐛 | FAIL: pf-err |
| D-network/D15-xhr-overrideMime | 🐛 | FAIL: xomt-noerr |
| D-network/D17-xhr-getAllResponseHeaders | ⚠️ | JS_ERR: [js] [xhr] send data:text/plain,hx → null |
| D-network/D19-fetch-body-json | 🐛 | FAIL: fbj-err |
| D-network/D20-url-searchParams | 💥 | SCRIPT_THREW: cannot read property 'get' of undefined |
| D-network/D21-fetch-body-text | 🐛 | FAIL: fbt-err |
| D-network/D22-fetch-body-blob | 🐛 | FAIL: fbl-err |
| D-network/D23-xhr-upload | 🐛 | FAIL: xul-ex |
| E-console/E06-console-time | 🐛 | FAIL: ctm-err |
| E-console/E08-console-count | 🐛 | FAIL: cc-err |
| E-console/E20-console-timeLog | 🐛 | FAIL: ctl-err |
| E-console/E25-groupCollapsed | 🐛 | FAIL: gc-err |
| E-console/E27-console-profile | 🐛 | FAIL: cpr-err |
| E-console/E36-error-lineno | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| E-console/E37-console-complex | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| E-console/E38-throw-string | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| E-console/E39-throw-number | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| E-console/E40-catch-rethrow | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| F-edge/F07-deep-nesting | 🐛 | FAIL: dn-leaf |
| F-edge/F08-empty-text | 🐛 | FAIL: et-nodeType |
| F-edge/F13-template-in-html | 💥 | SCRIPT_THREW: DocumentFragment is not defined |
| F-edge/F16-special-chars-attr | ⚠️ | JS_ERR: [js] [quickjs] Error: Unexpected identifier 'c' |
| F-edge/F21-doctype | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| F-edge/F22-characterData | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| F-edge/F23-xlink-ns | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| F-edge/F24-embed-object | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
| F-edge/F25-documentMode | ⚠️ | JS_ERR: [js] [quickjs] Error: unexpected end of string |
