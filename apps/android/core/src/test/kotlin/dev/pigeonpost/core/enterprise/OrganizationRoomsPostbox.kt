package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.*
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.File
import java.io.IOException
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import kotlin.system.exitProcess

/** Actual room wire contract and MLS, using only the explicit disposable loopback fixture. */
object OrganizationRoomsPostbox {
    private var stage = "setup"
    private val root = AuthorizationLease()
    private val controls = HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build()
    private fun verify(value: Boolean, label: String) { if (!value) throw AssertionError(label) }
    private fun origin(value: String): String {
        val url = value.toHttpUrl()
        require(url.scheme == "http" && url.host == "127.0.0.1" && url.encodedPath == "/")
        require(url.username.isEmpty() && url.password.isEmpty() && url.query == null && url.fragment == null)
        return value.trimEnd('/')
    }
    private fun api(base: String, credential: String) = PostboxClient(object : TokenProvider {
        override val authorizationLease = root
        override suspend fun token(rejected: String?) = credential
    }, "$base/".toHttpUrl(), allowLoopbackForTests = true)
    private fun control(base: String, mode: String? = null): JsonObject {
        val request = HttpRequest.newBuilder(URI.create(base + if (mode == null) "/__test/attempts" else "/__test/control"))
        if (mode != null) request.header("content-type", "application/json").POST(HttpRequest.BodyPublishers.ofString(buildJsonObject { put("mode", mode) }.toString()))
        val response = controls.send(request.build(), HttpResponse.BodyHandlers.ofString())
        verify(response.statusCode() == 200, "Loopback control failed")
        return WireJson.parseToJsonElement(response.body()).jsonObject
    }
    private suspend fun denied(work: suspend () -> Unit) {
        try { work() } catch (failure: ApiException) {
            verify(failure.status in setOf(403, 404), "Expected a live membership refusal")
            return
        }
        throw AssertionError("Missing organization authority was accepted")
    }
    @JvmStatic fun main(args: Array<String>) {
        try {
            require(args.size == 1)
            runBlocking(AuthorizationContext(root)) { qualify(WireJson.parseToJsonElement(File(args.single()).readText()).jsonObject) }
            println("PASS: real Android organization catalog, scoped directory, exact creation retry, raw group reads, MLS join/invitation and offboarding.")
            exitProcess(0)
        } catch (failure: Throwable) {
            System.err.println("FAIL: organization-room interop at $stage (${failure.javaClass.simpleName}).")
            if (failure is AssertionError) System.err.println(failure.message)
            if (failure is ApiException) System.err.println("Postbox status ${failure.status}, code ${failure.code}")
            exitProcess(1)
        }
    }
    private suspend fun qualify(setup: JsonObject) {
        val base = origin(setup.getValue("postbox").jsonPrimitive.content)
        val proxy = origin(setup.getValue("lossProxy").jsonPrimitive.content)
        val org = setup.getValue("organizationId").jsonPrimitive.content
        require(OrganizationRoomInput.identifier(org))
        val people = setup.getValue("people").jsonObject
        val a = people.getValue("alice").jsonObject
        val b = people.getValue("bob").jsonObject
        val c = people.getValue("carol").jsonObject
        val aliceMailbox = a.getValue("address").jsonPrimitive.content
        val bobMailbox = b.getValue("address").jsonPrimitive.content
        val aliceApi = api(base, a.getValue("token").jsonPrimitive.content)
        val bobApi = api(base, b.getValue("token").jsonPrimitive.content)
        val carolApi = api(base, c.getValue("token").jsonPrimitive.content)
        val aliceRooms = OrganizationRoomClient(aliceApi)
        val bobRooms = OrganizationRoomClient(bobApi)
        val lostRooms = OrganizationRoomClient(api(proxy, a.getValue("token").jsonPrimitive.content))

        stage = "canonical organization membership and personal-inbox separation"
        val aliceOrg = aliceRooms.organizations(aliceMailbox).forMailbox(aliceMailbox).single()
        val bobOrg = bobRooms.organizations(bobMailbox).forMailbox(bobMailbox).single()
        verify(aliceOrg.id == org && bobOrg.id == org && aliceOrg.mayCreate(aliceMailbox), "Organization ownership did not match canonical member mailboxes")
        verify(bobRooms.organizations(bobMailbox).forMailbox(b.getValue("personalAddress").jsonPrimitive.content).isEmpty(), "Personal inbox inherited organization membership")
        denied { bobRooms.rooms(b.getValue("personalAddress").jsonPrimitive.content, org, "") }
        denied { OrganizationRoomClient(carolApi).rooms(c.getValue("address").jsonPrimitive.content, org, "") }

        stage = "real MLS device registration"
        val crypto = UniffiGroupCrypto().also { it.generateStateKey() }
        val alice = GroupClient(crypto, aliceApi, MemoryGroupStorage(), aliceMailbox, sleep = {}, jitter = { 0 }, authorizationLease = root.child())
        val bob = GroupClient(crypto, bobApi, MemoryGroupStorage(), bobMailbox, sleep = {}, jitter = { 0 }, authorizationLease = root.child())
        alice.init(); bob.init(); alice.topUpKeyPackages(); bob.topUpKeyPackages()

        stage = "committed room creation loss and explicit exact retry"
        val payload = OrganizationRoomCreate(org, "Loopback design", "Synthetic room", RoomJoinPolicy.MEMBERS, RoomPostPolicy.EVERYONE, true, "room-loopback-request-0123456789")
        control(proxy, "after")
        try { lostRooms.create(aliceMailbox, payload); throw AssertionError("Committed creation response was not dropped") }
        catch (failure: IOException) { verify(failure !is ApiException, "Creation was refused rather than committed") }
        verify(control(proxy).getValue("attempts").jsonArray.size == 1, "Creation replayed without an explicit retry")
        control(proxy, "off")
        val created = lostRooms.create(aliceMailbox, payload)
        val attempts = control(proxy).getValue("attempts").jsonArray.map { it.jsonObject }
        verify(attempts.size == 2 && attempts.map { it["sha"] }.distinct().size == 1 &&
            attempts.all { it["requestKey"]?.jsonPrimitive?.content == payload.requestKey && it.getValue("status").jsonPrimitive.int in 200..299 }, "Room retry changed the exact accepted body/key")
        verify(created.orgId == org && created.createdBy == aliceMailbox && created.my?.role == "owner", "Creation receipt did not confirm the reviewed organization/owner")
        verify(aliceRooms.group(aliceMailbox, created.id).id == created.id, "Bare group response failed strict decoding")
        alice.activate(created.id)
        // The public directory deliberately omits provisioning rooms until MLS activation.
        verify(aliceRooms.rooms(aliceMailbox, org, "Loopback design").count { it.id == created.id } == 1, "Creation retry made duplicate rooms")

        stage = "explicit public join and verified encrypted membership"
        verify(bobRooms.rooms(bobMailbox, org, "Loopback design").single().action == RoomAction.JOIN, "Public room was already joined")
        bobRooms.preview(bobMailbox, created.id)
        bobRooms.join(bobMailbox, created.id)
        verify(bobRooms.group(bobMailbox, created.id).my?.state == "keying", "Join did not confirm live membership")
        verify(alice.processPending(created.id, noJitter = true) != null, "Owner did not commit the new member")
        verify(created.id in bob.processWelcomes(), "New member did not verify a real Welcome")
        verify(alice.safetyNumber(created.id) == bob.safetyNumber(created.id), "Room membership safety values disagree")
        alice.send(created.id, OutgoingDocument("Synthetic organization message")); bob.sync(created.id)
        verify(bob.history(created.id).any { it.text == "Synthetic organization message" }, "Joined room could not decrypt the real MLS message")

        stage = "private invitation and explicit acceptance"
        val privateRoom = alice.createGroup(buildJsonObject {
            put("org_id", org); put("name", "Loopback private"); put("join_policy", "private"); put("visibility", "listed")
            putJsonArray("invite") { add(JsonPrimitive(bobMailbox)) }
        }).group.id
        verify(bobRooms.rooms(bobMailbox, org, "Loopback private").single().action == RoomAction.ACCEPT_INVITATION, "Private room invitation state was lost")
        bobRooms.acceptInvitation(bobMailbox, privateRoom)
        verify(bobRooms.group(bobMailbox, privateRoom).orgId == org, "Accepted room changed organizations")
        verify(alice.processPending(privateRoom, noJitter = true) != null && privateRoom in bob.processWelcomes(), "Private acceptance did not complete real MLS")
        verify(alice.safetyNumber(privateRoom) == bob.safetyNumber(privateRoom), "Private room membership safety values disagree")

        stage = "live offboarding denies old room metadata"
        aliceApi.groupJson(aliceMailbox, "POST", "/v1/orgs/$org/members/${bobOrg.memberId}/remove", buildJsonObject {})
        denied { bobRooms.group(bobMailbox, created.id) }
        denied { bobRooms.rooms(bobMailbox, org, "") }
        verify(bobRooms.organizations(bobMailbox).forMailbox(bobMailbox).isEmpty(), "Offboarded membership remained discoverable")
    }
}
