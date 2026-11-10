package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.Serializable

/**
 * The parts of an organization a member sees inside the normal inbox (SDS §5.4): the org name on
 * their org mailbox, "Acme Inc. · Member" on their profile, and org addresses while composing.
 * Display only; every action is still authorized by the postbox.
 */
data class OrganizationBadge(val organizationId: String, val name: String, val role: String) {
    val label get() = "$name · $role"
}

@Serializable
data class OrganizationDirectoryEntry(val handle: String, val address: String, val kind: String, val name: String? = null) {
    val valid get() = OrganizationRoomInput.text(handle, 256) && handle.startsWith("/") && validAddress(handle) &&
        OrganizationRoomInput.mailbox(address) && kind in setOf("member", "agent") && (name == null || OrganizationRoomInput.text(name, 256))
    /** What a person reads in the suggestion: the name if there is one, otherwise the handle. */
    val title get() = name?.takeIf { it.isNotBlank() } ?: handle
}

@Serializable
internal data class OrganizationDirectoryAnswer(val entries: List<OrganizationDirectoryEntry> = emptyList())

interface OrganizationDirectoryApi {
    suspend fun directory(mailbox: String, organizationId: String, query: String): List<OrganizationDirectoryEntry>
}

object OrganizationPresence {
    /** The highest role, in the words the portal uses. A member with no extra role is a "Member". */
    fun role(roles: List<String>): String = when {
        "owner" in roles -> "Owner"
        "admin" in roles -> "Admin"
        "people_admin" in roles -> "People admin"
        "agent_admin" in roles -> "Agent admin"
        "billing_admin" in roles -> "Billing admin"
        else -> "Member"
    }

    /** Badges keyed by the member's own org mailbox address. Only orgs a member may still read. */
    fun badges(catalog: RoomOrganizations?): Map<String, OrganizationBadge> {
        if (catalog == null || !catalog.valid) return emptyMap()
        return catalog.orgs.filter { it.readable && it.mailbox != null }
            .associate { org -> org.mailbox!!.address to OrganizationBadge(org.id, org.name, role(org.roles)) }
    }

    /** Directory queries the postbox accepts: trimmed, at most 80 characters, no wildcards. */
    fun query(text: String): String? = text.trim().trim('/').replace("%", "").replace("_", "").take(80).takeIf { it.isNotEmpty() }
}

data class OrganizationPresenceState(
    val badges: Map<String, OrganizationBadge> = emptyMap(),
    val directoryQuery: String = "",
    val directory: List<OrganizationDirectoryEntry> = emptyList(),
)

/**
 * Loads `GET /v1/orgs` once per signed-in person (the catalog is account wide, so it survives an
 * inbox switch) and searches `GET /v1/orgs/{id}/directory` for the inbox in use. Every request
 * runs under a child of the current sign-in, and a replaced sign-in drops what was loaded.
 */
class OrganizationPresenceStore(
    private val api: OrganizationRoomApi, private val directoryApi: OrganizationDirectoryApi?, private val scope: CoroutineScope,
    private val identity: () -> OrganizationRoomIdentity?, private val debounceMillis: Long = 250,
) {
    private val mutable = MutableStateFlow(OrganizationPresenceState())
    val state = mutable.asStateFlow()
    private var subject: String? = null
    private var root: AuthorizationLease? = null
    private var catalogJob: Job? = null
    private var searchJob: Job? = null

    private fun live() = identity()?.takeIf { it.subject.isNotEmpty() && it.authorization.isValid && OrganizationRoomInput.mailbox(it.mailbox) }

    /** Call when the sign-in or the inbox changes. Loads the catalog the first time for a person. */
    fun synchronize() {
        val next = live()
        if (next == null) { if (subject != null || root != null) reset(); return }
        if (next.subject != subject || next.authorization !== root) {
            reset(); subject = next.subject; root = next.authorization; refresh()
        } else clearSearch()
    }

    fun refresh(): Job? {
        val source = live() ?: return null
        if (source.subject != subject || source.authorization !== root) return null
        catalogJob?.cancel()
        val request = try { source.authorization.child() } catch (_: AuthorizationRetired) { return null }
        return scope.launch(AuthorizationContext(request)) {
            try {
                val catalog = api.organizations(source.mailbox)
                if (live()?.let { it.subject == source.subject && it.authorization === source.authorization } == true)
                    mutable.update { it.copy(badges = OrganizationPresence.badges(catalog)) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { /* Badges are a hint; the inbox works without them. */ }
            finally { request.retire() }
        }.also { catalogJob = it }
    }

    /** Org addresses for the inbox in use, when that inbox is an org mailbox. */
    fun search(text: String): Job? {
        searchJob?.cancel()
        val source = live()
        val badge = source?.let { mutable.value.badges[it.mailbox] }
        val query = OrganizationPresence.query(text)
        if (source == null || badge == null || query == null || directoryApi == null) { clearSearch(); return null }
        mutable.update { it.copy(directoryQuery = query) }
        val request = try { source.authorization.child() } catch (_: AuthorizationRetired) { return null }
        return scope.launch(AuthorizationContext(request)) {
            try {
                delay(debounceMillis)
                val found = directoryApi.directory(source.mailbox, badge.organizationId, query)
                if (live()?.let { it.mailbox == source.mailbox && it.authorization === source.authorization } == true && mutable.value.directoryQuery == query)
                    mutable.update { it.copy(directory = found.filter { entry -> entry.address != source.mailbox }) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (mutable.value.directoryQuery == query) mutable.update { it.copy(directory = emptyList()) } }
            finally { request.retire() }
        }.also { searchJob = it }
    }

    fun clearSearch() { searchJob?.cancel(); searchJob = null; mutable.update { it.copy(directoryQuery = "", directory = emptyList()) } }

    fun reset() {
        catalogJob?.cancel(); searchJob?.cancel(); catalogJob = null; searchJob = null
        subject = null; root = null; mutable.value = OrganizationPresenceState()
    }
}
