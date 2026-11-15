"""Extract the design kit's rules for chosen pp-* component families into one stylesheet.

usage: python3 deploy/sites/kit/extract_kit.py <pp-ui.css (bundled)> <out.css>
A rule is kept when every class in at least one of its selectors belongs to an allowed family.
At-rules (@media/@container/@supports) are kept with only their kept rules; @keyframes are kept
when a kept rule names them.
"""
import re
import sys

ALLOW = [
    r"pp-shell(__\w[\w-]*|--[\w-]+)?", r"pp-sidebar(__[\w-]+|--[\w-]+)?", r"pp-topbar(__[\w-]+|--[\w-]+)?",
    r"pp-navbtn(__[\w-]+)?", r"pp-navmenu(__[\w-]+|--[\w-]+)?", r"pp-platforms(__[\w-]+)?", r"pp-platform(__[\w-]+|--[\w-]+)?",
    r"pp-usermenu(__[\w-]+|--[\w-]+)?", r"pp-credit-chip(__[\w-]+|--[\w-]+)?", r"pp-initials(--[\w-]+)?",
    r"pp-brand(__[\w-]+)?", r"pp-card(__[\w-]+|--[\w-]+)", r"pp-stat(__[\w-]+|--[\w-]+)", r"pp-stat-group",
    r"pp-badge--[\w-]+", r"pp-table(__[\w-]+|--[\w-]+)", r"pp-table-toolbar(__[\w-]+)?", r"pp-table-footer(__[\w-]+)?",
    r"pp-empty(__[\w-]+|--[\w-]+)", r"pp-listing[\w-]*", r"pp-verified", r"pp-order(__[\w-]+|--[\w-]+)?",
    r"pp-chip(__[\w-]+|--[\w-]+)?", r"pp-chip-group(__[\w-]+|--[\w-]+)?", r"pp-wallet(__[\w-]+|--[\w-]+)?", r"pp-pack(__[\w-]+|--[\w-]+)?", r"pp-pack-grid",
    r"pp-ledger(__[\w-]+|--[\w-]+)?", r"pp-nest-card(__[\w-]+|--[\w-]+)?", r"pp-nest-status", r"pp-meter(__[\w-]+|--[\w-]+)?",
    r"pp-gate__[\w-]+", r"pp-breadcrumb(__[\w-]+)?", r"pp-h[1-4]", r"pp-overline", r"pp-caption", r"pp-body(-s|-l)?",
    r"pp-display-[sml]", r"pp-cluster(--[\w-]+)?", r"pp-stack(--[\w-]+)?", r"pp-grow", r"pp-split-layout(--[\w-]+)?",
    r"pp-icon(--[\w-]+)?", r"pp-segmented(__[\w-]+)?", r"pp-kanban[\w-]*", r"pp-dl(__[\w-]+)?", r"pp-timeline(__[\w-]+|--[\w-]+)?",
    r"pp-progress(__[\w-]+|--[\w-]+)?", r"pp-filter-bar[\w-]*", r"pp-status-dot(--[\w-]+)?", r"pp-dot", r"pp-count(--[\w-]+)?",
    r"pp-quote[\w-]*", r"pp-callout(__[\w-]+|--[\w-]+)?", r"pp-alert(__[\w-]+|--[\w-]+)", r"pp-review[\w-]*", r"pp-audit[\w-]*",
    r"pp-search--[\w-]+", r"pp-tab__count", r"pp-banner(__[\w-]+|--[\w-]+)?", r"pp-status-list", r"pp-status-row(__[\w-]+)?", r"pp-status", r"pp-status-card(__[\w-]+|--[\w-]+)?", r"pp-uptime", r"pp-navbar(__[\w-]+|--[\w-]+)?", r"pp-footer(__[\w-]+|--[\w-]+)?",
]
ALLOW_RE = re.compile(r"^(?:%s)$" % "|".join(ALLOW))
# Base blocks the platforms already define with different rules; never take the kit's base rule.
NEVER = {"pp-card", "pp-stat", "pp-badge", "pp-table", "pp-table-wrap", "pp-empty", "pp-gate", "pp-alert", "pp-tabs", "pp-tab",
         "pp-input", "pp-select", "pp-field", "pp-form", "pp-btn", "pp-switch", "pp-avatar", "pp-grid", "pp-search", "pp-step", "pp-stepper"}


def strip_comments(css):
    return re.sub(r"/\*.*?\*/", "", css, flags=re.S)


def blocks(css):
    """Yield (prelude, body) for top-level blocks."""
    i, n = 0, len(css)
    while i < n:
        j = css.find("{", i)
        if j < 0:
            break
        prelude = css[i:j].strip()
        depth, k = 1, j + 1
        while k < n and depth:
            if css[k] == "{":
                depth += 1
            elif css[k] == "}":
                depth -= 1
            k += 1
        yield prelude, css[j + 1:k - 1]
        i = k


def keep_selector(sel):
    classes = re.findall(r"\.(pp-[\w-]+)", sel)
    if not classes:
        return False
    for c in classes:
        if c in NEVER:
            continue  # a descendant/compound with a platform block is fine (".pp-card .pp-card__title")
        if not ALLOW_RE.match(c):
            return False
    # at least one allowed (non-NEVER) class
    return any(c not in NEVER for c in classes) and not re.search(r"\.(kt|kit)-", sel)


def walk(css, out, keyframes_used):
    for prelude, body in blocks(css):
        if prelude.startswith("@keyframes") or prelude.startswith("@font-face") or prelude.startswith("@import"):
            continue
        if prelude.startswith("@"):
            inner = []
            walk(body, inner, keyframes_used)
            if inner:
                out.append(prelude + " {\n" + "\n".join("  " + r for r in inner) + "\n}")
            continue
        sels, depth, cur = [], 0, ""
        for ch in prelude:
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
            if ch == "," and depth == 0:
                sels.append(cur.strip()); cur = ""
            else:
                cur += ch
        sels.append(cur.strip())
        kept = [s for s in sels if keep_selector(s)]
        if kept:
            out.append(", ".join(kept) + " { " + " ".join(body.split()) + " }")
            for name in re.findall(r"animation(?:-name)?:\s*([\w-]+)", body):
                keyframes_used.add(name)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    css = strip_comments(open(src).read())
    out, used = [], set()
    walk(css, out, used)
    frames = []
    for prelude, body in blocks(css):
        m = re.match(r"@keyframes\s+([\w-]+)", prelude)
        if m and m.group(1) in used:
            frames.append(prelude + " { " + " ".join(body.split()) + " }")
    head = ("/* GENERATED from the Pigeonpost Design Kit (pp-ui.css, CSS kit 1.0.0) by extracting the component\n"
            "   families the platforms use: app shell, sidebar, top bar, platform switcher, user menu, credit chip,\n"
            "   card/stat/table/badge parts, listing cards, order summary, wallet, ledger, Nest card, meters.\n"
            "   Do not edit by hand: re-run deploy/sites/kit/extract_kit.py on the kit's kits/web/css/dist/pp-ui.css.\n"
            "   platform.css loads after this file. */\n")
    open(dst, "w").write(head + "\n".join(out + frames) + "\n")
    print(len(out), "rules,", len(frames), "keyframes")


main()
