#!/usr/bin/env python3
"""Inlines demo.svg into deploy/landing/index.html, between <!-- demo.svg --> markers.

The splash page is a single self-contained file (its CSP allows only data: images), so
the demo travels inside it as a data URI; demo.svg at the repository root stays the one
source. Run after changing demo.svg: python3 scripts/landing-demo.py
"""
import pathlib
import re
import urllib.parse

root = pathlib.Path(__file__).resolve().parent.parent
page = root / "deploy" / "landing" / "index.html"
svg = (root / "demo.svg").read_text(encoding="utf-8").strip()
svg = re.sub(r">\s+<", "><", svg)  # whitespace between tags
uri = "data:image/svg+xml," + urllib.parse.quote(svg, safe=" :/=;,'()-_.!~*@+?$")  # not &: the attribute would decode &amp; inside the SVG
img = (
    f'<img class="demo" src="{uri}" width="1220" height="800" loading="lazy" decoding="async" '
    'alt="A chat continues in the style of a real person, then a friend opens the shared link on a phone">'
)
html = page.read_text(encoding="utf-8")
new, n = re.subn(r"(<!-- demo\.svg -->).*?(<!-- /demo\.svg -->)", lambda m: m[1] + img + m[2], html, flags=re.S)
if n != 1:
    raise SystemExit(f"{page}: expected one <!-- demo.svg --> … <!-- /demo.svg --> block, found {n}")
page.write_text(new, encoding="utf-8")
print(f"inlined demo.svg ({len(uri) // 1024} KiB) into {page.relative_to(root)}")
