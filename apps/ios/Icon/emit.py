import json, sys
d = json.load(open("traced.json"))
paths, colors = d["paths"], d["colors"]

# Bounding box of the ink, so the mark is centred on its own extent rather than on the empty
# canvas the source PNG happens to have around it.
xs, ys = [], []
for loops in paths.values():
    for loop in loops:
        for x, y in loop:
            xs.append(x); ys.append(y)
minx, maxx, miny, maxy = min(xs), max(xs), min(ys), max(ys)
w, h = maxx - minx, maxy - miny
print(f"ink box {w:.0f}x{h:.0f} at ({minx:.0f},{miny:.0f})", file=sys.stderr)
counts = {k: [len(l) for l in v] for k, v in paths.items()}
print("points per loop:", counts, file=sys.stderr)

SIZE = 1024
MARGIN = 0.10                       # optical margin; the bird fills the icon rather than floats in it
box = SIZE * (1 - 2 * MARGIN)
scale = min(box / w, box / h)
ox = (SIZE - w * scale) / 2 - minx * scale
oy = (SIZE - h * scale) / 2 - miny * scale

def d_for(loops):
    out = []
    for loop in loops:
        pts = [f"{x*scale+ox:.2f} {y*scale+oy:.2f}" for x, y in loop]
        out.append("M " + " L ".join(pts) + " Z")
    return " ".join(out)

hexes = {k: "#%02X%02X%02X" % tuple(v) for k, v in colors.items()}
svg = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" viewBox="0 0 {SIZE} {SIZE}">']
svg.append(f'<rect width="{SIZE}" height="{SIZE}" fill="{{GROUND}}"/>')
for name in ("navy", "blue", "green"):
    svg.append(f'<path fill="{{%s}}" fill-rule="evenodd" d="{d_for(paths[name])}"/>' % name.upper())
svg.append("</svg>")
svg = "\n".join(svg)

light = svg.replace("{GROUND}", "#FFFFFF")
for name in ("navy", "blue", "green"):
    light = light.replace("{%s}" % name.upper(), hexes[name])
open("icon-light.svg", "w").write(light)

# Dark: the ground becomes the navy the bird is drawn in, so the bird itself has to become the
# paper. The bubbles keep their colours — they are the part that reads as the mark.
dark = svg.replace("{GROUND}", hexes["navy"])
dark = dark.replace("{NAVY}", "#FFFFFF").replace("{BLUE}", hexes["blue"]).replace("{GREEN}", hexes["green"])
open("icon-dark.svg", "w").write(dark)
print("wrote icon-light.svg, icon-dark.svg", file=sys.stderr)
