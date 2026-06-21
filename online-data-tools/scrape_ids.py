#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["requests", "beautifulsoup4"]
# ///
"""Scrape vendor names for a list of USB VIDs from usb-ids.gowdy.us.

Reads a flat list of 4-hex-digit VIDs (one per line, whitespace-trimmed)
from `--input`, fetches https://usb-ids.gowdy.us/read/UD/<vid> for each,
parses the page with BeautifulSoup, and writes/updates a JSON mapping
`{vid_lower_hex: vendor_name_string}` to `--output`.

Behavior:

- **Single-threaded** with a polite 0.5 s base delay between requests.
- **Incremental save**: after each successful scrape the JSON is rewritten,
  so a Ctrl-C never loses prior work. Re-running resumes where we left off
  (entries already in the JSON are skipped unless `--refetch` is passed).
- **Vendor-not-found** pages → "not found".
- **HTTP 404 / other transient errors** → "error".
- **Exponential backoff** (1 → 2 → 4 → 8 → 16 → 32 → 60 s, capped) on
  network errors and 5xx; the request is retried up to 5 times before
  giving up with "error".
- **Fail2ban probe**: after 3 consecutive 404s, we pause and re-fetch a
  list of canary VIDs (known-good entries from a recent run). If the
  canaries also 404, we assume rate-limit / IP-block and back off 5 min
  before continuing; if they succeed, the 404s were real and we continue.

Run:
    scrape_ids.py --input ids.txt --output ids.json
"""

from __future__ import annotations

import argparse
import json
import re
import ssl
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Callable

try:
    from bs4 import BeautifulSoup  # type: ignore  # noqa: F401  (kept for fallback)
except ImportError as e:  # pragma: no cover — uv installs it from PEP 723
    raise SystemExit(f"bs4 missing — uv should auto-install. {e}")


BASE_URL = "https://usb-ids.gowdy.us/read/UD"

# Known-good canary VIDs, used to detect fail2ban / IP-block when we see
# a burst of 404s. These are deliberately stable, well-known vendors.
CANARY_VIDS = ("303a", "0483", "10c4", "1a86", "2341", "239a", "16c0")

# Polite delay between successful requests, in seconds.
BASE_DELAY = 0.5

# Backoff schedule for retries (caps at 60 s per spec).
BACKOFF_STEPS = (1, 2, 4, 8, 16, 32, 60)

# Number of consecutive 404s that triggers a canary probe.
CANARY_TRIGGER = 3

# Sleep duration when fail2ban is suspected, in seconds.
FAIL2BAN_SLEEP = 300


def _make_ssl_ctx() -> ssl.SSLContext:
    # gowdy.us has had self-signed / expired-cert episodes in the past.
    # The scraped page is structural / public, so accept the cert.
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def _fetch_html(url: str, *, timeout: float = 30.0) -> tuple[int, str]:
    """Return (status_code, body) for the URL. Raises on network errors."""
    req = urllib.request.Request(url, headers={
        "User-Agent": "fbuild-bot/1.0 (+https://github.com/FastLED/fbuild)",
        "Accept": "text/html",
    })
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_make_ssl_ctx()) as resp:
            return resp.status, resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as e:
        # 4xx and 5xx come back here. Read the body anyway so the caller can
        # decide what to log; the status is what matters for the policy.
        try:
            body = e.read().decode("utf-8", errors="replace")
        except Exception:
            body = ""
        return e.code, body


# The vendor name on a /read/UD/<VID> page lives in:
#   <div class='discussion'>
#     <h2>Discussion</h2>
#     <div class='unseen-history'>
#       <p class='itemname'>Name: <VENDOR NAME>
#       <p class='author'>Bertold
#       <p class='time'>2021-11-10 14:17:35
#
# The `<p>` tags use HTML 4.01 implicit-close style. BeautifulSoup's
# html.parser concatenates the children, polluting the captured name
# with the author + timestamp. Regex against the raw HTML stops at the
# next `<` (the next `<p>` opening), which is exactly what we want.
_VENDOR_NAME_RE = re.compile(
    r"<p\s+class=['\"]itemname['\"]>\s*Name:\s*(.+?)\s*(?:<|$)",
    re.IGNORECASE | re.DOTALL,
)


def parse_vendor_name(html: str) -> str | None:
    """First-match vendor name (legacy single-value mode)."""
    m = _VENDOR_NAME_RE.search(html)
    if not m:
        return None
    name = m.group(1).strip()
    return name or None


def parse_all_names(html: str) -> list[str]:
    """Every `Name: ...` row on the page, in document order, de-duped while
    preserving first-seen order. Used by --all-names mode where the caller
    wants to see every submission / revision the gowdy.us page carries for
    a given VID."""
    seen: set[str] = set()
    out: list[str] = []
    for m in _VENDOR_NAME_RE.finditer(html):
        name = m.group(1).strip()
        if not name or name in seen:
            continue
        seen.add(name)
        out.append(name)
    return out


def scrape_one(
    vid: str,
    *,
    all_names: bool = False,
    fetch: Callable[[str], tuple[int, str]] = _fetch_html,
) -> str | list[str]:
    """Return the vendor verdict for a single VID.

    Modes:
      - single (default):  returns the first-match vendor name as `str`, or
                           "not found" / "error".
      - all_names=True:    returns every `Name:` row as `list[str]`. Empty
                           list = page loaded but had no names. `["error"]`
                           sentinel = HTTP/network failure.

    A 404 returns "error" immediately (no retries — the resource genuinely
    doesn't exist; the caller looks at consecutive 404 counts to decide
    whether to canary-probe). 5xx + network errors retry with exponential
    backoff per BACKOFF_STEPS.
    """
    url = f"{BASE_URL}/{vid.upper()}"
    for attempt, sleep_seconds in enumerate((0, *BACKOFF_STEPS), start=1):
        if sleep_seconds:
            time.sleep(sleep_seconds)
        try:
            status, body = fetch(url)
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            print(f"  attempt {attempt} {url}: network error: {e}", file=sys.stderr)
            continue
        if status == 200:
            if all_names:
                return parse_all_names(body)
            name = parse_vendor_name(body)
            return name if name else "not found"
        if status == 404:
            return ["error"] if all_names else "error"
        # 5xx etc — retry with the next backoff step.
        print(f"  attempt {attempt} {url}: HTTP {status}", file=sys.stderr)
    return ["error"] if all_names else "error"


def canary_probe(*, fetch: Callable[[str], tuple[int, str]] = _fetch_html) -> bool:
    """Re-fetch a small set of known-good VIDs to decide whether the
    server is genuinely 404-ing or whether we've been IP-blocked.

    Returns True if at least one canary returns 200 (we are NOT blocked),
    False if every canary 404s (probably fail2ban).
    """
    print("canary probe: checking known-good VIDs…", file=sys.stderr)
    for vid in CANARY_VIDS:
        try:
            status, _body = fetch(f"{BASE_URL}/{vid.upper()}")
        except Exception as e:
            print(f"  canary {vid}: {e}", file=sys.stderr)
            continue
        print(f"  canary {vid}: HTTP {status}", file=sys.stderr)
        if status == 200:
            return True
        time.sleep(BASE_DELAY)
    return False


def load_ids(path: Path) -> list[str]:
    """Parse the input file. Tolerates trailing whitespace / tabs per line."""
    out: list[str] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        tok = raw.strip().split()[0] if raw.strip() else ""
        tok = tok.strip().lower()
        if not tok:
            continue
        if not re.fullmatch(r"[0-9a-f]{1,4}", tok):
            print(f"warning: skipping malformed line: {raw!r}", file=sys.stderr)
            continue
        out.append(tok.zfill(4))
    return out


def load_resume(path: Path) -> dict:
    """Returns a dict of either {vid: str} (single mode) or {vid: list[str]}
    (all-names mode); the value shape is preserved verbatim from disk."""
    if not path.is_file():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        print(f"warning: {path} not valid JSON — starting fresh", file=sys.stderr)
        return {}
    if not isinstance(data, dict):
        return {}
    return {str(k).lower(): v for k, v in data.items()}


def save(path: Path, data: dict) -> None:
    path.write_text(
        json.dumps(dict(sorted(data.items())), indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )


def _is_error(verdict) -> bool:
    """Treat both the single- and list-mode error sentinels as 'error'
    for the consecutive-404 / canary policy."""
    return verdict == "error" or verdict == ["error"]


def _bucket(verdict) -> str:
    if _is_error(verdict):
        return "error"
    if verdict == "not found":
        return "not found"
    if isinstance(verdict, list) and not verdict:
        return "not found"
    return "named"


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input",   default="ids.txt", type=Path)
    p.add_argument("--output",  default="ids.json", type=Path)
    p.add_argument("--all-names", action="store_true",
                   help="Collect every `Name:` row per page and write values "
                        "as `list[str]`. Default emits the first match as a "
                        "single string.")
    p.add_argument("--refetch", action="store_true",
                   help="Ignore existing entries in --output and re-scrape every VID.")
    p.add_argument("--delay", type=float, default=BASE_DELAY,
                   help="Polite delay between successful requests, in seconds.")
    args = p.parse_args()

    vids = load_ids(args.input)
    results: dict = {} if args.refetch else load_resume(args.output)

    todo = [v for v in vids if v not in results]
    print(f"input: {len(vids)} VID(s), {len(todo)} to scrape "
          f"(resuming {len(results)}, mode={'all-names' if args.all_names else 'single'})",
          file=sys.stderr)

    consecutive_404 = 0
    for i, vid in enumerate(todo, start=1):
        url = f"{BASE_URL}/{vid.upper()}"
        print(f"[{i}/{len(todo)}] {url}", file=sys.stderr)
        verdict = scrape_one(vid, all_names=args.all_names)
        results[vid] = verdict
        save(args.output, results)

        if _is_error(verdict):
            consecutive_404 += 1
        else:
            consecutive_404 = 0

        if consecutive_404 >= CANARY_TRIGGER:
            if canary_probe():
                print(f"  canaries OK — the 404 streak is genuine, continuing",
                      file=sys.stderr)
                consecutive_404 = 0
            else:
                print(f"  every canary 404'd — assuming fail2ban; sleeping "
                      f"{FAIL2BAN_SLEEP}s before continuing", file=sys.stderr)
                time.sleep(FAIL2BAN_SLEEP)
                # After the cooldown, retry the canary once more. If still
                # bad, exit so a human can investigate rather than burning
                # the whole list against a blocked endpoint.
                if not canary_probe():
                    print("ERROR: canaries still blocked after cooldown — exiting",
                          file=sys.stderr)
                    return 2
                consecutive_404 = 0

        time.sleep(args.delay)

    # Summary.
    counts: dict[str, int] = {}
    for v in results.values():
        b = _bucket(v)
        counts[b] = counts.get(b, 0) + 1
    print(
        f"\ndone. {len(results)} total: "
        f"named={counts.get('named', 0)} "
        f"not_found={counts.get('not found', 0)} "
        f"error={counts.get('error', 0)}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
