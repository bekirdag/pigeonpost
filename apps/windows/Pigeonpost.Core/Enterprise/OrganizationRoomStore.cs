namespace Pigeonpost.Core.Enterprise;

public sealed class OrganizationRoomIdentity(string ownerKey, AuthorizationLease authorization, string mailbox)
{
    public string OwnerKey { get; } = ownerKey;
    public AuthorizationLease Authorization { get; } = authorization;
    public string Mailbox { get; } = mailbox;
    public bool Same(OrganizationRoomIdentity? other) => other is not null && OwnerKey == other.OwnerKey
        && ReferenceEquals(Authorization, other.Authorization) && Mailbox == other.Mailbox;
    public override string ToString() => "OrganizationRoomIdentity(redacted)";
}

public sealed class OrganizationRoomOpening(string contextId, string organizationId, string groupId)
{
    public string ContextId { get; } = contextId;
    public string OrganizationId { get; } = organizationId;
    public string GroupId { get; } = groupId;
    public override string ToString() => "OrganizationRoomOpening(redacted)";
}

public sealed record OrganizationRoomDraft(string OrganizationId, string Name = "", string Description = "",
    RoomJoinPolicy Join = RoomJoinPolicy.Members, RoomPostPolicy Post = RoomPostPolicy.Everyone, bool Listed = true)
{
    public string Id { get; init; } = Guid.NewGuid().ToString("N");
    public bool Dirty => Name.Trim().Length > 0 || Description.Trim().Length > 0;
    public override string ToString() => "OrganizationRoomDraft(redacted)";
}

public sealed record OrganizationRoomsState
{
    public string ContextId { get; init; } = Guid.NewGuid().ToString("N");
    public string? Mailbox { get; init; }
    public RoomOrganizations? Catalog { get; init; }
    public string? SelectedOrganizationId { get; init; }
    public IReadOnlyList<OrganizationRoom> Rooms { get; init; } = [];
    public string DirectoryProof { get; init; } = Guid.NewGuid().ToString("N");
    public RoomPreview? Preview { get; init; }
    public string ReviewProof { get; init; } = Guid.NewGuid().ToString("N");
    public bool MustReviewAgain { get; init; }
    public RoomCreationAttempt? PendingCreation { get; init; }
    public ConfirmedRoom? CompletedCreation { get; init; }
    public OrganizationRoomDraft? Draft { get; init; }
    public OrganizationRoomOpening? Opening { get; init; }
    public bool LoadingCatalog { get; init; }
    public bool LoadingRooms { get; init; }
    public bool Reviewing { get; init; }
    public bool Busy { get; init; }
    public bool CustodyChecked { get; init; }
    public string? Message { get; init; }
    public long? RetryAt { get; init; }
    public IReadOnlyList<RoomOrganization> Organizations => Catalog?.OwnedFor(Mailbox ?? "") ?? [];
    public RoomOrganization? Organization => Organizations.FirstOrDefault(o => o.Id == SelectedOrganizationId);
    // Organizations whose member inbox is another inbox of this person: offered as an explicit switch.
    public IReadOnlyList<RoomOrganization> OtherInboxes => Catalog is { Valid: true } catalog
        ? catalog.Orgs.Where(o => o.Readable && o.Mailbox is not null && o.Mailbox.Address != Mailbox).ToArray() : [];
    public IReadOnlyList<RoomOperatedMailbox> Delegated => Catalog?.Delegated ?? [];
    public bool Loading => LoadingCatalog || LoadingRooms || Reviewing;
    // The latest message, or the standing explanation of an unresolved creation in custody.
    public string? Notice => Message ?? PendingCreation switch
    {
        { CandidateId: null } => "A room creation from earlier is not confirmed. Retry the same details, or discard it after checking your rooms.",
        { } => "This room was created, but opening it is not confirmed. Check the created room.",
        _ => null
    };
    public override string ToString() => "OrganizationRoomsState(redacted)";
}

// Each directory, preview, decision and creation belongs to one person, accepted grant and canonical
// inbox. Construct and use on the UI context. Uncertain creations keep their exact bytes in protected
// custody until confirmed or explicitly discarded.
public sealed class OrganizationRoomStore(IOrganizationRoomApi api, IInboxService inbox, IRoomCreationStore custody,
    Func<OrganizationRoomIdentity?> identity, TimeProvider? clock = null) : ObservableObject, IDisposable
{
    private enum Lane { Catalog, Rooms, Review, Mutation }
    private sealed class Work(Lane lane, AuthorizationLease actor, AuthorizationLease request, string? organization)
    {
        public readonly Lane Lane = lane; public readonly AuthorizationLease Actor = actor, Request = request;
        public readonly string? Organization = organization; public readonly CancellationTokenSource Cancellation = new();
    }
    private readonly TimeProvider clock = clock ?? TimeProvider.System;
    private OrganizationRoomsState state = new();
    private OrganizationRoomIdentity? bound;
    private AuthorizationLease? actor;
    private readonly Dictionary<Lane, Work> works = [];
    private string? rateOwner;
    private long? deadline;
    private bool disposed;

    public OrganizationRoomsState State => state;
    public bool PersistentCustody => custody.Persistent;
    private long Now => clock.GetUtcNow().ToUnixTimeSeconds();
    private void Publish(OrganizationRoomsState next) { state = next; Changed(nameof(State)); }
    private void Update(Func<OrganizationRoomsState, OrganizationRoomsState> change) => Publish(change(state));

    private OrganizationRoomIdentity? Live()
    {
        if (disposed || identity() is not { } next || string.IsNullOrEmpty(next.OwnerKey) || System.Text.Encoding.UTF8.GetByteCount(next.OwnerKey) > 256
            || !next.Authorization.IsActive || !OrganizationRoomInput.Mailbox(next.Mailbox)
            || !ReferenceEquals(api.AcceptedAuthorization, next.Authorization) || !ReferenceEquals(inbox.AcceptedAuthorization, next.Authorization)) return null;
        return next;
    }
    public bool IsCurrent(string? context = null) => (context ?? state.ContextId) == state.ContextId && actor?.IsActive == true && bound?.Same(Live()) == true;
    private void Cancel(Lane lane)
    {
        if (works.Remove(lane, out var work)) { work.Request.Retire(); work.Cancellation.Cancel(); }
    }
    private void Bind(OrganizationRoomIdentity? next, bool preserveRate = false)
    {
        foreach (var lane in Enum.GetValues<Lane>()) Cancel(lane);
        actor?.Retire(); actor = null; bound = null;
        if (!preserveRate && next?.OwnerKey != rateOwner) { rateOwner = null; deadline = null; }
        AuthorizationLease? child = null;
        try { child = next?.Authorization.Child(); } catch (AuthorizationRetiredException) { }
        if (child is not null) { actor = child; bound = next; }
        Publish(new OrganizationRoomsState { Mailbox = child is null ? null : next!.Mailbox, RetryAt = deadline });
    }
    public void Synchronize()
    {
        var next = Live();
        if (next is null && bound is null && actor is null) return;
        if (bound?.Same(next) != true || actor?.IsActive != true) Bind(next);
    }
    // Uncertain creations stay in protected custody; closing only leaves this view.
    public bool Close()
    {
        if (state.Busy) return false;
        Bind(null, preserveRate: Live()?.OwnerKey == rateOwner); return true;
    }
    public void Dispose() { if (disposed) return; Bind(null); disposed = true; }

    public bool CanCreate(string context) => IsCurrent(context) && !state.Busy && state.CustodyChecked
        && state.Organization?.MayCreate(state.Mailbox ?? "") == true;
    public bool Allows(OrganizationRoomOpening opening) => IsCurrent(opening.ContextId) && ReferenceEquals(state.Opening, opening)
        && state.Organization is { Readable: true } org && org.Id == opening.OrganizationId;
    private bool Current(Work work) => works.GetValueOrDefault(work.Lane) == work && ReferenceEquals(actor, work.Actor) && IsCurrent()
        && (work.Organization is null || state.Organization is { Readable: true } org && org.Id == work.Organization);
    private bool Limited()
    {
        if (deadline is not { } until || until <= Now) { if (deadline is not null) { deadline = null; Update(s => s with { RetryAt = null }); } return false; }
        Update(s => s with { Message = "Wait before retrying this request.", RetryAt = until }); return true;
    }
    private OrganizationRoomsState Flag(OrganizationRoomsState s, Lane lane, bool value) => lane switch
    {
        Lane.Catalog => s with { LoadingCatalog = value },
        Lane.Rooms => s with { LoadingRooms = value },
        Lane.Review => s with { Reviewing = value },
        _ => s with { Busy = value }
    };
    private Task? Run(Lane lane, string context, string? organization, Func<Work, Task> action)
    {
        Synchronize();
        if (actor is not { } owner || !IsCurrent(context) || state.Busy || Limited()
            || organization is not null && state.Organization?.Id != organization) return null;
        Cancel(lane);
        AuthorizationLease request;
        try { request = owner.Child(); } catch (AuthorizationRetiredException) { Synchronize(); return null; }
        var work = new Work(lane, owner, request, organization); works[lane] = work;
        Update(s => Flag(s, lane, true) with { Message = null });
        return Execute();
        async Task Execute()
        {
            try { using (new AuthorizationContext(request)) await action(work); }
            catch (Exception failure) { if (Current(work)) Failed(failure, lane == Lane.Mutation); }
            finally
            {
                request.Retire();
                if (works.GetValueOrDefault(lane) == work) { works.Remove(lane); if (ReferenceEquals(actor, owner) && IsCurrent()) Update(s => Flag(s, lane, false)); }
                work.Cancellation.Dispose();
            }
        }
    }
    private void ClearReview()
    {
        Cancel(Lane.Review);
        Update(s => s with { Preview = null, ReviewProof = Guid.NewGuid().ToString("N"), MustReviewAgain = false, Reviewing = false, Opening = null });
    }
    public void CloseReview(string context) { if (IsCurrent(context) && !state.Busy) ClearReview(); }

    public bool SelectOrganization(string id, string context)
    {
        Synchronize();
        if (!IsCurrent(context) || state.Busy || state.Organizations.All(o => o.Id != id)) return false;
        if (state.SelectedOrganizationId == id) return true;
        if (state.PendingCreation is not null || state.Draft?.Dirty == true)
        { Update(s => s with { Message = "Finish or discard the room draft before changing organizations." }); return false; }
        Cancel(Lane.Rooms); ClearReview();
        Update(s => s with { SelectedOrganizationId = id, Rooms = [], DirectoryProof = Guid.NewGuid().ToString("N"), LoadingRooms = false,
            CompletedCreation = null, Draft = null, Message = null });
        return true;
    }

    private int catalogLoads;
    public Task? LoadOrganizations(string? context = null, string search = "")
    {
        var before = catalogLoads;
        var loading = Run(Lane.Catalog, context ?? state.ContextId, null, async work =>
        {
            var mailbox = bound!.Mailbox; var ct = work.Cancellation.Token;
            var catalog = await api.OrganizationsAsync(mailbox, ct);
            if (!Current(work)) return;
            if (!catalog.Valid) throw RoomWire.Bad();
            // Protected custody for this server-authorized inbox. Without it, creation stays disabled.
            RoomCreationAttempt? pending = null; var checkedCustody = false;
            using (var scope = await MessageRecoveryScope.AuthorizeAsync(inbox, mailbox, ct))
                if (scope is not null) { pending = await custody.LoadAsync(scope, ct); checkedCustody = true; }
            if (!Current(work)) return;
            Cancel(Lane.Rooms); ClearReview();
            var owned = catalog.OwnedFor(mailbox);
            if (pending is not null && owned.All(o => o.Id != pending.Payload.OrganizationId)) pending = null; // Kept on disk, not offered here.
            var selected = pending?.Payload.OrganizationId
                ?? (owned.Any(o => o.Id == state.SelectedOrganizationId) ? state.SelectedOrganizationId : owned.Count == 1 ? owned[0].Id : null);
            Update(s => s with
            {
                Catalog = catalog, SelectedOrganizationId = selected, Rooms = [], DirectoryProof = Guid.NewGuid().ToString("N"), LoadingRooms = false,
                PendingCreation = pending, CustodyChecked = checkedCustody,
                CompletedCreation = s.CompletedCreation?.OrganizationId == selected ? s.CompletedCreation : null,
                Draft = pending is { } p ? new(p.Payload.OrganizationId, p.Payload.Name, p.Payload.Description, p.Payload.JoinPolicy, p.Payload.PostPolicy, p.Payload.Listed)
                    : s.Draft?.OrganizationId == selected ? s.Draft : null,
                Message = pending is null ? null : "A room creation from earlier is not confirmed. Retry the same details, or discard it after checking your rooms."
            });
            catalogLoads++;
        });
        if (loading is null) return null;
        return Continue();
        // Outside the catalog request's actor: the directory read owns its own request.
        async Task Continue()
        {
            await loading;
            if (catalogLoads != before && state.Organization is not null && LoadRooms(search) is { } rooms) await rooms;
        }
    }

    public Task? LoadRooms(string search, string? context = null)
    {
        Synchronize();
        context ??= state.ContextId;
        if (!IsCurrent(context) || state.Organization is not { Readable: true } org || state.Busy) return null;
        ClearReview();
        Update(s => s with { DirectoryProof = Guid.NewGuid().ToString("N") });
        return Run(Lane.Rooms, context, org.Id, async work =>
        {
            var rooms = await api.RoomsAsync(bound!.Mailbox, org.Id, search, work.Cancellation.Token);
            if (!Current(work)) return;
            if (rooms.Count > 50 || rooms.Any(r => !r.Valid) || rooms.Select(r => r.Id).Distinct(StringComparer.Ordinal).Count() != rooms.Count) throw RoomWire.Bad();
            Update(s => s with { Rooms = rooms, DirectoryProof = Guid.NewGuid().ToString("N") });
        });
    }

    public Task? Inspect(string id, string proof, string context)
    {
        Synchronize();
        if (state.Organization is not { } org || !IsCurrent(context) || state.Busy || state.DirectoryProof != proof || state.Rooms.All(r => r.Id != id)) return null;
        ClearReview();
        return Run(Lane.Review, context, org.Id, async work =>
        {
            var preview = await api.PreviewRoomAsync(bound!.Mailbox, id, work.Cancellation.Token);
            if (Current(work)) Update(s => s with { Preview = preview.Id == id ? preview : throw RoomWire.Bad(), ReviewProof = Guid.NewGuid().ToString("N"), MustReviewAgain = false });
        });
    }

    public RoomAction ReviewAction(string proof, string context)
    {
        if (!IsCurrent(context) || state.ReviewProof != proof || state.Busy || state.MustReviewAgain || state.Organization is not { Readable: true } org
            || state.Preview is not { } preview) return RoomAction.Unavailable;
        var action = preview.MyState switch
        {
            "active" or "keying" => RoomAction.Open,
            "invited" => RoomAction.AcceptInvitation,
            null or "left" or "removed" => preview.JoinPolicy == "public" && preview.CanJoin ? RoomAction.Join : RoomAction.Unavailable,
            _ => RoomAction.Unavailable
        };
        return action == RoomAction.Open || org.Writable ? action : RoomAction.Unavailable;
    }

    private void Opened(ConfirmedRoom room, string organization) =>
        Update(s => s with { Opening = new OrganizationRoomOpening(s.ContextId, organization, room.Id), Message = null });

    public Task? OpenReviewed(string proof, string context)
    {
        Synchronize();
        var action = ReviewAction(proof, context);
        if (action == RoomAction.Unavailable) return null;
        var org = state.Organization!.Id; var id = state.Preview!.Id; var mailbox = bound!.Mailbox;
        return Run(Lane.Mutation, context, org, async work =>
        {
            try
            {
                if (action == RoomAction.Join) await api.JoinRoomAsync(mailbox, id, work.Cancellation.Token);
                if (action == RoomAction.AcceptInvitation) await api.AcceptRoomInvitationAsync(mailbox, id, work.Cancellation.Token);
            }
            catch (PostboxException failure) when (failure.Code == "already_member") { }
            if (!Current(work)) return;
            var room = await api.RoomAsync(mailbox, id, work.Cancellation.Token);
            if (Current(work) && state.ReviewProof == proof)
            {
                if (room.OrganizationId != org) throw RoomWire.Bad();
                Opened(room, org);
            }
        });
    }

    public bool BeginCreation(string context)
    {
        if (!CanCreate(context)) return false;
        ClearReview(); // A room under review is a separate decision.
        if (state.PendingCreation is null)
            Update(s => s with { CompletedCreation = null, Opening = null, Message = null,
                Draft = s.CompletedCreation is null ? s.Draft ?? new(s.Organization!.Id) : new(s.Organization!.Id) });
        return true;
    }
    public void ChangeDraft(OrganizationRoomDraft draft, string context)
    {
        if (!IsCurrent(context) || state.Busy || state.PendingCreation is not null || state.CompletedCreation is not null
            || state.Draft?.Id != draft.Id || state.Organization?.Id != draft.OrganizationId) return;
        Update(s => s with { Draft = draft with { Name = Clip(draft.Name, 320), Description = Clip(draft.Description, 2000) }, Message = null });
    }
    private static string Clip(string value, int length) => value.Length <= length ? value : value[..length];
    public void CancelDraft(string context)
    {
        if (!IsCurrent(context) || state.Busy || state.PendingCreation is not null) return;
        Update(s => s with { Draft = null, Message = null });
    }

    public Task? SubmitDraft(string id, string context)
    {
        Synchronize();
        if (state.Draft is not { } draft || draft.Id != id || !CanCreate(context) || state.Organization?.Id != draft.OrganizationId || Limited()) return null;
        if (state.CompletedCreation is not null) { Update(s => s with { Message = "This room was created. Open it or start another room." }); return null; }
        var previous = state.PendingCreation;
        OrganizationRoomCreate payload;
        try { payload = new(draft.OrganizationId, draft.Name, draft.Description, draft.Join, draft.Post, draft.Listed, previous?.Payload.RequestKey ?? "rk_" + Guid.NewGuid().ToString("N")); }
        catch (ArgumentException) { Update(s => s with { Message = "Use a name of 1–80 characters and a description of at most 500 characters." }); return null; }
        if (previous is not null && payload.Encoded != previous.Payload.Encoded)
        { Update(s => s with { Message = "Check or discard the earlier creation before changing its details." }); return null; }
        if (previous is { CandidateId: not null })
        { Update(s => s with { Message = "This room was already created. Check it instead of creating it again." }); return null; }
        if (previous is not null && !previous.CanRetry(Now))
        { Update(s => s with { Message = "This earlier creation is too old to repeat safely. Check your rooms, then discard it." }); return null; }
        var mailbox = bound!.Mailbox; var org = draft.OrganizationId;
        ClearReview();
        return Run(Lane.Mutation, context, org, async work =>
        {
            var ct = work.Cancellation.Token;
            using var scope = await MessageRecoveryScope.AuthorizeAsync(inbox, mailbox, ct) ?? throw new RoomCreationUnavailableException();
            if (!Current(work)) return;
            var attempt = previous ?? new RoomCreationAttempt(scope, payload, Now);
            // Protected custody first: an unsaved intent is never sent.
            if (previous is null) { await custody.SaveAsync(scope, attempt, ct); if (!Current(work)) return; Update(s => s with { PendingCreation = attempt }); }
            var created = await api.CreateRoomAsync(mailbox, attempt.Payload, ct);
            if (!Current(work)) return;
            if (created.OrganizationId != org) throw RoomWire.Bad();
            var known = attempt.WithCandidate(scope, created.Id);
            await custody.SaveAsync(scope, known, ct);
            if (!Current(work)) return;
            Update(s => s with { PendingCreation = known });
            await ConfirmCreatedAsync(work, scope, known, mailbox, org, ct);
        });
    }

    private async Task ConfirmCreatedAsync(Work work, MessageRecoveryScope scope, RoomCreationAttempt attempt, string mailbox, string org, CancellationToken ct)
    {
        var room = await api.RoomAsync(mailbox, attempt.CandidateId!, ct);
        if (!Current(work)) return;
        if (room.OrganizationId != org) throw RoomWire.Bad();
        await custody.RemoveAsync(scope, attempt, ct);
        if (!Current(work)) return;
        Update(s => s with { PendingCreation = null, CompletedCreation = room, Draft = null });
        Opened(room, org);
    }

    // A known created room is checked with a read; it is never created again.
    public Task? CheckCreated(string context)
    {
        Synchronize();
        if (!IsCurrent(context) || state.Organization is not { } org) return null;
        if (state.PendingCreation is { CandidateId: not null } pending)
        {
            var mailbox = bound!.Mailbox;
            return Run(Lane.Mutation, context, org.Id, async work =>
            {
                using var scope = await MessageRecoveryScope.AuthorizeAsync(inbox, mailbox, work.Cancellation.Token) ?? throw new RoomCreationUnavailableException();
                if (Current(work)) await ConfirmCreatedAsync(work, scope, pending, mailbox, org.Id, work.Cancellation.Token);
            });
        }
        if (state.CompletedCreation is { } done)
        {
            var mailbox = bound!.Mailbox;
            return Run(Lane.Mutation, context, org.Id, async work =>
            {
                var room = await api.RoomAsync(mailbox, done.Id, work.Cancellation.Token);
                if (Current(work)) { if (room.OrganizationId != org.Id) throw RoomWire.Bad(); Update(s => s with { CompletedCreation = room }); Opened(room, org.Id); }
            });
        }
        return null;
    }

    // Explicit local discard of protected custody; a room the server already created stays.
    public Task? DiscardCreation(string context)
    {
        Synchronize();
        if (!IsCurrent(context) || state.Busy) return null;
        if (state.PendingCreation is not { } pending)
        { Update(s => s with { CompletedCreation = null, Draft = null, Opening = null, Message = null }); return Task.CompletedTask; }
        var mailbox = bound!.Mailbox;
        return Run(Lane.Mutation, context, null, async work =>
        {
            using var scope = await MessageRecoveryScope.AuthorizeAsync(inbox, mailbox, work.Cancellation.Token) ?? throw new RoomCreationUnavailableException();
            if (!Current(work)) return;
            await custody.RemoveAsync(scope, pending, work.Cancellation.Token);
            if (Current(work)) Update(s => s with { PendingCreation = null, CompletedCreation = null, Draft = null, Opening = null, Message = null });
        });
    }

    public void OpeningFailed(OrganizationRoomOpening opening, Exception? failure = null)
    {
        if (!Allows(opening)) return;
        if (failure is PostboxException { StatusCode: 401 or 403 or 404 or 410 }) { Failed(failure, decision: false); return; }
        Update(s => s with { Opening = null, MustReviewAgain = s.Preview is not null,
            Message = "The room is confirmed, but its encrypted conversation is not ready. Open it again to retry." });
    }
    public void OpeningCompleted(OrganizationRoomOpening opening) { if (Allows(opening)) Update(s => s with { Opening = null }); }

    private void Failed(Exception failure, bool decision)
    {
        var problem = failure as PostboxException;
        if (failure is SignInException) { Update(s => s with { Message = "Sign in again to continue." }); return; }
        if (failure is RoomCreationUnavailableException or RoomCreationConflictException)
        { Update(s => s with { Message = failure.Message }); return; }
        if (problem?.StatusCode is 401 or 403 or 404 or 410 && problem.Code != "step_up_required")
        {
            Cancel(Lane.Rooms); ClearReview();
            // Access was refused: clear private metadata. Protected custody stays on disk for this inbox.
            Update(s => s with { Catalog = null, SelectedOrganizationId = null, Rooms = [], LoadingRooms = false, PendingCreation = null,
                CompletedCreation = null, Draft = null, Message = "Access could not be confirmed. Refresh your organizations." });
            return;
        }
        if (problem?.StatusCode == 429) { rateOwner = bound?.OwnerKey; deadline = Now + Math.Clamp(problem.RetryAfter ?? 30, 1, 3600); }
        Update(s => s with
        {
            MustReviewAgain = decision && s.Preview is not null, Opening = null, RetryAt = deadline,
            Message = problem?.StatusCode == 429 ? "Too many requests. Wait before trying again."
                : problem?.Code == "step_up_required" ? "Verify your sign-in on another device or in the web inbox, then try again."
                : s.PendingCreation is not null ? "Creation is not confirmed. Retry the same details or check your rooms."
                : decision ? "The result is not confirmed. Review the room again before deciding."
                : "The rooms could not be confirmed. Refresh and try again."
        });
    }
}
