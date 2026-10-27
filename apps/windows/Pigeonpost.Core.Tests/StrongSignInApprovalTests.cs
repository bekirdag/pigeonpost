using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

internal static partial class StrongSignInSessionTests
{
    private const string Agent="/k/ec55xq18rbwdnwpsvkht07az40";
    private sealed class PairingPostbox:IDisposable
    {
        public int Resolves,Approves;public string? LastBearer;public readonly HttpClient Http;
        public PairingPostbox(AccountSession session)
        {
            Http=new(new Handler((request,_)=>
            {
                LastBearer=request.Headers.Authorization?.Parameter;
                var now=DateTimeOffset.UtcNow.ToUnixTimeSeconds();
                if(request.RequestUri!.AbsolutePath=="/v1/device-pairings/resolve")
                {
                    Resolves++;
                    return Task.FromResult(Json(JsonSerializer.Serialize(new DevicePairingPreview("pair_fixture",new("org_fixture","Example"),"/example/build","123 456",
                        ["mailbox"],now,now+600,Address:Agent,CanApprove:true,State:"pending"),PostboxJson.Options)));
                }
                Require(request.RequestUri.AbsolutePath=="/v1/device-pairings/pair_fixture/approve");
                if(++Approves==1)
                {
                    var refused=new HttpResponseMessage(HttpStatusCode.Unauthorized){Content=new StringContent("{\"error\":\"step_up_required\",\"acr_values\":[\"mfa\"],\"max_age\":600}",Encoding.UTF8,"application/json")};
                    return Task.FromResult(refused);
                }
                return Task.FromResult(Json("{\"approved\":true,\"address\":\""+Agent+"\",\"handle\":\"/example/build\"}"));
            }));
            Client=new(Http,session,new Uri("http://127.0.0.1:1/"));
        }
        public PostboxClient Client{get;}
        private static HttpResponseMessage Json(string body)=>new(HttpStatusCode.OK){Content=new StringContent(body,Encoding.UTF8,"application/json")};
        public void Dispose(){Client.Dispose();Http.Dispose();}
    }
    private static DeviceApprovalStore ApprovalStore(Fixture f,PairingPostbox postbox)
    {
        var store=new DeviceApprovalStore(postbox.Client,()=>f.Session.AcceptedAuthorization is{IsActive:true} grant && f.Session.RecoveryOwnerKey is{} owner?new(owner,grant):null);
        store.Synchronize();return store;
    }

    public static IEnumerable<(string Name,Func<Task> Run)> Approval()
    {
        yield return ("System-browser verification returns through loopback, adopts the same person, rereads and waits for another approval",async()=>
        {
            using var f=await Fixture.Create();using var postbox=new PairingPostbox(f.Session);var store=ApprovalStore(f,postbox);
            var old=f.Session.AcceptedAuthorization!;var context=store.State.ContextId;
            await store.Resolve("ABCD-EFGH",context)!;await store.Approve(store.State.ProofId,context)!;
            Require(store.State.NeedsVerification && postbox.Approves==1 && f.CodeCalls==0);
            Uri? opened=null;string? page=null;
            await store.VerifySignIn(store.State.ProofId,context,(requirement,expected,ct)=>StrongSignInBroker.VerifyAsync(f.Session,requirement,expected,async uri=>
            {
                opened=uri;var callback=f.CallbackFor(uri);
                Require(callback.Host=="127.0.0.1" && callback.Port!=Redirect.Port && Query(uri)["code_challenge_method"]=="S256");
                page=await BrowserReturn(callback,callback.PathAndQuery);return true;
            },ct))!.WaitAsync(TimeSpan.FromSeconds(8));
            Require(opened is not null && page!.StartsWith("HTTP/1.1 200",StringComparison.Ordinal) && f.CodeCalls==1);
            Require(!old.IsActive && f.Session.AcceptedAuthorization is{IsActive:true} && f.Store.Value=="refresh-strong");
            Require(store.State.Phase==DeviceApprovalPhase.Review && !store.State.NeedsVerification && postbox.Resolves==2 && postbox.Approves==1);
            Require(store.Approve(store.State.ProofId,context) is null);
            await store.Approve(store.State.ProofId,store.State.ContextId)!;
            Require(postbox.Approves==2 && store.State.Phase==DeviceApprovalPhase.Approved);
        });
        yield return ("A browser that cannot open or a returned denial leaves the existing sign-in and decision untouched",async()=>
        {
            foreach(var outcome in new[]{"closed","denied"})
            {
                using var f=await Fixture.Create();using var postbox=new PairingPostbox(f.Session);var store=ApprovalStore(f,postbox);
                var old=f.Session.AcceptedAuthorization!;var context=store.State.ContextId;
                await store.Resolve("ABCD-EFGH",context)!;await store.Approve(store.State.ProofId,context)!;
                await store.VerifySignIn(store.State.ProofId,context,(requirement,expected,ct)=>StrongSignInBroker.VerifyAsync(f.Session,requirement,expected,async uri=>
                {
                    if(outcome=="closed")return false;
                    var fields=Query(uri);var redirect=new Uri(fields["redirect_uri"]);
                    await BrowserReturn(redirect,"/?state="+Uri.EscapeDataString(fields["state"])+"&error=access_denied");return true;
                },ct))!.WaitAsync(TimeSpan.FromSeconds(8));
                Require(f.CodeCalls==0 && old.IsActive && ReferenceEquals(old,f.Session.AcceptedAuthorization) && f.Store.Value=="refresh-restored");
                Require(store.State.Phase==DeviceApprovalPhase.Review && store.State.NeedsVerification && store.State.Message is not null);
                Require(postbox.Approves==1 && postbox.Resolves==1 && store.IsCurrent(context));
            }
        });
    }
}
