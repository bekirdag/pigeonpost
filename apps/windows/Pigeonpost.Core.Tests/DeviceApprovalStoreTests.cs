using System.Net;
using System.Text;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

internal static partial class DevicePairingTests
{
    private const string Person="synthetic-owner-key", Other="synthetic-other-owner";
    private sealed class ManualClock:TimeProvider
    {
        private long now=DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        public override DateTimeOffset GetUtcNow()=>DateTimeOffset.FromUnixTimeSeconds(Interlocked.Read(ref now));
        public void Advance(int seconds)=>Interlocked.Add(ref now,seconds);
    }
    // Synthetic pairing service: every call records the ambient actor and can be held or failed.
    private sealed class FakeApi:IDevicePairingApi
    {
        public AuthorizationLease? Accepted=new();public string Owner=Person;
        AuthorizationLease? IDevicePairingApi.AcceptedAuthorization=>Accepted;
        public DeviceApprovalIdentity? Identity()=>Accepted is{} a?new(Owner,a):null;
        public Func<DevicePairingCode,CancellationToken,Task<DevicePairingPreview>>? OnResolve;
        public Func<CancellationToken,Task>? OnApprove,OnDeny;
        public int Resolves,Approves,Denies;public readonly List<AuthorizationLease?> Actors=[];
        public DevicePairingPreview Current=Preview();
        public Task<DevicePairingPreview> ResolveDevicePairingAsync(DevicePairingCode code,CancellationToken ct)
        {Resolves++;Actors.Add(AuthorizationContext.Current);Require(code.Equals(Code));return OnResolve?.Invoke(code,ct)??Task.FromResult(Current);}
        public async Task ApproveDevicePairingAsync(DevicePairingPreview preview,DevicePairingCode code,CancellationToken ct)
        {Approves++;Actors.Add(AuthorizationContext.Current);Require(code.Equals(Code));if(OnApprove is not null)await OnApprove(ct);}
        public async Task DenyDevicePairingAsync(DevicePairingPreview preview,DevicePairingCode code,CancellationToken ct)
        {Denies++;Actors.Add(AuthorizationContext.Current);Require(code.Equals(Code));if(OnDeny is not null)await OnDeny(ct);}
        // A deliberate same-person (or substituted) grant replacement, as a verified adoption performs.
        public AuthorizationLease Rotate(string? owner=null){var old=Accepted;Accepted=new();old?.Retire();Owner=owner??Owner;return Accepted;}
    }
    private static (FakeApi Api,DeviceApprovalStore Store,ManualClock Clock) NewStore()
    {var api=new FakeApi();var clock=new ManualClock();var store=new DeviceApprovalStore(api,api.Identity,clock:clock);store.Synchronize();return(api,store,clock);}
    private static async Task Done(Task? task){Require(task is not null);await task!.WaitAsync(TimeSpan.FromSeconds(5));}
    private static PostboxException StepUp()=>new(401,"step_up_required",strongSignIn:StrongSignInRequirement.Create(["mfa"],600));

    public static IEnumerable<(string Name,Func<Task> Run)> Store()
    {
        yield return ("Approval store resolves inside the account actor and approves once only from the reviewed screen",async()=>
        {
            var (api,store,_)=NewStore();var context=store.State.ContextId;
            Require(store.Resolve("no-code",context) is null && api.Resolves==0 && store.State.Message is not null);
            await Done(store.Resolve("https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH",context));
            Require(store.State.Phase==DeviceApprovalPhase.Review && store.State.Preview!.Address==Alice && store.State.HasCode);
            Require(api.Actors.Single() is{} actor && actor.SharesAuthorization(api.Accepted!) && !actor.IsActive);
            var proof=store.State.ProofId;
            Require(store.Approve("stale-proof",context) is null && store.Approve(proof,"other-context") is null && api.Approves==0);
            await Done(store.Approve(proof,context));
            Require(store.State.Phase==DeviceApprovalPhase.Approved && api.Approves==1 && store.Approve(proof,context) is null && store.Deny(proof,context) is null);
            Require(!store.State.ToString().Contains("ABCD") && !store.State.ToString().Contains("123"));
        });
        yield return ("A lost approval reply is unconfirmed and never resent without a fresh review and human action",async()=>
        {
            var (api,store,_)=NewStore();var context=store.State.ContextId;await Done(store.Resolve("ABCD-EFGH",context));
            api.OnApprove=_=>throw new HttpRequestException("synthetic lost reply");var proof=store.State.ProofId;
            await Done(store.Approve(proof,context));
            Require(store.State.Phase==DeviceApprovalPhase.Unconfirmed && api.Approves==1 && store.State.Message!.Contains("not confirmed"));
            Require(store.Approve(proof,context) is null && store.Approve(store.State.ProofId,context) is null && api.Approves==1);
            api.OnApprove=null;await Done(store.CheckStatus(context));
            Require(store.State.Phase==DeviceApprovalPhase.Review && api.Approves==1 && api.Resolves==2);
            api.Current=Preview() with{State="approved"};await Done(store.CheckStatus(context));
            Require(store.State.Phase==DeviceApprovalPhase.Approved && api.Approves==1 && store.Approve(store.State.ProofId,context) is null);
        });
        yield return ("Denial is acknowledgment only and a fresh resolve decides the final state",async()=>
        {
            foreach(var after in new[]{"denied","approved","pending"})
            {
                var (api,store,_)=NewStore();var context=store.State.ContextId;await Done(store.Resolve("ABCD-EFGH",context));
                api.OnDeny=_=>{api.Current=Preview() with{State=after=="pending"?"pending":after};return Task.CompletedTask;};
                await Done(store.Deny(store.State.ProofId,context));
                Require(api.Denies==1 && api.Resolves==2 && api.Approves==0);
                Require(store.State.Phase==(after switch{"denied"=>DeviceApprovalPhase.Denied,"approved"=>DeviceApprovalPhase.Approved,_=>DeviceApprovalPhase.Unconfirmed}));
                Require(store.Approve(store.State.ProofId,context) is null && store.Deny(store.State.ProofId,context) is null);
            }
        });
        yield return ("Strong sign-in returns to a fresh read of the same code and needs another explicit approval",async()=>
        {
            var (api,store,_)=NewStore();var context=store.State.ContextId;await Done(store.Resolve("ABCD-EFGH",context));
            var calls=0;api.OnApprove=_=>++calls==1?throw StepUp():Task.CompletedTask;
            await Done(store.Approve(store.State.ProofId,context));
            Require(store.State.NeedsVerification && store.State.Phase==DeviceApprovalPhase.Review && !store.CanApprove(store.State.ProofId,context));
            var old=api.Accepted!;AuthorizationLease? seen=null;
            await Done(store.VerifySignIn(store.State.ProofId,context,(requirement,expected,_)=>
            {Require(requirement.RequestedAcr=="mfa" && ReferenceEquals(expected,old));seen=expected;return Task.FromResult(api.Rotate());}));
            Require(seen is not null && !old.IsActive && store.State.Phase==DeviceApprovalPhase.Review && !store.State.NeedsVerification);
            Require(api.Resolves==2 && api.Approves==1 && store.State.ContextId!=context && api.Actors[^1]!.SharesAuthorization(api.Accepted!));
            Require(store.Approve(store.State.ProofId,context) is null);
            await Done(store.Approve(store.State.ProofId,store.State.ContextId));
            Require(api.Approves==2 && store.State.Phase==DeviceApprovalPhase.Approved);
        });
        yield return ("Verification for another person, a failure or a cancellation never resolves or approves",async()=>
        {
            foreach(var outcome in new[]{"other","failure","cancel","same-grant"})
            {
                var (api,store,_)=NewStore();var context=store.State.ContextId;await Done(store.Resolve("ABCD-EFGH",context));
                api.OnApprove=_=>throw StepUp();await Done(store.Approve(store.State.ProofId,context));
                var proof=store.State.ProofId;var started=new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
                var pending=store.VerifySignIn(proof,context,async(_,expected,ct)=>
                {
                    started.SetResult();
                    if(outcome=="other")return api.Rotate(Other);
                    if(outcome=="failure")throw new StrongSignInException();
                    if(outcome=="same-grant")return expected;
                    await Task.Delay(Timeout.Infinite,ct);throw new Exception("unreachable");
                });
                await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
                if(outcome=="cancel"){Require(store.State.Phase==DeviceApprovalPhase.Verifying);store.CancelVerification(context);}
                await Done(pending);
                Require(api.Resolves==1 && api.Approves==1);
                if(outcome is "other" or "same-grant")Require(store.State.Phase==DeviceApprovalPhase.Idle && !store.State.HasCode && store.State.Preview is null);
                else Require(store.State.Phase==DeviceApprovalPhase.Review && store.State.NeedsVerification && !store.CanApprove(store.State.ProofId,store.State.ContextId));
            }
        });
        yield return ("A replaced account discards delayed approval work and its late results",async()=>
        {
            var (api,store,_)=NewStore();var context=store.State.ContextId;var release=new TaskCompletionSource<DevicePairingPreview>(TaskCreationOptions.RunContinuationsAsynchronously);
            api.OnResolve=(_,_)=>release.Task;var pending=store.Resolve("ABCD-EFGH",context);
            Require(store.State.Phase==DeviceApprovalPhase.Resolving && store.Resolve("ABCD-EFGH",context) is null);
            var actor=api.Actors.Single()!;api.Rotate(Other);store.Synchronize();
            Require(!actor.IsActive && store.State.Phase==DeviceApprovalPhase.Idle && store.State.ContextId!=context);
            release.SetResult(Preview());await Done(pending);
            Require(store.State.Preview is null && !store.State.HasCode && store.State.Phase==DeviceApprovalPhase.Idle);
            api.Accepted!.Retire();store.Synchronize();Require(!store.IsCurrent() && store.Resolve("ABCD-EFGH",store.State.ContextId) is null);
        });
        yield return ("Server waits, expiry, missing authority and unsupported access disable decisions",async()=>
        {
            var (api,store,clock)=NewStore();var context=store.State.ContextId;
            api.OnResolve=(_,_)=>throw new PostboxException(429,"rate_limited",retryAfter:120);
            await Done(store.Resolve("ABCD-EFGH",context));
            Require(store.State.RetryAt is{} until && until>=clock.GetUtcNow().ToUnixTimeSeconds()+120 && store.Resolve("ABCD-EFGH",context) is null && api.Resolves==1);
            clock.Advance(121);api.OnResolve=null;store.RefreshExpiry();await Done(store.Resolve("ABCD-EFGH",context));
            Require(store.State.Phase==DeviceApprovalPhase.Review && store.CanApprove(store.State.ProofId,context));
            clock.Advance(601);store.RefreshExpiry();
            Require(store.State.Phase==DeviceApprovalPhase.Expired && store.Approve(store.State.ProofId,context) is null);
            foreach(var preview in new[]{Preview() with{CanApprove=false,Reason="delegation_required"},Preview() with{Capabilities=["mailbox","ssh"]}})
            {
                var (other,next,_)=NewStore();other.Current=preview;await Done(next.Resolve("ABCD-EFGH",next.State.ContextId));
                Require(next.State.Phase==DeviceApprovalPhase.Review && !next.CanApprove(next.State.ProofId,next.State.ContextId) && next.State.Message is not null);
                Require(next.Approve(next.State.ProofId,next.State.ContextId) is null && other.Approves==0);
            }
            var (missing,gone,_)=NewStore();missing.OnResolve=(_,_)=>throw new PostboxException(404,"pairing_not_found");
            await Done(gone.Resolve("ABCD-EFGH",gone.State.ContextId));Require(gone.State.Phase==DeviceApprovalPhase.Unavailable && gone.State.Preview is null);
        });
        yield return ("Approval over actual HTTP stops at a strong-authentication challenge and keeps Retry-After",async()=>
        {
            var tokens=new Tokens();var approvals=0;var mode="challenge";
            using var http=new HttpClient(new Handler((request,_)=>
            {
                if(request.RequestUri!.AbsolutePath.EndsWith("/resolve",StringComparison.Ordinal))
                {
                    if(mode=="limited"){var limited=new HttpResponseMessage(HttpStatusCode.TooManyRequests){Content=new StringContent("{\"error\":\"rate_limited\"}",Encoding.UTF8,"application/json")};
                        limited.Headers.RetryAfter=new(TimeSpan.FromSeconds(45));return Task.FromResult(limited);}
                    return Task.FromResult(Json(Reply("resolve")));
                }
                approvals++;
                var refused=new HttpResponseMessage(HttpStatusCode.Unauthorized){Content=new StringContent("{\"error\":\"step_up_required\"}",Encoding.UTF8,"application/json")};
                refused.Headers.TryAddWithoutValidation("WWW-Authenticate","Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", max_age=600");
                return Task.FromResult(refused);
            }));
            using var api=Client(http,tokens);var clock=new ManualClock();
            var store=new DeviceApprovalStore(api,()=>new(Person,tokens.AcceptedAuthorization!),clock:clock);store.Synchronize();var context=store.State.ContextId;
            await Done(store.Resolve("ABCD-EFGH",context));await Done(store.Approve(store.State.ProofId,context));
            Require(approvals==1 && store.State.NeedsVerification && store.State.Challenge!.MaxAge==600);
            mode="limited";store.ChangeCode(context);await Done(store.Resolve("ABCD-EFGH",context));
            Require(store.State.RetryAt==clock.GetUtcNow().ToUnixTimeSeconds()+45 && store.State.Phase==DeviceApprovalPhase.Idle);
        });
    }
}
