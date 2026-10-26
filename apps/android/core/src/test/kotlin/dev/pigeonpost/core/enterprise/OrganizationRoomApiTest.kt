package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.net.InetAddress
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class OrganizationRoomApiTest {
    private class Tokens : TokenProvider {
        @Volatile var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        var acquisitions = 0; var invalidations = 0
        var entered: CompletableDeferred<Unit>? = null; var release: CompletableDeferred<Unit>? = null
        override suspend fun token(rejected: String?): String { acquisitions++; entered?.complete(Unit); release?.await(); return if (rejected == null) "fixture-first" else "fixture-renewed" }
        override suspend fun invalidate(rejected: String) { invalidations++ }
    }
    private lateinit var tokens: Tokens
    private lateinit var server: MockWebServer
    private lateinit var api: OrganizationRoomClient
    private val mailbox = "/k/member"
    private val catalog = """{"orgs":[{"id":"org_fixture","name":"Example","status":"active","my_member_id":"member_fixture","my_roles":["owner"],"my_mailbox":{"address":"/k/member","handle":"/example/alice"},"settings":{"rooms_creation":"admins"}}]}"""
    private val validGroup = """{"group":{"id":"room_fixture","name":"Review","org_id":"org_fixture","created_by":"/k/member","activation_state":"provisioning","my":{"state":"keying","role":"owner"}}}"""
    private fun payload() = OrganizationRoomCreate("org_fixture", "Review", "Notes", RoomJoinPolicy.MEMBERS, RoomPostPolicy.EVERYONE, true, "rk_fixture")
    private fun response(text: String) = MockResponse().setBody(text)
    private suspend fun <T> accepted(block: suspend () -> T): T = withContext(AuthorizationContext(tokens.grant.child())) { block() }
    @Before fun setup() {
        tokens = Tokens(); server = MockWebServer().apply { start(InetAddress.getByName("127.0.0.1"), 0) }
        api = OrganizationRoomClient(PostboxClient(tokens, server.url("/").newBuilder().host("127.0.0.1").build(), allowLoopbackForTests = true))
    }
    @After fun teardown() { server.shutdown() }

    @Test fun catalogAndDirectoryUseTheAcceptedCanonicalActorAndRequiredOrganization() = runBlocking<Unit> {
        server.enqueue(response(catalog)); server.enqueue(response("""{"rooms":[{"id":"room_fixture","name":"Review","join_policy":"public","member_count":2}]}"""))
        assertEquals("org_fixture", accepted { api.organizations(mailbox) }.forMailbox(mailbox).single().id)
        val room = accepted { api.rooms(mailbox, "org_fixture", "Tasarım & plan") }.single()
        assertEquals(RoomAction.JOIN, room.action)
        val first = server.takeRequest(); val next = server.takeRequest()
        assertEquals("/v1/orgs", first.path); assertEquals("Bearer fixture-first", first.getHeader("Authorization"))
        assertEquals(mailbox, first.getHeader("x-pigeonpost-identity")); assertEquals("org_fixture", next.requestUrl!!.queryParameter("org_id"))
        assertEquals("Tasarım & plan", next.requestUrl!!.queryParameter("q"))
    }
    @Test fun invalidOrganizationAndSearchNeverFallBackToThePersonalDirectoryOrAcquireTokens() = runBlocking<Unit> {
        for (id in listOf("", "../other", "x?org_id=other", " org_fixture", "ö", "x".repeat(129)))
            assertTrue(runCatching { accepted { api.rooms(mailbox, id, "") } }.exceptionOrNull() is IllegalArgumentException)
        assertTrue(runCatching { accepted { api.rooms(mailbox, "org_fixture", "x".repeat(81)) } }.isFailure)
        assertEquals(0, server.requestCount); assertEquals(0, tokens.acquisitions)
    }
    @Test fun creationRetainsExactlyTheSameBodyAndKeyThroughOrdinaryRenewal() = runBlocking<Unit> {
        server.enqueue(MockResponse().setResponseCode(401)); server.enqueue(response(validGroup))
        val input = payload(); val created = accepted { api.create(mailbox, input) }
        assertEquals("org_fixture", created.orgId); assertEquals("keying", created.state)
        val first = server.takeRequest(); val next = server.takeRequest()
        assertEquals("POST", first.method); assertEquals("/v1/groups", first.path)
        assertEquals(input.encoded, first.body.readUtf8()); assertEquals(input.encoded, next.body.readUtf8())
        assertEquals(mailbox, next.getHeader("x-pigeonpost-identity")); assertEquals("Bearer fixture-renewed", next.getHeader("Authorization"))
        assertEquals(2, tokens.acquisitions); assertEquals(0, tokens.invalidations)
    }
    @Test fun creationChallengesNeverReplayOrRenewAutomatically() = runBlocking<Unit> {
        server.enqueue(MockResponse().setResponseCode(401).setBody("""{"error":"step_up_required","acr_values":["2"],"max_age":600}"""))
        val failure = runCatching { accepted { api.create(mailbox, payload()) } }.exceptionOrNull() as ApiException
        assertTrue(failure.needsStrongSignIn); assertEquals(1, tokens.acquisitions); assertEquals(1, server.requestCount)
    }
    @Test fun aLostCreationResponseDoesNotRetryUntilTheCallerExplicitlyReusesTheIntent() = runBlocking<Unit> {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)); server.enqueue(response(validGroup))
        val input = payload()
        assertTrue(runCatching { accepted { api.create(mailbox, input) } }.isFailure); assertEquals(1, server.requestCount)
        accepted { api.create(mailbox, input) }
        assertEquals(input.encoded, server.takeRequest().body.readUtf8()); assertEquals(input.encoded, server.takeRequest().body.readUtf8())
        assertEquals(2, server.requestCount)
    }
    @Test fun creationRequiresTheExactOrganizationCreatorAndExplicitOwnerMembership() = runBlocking<Unit> {
        val original = WireJson.parseToJsonElement(validGroup).jsonObject.getValue("group").jsonObject
        val variants = listOf(
            JsonObject(original - "org_id"), JsonObject(original + ("org_id" to JsonPrimitive("org_other"))),
            JsonObject(original + ("created_by" to JsonPrimitive("/k/other"))),
            JsonObject(original - "my"), JsonObject(original + ("my" to buildJsonObject { put("role", "owner") })),
            JsonObject(original + ("my" to buildJsonObject { put("state", "keying"); put("role", "member") })),
            JsonObject(original + ("my" to buildJsonObject { put("state", "future"); put("role", "owner") })))
        for (value in variants) {
            server.enqueue(response(buildJsonObject { put("group", value) }.toString()))
            assertEquals("bad_response", (runCatching { accepted { api.create(mailbox, payload()) } }.exceptionOrNull() as ApiException).code)
        }
    }
    @Test fun confirmedReadsRefuseAbsentDefaultedOrUnknownMembership() = runBlocking<Unit> {
        val original = WireJson.parseToJsonElement(validGroup).jsonObject.getValue("group").jsonObject
        for (membership in listOf(buildJsonObject { put("role", "owner") }, buildJsonObject { put("state", "active") },
            buildJsonObject { put("state", "invited"); put("role", "member") }, buildJsonObject { put("state", "active"); put("role", "future") })) {
            server.enqueue(response(buildJsonObject { put("group", JsonObject(original + ("my" to membership))) }.toString()))
            assertEquals("bad_response", (runCatching { accepted { api.group(mailbox, "room_fixture") } }.exceptionOrNull() as ApiException).code)
        }
        server.enqueue(response(validGroup)); assertEquals("room_fixture", accepted { api.group(mailbox, "room_fixture") }.id)
    }
    @Test fun singleGroupReadsAcceptTheBareServerObjectWithTheSameExplicitMembershipChecks() = runBlocking<Unit> {
        val raw = WireJson.parseToJsonElement(validGroup).jsonObject.getValue("group").jsonObject
        server.enqueue(response(raw.toString()))
        val group = accepted { api.group(mailbox, "room_fixture") }
        assertEquals("room_fixture", group.id); assertEquals("org_fixture", group.orgId); assertEquals("keying", group.my!!.state)
        assertEquals("/v1/groups/room_fixture", server.takeRequest().path)
        server.enqueue(response(JsonObject(raw - "my").toString()))
        assertEquals("bad_response", (runCatching { accepted { api.group(mailbox, "room_fixture") } }.exceptionOrNull() as ApiException).code)
        server.enqueue(response(raw.toString()))
        assertEquals("bad_response", (runCatching { accepted { api.create(mailbox, payload()) } }.exceptionOrNull() as ApiException).code)
    }
    @Test fun joiningAndInvitationAcceptanceUseOnlyTheReviewedGroup() = runBlocking<Unit> {
        server.enqueue(response("{}")); server.enqueue(response("{}"))
        accepted { api.join(mailbox, "room_fixture"); api.acceptInvitation(mailbox, "room_fixture") }
        val joined = server.takeRequest(); val accepted = server.takeRequest()
        assertEquals("/v1/groups/room_fixture/join", joined.path); assertEquals("{}", joined.body.readUtf8())
        assertEquals("/v1/groups/room_fixture/invitation", accepted.path); assertEquals("{\"accept\":true}", accepted.body.readUtf8())
    }
    @Test fun revokedMembershipHasNoDirectoryFallback() = runBlocking<Unit> {
        server.enqueue(MockResponse().setResponseCode(403).setBody("""{"error":"not_org_member"}"""))
        assertEquals(403, (runCatching { accepted { api.rooms(mailbox, "org_fixture", "") } }.exceptionOrNull() as ApiException).status)
        assertEquals("org_fixture", server.takeRequest().requestUrl!!.queryParameter("org_id")); assertEquals(1, server.requestCount)
    }
    @Test fun malformedAndOversizedMetadataProducesFixedErrors() = runBlocking<Unit> {
        for (text in listOf("private malformed response", "{\"orgs\":[]}" + " ".repeat(1024 * 1024), """{"rooms":[{"id":"room_fixture","name":"Review","member_count":-1,"join_policy":"public"}]}""")) {
            server.enqueue(response(text))
            val error = runCatching { accepted { if (text.contains("rooms")) api.rooms(mailbox, "org_fixture", "") else api.organizations(mailbox) } }.exceptionOrNull() as ApiException
            assertEquals("bad_response", error.code); assertFalse(error.toString().contains("private malformed"))
        }
    }
    @Test fun inactiveOrAbsentContextsCannotBorrowTheCurrentToken() = runBlocking<Unit> {
        assertTrue(runCatching { api.organizations(mailbox) }.exceptionOrNull() is AuthorizationRetired)
        assertTrue(runCatching { withContext(AuthorizationContext(null)) { api.organizations(mailbox) } }.exceptionOrNull() is AuthorizationRetired)
        val root = tokens.grant.child(); root.retire()
        assertTrue(runCatching { withContext(AuthorizationContext(root)) { api.organizations(mailbox) } }.exceptionOrNull() is AuthorizationRetired)
        assertEquals(0, tokens.acquisitions); assertEquals(0, server.requestCount)
    }
    @Test fun delayedTokenAcquisitionCannotSendUnderAReplacementLogin() = runBlocking<Unit> {
        tokens.entered = CompletableDeferred(); tokens.release = CompletableDeferred()
        coroutineScope {
            val old = async { accepted { api.organizations(mailbox) } }
            tokens.entered!!.await(); tokens.grant.retire(); tokens.grant = AuthorizationLease(); tokens.release!!.complete(Unit)
            assertTrue(runCatching { old.await() }.exceptionOrNull() is AuthorizationRetired)
        }
        assertEquals(0, server.requestCount); assertEquals(1, tokens.acquisitions)
    }
    @Test fun aLateCreationAcknowledgmentCannotBecomeAnotherPersonsRoom() = runBlocking<Unit> {
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); return response(validGroup) }
        }
        coroutineScope {
            val old = async(Dispatchers.IO) { accepted { api.create(mailbox, payload()) } }
            assertTrue(withContext(Dispatchers.IO) { entered.await(5, TimeUnit.SECONDS) })
            tokens.grant.retire(); tokens.grant = AuthorizationLease(); release.countDown()
            assertTrue(runCatching { old.await() }.exceptionOrNull() is AuthorizationRetired)
        }
        assertEquals(1, server.requestCount); assertEquals(0, tokens.invalidations)
    }
}
