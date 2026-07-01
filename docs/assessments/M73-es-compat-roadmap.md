# M73: Advanced ES Syntax Compatibility Roadmap

Date: 2026-07-01
Status: Draft

## Status: Syntax Layer (30/30 ✅ — Full ES2020-ES2024)

受控实验验证了以下全部语法通过：

| Version | Features | Count |
|---------|----------|:-----:|
| ES2020 | optional chaining, nullish, BigInt, dynamic import | 4/4 |
| ES2021 | numeric separators, logical assign, replaceAll, Promise.any, WeakRef, AggregateError | 6/6 |
| ES2022 | class fields, private fields, static fields, static block, private methods, Array.at, Object.hasOwn, Error.cause, structuredClone | 9/9 |
| ES2023 | findLast, findLastIndex, toSorted, toReversed, Array.with | 5/5 |
| ES2024 | Promise.withResolvers, Object.groupBy, String.isWellFormed | 3/3 |
| All | | **30/30 ✅** |

## Issue 1: Class static block variable in QuickJS (rare)

`class A { static { let x = 1; } }` OK, but referencing from outside is error — V8 behavior.

## Issue 2: `let` redeclaration in eval (lit.dev)

In strict mode eval (our default for module scripts), `catch(e) { let e = 2 }` is an error in both V8 and QuickJS. This is likely a bundle bug from lit.dev, not our problem.

## Known Failures / Pending Fixes

| Site | Error | Root Cause | Impact | Priority |
|------|-------|-----------|--------|:--------:|
| **solidjs.com** | `not a function` | ESM module chunk 301 redirect not followed | Blocked | **P0** |
| **github.com** | `.toUpperCase of undefined` | Runtime property missing | Medium | P2 |
| **emberjs.com** | timeout 20s | Page too heavy, needs longer timeout | Low | P3 |
| **astro.build** | ESM module promise hang | Module chain never completing | Medium | P2 |

## Plan

### P0: solidjs redirect follow (1 commit)
- HttpLoader uses `crate::bridge::fetch_sync()` which does NOT follow redirects for ESM chunks
- Need: `HttpLoader::load` → allow redirects OR use `with_net_worker` path which follows redirects
- **Fix:** Change HttpLoader to use HttpClient directly or ensure redirect policy applies to module fetches
- **Acceptance:** `browser fetch https://solidjs.com/ --format text` output > 1000B

### P2: github toUpperCase fix (1 commit)
- Diagnose what undefined property leads to `.toUpperCase` call
- Add appropriate guard or stub

### P2: astro ESM module hang (investigate)
- Module promise never completes — event loop integration needed
- Longer term work

### P3: ember timeout
- Increase default timeout from 20s to 30s, or add `--timeout` flag
