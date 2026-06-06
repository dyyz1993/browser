# Rendering snapshots

This directory holds the M6.0d acceptance artifacts: side-by-side
comparison images showing the gap between our pure-Rust browser
renderer and the system WebKit renderer (macOS Quick Look) on the
same HTML fixtures.

## Regenerating

```sh
python3 tests/snapshots/compare.py
```

Requires `pip3 install Pillow` (dev-only dependency). Outputs:

| File | Description |
|------|-------------|
| `demo-safari.png` | Safari/WebKit rendering of `tests/fixtures/demo.html` |
| `demo-ours.txt`   | Our `browser render-file` ASCII output for the same HTML |
| `demo-compare.png`| Side-by-side composite |
| `example-safari.png` | WebKit view of https://example.com/ |
| `example-ours.txt`   | Our `render-url` ASCII output |
| `example-compare.png`| Side-by-side composite |

## Why

M6.0a/b/c fixed three layout bugs that the original user-acceptance
screenshot revealed (long-paragraph truncation, `<title>` leak,
missing `<li>` bullets / paragraph spacing). This script lets us
re-verify those fixes at any time, on any developer's Mac, without
re-deriving the comparison from scratch.

`compare.py` is macOS-only because `qlmanage` is macOS-only. The
comparison images are committed to git so non-Mac developers can
inspect them without re-running.
