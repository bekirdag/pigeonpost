# Trace the flat-colour logo into polygons: classify each pixel to the nearest of the artwork's
# four colours, walk the boundary of each colour region with marching squares, then simplify.
# No dependencies — this machine has no PIL, no numpy, and no potrace.
import json, sys
from collections import defaultdict

PALETTE = {
    "ground": (255, 255, 255),
    "navy":   (5, 54, 139),
    "blue":   (8, 108, 255),
    "green":  (38, 206, 45),
}
INK = ["navy", "blue", "green"]

def read_ppm(path):
    with open(path, "rb") as f:
        assert f.readline().strip() == b"P6"
        line = f.readline()
        while line.startswith(b"#"):
            line = f.readline()
        w, h = map(int, line.split())
        assert int(f.readline()) == 255
        return w, h, f.read()

w, h, data = read_ppm(sys.argv[1])
names = list(PALETTE)
cols = [PALETTE[n] for n in names]

# Nearest-colour classification. Anti-aliased edge pixels land on whichever side they are closer
# to, which is the 50% threshold an edge wants.
cls = bytearray(w * h)
cache = {}
for i in range(w * h):
    px = data[3*i:3*i+3]
    got = cache.get(px)
    if got is None:
        r, g, b = px[0], px[1], px[2]
        got = min(range(4), key=lambda k: (r-cols[k][0])**2 + (g-cols[k][1])**2 + (b-cols[k][2])**2)
        cache[px] = got
    cls[i] = got

def contours(mask_index):
    """Marching squares over the pixel grid, chained into closed loops."""
    inside = lambda x, y: 0 <= x < w and 0 <= y < h and cls[y*w + x] == mask_index
    segs = defaultdict(list)
    for y in range(h + 1):
        for x in range(w + 1):
            tl = inside(x-1, y-1); tr = inside(x, y-1)
            bl = inside(x-1, y);   br = inside(x, y)
            code = (tl << 3) | (tr << 2) | (br << 1) | bl
            if code in (0, 15):
                continue
            # Edge midpoints of the cell centred on this corner.
            N, E, S, W = (x, y-0.5), (x+0.5, y), (x, y+0.5), (x-0.5, y)
            add = lambda a, b: segs[a].append(b)
            if code in (1, 14):   add(S, W) if code == 1 else add(W, S)
            elif code in (2, 13): add(E, S) if code == 2 else add(S, E)
            elif code in (3, 12): add(E, W) if code == 3 else add(W, E)
            elif code in (4, 11): add(N, E) if code == 4 else add(E, N)
            elif code == 5:       add(N, W); add(S, E)
            elif code == 10:      add(E, S); add(W, N)
            elif code in (6, 9):  add(N, S) if code == 6 else add(S, N)
            elif code in (7, 8):  add(N, W) if code == 7 else add(W, N)
    loops = []
    while segs:
        start = next(iter(segs))
        loop = [start]
        cur = start
        while True:
            nxts = segs.get(cur)
            if not nxts:
                break
            nxt = nxts.pop()
            if not nxts:
                del segs[cur]
            if nxt == start:
                break
            loop.append(nxt)
            cur = nxt
        if len(loop) > 8:
            loops.append(loop)
    return loops

def simplify_closed(loop, eps):
    """Douglas-Peucker on a closed loop.

    Split it into two open chains first. Running the plain algorithm on a loop whose last point
    repeats its first gives a zero-length baseline, every point then measures as zero deviation from
    it, and the whole contour simplifies away to nothing.
    """
    if len(loop) < 8:
        return loop
    ax, ay = loop[0]
    far = max(range(len(loop)), key=lambda k: (loop[k][0]-ax)**2 + (loop[k][1]-ay)**2)
    head = simplify(loop[:far+1], eps)
    tail = simplify(loop[far:] + [loop[0]], eps)
    return head[:-1] + tail[:-1]

def simplify(points, eps):
    """Douglas-Peucker, iterative so a long contour cannot blow the stack."""
    if len(points) < 3:
        return points
    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    stack = [(0, len(points) - 1)]
    while stack:
        i, j = stack.pop()
        if j <= i + 1:
            continue
        ax, ay = points[i]; bx, by = points[j]
        dx, dy = bx - ax, by - ay
        norm = (dx*dx + dy*dy) ** 0.5 or 1.0
        worst, at = 0.0, -1
        for k in range(i + 1, j):
            px, py = points[k]
            d = abs(dy*px - dx*py + bx*ay - by*ax) / norm
            if d > worst:
                worst, at = d, k
        if worst > eps and at > 0:
            keep[at] = True
            stack.append((i, at)); stack.append((at, j))
    return [p for p, k in zip(points, keep) if k]

out = {}
total = 0
for name in INK:
    idx = names.index(name)
    loops = []
    for loop in contours(idx):
        # Epsilon scaled to the shape. A tolerance that is invisible on a 600px bird outline is a
        # visible octagon on a 20px dot inside a speech bubble, and the dots are what say "message".
        span = max(max(p[0] for p in loop) - min(p[0] for p in loop),
                   max(p[1] for p in loop) - min(p[1] for p in loop))
        s = simplify_closed(loop, max(0.12, min(0.45, span / 900)))
        if len(s) > 6:
            loops.append([[round(x, 2), round(y, 2)] for x, y in s])
            total += len(s)
    out[name] = loops
    print(f"{name}: {len(loops)} loops", file=sys.stderr)

print(f"{total} points total", file=sys.stderr)
json.dump({"width": w, "height": h, "colors": PALETTE, "paths": out}, open(sys.argv[2], "w"))
