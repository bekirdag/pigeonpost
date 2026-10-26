package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException
import java.time.Instant
import java.util.concurrent.Executors

class CalendarStoreTest {
    private data class Call(val mailbox: String, val method: String, val path: String, val query: Map<String, String>, val body: String?) {
        fun fields() = WireJson.parseToJsonElement(body!!).jsonObject
    }
    private class Transport : CalendarTransport {
        val calls = mutableListOf<Call>()
        val rows = mutableMapOf<String, List<EventOccurrence>>()
        val receipts = mutableMapOf<String, ByteArray>()
        var settings = CalendarSettings(timezone = "UTC", weekStart = 1)
        var hook: suspend (Call) -> ByteArray? = { null }
        var afterCreate: suspend (Call) -> Unit = {}
        var createFailure: Exception? = null
        override suspend fun calendarRequest(mailbox: String, method: String, path: String, query: Map<String, String>, body: ByteArray?, mediaType: String): ByteArray {
            val call = Call(mailbox, method, path, query, body?.toString(Charsets.UTF_8)); calls += call
            hook(call)?.let { return it }
            return when {
                path == "/v1/calendars" && method == "GET" -> """{"calendars":[{"id":"cal_one","name":"Calendar","is_default":true,"revision":1}]}""".toByteArray()
                path == "/v1/calendar/settings" -> WireJson.encodeToString(CalendarSettings.serializer(), settings).toByteArray()
                path == "/v1/calendar/invitations" -> """{"invitations":[]}""".toByteArray()
                path == "/v1/calendar/events" && method == "GET" -> WireJson.encodeToString(EventOccurrencesResponse.serializer(), EventOccurrencesResponse(rows[mailbox].orEmpty())).toByteArray()
                path == "/v1/calendar/events" && method == "POST" -> {
                    createFailure?.let { throw it }
                    val fields = call.fields(); val key = fields["request_key"]!!.jsonPrimitive.content
                    val result = receipts.getOrPut(key) {
                        val id = "evt_${receipts.size + 1}"
                        val title = fields["title"]?.jsonPrimitive?.content ?: "Created"
                        rows[mailbox] = rows[mailbox].orEmpty() + EventOccurrence("occ_$id", id, title = title, allDay = true, startDate = "2026-10-28", endDate = "2026-10-29")
                        """{"event":{"id":"$id","title":"$title","revision":1}}""".toByteArray()
                    }
                    afterCreate(call); result
                }
                path == "/v1/calendars" && method == "POST" -> """{"calendar":{"id":"cal_created","name":"Created","revision":1}}""".toByteArray()
                path.startsWith("/v1/calendar/import/") -> """{"created":[{"id":"evt_imported"}]}""".toByteArray()
                path.startsWith("/v1/calendar/events/") && method == "GET" -> {
                    val id = path.substringAfterLast('/')
                    buildJsonObject {
                        put("event", buildJsonObject { put("id", id); put("revision", 7); put("title", "Reviewed"); put("can_edit", true) })
                        query["occurrence_id"]?.let { put("occurrence", buildJsonObject { put("occurrence_id", it) }) }
                    }.toString().toByteArray()
                }
                else -> "{}".toByteArray()
            }
        }
        fun creates() = calls.filter { it.path == "/v1/calendar/events" && it.method == "POST" }
    }
    private class Fixture(val transport: Transport, val storage: CalendarAttemptStorage, val base: CoroutineScope) {
        var clock = Instant.parse("2026-10-26T23:30:00Z")
        val authorization = AuthorizationLease()
        val scope = AuthorizedStoreScope(base).apply { activate(authorization) }
        val store = CalendarStore(transport, storage, scope, now = { clock }, allowVolatileStorage = true)
        val context get() = store.state.value.contextId
        fun connect(mailbox: String = "/alice") = store.onMailbox(mailbox, setOf("calendar"))
        suspend fun ready() { store.loadAttempts()!!.join() }
        suspend fun refreshed() { store.refresh()!!.join() }
        fun fields(title: String = "Original") = buildJsonObject { put("title", title) }
    }
    private suspend fun world(storage: CalendarAttemptStorage = MemoryCalendarAttemptStorage(), configure: Boolean = true, work: suspend Fixture.() -> Unit) {
        val dispatcher = Executors.newSingleThreadExecutor { Thread(it, "calendar-store-test").apply { isDaemon = true } }.asCoroutineDispatcher()
        val base = CoroutineScope(SupervisorJob() + dispatcher)
        val fixture = Fixture(Transport(), storage, base)
        try { withTimeout(8_000) { withContext(dispatcher) { if (configure) { fixture.connect(); fixture.ready() }; fixture.work() } } }
        finally { withContext(dispatcher) { fixture.store.reset(); fixture.scope.retire() }; base.cancel(); dispatcher.close() }
    }
    private fun pending(id: String = "restored-key", at: Long = Instant.parse("2026-10-26T23:30:00Z").epochSecond) =
        CalendarPendingAttempt(id, "/alice", CalendarAttemptKind.EVENT, CalendarWritePayload(buildJsonObject { put("request_key", id); put("title", "Restored") }).text, at)

    @Test fun anInactiveAuthorizationCannotEvenLoadPrivateLocalAttempts() = runBlocking<Unit> {
        var loads = 0
        val memory = MemoryCalendarAttemptStorage()
        val storage = object : CalendarAttemptStorage by memory {
            override suspend fun load(mailbox: String, authorization: AuthorizationLease): List<CalendarPendingAttempt> { loads++; return memory.load(mailbox, authorization) }
        }
        world(storage, configure = false) {
            scope.retire(); connect()
            assertFalse(store.state.value.enabled); assertEquals(0, loads); assertTrue(transport.calls.isEmpty())
        }
    }
    @Test fun preferencesChooseTodayInTheSavedZoneAndAnEmptySelectionMeansNone() = runBlocking<Unit> { world {
        transport.settings = CalendarSettings(timezone = "America/Los_Angeles", weekStart = 0)
        refreshed()
        assertEquals("2026-10-26", store.state.value.selectedDay.text)
        assertEquals(0, store.state.value.weekStart)
        assertTrue(transport.calls.none { it.path == "/v1/calendars" && it.method != "GET" })
        val before = transport.calls.count { it.path == "/v1/calendar/events" }
        store.selectFolders(emptySet()); refreshed()
        assertEquals(before, transport.calls.count { it.path == "/v1/calendar/events" })
        assertEquals(emptySet<String>(), store.state.value.selectedFolders)
    } }
    @Test fun aLateOldMailboxListCannotReplaceTheCurrentCalendar() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        transport.rows["/bob"] = listOf(EventOccurrence("occ_bob", "evt_bob", title = "Bob"))
        transport.hook = { call -> if (call.mailbox == "/alice" && call.path == "/v1/calendar/events" && call.method == "GET") {
            entered.complete(Unit); withContext(NonCancellable) { release.await() }
            """{"occurrences":[{"event_id":"evt_old","occurrence_id":"occ_old","title":"Old"}]}""".toByteArray()
        } else null }
        val old = store.refresh()!!; entered.await()
        val prior = context; connect("/bob"); ready(); refreshed()
        release.complete(Unit); old.join()
        assertNotEquals(prior, context); assertEquals("/bob", store.state.value.mailbox)
        assertEquals(listOf("evt_bob"), store.state.value.occurrences.map { it.eventId })
    } }
    @Test fun anUnknownCreationKeepsItsExactBodyAndBlocksAChangedDraft() = runBlocking<Unit> { world {
        var lose = true
        transport.afterCreate = { if (lose) { lose = false; throw IOException("fixture committed response loss") } }
        store.createEvent(fields(), context)!!.join()
        val frozen = store.state.value.pending.single()
        assertEquals(1, transport.creates().size)
        assertEquals(frozen.encoded, transport.creates().single().body)
        store.createEvent(fields("Changed draft"), context)!!.join()
        assertEquals(1, transport.creates().size); assertEquals(frozen, store.state.value.pending.single())
        transport.rows["/alice"] = transport.rows["/alice"]!!.map { it.copy(title = "Edited since original acceptance") }
        var saved: CalendarCreated? = null
        store.retry(frozen.id, context) { saved = it }!!.join(); refreshed()
        assertEquals(2, transport.creates().size)
        assertEquals(transport.creates()[0].body, transport.creates()[1].body)
        assertEquals("evt_1", saved!!.eventId); assertTrue(store.state.value.pending.isEmpty())
        assertEquals("Edited since original acceptance", store.state.value.occurrences.single().title)
    } }
    @Test fun storageFailurePreventsTheFirstNetworkWrite() = runBlocking<Unit> {
        val memory = MemoryCalendarAttemptStorage()
        val storage = object : CalendarAttemptStorage by memory {
            override suspend fun save(mailbox: String, attempts: List<CalendarPendingAttempt>, authorization: AuthorizationLease) { throw CalendarAttemptStorageException() }
        }
        world(storage) {
            store.createEvent(fields(), context)!!.join()
            assertTrue(transport.creates().isEmpty()); assertTrue(store.state.value.error != null)
        }
    }
    @Test fun restoringAnAttemptNeverAutomaticallySendsIt() = runBlocking<Unit> {
        val memory = MemoryCalendarAttemptStorage(); val record = pending()
        memory.save("/alice", listOf(record), AuthorizationLease())
        world(memory) {
            refreshed(); assertTrue(transport.creates().isEmpty()); assertEquals(listOf(record), store.state.value.pending)
            store.retry(record.id, context)!!.join()
            assertEquals(record.encoded, transport.creates().single().body)
            assertTrue(memory.load("/alice", authorization).isEmpty())
        }
    }
    @Test fun expiredAttemptsRemainBlockedUntilExplicitDiscard() = runBlocking<Unit> {
        val memory = MemoryCalendarAttemptStorage(); val record = pending(at = 1)
        memory.save("/alice", listOf(record), AuthorizationLease())
        world(memory) {
            store.retry(record.id, context)!!.join()
            assertTrue(transport.creates().isEmpty()); assertEquals(listOf(record), store.state.value.pending)
            store.discard(record.id, context)!!.join()
            assertTrue(store.state.value.pending.isEmpty()); assertTrue(transport.creates().isEmpty())
        }
    }
    @Test fun aDefiniteFirstRejectionClearsTheIntentButDeniedUnknownReplayDoesNot() = runBlocking<Unit> { world {
        transport.createFailure = ApiException(422, "calendar_input")
        store.createEvent(fields(), context)!!.join()
        assertTrue(store.state.value.pending.isEmpty())
        transport.createFailure = null; transport.afterCreate = { throw IOException("fixture loss") }
        store.createEvent(fields(), context)!!.join()
        val record = store.state.value.pending.single()
        transport.createFailure = ApiException(403, "forbidden")
        store.retry(record.id, context)!!.join()
        assertEquals(listOf(record), store.state.value.pending)
    } }
    @Test fun unreadableRecoveryDataBlocksCreationButNotCalendarReads() = runBlocking<Unit> {
        val memory = MemoryCalendarAttemptStorage()
        val storage = object : CalendarAttemptStorage by memory {
            override suspend fun load(mailbox: String, authorization: AuthorizationLease): List<CalendarPendingAttempt> = throw CalendarAttemptStorageException()
        }
        world(storage) {
            assertTrue(store.state.value.attemptsUnreadable); assertFalse(store.state.value.canCreate)
            store.createEvent(fields(), context)!!.join(); assertTrue(transport.creates().isEmpty())
            refreshed(); assertEquals(1, store.state.value.folders.size)
        }
    }
    @Test fun anOldConfirmationOrEditorCannotActOnANewContext() = runBlocking<Unit> { world {
        val reviewed = context; connect("/bob"); ready()
        assertNull(store.createEvent(fields(), reviewed))
        assertNull(store.cancel(CalendarEventKey("evt_one"), 7, CalendarEventScope.ALL, reviewed))
        assertNull(store.edit(CalendarEventKey("evt_one"), CalendarWritePayload(buildJsonObject { put("expected_revision", 7); put("scope", "all") }), reviewed))
        assertTrue(transport.calls.none { it.method != "GET" }); assertTrue(store.state.value.error == null)
    } }
    @Test fun acceptedReauthenticationCannotLetALateCreateClearTheRestoredAttempt() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        transport.afterCreate = { entered.complete(Unit); withContext(NonCancellable) { release.await() } }
        var callbacks = 0
        val old = store.createEvent(fields(), context) { callbacks++ }!!
        entered.await(); val record = store.state.value.pending.single(); val prior = context
        scope.retire(); authorization.retire(); scope.activate(AuthorizationLease()); connect(); ready()
        assertNotEquals(prior, context); assertEquals(listOf(record), store.state.value.pending)
        release.complete(Unit); old.join()
        assertEquals(0, callbacks); assertEquals(listOf(record), store.state.value.pending)
        assertEquals(1, transport.creates().size)
    } }
    @Test fun attemptReloadWaitsForTheCurrentWriteAndCannotResurrectOldState() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        transport.afterCreate = { entered.complete(Unit); release.await() }
        val create = store.createEvent(fields(), context)!!; entered.await()
        val read = store.loadAttempts()!!
        assertFalse(store.state.value.attemptsLoaded)
        release.complete(Unit); create.join(); read.join()
        assertTrue(store.state.value.attemptsLoaded); assertTrue(store.state.value.pending.isEmpty())
        assertEquals(1, transport.creates().size)
    } }
    @Test fun aMissingEventInvalidatesSiblingDetailsAndAnOlderListResponse() = runBlocking<Unit> { world {
        val key = CalendarEventKey("evt_one", "occ_original")
        val detailEntered = CompletableDeferred<Unit>(); val listEntered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        transport.hook = { call ->
            when {
                call.path == "/v1/calendar/events" && call.method == "GET" -> {
                    listEntered.complete(Unit); release.await()
                    """{"occurrences":[{"event_id":"evt_one","occurrence_id":"occ_original"}]}""".toByteArray()
                }
                call.path == "/v1/calendar/events/evt_one" && call.query["occurrence_id"] != null -> {
                    detailEntered.complete(Unit); release.await()
                    """{"event":{"id":"evt_one"},"occurrence":{"occurrence_id":"occ_original"}}""".toByteArray()
                }
                call.path == "/v1/calendar/events/evt_one" -> throw ApiException(404, "not_found")
                else -> null
            }
        }
        val detail = store.loadDetail(key)!!; val list = store.refresh()!!
        detailEntered.await(); listEntered.await()
        store.loadDetail(CalendarEventKey("evt_one"))!!.join()
        release.complete(Unit); detail.join(); list.join()
        assertTrue(store.state.value.details.isEmpty()); assertTrue(store.state.value.occurrences.isEmpty()); assertFalse(store.state.value.loading)
    } }
    @Test fun schedulingConflictDoesNotAutomaticallyRebaseOrResendTheEditor() = runBlocking<Unit> { world {
        transport.hook = { call -> if (call.method == "PATCH") throw ApiException(409, "revision_conflict") else null }
        val body = CalendarWritePayload(buildJsonObject { put("expected_revision", 7); put("scope", "all"); put("title", "Reviewed edit") })
        store.edit(CalendarEventKey("evt_one"), body, context)!!.join()
        assertTrue(store.state.value.revisionConflict)
        val calls = transport.calls.filter { it.method == "PATCH" }
        assertEquals(1, calls.size); assertEquals(body.text, calls.single().body)
    } }

    @Test fun failedConfirmationStorageKeepsOneAcceptedIntentUntilItCanBeCleared() = runBlocking<Unit> {
        val memory = MemoryCalendarAttemptStorage(); var denyClear = true
        val storage = object : CalendarAttemptStorage by memory {
            override suspend fun save(mailbox: String, attempts: List<CalendarPendingAttempt>, authorization: AuthorizationLease) {
                if (attempts.isEmpty() && denyClear) throw CalendarAttemptStorageException()
                memory.save(mailbox, attempts, authorization)
            }
        }
        world(storage) {
            store.createEvent(fields(), context)!!.join()
            val original = store.state.value.pending.single()
            store.createEvent(fields("Another draft"), context)!!.join()
            assertEquals(1, transport.creates().size)
            denyClear = false
            store.retry(original.id, context)!!.join()
            assertEquals(2, transport.creates().size); assertEquals(1, transport.receipts.size)
            assertEquals(transport.creates()[0].body, transport.creates()[1].body)
            assertTrue(store.state.value.pending.isEmpty())
        }
    }
    @Test fun aDoubleSubmitCannotStartTwoWritesBeforeEitherCoroutineRuns() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        transport.afterCreate = { entered.complete(Unit); release.await() }
        val first = store.createEvent(fields(), context)!!
        assertNull(store.createEvent(fields(), context))
        entered.await(); assertEquals(1, transport.creates().size)
        release.complete(Unit); first.join()
        assertTrue(store.state.value.pending.isEmpty()); assertEquals(1, transport.receipts.size)
    } }
    @Test fun restoredFolderAndImportIntentsKeepTheirOriginalBodies() = runBlocking<Unit> {
        val memory = MemoryCalendarAttemptStorage(); val at = pending().createdAt
        val folder = CalendarPendingAttempt("folder-key", "/alice", CalendarAttemptKind.CALENDAR, CalendarWritePayload(buildJsonObject {
            put("request_key", "folder-key"); put("name", "Restored folder"); put("color", "green"); put("future_field", "retained")
        }).text, at)
        val imported = CalendarPendingAttempt("import-key", "/alice", CalendarAttemptKind.IMPORT, CalendarWritePayload(buildJsonObject {
            put("select", JsonArray(listOf(JsonPrimitive(2), JsonPrimitive(0)))); put("calendar_id", "cal_one"); put("future_field", "retained")
        }).text, at, "imp_one")
        memory.save("/alice", listOf(folder, imported), AuthorizationLease())
        world(memory) {
            store.retry(folder.id, context)!!.join(); store.retry(imported.id, context)!!.join()
            val writes = transport.calls.filter { it.method == "POST" }
            assertEquals(listOf(folder.encoded, imported.encoded), writes.map { it.body })
            assertTrue(store.state.value.pending.isEmpty())
        }
    }
    @Test fun answeringAndPersonalRemindersReloadWithoutClosingTheSelectedOccurrence() = runBlocking<Unit> { world {
        val key = CalendarEventKey("evt_one", "occ_original")
        store.openEvent(key); store.loadDetail(key)!!.join()
        store.rsvp(key, CalendarAnswer.ACCEPTED, CalendarAnswerScope.THIS, context)!!.join()
        store.loadDetail(key)!!.join()
        assertEquals(key, store.state.value.openEvent)
        store.reminders(key.eventId, listOf(CalendarReminder(15, "push")), context)!!.join()
        store.loadDetail(key)!!.join()
        assertEquals(key, store.state.value.openEvent)
        assertTrue(store.state.value.details.containsKey(key))
        val answers = transport.calls.filter { it.path.endsWith("/rsvp") }
        assertEquals("this", answers.single().fields()["scope"]!!.jsonPrimitive.content)
        assertEquals("occ_original", answers.single().fields()["occurrence_id"]!!.jsonPrimitive.content)
        assertFalse("expected_revision" in answers.single().fields()) // Attendee revision is separate.
    } }
    @Test fun aLocalFileReadCannotSendItsBytesAsAReplacementActor() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        var delivered = false
        val original = store.previewLocalImport({ withContext(NonCancellable) { entered.complete(Unit); release.await(); "BEGIN:VCALENDAR".toByteArray() } }, null, context) { delivered = true }!!
        entered.await(); scope.retire(); scope.activate(AuthorizationLease()); connect(); ready()
        release.complete(Unit); original.join()
        assertFalse(delivered)
        assertTrue(transport.calls.none { it.path.endsWith("/import/preview") })
        assertTrue(store.state.value.attemptsLoaded)
    } }
    @Test fun aLatePersonalLinkCannotReachTheReplacementPresentation() = runBlocking<Unit> { world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        transport.hook = { call -> if (call.path.endsWith("/link")) withContext(NonCancellable) { entered.complete(Unit); release.await(); """{"url":"https://fixture.invalid/private-capability"}""".toByteArray() } else null }
        var delivered = false
        val original = store.guestLink("evt_one", "att_one", context) { delivered = true }!!
        entered.await(); connect("/bob"); ready(); release.complete(Unit); original.join()
        assertFalse(delivered); assertEquals("/bob", store.state.value.mailbox)
    } }
    @Test fun folderUpdatesUseReviewedRevisionAndStaleDeleteConfirmationsAreRefused() = runBlocking<Unit> { world {
        transport.hook = { call -> if (call.method == "PATCH" && call.path.startsWith("/v1/calendars/")) """{"calendar":{"id":"cal_one","revision":8}}""".toByteArray() else null }
        store.updateCalendar("cal_one", 7, buildJsonObject { put("name", "Renamed") }, context)!!.join()
        val edit = transport.calls.single { it.method == "PATCH" }
        assertEquals(7L, edit.fields()["expected_revision"]!!.jsonPrimitive.long)
        val old = context; connect("/bob"); ready()
        assertNull(store.deleteCalendar("cal_one", 7, old))
        assertTrue(transport.calls.none { it.method == "DELETE" })
    } }
    @Test fun aPreparedExportCannotWriteAfterSwitchingMailboxes() = runBlocking<Unit> { world {
        transport.hook = { call -> if (call.path.endsWith(".ics")) "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".toByteArray() else null }
        var packet: CalendarIcsExport? = null
        store.prepareIcsExport("evt_one", context) { packet = it }!!.join()
        assertNotNull(packet)
        connect("/bob"); ready()
        var opened = false
        try { packet!!.write { opened = true; java.io.ByteArrayOutputStream() }; fail("Old mailbox export must be retired") }
        catch (_: AuthorizationRetired) { }
        assertFalse(opened); assertTrue(authorization.isValid)
    } }
}
