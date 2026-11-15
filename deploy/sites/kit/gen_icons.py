"""icons.svg (kit sprite, Lucide) -> site-shared/icons.js: PP_ICONS = { name: [[tag, {attr: value}], ...] }."""
import json
import re
import sys
import xml.etree.ElementTree as ET

src, dst = sys.argv[1], sys.argv[2]
ns = "{http://www.w3.org/2000/svg}"
root = ET.parse(src).getroot()
icons = {}
for sym in root.iter(ns + "symbol"):
    name = sym.get("id", "")[2:]
    parts = []
    for child in sym:
        tag = child.tag.replace(ns, "")
        attrs = {k: v for k, v in child.attrib.items() if k in ("d", "cx", "cy", "r", "x", "y", "width", "height", "rx", "ry", "x1", "x2", "y1", "y2", "points")}
        parts.append([tag, attrs])
    icons[name] = parts
body = json.dumps(icons, separators=(",", ":"), ensure_ascii=False)
open(dst, "w").write(
    "// GENERATED from the Pigeonpost Design Kit icon sprite (assets/icons.svg: Lucide, ISC licence).\n"
    "// Do not edit by hand: re-run deploy/sites/kit/gen_icons.py on the kit's icons.svg. shell.js draws PP.icon(name) from these shapes (24x24, stroke).\n"
    "window.PP_ICONS = " + body + ";\n")
print(len(icons), "icons")
