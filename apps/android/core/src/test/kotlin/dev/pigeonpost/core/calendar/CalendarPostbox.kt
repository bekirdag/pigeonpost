package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.File
import java.io.IOException
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Instant
import kotlin.system.exitProcess

/** Opt-in protocol proof. Synthetic people, temporary state, loopback origins, no delivered emails. */
object CalendarPostbox {
    private var stage = "setup"
    private val control = HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build()
    private class Credentials(private val value: String) : TokenProvider {
        override val authorizationLease = AuthorizationLease()
        override suspend fun token(rejected: String?) = value
    }
    private class Person(val address: String, val credentials: Credentials, val api: PostboxClient, val calendar: CalendarClient)
    @JvmStatic fun main(args: Array<String>) {
        try {
            require(args.size == 1)
            runBlocking { qualify(WireJson.parseToJsonElement(File(args.single()).readText()).jsonObject) }
            println("PASS: Android calendar ownership, invitations, revisions, export and exact create/import retries.")
            exitProcess(0)
        } catch (failure: Throwable) {
            System.err.println("FAIL: Android calendar interop at $stage (${failure.javaClass.simpleName}).")
            if (failure is ApiException) System.err.println("Postbox status ${failure.status}, code ${failure.code?.takeIf { Regex("[a-z0-9_]{1,64}").matches(it) } ?: "unknown"}.")
            if (failure is AssertionError) System.err.println(failure.message)
            exitProcess(1)
        }
    }
    private fun origin(value: String): String {
        val url = value.toHttpUrl()
        require(url.scheme == "http" && url.host == "127.0.0.1" && url.port in 1..65535 && url.username.isEmpty() && url.password.isEmpty() && url.query == null && url.fragment == null && url.encodedPath == "/")
        return value.trimEnd('/')
    }
    private fun postbox(base: String, source: Credentials) = PostboxClient(source, (base + "/").toHttpUrl(), allowLoopbackForTests = true)
    private fun verify(value: Boolean, label: String) { if (!value) throw AssertionError(label) }
    private suspend fun rejected(code: Set<Int>, work: suspend () -> Unit): ApiException {
        try { work() } catch (failure: ApiException) { verify(failure.status in code, "The server returned the wrong refusal category"); return failure }
        throw AssertionError("The server accepted a forbidden or conflicting request")
    }
    private suspend fun lost(work: suspend () -> Unit) {
        try { work() } catch (failure: IOException) { verify(failure !is ApiException, "A response-loss test must lose transport"); return }
        throw AssertionError("The committed response unexpectedly arrived")
    }
    private fun request(base: String, path: String, body: JsonObject? = null): JsonObject {
        val builder = HttpRequest.newBuilder(URI.create(base + path))
        if (body != null) builder.header("content-type", "application/json").POST(HttpRequest.BodyPublishers.ofString(body.toString()))
        val response = control.send(builder.build(), HttpResponse.BodyHandlers.ofString())
        verify(response.statusCode() in 200..299, "Fixture control request failed")
        return WireJson.parseToJsonElement(response.body()).jsonObject
    }
    private fun mode(proxy: String, next: String) { request(proxy, "/__test/control", buildJsonObject { put("mode", next) }) }
    private fun attempts(proxy: String) = request(proxy, "/__test/attempts")["attempts"]!!.jsonArray.map { it.jsonObject }
    private fun sameAttempts(proxy: String) {
        val rows = attempts(proxy)
        verify(rows.size == 2, "An explicit retry must make exactly two total attempts")
        verify(rows[0]["sha"] == rows[1]["sha"], "A retry changed its exact body")
        verify(rows[0]["requestKey"] == rows[1]["requestKey"], "A retry changed its identity")
        verify(rows.all { it["status"]!!.jsonPrimitive.int in 200..299 }, "Both attempts must have been accepted by the server")
    }
    private fun feed(url: String): HttpResponse<String> {
        val uri = URI.create(url)
        require(uri.scheme == "http" && uri.host == "127.0.0.1" && uri.port in 1..65535 && uri.userInfo == null && uri.fragment == null && uri.path.startsWith("/v1/calendar/feed/"))
        return control.send(HttpRequest.newBuilder(uri).GET().build(), HttpResponse.BodyHandlers.ofString())
    }
    private suspend fun qualify(setup: JsonObject) {
        val base = origin(setup["postbox"]!!.jsonPrimitive.content)
        val proxy = origin(setup["lossProxy"]!!.jsonPrimitive.content)
        fun person(name: String): Person {
            val data = setup["people"]!!.jsonObject[name]!!.jsonObject
            val credentials = Credentials(data["token"]!!.jsonPrimitive.content)
            val api = postbox(base, credentials)
            val address = data["address"]!!.jsonPrimitive.content
            return Person(address, credentials, api, CalendarClient(api, address, credentials.authorizationLease.child()))
        }
        val alice = person("alice"); val bob = person("bob"); val carol = person("carol")
        val uncertain = CalendarClient(postbox(proxy, alice.credentials), alice.address, alice.credentials.authorizationLease.child())
        val zone = CalendarTime.zone("Europe/Istanbul")
        val day = CalendarDay.at(Instant.now(), zone).plus(2)
        val from = day.plus(-1).start(zone); val through = day.plus(6).start(zone)
        stage = "capabilities, empty reads and isolated admission policies"
        verify("calendar" in alice.api.capabilities().enabled, "Calendar capability is missing")
        verify("calendar_email" !in alice.api.capabilities().enabled, "Fixture must never enable contact delivery")
        verify(alice.calendar.calendars().isEmpty(), "Reading calendars created a default")
        for (p in listOf(alice, bob, carol)) {
            p.api.setPolicy(p.address, "unnamed_messages", "show")
            p.api.setPolicy(p.address, "unknown_messages", "show")
            p.api.setPolicy(p.address, "low_reputation", "allow")
        }
        stage = "lost calendar creation response"
        mode(proxy, "after")
        lost { uncertain.createCalendar("Android fixture", "green", "android-folder-one") }
        mode(proxy, "off")
        val folder = uncertain.createCalendar("Android fixture", "green", "android-folder-one")
        sameAttempts(proxy)
        verify(alice.calendar.calendars().count { it.name == "Android fixture" } == 1, "Calendar retry duplicated the folder")
        fun eventBody(key: String, title: String, invited: Boolean = false, recurring: Boolean = false) = CalendarWritePayload(buildJsonObject {
            put("request_key", key); put("calendar_id", folder.id); put("title", title)
            put("description", "Private synthetic notes"); put("location", "Fixture room")
            put("all_day", false); put("start", day.text + "T09:00"); put("end", day.text + "T10:00"); put("tzid", zone.id)
            put("privacy", "default"); put("transparency", "opaque"); put("locale", "en")
            if (recurring) put("rrule", "FREQ=DAILY;COUNT=3")
            putJsonArray("reminders") { add(buildJsonObject { put("minutes_before", 10); put("channel", "push") }) }
            if (invited) {
                putJsonArray("attendees") {
                    add(buildJsonObject { put("handle", bob.address) })
                    add(buildJsonObject { put("contact", "guest@calendar-fixture.invalid"); put("name", "External fixture guest") })
                }
                put("meeting", buildJsonObject { put("enabled", true); put("waiting", "guests"); put("admit", "hosts"); put("allow_knock", false); put("transport", "mesh") })
            }
        })
        stage = "lost event creation response and exact replay"
        val body = eventBody("android-event-one", "Android review", invited = true)
        mode(proxy, "after"); lost { uncertain.createEvent(body) }; mode(proxy, "off")
        val created = uncertain.createEvent(body)
        sameAttempts(proxy)
        val key = CalendarEventKey(created.event.id)
        verify(alice.calendar.occurrences(from, through).count { it.eventId == key.eventId } == 1, "Event retry duplicated the event")
        rejected(setOf(409)) { alice.calendar.createEvent(eventBody("android-event-one", "Changed draft", invited = true)) }
        rejected(setOf(403, 404)) { carol.calendar.detail(key) }
        rejected(setOf(403, 404)) { bob.calendar.createEvent(body) }

        stage = "invitation privacy, guest links and optimistic revisions"
        verify(bob.calendar.invitations(false).any { it.eventId == key.eventId }, "The invited mailbox cannot see its invitation")
        val attendeeView = bob.calendar.detail(key)
        verify(attendeeView.event.canEdit != true, "An attendee was granted organizer editing")
        verify(attendeeView.event.attendees.orEmpty().filter { it.kind == "external" }.all { it.contact == null && it.proof == null }, "A contact guest's private address/proof leaked to an attendee")
        val guest = created.event.attendees.orEmpty().single { it.kind == "external" }
        val link = alice.calendar.guestLink(key.eventId, guest.attendeeId)
        verify(URI.create(link.url).fragment?.contains("t=") == true, "The organizer's guest link has no private fragment")
        stage = "attendee cannot mint an organizer guest link"
        rejected(setOf(403, 404)) { bob.calendar.guestLink(key.eventId, guest.attendeeId) }
        bob.calendar.rsvp(key, CalendarAnswer.ACCEPTED, CalendarAnswerScope.ALL)
        stage = "organizer revision after independent RSVP"
        val fresh = alice.calendar.detail(key)
        val changed = alice.calendar.updateEvent(key.eventId, CalendarWritePayload(buildJsonObject {
            put("expected_revision", fresh.event.revision!!); put("scope", "all"); put("title", "Updated Android review")
        }))
        verify(changed.event.title == "Updated Android review", "The current revision could not be edited")
        verify(changed.event.attendees.orEmpty().size == fresh.event.attendees.orEmpty().size && changed.event.meeting?.code == fresh.event.meeting?.code, "A text edit erased attendees or the scheduled meeting")
        stage = "conflicting organizer revision"
        rejected(setOf(409)) { alice.calendar.updateEvent(key.eventId, CalendarWritePayload(buildJsonObject {
            put("expected_revision", fresh.event.revision!!); put("scope", "all"); put("title", "Stale edit")
        })) }
        bob.calendar.reminders(key.eventId, listOf(CalendarReminder(5), CalendarReminder(30)))
        verify(bob.calendar.detail(key).event.reminders.orEmpty().map { it.minutesBefore }.toSet() == setOf(5, 30), "Personal reminders were not retained")

        stage = "recurring occurrence identity and cancellation scopes"
        val series = alice.calendar.createEvent(eventBody("android-series-one", "Recurring fixture", recurring = true))
        val instances = alice.calendar.occurrences(from, through).filter { it.eventId == series.event.id }.sortedBy { it.startsAt }
        verify(instances.size == 3, "The recurring series did not expand into three occurrences")
        val selected = instances[1].key
        val occurrence = alice.calendar.detail(selected)
        val moved = alice.calendar.updateEvent(series.event.id, CalendarWritePayload(buildJsonObject {
            put("expected_revision", occurrence.event.revision!!); put("scope", "this"); put("occurrence_id", selected.occurrenceId)
            put("start", day.plus(1).text + "T11:00"); put("end", day.plus(1).text + "T12:00"); put("tzid", zone.id)
        }))
        val movedAgain = alice.calendar.detail(selected)
        verify(movedAgain.occurrence?.occurrenceId == selected.occurrenceId, "Moving an occurrence changed its opaque identity")
        verify(movedAgain.occurrence?.start == day.plus(1).text + "T11:00", "The occurrence override was not effective")
        alice.calendar.cancelEvent(selected, moved.event.revision!!, CalendarEventScope.THIS)
        verify(alice.calendar.detail(selected).occurrence?.status == "cancelled", "One-occurrence cancellation was not retained")

        stage = "ICS projection, private settings and revocable subscription feeds"
        val ics = String(bob.calendar.ics(key.eventId))
        verify("BEGIN:VCALENDAR" in ics && "VTIMEZONE" in ics, "ICS export lost its calendar/timezone")
        verify("guest@calendar-fixture.invalid" !in ics && "#t=" !in ics, "ICS export leaked another guest or a credential")
        alice.calendar.saveSettings(buildJsonObject { put("timezone", zone.id); put("previews", false); put("week_start", 1) })
        alice.calendar.saveSettings(buildJsonObject { put("previews", true) })
        verify(alice.calendar.settings().timezone == zone.id, "A privacy-only update reset the saved zone")
        val subscription = alice.calendar.setFeed(folder.id, true, "busy")
        val oldFeed = subscription.url!!
        verify(alice.calendar.feed(folder.id).url == null, "A read disclosed the one-time feed secret")
        val exported = feed(oldFeed)
        verify(exported.statusCode() == 200 && "Updated Android review" !in exported.body() && "Private synthetic notes" !in exported.body(), "Busy feed exposed private details")
        val rotated = alice.calendar.setFeed(folder.id, true, "busy")
        verify(feed(oldFeed).statusCode() in setOf(401, 403, 404, 410), "The old feed survived rotation")
        verify(feed(rotated.url!!).statusCode() == 200, "The rotated feed was not usable")
        alice.calendar.setFeed(folder.id, false, "busy")
        verify(feed(rotated.url!!).statusCode() in setOf(401, 403, 404, 410), "The revoked feed remained usable")

        stage = "local ICS preview and committed import-confirmation retry"
        val localIcs = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Pigeonpost fixture//EN\r\nBEGIN:VEVENT\r\nUID:android-import@calendar-fixture.invalid\r\nDTSTART;VALUE=DATE:${day.text.replace("-", "")}\r\nDTEND;VALUE=DATE:${day.plus(1).text.replace("-", "")}\r\nSUMMARY:Local import fixture\r\nATTENDEE:mailto:never-send@calendar-fixture.invalid\r\nURL:https://never-fetch.invalid/private.ics\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        val preview = alice.calendar.previewImport(localIcs.toByteArray(), folder.id)
        verify(preview.events.orEmpty().isNotEmpty(), "The local calendar had no importable preview")
        val selection = preview.events.orEmpty().filter { it.importable != false }.map { it.index }
        mode(proxy, "after"); lost { uncertain.confirmImport(preview.draftId, selection, folder.id) }; mode(proxy, "off")
        val imported = uncertain.confirmImport(preview.draftId, selection, folder.id)
        sameAttempts(proxy)
        val replayed = alice.calendar.confirmImport(preview.draftId, selection, folder.id)
        verify(imported.created.orEmpty().map { it.id } == replayed.created.orEmpty().map { it.id }, "Import replay changed its event identities")
        verify(alice.calendar.occurrences(from, through).count { it.title == "Local import fixture" } == selection.size, "Import confirmation duplicated events")
        val destination = alice.calendar.createCalendar("Other fixture", "blue", "android-folder-other")
        verify(rejected(setOf(409)) { alice.calendar.confirmImport(preview.draftId, selection, destination.id) }.code == "idempotency_conflict", "Changing a completed import destination was accepted")
        rejected(setOf(403, 404)) { carol.calendar.confirmImport(preview.draftId, selection, folder.id) }

        stage = "unknown availability remains distinct from free"
        val availability = alice.calendar.freeBusy(listOf(alice.address, bob.address), from, through)
        verify(availability[alice.address] is CalendarAvailability.Known && availability[bob.address] == CalendarAvailability.Unknown, "Another mailbox's unknown availability was shown as free")

        stage = "native draft preserves real participant and meeting policy"
        val reviewed = alice.calendar.detail(key)
        val draft = CalendarEventDraft.from(CalendarEditTarget(reviewed, key, CalendarEventScope.ALL, "fixture-editor"), zone.id)
        verify(draft.changedFields().isEmpty(), "Opening an editor changed the event")
        val edited = alice.calendar.updateEvent(key.eventId, draft.copy(description = "Revised synthetic notes").editPayload())
        verify(edited.event.attendees?.size == reviewed.event.attendees?.size && edited.event.meeting?.code == reviewed.event.meeting?.code && edited.event.meeting?.waiting == reviewed.event.meeting?.waiting,
            "A draft detail edit changed participants or meeting admission")
        rejected(setOf(409)) { alice.calendar.updateEvent(key.eventId, draft.copy(title = "Stale editor intent").editPayload()) }

        stage = "calendar store restores a committed uncertain create without a replacement intent"
        coroutineScope {
            val actor = AuthorizedStoreScope(this).apply { activate(alice.credentials.authorizationLease) }
            val custody = MemoryCalendarAttemptStorage()
            val api = postbox(proxy, alice.credentials)
            var store = CalendarStore(api, custody, actor, allowVolatileStorage = true)
            try {
                store.onMailbox(alice.address, setOf("calendar")); store.loadAttempts()!!.join()
                val authored = CalendarEventDraft(store.state.value.contextId, day.text, tzid = zone.id, title = "Native durable intent", calendarId = folder.id).fields()
                mode(proxy, "after"); store.createEvent(authored, store.state.value.contextId)!!.join()
                verify(store.state.value.pending.size == 1, "The store lost the accepted uncertain intent")
                val original = store.state.value.pending.single()
                store.createEvent(JsonObject(authored + ("title" to JsonPrimitive("Changed draft"))), store.state.value.contextId)!!.join()
                verify(attempts(proxy).size == 1, "A changed draft sent a replacement create")
                store.reset(); store = CalendarStore(api, custody, actor, allowVolatileStorage = true)
                store.onMailbox(alice.address, setOf("calendar")); store.loadAttempts()!!.join()
                verify(store.state.value.pending.single().encoded == original.encoded && attempts(proxy).size == 1, "Restoration changed or automatically sent the original attempt")
                mode(proxy, "off"); store.retry(original.id, store.state.value.contextId)!!.join()
                sameAttempts(proxy)
                verify(store.state.value.pending.isEmpty(), "A confirmed intent was not cleared")
                verify(alice.calendar.occurrences(from, through).count { it.title == "Native durable intent" } == 1, "A restored store retry duplicated the event")
            } finally { store.reset(); actor.retire() }
        }
    }
}
