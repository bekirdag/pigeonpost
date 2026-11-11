#!/usr/bin/env python3
"""Audit or provision Cubemeld's Game Center achievements and leaderboards.

The manifest checked into Cubemeld (fastlane/gamecenter/achievements.json) is the source of
truth. APPLY=false is a read-only audit. APPLY=true enables Game Center for the app if needed,
creates missing achievements and leaderboards, and fills missing or changed localizations and
achievement images on editable (never live) versions. It never deletes, archives, or changes
an existing achievement's points or visibility: that drift is reported and fails the run.
"""

import hashlib
import json
import os
import re
import sys
from pathlib import Path

from cubemeld_iap import (
    EXPECTED_BUNDLE_ID,
    ApiError,
    Client,
    _git_tracked_bytes,
    ordered_upload_operations,
)

APPLY = os.environ.get("APPLY", "false").lower() == "true"
MANIFEST_PATH = os.environ.get(
    "GAMECENTER_MANIFEST_PATH", "cubemeld/fastlane/gamecenter/achievements.json"
)
ACHIEVEMENT_PREFIX = "cubemeld.achievement."
LEADERBOARD_PREFIX = "cubemeld."
EDITABLE_STATES = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED"}
MAX_NAME = 30
MAX_DESCRIPTION = 200
FORMATTERS = {"INTEGER"}


# MARK: Manifest


def _png_size(content: bytes) -> tuple[int, int]:
    if content[:8] != b"\x89PNG\r\n\x1a\n" or content[12:16] != b"IHDR":
        raise RuntimeError("achievement image is not a PNG")
    return int.from_bytes(content[16:20], "big"), int.from_bytes(content[20:24], "big")


def load_manifest(path: str = MANIFEST_PATH):
    manifest_path = Path(path)
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("bundleId") != EXPECTED_BUNDLE_ID:
        raise RuntimeError("Game Center manifest is for a different bundle ID")
    achievements = manifest.get("achievements")
    leaderboards = manifest.get("leaderboards")
    if not isinstance(achievements, list) or not achievements or len(achievements) > 100:
        raise RuntimeError("Game Center manifest must list 1...100 achievements")
    if not isinstance(leaderboards, list):
        raise RuntimeError("Game Center manifest must list leaderboards")

    seen = set()
    total = 0
    for item in achievements:
        vendor = item.get("vendorIdentifier", "")
        if not vendor.startswith(ACHIEVEMENT_PREFIX) or not re.fullmatch(r"[A-Za-z0-9._]+", vendor):
            raise RuntimeError(f"bad achievement ID: {vendor!r}")
        if vendor in seen:
            raise RuntimeError(f"duplicate achievement ID: {vendor}")
        seen.add(vendor)
        points = item.get("points")
        if isinstance(points, bool) or not isinstance(points, int) or not 1 <= points <= 100:
            raise RuntimeError(f"{vendor}: points must be 1...100")
        total += points
        if not isinstance(item.get("showBeforeEarned"), bool):
            raise RuntimeError(f"{vendor}: showBeforeEarned must be a boolean")
        localizations = item.get("localizations")
        if not isinstance(localizations, dict) or "en-US" not in localizations:
            raise RuntimeError(f"{vendor}: needs an en-US localization")
        for locale, text in localizations.items():
            name = text.get("name", "")
            before = text.get("beforeEarnedDescription", "")
            after = text.get("afterEarnedDescription", "")
            if not name or len(name) > MAX_NAME:
                raise RuntimeError(f"{vendor} {locale}: name must be 1...{MAX_NAME} characters")
            for label, value in (("before", before), ("after", after)):
                if not value or len(value) > MAX_DESCRIPTION:
                    raise RuntimeError(
                        f"{vendor} {locale}: {label} description must be 1...{MAX_DESCRIPTION}"
                    )
        image = item.get("image", {})
        image_path = manifest_path.parent / image.get("path", "")
        if APPLY:
            # Uploads come only from the exact bytes committed at the pinned source commit.
            image_path, content = _git_tracked_bytes(image_path)
        else:
            content = image_path.read_bytes()
        if hashlib.sha256(content).hexdigest() != image.get("sha256"):
            raise RuntimeError(f"{vendor}: image checksum does not match the manifest")
        if _png_size(content) not in {(512, 512), (1024, 1024)}:
            raise RuntimeError(f"{vendor}: image must be 512x512 or 1024x1024")
        item["_image"] = (image_path.name, content)
    if total > 1000:
        raise RuntimeError(f"achievements total {total} points; Game Center allows 1000")

    for board in leaderboards:
        vendor = board.get("vendorIdentifier", "")
        if not vendor.startswith(LEADERBOARD_PREFIX) or not re.fullmatch(r"[A-Za-z0-9._]+", vendor):
            raise RuntimeError(f"bad leaderboard ID: {vendor!r}")
        if board.get("defaultFormatter") not in FORMATTERS:
            raise RuntimeError(f"{vendor}: unsupported formatter")
        if board.get("scoreSortType") not in {"ASC", "DESC"}:
            raise RuntimeError(f"{vendor}: scoreSortType must be ASC or DESC")
        if board.get("submissionType") not in {"BEST_SCORE", "MOST_RECENT_SCORE"}:
            raise RuntimeError(f"{vendor}: bad submissionType")
        localizations = board.get("localizations")
        if not isinstance(localizations, dict) or "en-US" not in localizations:
            raise RuntimeError(f"{vendor}: needs an en-US localization")
        for locale, text in localizations.items():
            if not text.get("name") or len(text["name"]) > MAX_NAME:
                raise RuntimeError(f"{vendor} {locale}: name must be 1...{MAX_NAME} characters")
    return achievements, leaderboards


# MARK: App Store Connect


def relation(kind: str, ident: str):
    return {"data": {"type": kind, "id": ident}}


def data_or_none(response):
    if response is None:
        return None
    return response.get("data")


def ensure_detail(client: Client, app_id: str):
    detail = data_or_none(client.call("GET", f"/v1/apps/{app_id}/gameCenterDetail", allow_404=True))
    if detail:
        print(f"Game Center: enabled (detail {detail['id']})")
        return detail["id"]
    if not APPLY:
        print("Game Center: not enabled for this app yet")
        return None
    created = client.call(
        "POST",
        "/v1/gameCenterDetails",
        {"data": {"type": "gameCenterDetails", "relationships": {"app": relation("apps", app_id)}}},
    )["data"]
    print(f"Game Center: enabled now (detail {created['id']})")
    return created["id"]


def editable_version(client: Client, owner_path: str, create_path: str, kind: str, owner_rel: str,
                     owner_kind: str, owner_id: str):
    """Return (version, editable). Creates a first version when none exists."""
    versions = client.list_all(f"{owner_path}/{owner_id}/versions", {"limit": 50})
    versions.sort(key=lambda v: v.get("attributes", {}).get("version") or 0)
    if versions:
        latest = versions[-1]
        state = latest.get("attributes", {}).get("state")
        return latest, state in EDITABLE_STATES
    if not APPLY:
        return None, False
    created = client.call(
        "POST",
        create_path,
        {"data": {"type": kind, "relationships": {owner_rel: relation(owner_kind, owner_id)}}},
    )["data"]
    return created, True


def sync_localizations(client, version_id, versions_path, create_path, kind, wanted, editable,
                       label, fields):
    """Creates missing locales and updates changed text. Returns {locale: localization}."""
    existing = {
        loc["attributes"]["locale"]: loc
        for loc in client.list_all(f"{versions_path}/{version_id}/localizations", {"limit": 50})
    }
    ok = True
    result = {}
    for locale, text in wanted.items():
        attributes = {field: text[field] for field in fields if field in text}
        current = existing.get(locale)
        if current is None:
            if not (APPLY and editable):
                print(f"  {label} {locale}: missing")
                ok = False
                continue
            current = client.call(
                "POST",
                create_path,
                {"data": {"type": kind, "attributes": {"locale": locale, **attributes},
                          "relationships": {"version": relation(
                              versions_path.rsplit("/", 1)[-1], version_id)}}},
            )["data"]
            print(f"  {label} {locale}: created")
        elif any(current["attributes"].get(k) != v for k, v in attributes.items()):
            if not (APPLY and editable):
                print(f"  {label} {locale}: text differs from the manifest")
                ok = False
            else:
                client.call(
                    "PATCH",
                    f"{create_path}/{current['id']}",
                    {"data": {"type": kind, "id": current["id"], "attributes": attributes}},
                )
                print(f"  {label} {locale}: updated")
        result[locale] = current
    return result, ok


def ensure_image(client: Client, localization, file_name: str, content: bytes, label: str) -> bool:
    image = data_or_none(client.call(
        "GET", f"/v2/gameCenterAchievementLocalizations/{localization['id']}/image",
        allow_404=True))
    if image:
        state = (image.get("attributes", {}).get("assetDeliveryState") or {}).get("state")
        if state in {"COMPLETE", "UPLOAD_COMPLETE", None}:
            return True
        print(f"  {label}: image state {state}")
        return False
    if not APPLY:
        print(f"  {label}: image missing")
        return False
    reservation = client.call(
        "POST",
        "/v2/gameCenterAchievementImages",
        {"data": {"type": "gameCenterAchievementImages",
                  "attributes": {"fileName": file_name, "fileSize": len(content)},
                  "relationships": {"localization": relation(
                      "gameCenterAchievementLocalizations", localization["id"])}}},
    )["data"]
    for operation in ordered_upload_operations(
        reservation["attributes"].get("uploadOperations"), len(content)
    ):
        client.upload(operation, content)
    client.call(
        "PATCH",
        f"/v2/gameCenterAchievementImages/{reservation['id']}",
        {"data": {"type": "gameCenterAchievementImages", "id": reservation["id"],
                  "attributes": {"uploaded": True}}},
    )
    print(f"  {label}: image uploaded")
    return True


def sync_achievement(client: Client, detail_id, remote, item) -> bool:
    vendor = item["vendorIdentifier"]
    print(f"\n{vendor}")
    if remote is None:
        if not APPLY:
            print("  achievement: missing")
            return False
        remote = client.call(
            "POST",
            "/v2/gameCenterAchievements",
            {"data": {"type": "gameCenterAchievements",
                      "attributes": {"referenceName": item["referenceName"],
                                     "vendorIdentifier": vendor, "points": item["points"],
                                     "showBeforeEarned": item["showBeforeEarned"],
                                     "repeatable": False},
                      "relationships": {"gameCenterDetail": relation(
                          "gameCenterDetails", detail_id)}}},
        )["data"]
        print(f"  achievement: created id={remote['id']}")
    else:
        attributes = remote.get("attributes", {})
        drift = [
            key for key in ("points", "showBeforeEarned")
            if attributes.get(key) != item[key]
        ]
        if drift:
            print(f"  achievement: {', '.join(drift)} differ from the manifest (left unchanged)")
            return False
        print(f"  achievement: exists id={remote['id']}")

    version, editable = editable_version(
        client, "/v2/gameCenterAchievements", "/v2/gameCenterAchievementVersions",
        "gameCenterAchievementVersions", "achievement", "gameCenterAchievements", remote["id"])
    if version is None:
        print("  version: missing")
        return False
    localizations, ok = sync_localizations(
        client, version["id"], "/v2/gameCenterAchievementVersions",
        "/v2/gameCenterAchievementLocalizations", "gameCenterAchievementLocalizations",
        item["localizations"], editable, "localization",
        ("name", "beforeEarnedDescription", "afterEarnedDescription"))
    file_name, content = item["_image"]
    for locale, localization in localizations.items():
        ok = ensure_image(client, localization, file_name, content, f"image {locale}") and ok
    return ok


def sync_leaderboard(client: Client, detail_id, remote, board) -> bool:
    vendor = board["vendorIdentifier"]
    print(f"\n{vendor}")
    if remote is None:
        if not APPLY:
            print("  leaderboard: missing")
            return False
        remote = client.call(
            "POST",
            "/v2/gameCenterLeaderboards",
            {"data": {"type": "gameCenterLeaderboards",
                      "attributes": {key: board[key] for key in (
                          "referenceName", "vendorIdentifier", "defaultFormatter",
                          "submissionType", "scoreSortType")},
                      "relationships": {"gameCenterDetail": relation(
                          "gameCenterDetails", detail_id)}}},
        )["data"]
        print(f"  leaderboard: created id={remote['id']}")
    else:
        print(f"  leaderboard: exists id={remote['id']}")
    version, editable = editable_version(
        client, "/v2/gameCenterLeaderboards", "/v2/gameCenterLeaderboardVersions",
        "gameCenterLeaderboardVersions", "leaderboard", "gameCenterLeaderboards", remote["id"])
    if version is None:
        print("  version: missing")
        return False
    _, ok = sync_localizations(
        client, version["id"], "/v2/gameCenterLeaderboardVersions",
        "/v2/gameCenterLeaderboardLocalizations", "gameCenterLeaderboardLocalizations",
        board["localizations"], editable, "localization", ("name", "formatterSuffix"))
    return ok


def by_vendor(items):
    result = {}
    for item in items:
        vendor = item.get("attributes", {}).get("vendorIdentifier")
        if vendor in result:
            raise RuntimeError(f"duplicate remote ID {vendor}")
        result[vendor] = item
    return result


def main() -> None:
    achievements, leaderboards = load_manifest()
    client = Client()
    apps = client.list_all("/v1/apps", {"filter[bundleId]": EXPECTED_BUNDLE_ID, "limit": 10})
    if len(apps) != 1:
        raise RuntimeError(f"Expected one {EXPECTED_BUNDLE_ID} app, found {len(apps)}")
    app = apps[0]
    print(f"Mode: {'APPLY' if APPLY else 'AUDIT ONLY'}")
    print(f"App: {app.get('attributes', {}).get('name')} ({EXPECTED_BUNDLE_ID}) id={app['id']}")
    print(f"Manifest: {len(achievements)} achievements "
          f"({sum(a['points'] for a in achievements)} points), {len(leaderboards)} leaderboards")

    detail_id = ensure_detail(client, app["id"])
    if detail_id is None:
        raise RuntimeError("Audit completed: Game Center is not enabled; run with apply")
    remote_achievements = by_vendor(client.list_all(
        f"/v1/gameCenterDetails/{detail_id}/gameCenterAchievementsV2", {"limit": 200}))
    remote_leaderboards = by_vendor(client.list_all(
        f"/v1/gameCenterDetails/{detail_id}/gameCenterLeaderboardsV2", {"limit": 200}))
    unknown = sorted(set(remote_achievements) - {a["vendorIdentifier"] for a in achievements})
    if unknown:
        print(f"Note: App Store Connect also has achievements not in the manifest: {unknown}")

    ready = True
    for item in achievements:
        ready = sync_achievement(
            client, detail_id, remote_achievements.get(item["vendorIdentifier"]), item) and ready
    for board in leaderboards:
        ready = sync_leaderboard(
            client, detail_id, remote_leaderboards.get(board["vendorIdentifier"]), board) and ready

    if not ready:
        raise RuntimeError(
            f"{'Apply' if APPLY else 'Audit'} completed, but Game Center is not complete; "
            "see the findings above")
    print("\nGame Center achievements and leaderboards match the manifest.")
    if not APPLY:
        print("Audit completed without changing App Store Connect.")
    print("New achievements and leaderboards work in TestFlight/sandbox right away; they go live "
          "for players with the next app version submitted for review.")


if __name__ == "__main__":
    try:
        main()
    except (ApiError, RuntimeError, ValueError, KeyError) as error:
        print(f"::error::{error}", file=sys.stderr)
        raise SystemExit(1)
