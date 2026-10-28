#!/usr/bin/env python3
"""Check the public security headers of every Pigeonpost web origin.

Read-only: one GET per URL with a browser User-Agent (Cloudflare refuses library defaults).
Run after any nginx or site deployment; exits 1 on the first missing or weakened header so it can
gate a rollout. See deploy/sites/README.md, "Edge security headers".
"""
import sys
import urllib.error
import urllib.request

UA = "Mozilla/5.0 (pigeonpost header check)"
HSTS_MIN_AGE = 31536000

# Every HTML origin: strict CSP directives that must be present.
HTML = {
    # pigeonpost.dev and developers have no object-src; default-src 'self' already confines plugins.
    "https://pigeonpost.dev/": ["frame-ancestors 'none'", "base-uri 'none'"],
    "https://pigeonpost.dev/privacy.html": ["script-src 'none'", "frame-ancestors 'none'"],
    "https://inbox.pigeonpost.dev/": ["frame-ancestors 'none'", "base-uri 'none'", "object-src 'none'"],
    "https://inbox.pigeonpost.dev/join.html": ["frame-ancestors 'none'", "object-src 'none'"],
    "https://developers.pigeonpost.dev/": ["frame-ancestors 'none'", "base-uri 'none'"],
    "https://enterprise.pigeonpost.dev/": ["frame-ancestors 'none'", "base-uri 'none'", "object-src 'none'"],
}
# API and tunnel origins: no CSP needed, but HSTS and nosniff are.
API = ["https://postbox.pigeonpost.dev/health", "https://tunnel.pigeonpost.dev/healthz"]


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.status, r.headers
    except urllib.error.HTTPError as e:
        return e.code, e.headers


def hsts_ok(value):
    for part in (value or "").split(";"):
        k, _, v = part.strip().partition("=")
        if k.lower() == "max-age":
            return v.isdigit() and int(v) >= HSTS_MIN_AGE
    return False


def main():
    problems = []
    for url, needed in list(HTML.items()) + [(u, None) for u in API]:
        status, h = fetch(url)
        if status >= 500:
            problems.append(f"{url}: HTTP {status}")
            continue
        if not hsts_ok(h.get("Strict-Transport-Security")):
            problems.append(f"{url}: Strict-Transport-Security missing or max-age < {HSTS_MIN_AGE}")
        if (h.get("X-Content-Type-Options") or "").lower() != "nosniff":
            problems.append(f"{url}: X-Content-Type-Options nosniff missing")
        if needed is not None:
            csp = h.get("Content-Security-Policy") or ""
            if not csp:
                problems.append(f"{url}: Content-Security-Policy missing")
            if "'unsafe-inline'" in csp.split("script-src", 1)[-1].split(";", 1)[0] and "script-src" in csp:
                problems.append(f"{url}: script-src allows 'unsafe-inline'")
            for d in needed:
                if d not in csp:
                    problems.append(f"{url}: CSP lacks {d}")
            if not h.get("Referrer-Policy"):
                problems.append(f"{url}: Referrer-Policy missing")
        print(f"{status} {url}")
    for p in problems:
        print("FAIL", p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
