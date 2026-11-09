"""Contacts, phone numbers and names: the rules, with no GUI dependency (SDS §14D).

docs/contacts-and-phone.md is the contract and `site-inbox/contacts.js` the reference. Everything
here is pure so it is unit tested with the same tables as the web inbox:

  normalize_e164   a typed or imported number -> "+<8-15 digits>" or None (§2)
  hash_phone       E.164 -> 32 hex characters, the only thing discovery ever sends (§3.1)
  parse_vcard      a .vcf export -> [{name, phones}] (names stay on this computer; §1.2)
  parse_csv        a Google / Outlook / generic .csv export -> the same shape
  pick_name        which name to show for a peer, and how (§6.3)
  Aliases          what *you* call someone, on this computer only (§6.2)

Nothing read from a contacts file is stored: the window parses it, hashes the numbers and lets the
list go when the dialog closes.
"""

import csv
import hashlib
import io
import json
import os
import quopri
import re
import time
import unicodedata
from pathlib import Path

# Feature flags from GET /v1/capabilities (docs/contacts-and-phone.md table at the top).
PHONE_NUMBERS = "phone_numbers"
CONTACT_DISCOVERY = "contact_discovery"
SMS_INVITES = "sms_invites"
DISPLAY_NAMES = "display_names"

# ---- phone numbers (§2) -------------------------------------------------------------------------

_TRUNK_IN_BRACKETS = re.compile(r"^(\+\d{1,3})[\s.-]*\(0\)")
_EXTENSION = re.compile(r"\s*(?:ext(?:ension)?\.?|x|#)\s*\d*\s*$", re.IGNORECASE)
_SEPARATORS = re.compile(r"[\s.\-() ]")


def normalize_e164(raw, default_calling_code=""):
    """The web inbox's `normalizeE164`, deliberately simpler than libphonenumber: a number it gets
    wrong simply fails to match."""
    if raw is None:
        return None
    s = str(raw).strip()
    s = re.sub(r"^tel:", "", s, flags=re.IGNORECASE)
    s = s.split(";")[0]
    s = _TRUNK_IN_BRACKETS.sub(r"\1", s)
    s = _EXTENSION.sub("", s)
    s = _SEPARATORS.sub("", s)
    if not re.fullmatch(r"\+?\d+", s):
        return None
    cc = re.sub(r"\D", "", str(default_calling_code or ""))
    if s.startswith("+"):
        digits = s[1:]
    elif s.startswith("00"):
        digits = s[2:]
    elif cc == "1" and s.startswith("011"):
        digits = s[3:]
    elif not cc:
        return None
    elif cc == "1":
        # NANP has no trunk prefix: an 11-digit number starting with 1 already carries the country code.
        digits = s if len(s) == 11 and s.startswith("1") else "1" + s
    else:
        digits = cc + (s[1:] if s.startswith("0") else s)
    return "+" + digits if re.fullmatch(r"[1-9]\d{7,14}", digits) else None


HASH_PREFIX = "pigeonpost:phone:v1:"


def hash_phone(e164):
    """First 16 bytes of SHA-256("pigeonpost:phone:v1:" + E.164), lowercase hex (§3.1)."""
    return hashlib.sha256((HASH_PREFIX + e164).encode()).digest()[:16].hex()


# Common countries first; anything else is typed as "+code". The same list as the web inbox.
COUNTRIES = [
    ("TR", "Türkiye", "90"), ("US", "United States / Canada", "1"), ("GB", "United Kingdom", "44"),
    ("DE", "Germany", "49"), ("FR", "France", "33"), ("NL", "Netherlands", "31"), ("ES", "Spain", "34"),
    ("IT", "Italy", "39"), ("AU", "Australia", "61"), ("IN", "India", "91"),
]


def guess_calling_code(environ=None):
    """A starting guess from LC_TELEPHONE, then LC_ALL / LANG (en_GB.UTF-8 -> 44). Only a default;
    the person picks. "1" when nothing matches, as on the web."""
    env = os.environ if environ is None else environ
    for key in ("LC_ALL", "LC_TELEPHONE", "LANG"):
        match = re.match(r"^[A-Za-z]{2,3}_([A-Za-z]{2})\b", env.get(key) or "")
        if not match:
            continue
        code = match.group(1).upper()
        for r, _, cc in COUNTRIES:
            if r == code or (r == "US" and code == "CA"):
                return cc
    return "1"


# ---- vCard --------------------------------------------------------------------------------------

def _decode_qp(value):
    raw = quopri.decodestring(value.encode("utf-8", "surrogateescape"))
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("latin-1")


def _vcard_unescape(value):
    return re.sub(r"\\([\\;,nN])", lambda m: " " if m.group(1) in "nN" else m.group(1), value)


def _split_unescaped(value, sep):
    out, cur, i = [], "", 0
    while i < len(value):
        if value[i] == "\\" and i + 1 < len(value):
            cur += value[i:i + 2]
            i += 2
            continue
        if value[i] == sep:
            out.append(cur)
            cur = ""
        else:
            cur += value[i]
        i += 1
    out.append(cur)
    return out


def parse_vcard(text):
    """vCard 2.1, 3.0 and 4.0: folded lines, several TELs, quoted-printable, `item1.` groups and
    `tel:` URIs. Returns [{"name", "phones"}] with the numbers as written."""
    physical = re.sub(r"\r\n?", "\n", str(text or "").lstrip("﻿")).split("\n")
    lines = []
    for line in physical:
        prev = lines[-1] if lines else None
        if prev is not None and line[:1] in (" ", "\t"):
            lines[-1] = prev + line[1:]
        elif prev is not None and "QUOTED-PRINTABLE" in prev.split(":")[0].upper() and prev.endswith("="):
            lines[-1] = prev[:-1] + line
        else:
            lines.append(line)
    cards, card = [], None
    for line in lines:
        if ":" not in line:
            continue
        head, raw_value = line.split(":", 1)
        parts = head.split(";")
        name = re.sub(r"^[^.]*\.", "", parts[0]).strip().upper()
        params = [p.strip().upper() for p in parts[1:]]
        if name == "BEGIN" and raw_value.strip().upper() == "VCARD":
            card = {"fn": "", "n": "", "phones": []}
            continue
        if card is None:
            continue
        if name == "END" and raw_value.strip().upper() == "VCARD":
            cards.append({"name": (card["fn"] or card["n"]).strip(), "phones": card["phones"]})
            card = None
            continue
        qp = any(p in ("QUOTED-PRINTABLE", "ENCODING=QUOTED-PRINTABLE") for p in params)
        value = _decode_qp(raw_value) if qp else raw_value
        if name == "FN":
            card["fn"] = re.sub(r"\s+", " ", _vcard_unescape(value))
        elif name == "N":
            fields = [_vcard_unescape(f) for f in _split_unescaped(value, ";")] + [""] * 5
            family, given, additional, prefix, suffix = fields[:5]
            card["n"] = " ".join(s.strip() for s in (prefix, given, additional, family, suffix) if s.strip())
        elif name == "TEL":
            number = value.strip()
            if number:
                card["phones"].append(number)
    return cards


# ---- CSV ----------------------------------------------------------------------------------------

_PHONE_HEADER = re.compile(r"\b(phone|mobile|cell|tel|telephone)\b")
_LABEL_HEADER = re.compile(r"(type|label)\b")


def parse_csv(text):
    """Google Contacts ("Phone 1 - Value" holding several numbers joined by " ::: "), Outlook
    ("Mobile Phone", "Business Phone", ...) and any file whose header says phone, mobile, cell or tel."""
    rows = [r for r in csv.reader(io.StringIO(str(text or "").lstrip("﻿"), newline=""))
            if any(f.strip() for f in r)]
    if len(rows) < 2:
        return []
    lower = [h.strip().lower() for h in rows[0]]

    def col(*names):
        return next((lower.index(n) for n in names if n in lower), -1)

    name_col = col("name", "display name", "full name", "file as")
    first_col = col("first name", "given name")
    middle_col = col("middle name", "additional name")
    last_col = col("last name", "family name", "surname")
    fallback_col = col("nickname", "organization name", "organization 1 - name", "company")
    phone_cols = [i for i, h in enumerate(lower) if _PHONE_HEADER.search(h) and not _LABEL_HEADER.search(h)]
    out = []
    for r in rows[1:]:
        def get(i, r=r):
            return r[i].strip() if 0 <= i < len(r) else ""
        name = (get(name_col) or " ".join(x for x in (get(first_col), get(middle_col), get(last_col)) if x)
                or get(fallback_col))
        phones = [p.strip() for i in phone_cols for p in get(i).split(":::") if p.strip()]
        out.append({"name": re.sub(r"\s+", " ", name), "phones": phones})
    return out


def parse_contacts_file(text):
    """A .vcf or .csv export, told apart by its content as the web inbox does."""
    return parse_vcard(text) if re.search(r"BEGIN:VCARD", text or "", re.IGNORECASE) else parse_csv(text)


def numbers_from(people, calling_code):
    """Unique E.164 numbers with the first name seen for each, plus how many could not be read."""
    seen, unreadable = {}, 0
    for person in people:
        for raw in person.get("phones", []):
            e164 = normalize_e164(raw, calling_code)
            if not e164:
                unreadable += 1
                continue
            entry = seen.setdefault(e164, {"name": person.get("name") or "", "e164": e164, "hash": hash_phone(e164), "invite": None})
            if not entry["name"] and person.get("name"):
                entry["name"] = person["name"]
    return list(seen.values()), unreadable


# New accounts may check 200 fresh numbers per day; a 500-number first batch would be refused whole.
DISCOVERY_BATCH = 200


def next_batch(hashes, start, remaining):
    """The next slice to send, or [] when the day's allowance is used. `remaining` is None until the
    server has said how many checks are left."""
    available = DISCOVERY_BATCH if remaining is None else max(0, int(remaining))
    if available == 0:
        return []
    return hashes[start:start + min(DISCOVERY_BATCH, available)]


# ---- names (§6) ---------------------------------------------------------------------------------

# The server's `profile_names.rs::forbidden` set, as in site-inbox/contacts.js.
_FORBIDDEN_RANGES = [
    (0x00AD, 0x00AD), (0x034F, 0x034F), (0x0600, 0x0605), (0x061C, 0x061C), (0x06DD, 0x06DD), (0x070F, 0x070F),
    (0x115F, 0x1160), (0x17B4, 0x17B5), (0x180B, 0x180F), (0x200B, 0x200F), (0x2028, 0x202E), (0x2060, 0x206F),
    (0x2800, 0x2800), (0x3164, 0x3164), (0xFE00, 0xFE0F), (0xFEFF, 0xFEFF), (0xFFA0, 0xFFA0), (0xFFF0, 0xFFFB),
    (0xE0000, 0xE007F),
]
RESERVED = ("admin", "administrator", "support", "official", "verified", "security", "system", "moderator")


def _forbidden(ch):
    if ch in "/@" or unicodedata.category(ch) == "Cc":
        return True
    code = ord(ch)
    return any(lo <= code <= hi for lo, hi in _FORBIDDEN_RANGES)


def _letter_or_digit(ch):
    return unicodedata.category(ch)[0] in ("L", "N")


def validate_name(raw, maximum, reserved):
    """(True, tidied) or (False, words a person can act on)."""
    text = unicodedata.normalize("NFC", "" if raw is None else str(raw))
    if any(_forbidden(c) for c in text):
        return False, "Names can’t contain /, @, or invisible characters."
    name = " ".join(text.split())
    if not name:
        return False, "Enter a name."
    if len(name) > maximum:
        return False, f"Use at most {maximum} characters."
    if not any(_letter_or_digit(c) for c in name):
        return False, "Use at least one letter or digit."
    if reserved:
        squashed = "".join(c for c in name.lower() if _letter_or_digit(c))
        if "pigeonpost" in squashed or squashed in RESERVED:
            return False, "That name is reserved. Choose another."
    return True, name


def validate_display_name(raw):
    return validate_name(raw, 40, True)


def validate_alias(raw):
    return validate_name(raw, 64, False)


def pick_name(alias=None, contact_alias=None, display_name=None, derived=None):
    """§6.3: local alias, then the server contact alias, then a self-set display name (shown as
    "~Name"), then the name derived from the handle. Returns (text, kind)."""
    def clean(value):
        return value.strip() if isinstance(value, str) else ""
    if clean(alias):
        return clean(alias), "alias"
    if clean(contact_alias):
        return clean(contact_alias), "contact"
    if clean(display_name):
        return "~" + clean(display_name), "display"
    return clean(derived) or "unknown", "derived"


def invite_name(display_name):
    """The sender's name as the invitation SMS carries it (§5): letters, digits, spaces and .'- only,
    at most 24 characters, otherwise "A friend"."""
    kept = "".join(c for c in str(display_name or "") if _letter_or_digit(c) or c in " .'-")
    kept = " ".join(kept.split())
    return kept[:24].strip() or "A friend"


def invite_text(display_name):
    return ("Pigeonpost: " + invite_name(display_name)
            + " invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=… Reply STOP to opt out.")


# ---- server answers (bounded and typed; one bad row is dropped, never shown) ---------------------

_HASH = re.compile(r"[0-9a-f]{32}")


def _text(value, limit):
    return value if isinstance(value, str) and 0 < len(value) <= limit else None


def parse_phones(raw):
    """`GET /v1/phone` -> (numbers, max_numbers)."""
    rows = raw.get("numbers") if isinstance(raw, dict) else None
    numbers = []
    for row in rows if isinstance(rows, list) else []:
        if not isinstance(row, dict) or not _text(row.get("id"), 128) or not _text(row.get("identity"), 300):
            continue
        numbers.append({"id": row["id"], "identity": row["identity"], "masked": _text(row.get("masked"), 64) or "Phone number",
                        "discoverable": row.get("discoverable") is True})
    maximum = raw.get("max_numbers") if isinstance(raw, dict) else None
    return numbers[:16], maximum if isinstance(maximum, int) and 0 < maximum <= 16 else 3


def parse_discovery(raw, sent):
    """`POST /v1/contacts/discover` -> ({hash: match}, remaining_today or None). Only hashes this
    request sent are accepted."""
    sent = set(sent)
    matches = {}
    rows = raw.get("matches") if isinstance(raw, dict) else None
    for row in rows if isinstance(rows, list) else []:
        if not isinstance(row, dict) or row.get("hash") not in sent or not _HASH.fullmatch(row["hash"]):
            continue
        address = _text(row.get("address"), 300)
        if not address or not address.startswith("/"):
            continue
        handle = _text(row.get("handle"), 300)
        matches[row["hash"]] = {"address": address, "handle": handle if handle and handle.startswith("/") else None,
                                "display_name": _text(row.get("display_name"), 80)}
    remaining = raw.get("remaining_today") if isinstance(raw, dict) else None
    return matches, remaining if isinstance(remaining, int) and not isinstance(remaining, bool) and remaining >= 0 else None


def parse_profiles(raw, asked):
    """`GET /v1/profiles?addresses=` -> {address: display_name or None} for the addresses asked."""
    found = raw.get("profiles") if isinstance(raw, dict) else None
    found = found if isinstance(found, dict) else {}
    out = {}
    for key in asked:
        row = found.get(key)
        out[key] = _text(row.get("display_name"), 80) if isinstance(row, dict) else None
    return out


def parse_own_name(raw):
    """`GET /v1/profile/name` -> {display_name, visibility} or None."""
    row = raw.get("name") if isinstance(raw, dict) else None
    if not isinstance(row, dict) or not _text(row.get("display_name"), 80):
        return None
    visibility = row.get("visibility") if row.get("visibility") in ("known", "everyone", "nobody") else "known"
    return {"display_name": row["display_name"], "visibility": visibility}


# ---- words for refusals (the web inbox's phoneError / discoverError / invite handling) ------------

def wait_words(seconds):
    s = max(1, round(float(seconds or 0)))
    if s < 90:
        return f"{s} seconds"
    if s < 5400:
        return f"{round(s / 60)} minutes"
    if s < 129600:
        return f"{round(s / 3600)} hours"
    return f"{round(s / 86400)} days"


def _retry_after(error, default):
    headers = getattr(error, "headers", None) or {}
    try:
        return float(headers.get("Retry-After") or headers.get("retry-after") or default)
    except (TypeError, ValueError, AttributeError):
        return default


def _body(error):
    body = getattr(error, "body", None)
    return body if isinstance(body, dict) else {}


def phone_error(error):
    code, status = getattr(error, "code", ""), getattr(error, "status", 0)
    if not status:
        return "Could not reach Pigeonpost. Please try again."
    if code == "invalid_code":
        left = _body(error).get("attempts_left")
        return "That code isn’t right." + (f" {left} {'try' if left == 1 else 'tries'} left." if isinstance(left, int) else "")
    words = {
        "challenge_expired": "That code has expired. Send a new one.",
        "phone_not_supported": "This number can’t be used with Pigeonpost. Premium-rate and satellite numbers aren’t supported.",
        "invalid_phone": "That doesn’t look like a phone number. Check the country and the number.",
        "sms_unavailable": "We can’t send text messages right now. Try again later.",
        "phone_limit": "You already have as many numbers as you can. Unlink one first, or use another mailbox.",
        "mailbox_has_number": "This mailbox already has a number.",
    }
    if code == "rate_limited" or status == 429:
        return "Too many codes for now. Try again in " + wait_words(_retry_after(error, 60)) + "."
    if code in words:
        return words[code]
    return "Only you, signed in, can add a number." if status == 403 else "Could not send the code. Please try again."


def discover_error(error):
    code, status = getattr(error, "code", ""), getattr(error, "status", 0)
    if not status:
        return "Could not reach Pigeonpost. Please try again."
    body = _body(error)
    if status == 429:
        wait = wait_words(_retry_after(error, 3600))
        if body.get("limit") == "daily":
            return f"You’ve checked as many new numbers as you can today. Try again in {wait}."
        if body.get("limit") == "monthly":
            return f"You’ve checked as many new numbers as you can this month. Try again in {wait}."
        return f"Too many checks for now. Try again in {wait}."
    if code == "discovery_locked":
        until = body.get("until")
        when = time.strftime(" until %d %b %Y", time.localtime(until)) if isinstance(until, (int, float)) and until > 0 else ""
        return f"Finding people is paused for this account{when}."
    if status == 403:
        return "Only you, signed in, can do this."
    if code == "discovery_busy" or status == 503:
        return "Lots of people are doing this right now. Try again in " + wait_words(_retry_after(error, 300)) + "."
    if code == "feature_disabled":
        return "Finding people isn’t available yet."
    return "Could not check your contacts. Please try again."


def invite_error(error):
    code, status = getattr(error, "code", ""), getattr(error, "status", 0)
    if code == "phone_required":
        return "Verify your own number first."
    if status == 429:
        return "You can send 5 invitations a day and 20 a month. Try again in " + wait_words(_retry_after(error, 86400)) + "."
    if code == "sms_unavailable":
        return "We can’t send text messages right now. Try again later."
    return "Could not send the invitation. Please try again."


def invite_outcome(raw):
    """(sent, label for the row, toast) for a 200 answer of `POST /v1/contacts/invites`."""
    if isinstance(raw, dict) and raw.get("status") == "sent":
        return True, "Invited", "Invitation sent."
    if isinstance(raw, dict) and raw.get("reason") == "already_invited":
        return False, "Already invited", "You invited this number recently."
    return False, "Can’t be invited right now", "This number can’t get an invitation right now."


def name_error(error):
    status = getattr(error, "status", 0)
    if status == 429:
        return "You can change your name 10 times a day. Try again tomorrow."
    detail = str(error) if status == 400 and getattr(error, "code", "") not in ("", "bad_request") else ""
    if detail:
        return detail[:1].upper() + detail[1:].rstrip(".") + "."
    return "Could not save your name. Please try again."


# ---- local aliases (§6.2) -----------------------------------------------------------------------

def aliases_path(environ=None):
    env = os.environ if environ is None else environ
    base = env.get("XDG_DATA_HOME") or os.path.join(os.path.expanduser("~"), ".local", "share")
    return Path(base) / "pigeonpost" / "aliases.json"


class Aliases:
    """What *you* call someone, on this computer only: `$XDG_DATA_HOME/pigeonpost/aliases.json`
    (mode 600), one map per signed-in account. Never sent anywhere. A file that cannot be read or
    written leaves the aliases in memory for this session; the window works the same without it."""

    def __init__(self, path=None):
        self.path = Path(path) if path else aliases_path()
        self.owner = None
        self.memory = {}

    def _read_all(self):
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {}
        return data if isinstance(data, dict) else {}

    def _write_all(self, data):
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            temp = self.path.with_suffix(".tmp")
            fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(data, handle, ensure_ascii=False)
            os.chmod(temp, 0o600)
            os.replace(temp, self.path)
        except OSError:
            pass

    def bind(self, owner):
        """Switch to an account (an opaque owner key, see binding.owner_key); None forgets."""
        self.owner = owner
        rows = self._read_all().get(owner) if owner else None
        self.memory = {p: a for p, a in rows.items() if isinstance(p, str) and isinstance(a, str) and a} if isinstance(rows, dict) else {}

    def _save(self):
        if not self.owner:
            return
        data = self._read_all()
        if self.memory:
            data[self.owner] = dict(self.memory)
        else:
            data.pop(self.owner, None)
        self._write_all(data)

    def get(self, peer):
        return self.memory.get(peer) if peer else None

    def set(self, peer, alias):
        ok, value = validate_alias(alias)
        if not ok:
            raise ValueError(value)
        self.memory[peer] = value
        self._save()
        return value

    def remove(self, peer):
        self.memory.pop(peer, None)
        self._save()

    def all(self):
        return dict(self.memory)

    def clear_all(self):
        """Signing out removes every alias this computer holds, whichever account set them."""
        self.memory = {}
        self.owner = None
        try:
            self.path.unlink()
        except OSError:
            pass
