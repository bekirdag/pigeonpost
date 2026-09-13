package dev.pigeonpost.inbox

object AppPolicy {
    // Bump when the terms change so existing sessions request fresh acceptance.
    const val TERMS_VERSION = "2026-09-13"
    const val TERMS_URL = "https://pigeonpost.dev/app-terms.html"
    const val PRIVACY_URL = "https://pigeonpost.dev/app-privacy.html"
    const val SUPPORT_URL = "https://pigeonpost.dev/app-support.html"
    const val DELETE_URL = "https://pigeonpost.dev/delete-account.html"
}
