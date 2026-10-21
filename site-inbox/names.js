// How a person is named on screen, and the two letters on their face.
//
// One rule for every place a person appears — the conversation list and header, the mailbox
// switcher, the people panel, call tiles, the call header and the ring card — so "/bekir/main"
// reads as "Bekir" everywhere and never as "main" (or "MA" on a call tile without video).
//
//   /bekir/main  -> "Bekir"          /bekir         -> "Bekir"
//   /wodo/hr     -> "Wodo HR"        /acme/john-doe -> "Acme John Doe"
//   /acme/qa/bot -> "Acme QA Bot"
//
// Handles that are not names keep their old display: key addresses (/k/…) stay a short hash,
// address-style handles (/alex@example.com) and /github/<login> stay as they were. A contact alias
// or profile name always wins over this; callers pass it in before falling back to the handle.
//
// The same rule lives in the native clients (PeerFace.swift, Models.kt, Models.cs, model.py) with
// the same acronym list; their tests assert the same table.
window.PigeonpostNames = (function () {
  "use strict";

  // Short segments that read as acronyms. Edit this list here and in the native copies together.
  const ACRONYMS = ["hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"];

  function word(w) {
    const lower = w.toLowerCase();
    if (ACRONYMS.indexOf(lower) >= 0) return lower.toUpperCase();
    return lower.charAt(0).toUpperCase() + lower.slice(1);
  }

  function segment(s) {
    return s.split(/[-_]+/).filter(Boolean).map(word).join(" ");
  }

  // The display name for a handle or address. Anything that is not a slash-handle is returned as is.
  function displayName(peer) {
    if (!peer) return "unknown";
    peer = String(peer);
    if (peer.startsWith("/k/")) return peer.slice(0, 12) + "…";
    const parts = peer.split("/").filter(Boolean);
    if (!peer.startsWith("/") || parts.length === 0) return peer;
    // Unchanged shapes: address-style handles and GitHub logins.
    if (peer.indexOf("@") >= 0 || parts[0] === "github") return parts.length > 1 ? parts[parts.length - 1] : peer;
    if (parts.length > 1 && parts[parts.length - 1] === "main") parts.pop();
    const name = parts.map(segment).filter(Boolean).join(" ");
    return name || peer;
  }

  // Two letters from a display name: the first letters of its first and last words, or the first
  // two letters of a one-word name. A name that is still a path (a key or an address) gives the
  // letters of its last segment, as before.
  function initials(name) {
    const text = String(name || "").trim();
    const base = text.indexOf("/") >= 0 ? (text.split("/").filter(Boolean).pop() || "") : text;
    const words = base.split(/[\s\-_]+/).map((w) => w.replace(/[^\p{L}\p{N}]/gu, "")).filter(Boolean);
    if (words.length === 0) return "··";
    const letters = words.length > 1
      ? Array.from(words[0])[0] + Array.from(words[words.length - 1])[0]
      : Array.from(words[0]).slice(0, 2).join("");
    return letters.toUpperCase();
  }

  return { ACRONYMS, displayName, initials };
})();
