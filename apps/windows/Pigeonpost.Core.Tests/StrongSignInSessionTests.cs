using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static partial class StrongSignInSessionTests
{
    private const string Issuer="https://issuer.invalid/realms/synthetic", Subject="synthetic-member";
    private static readonly Uri Redirect=new("http://127.0.0.1:49152/");
    public static IEnumerable<(string Name,Func<Task> Run)> All()
    {
        yield return ("Windows strong sign-in freezes the trusted issuer and generates fresh PKCE state and nonce",async()=>
        {
            using var f=await Fixture.Create();using var first=f.Begin();using var second=f.Begin();
            var one=Query(first.AuthorizationUri);var two=Query(second.AuthorizationUri);
            Require(first.AuthorizationUri.GetLeftPart(UriPartial.Path)==Issuer+"/protocol/openid-connect/auth");
            Require(one["client_id"]==AccountSession.ClientId && one["response_type"]=="code" && one["redirect_uri"]==Redirect.AbsoluteUri);
            Require(one["code_challenge_method"]=="S256" && one["acr_values"]=="mfa" && one["max_age"]=="600");
            Require(new[]{"state","nonce","code_challenge"}.All(k=>one[k].Length==43 && one[k]!=two[k]));
            Require(!first.ToString().Contains(one["state"]) && !first.MatchesCallback(f.Callback(first)));
        });
        yield return ("Only an exact owned loopback return and correlated callback can start verification",async()=>
        {
            using var f=await Fixture.Create();var root=f.Session.AcceptedAuthorization!;
            foreach(var uri in new[]{"https://127.0.0.1:49152/","http://localhost:49152/","http://127.0.0.1:49152/callback","http://127.0.0.1:49152/?x=1","http://127.0.0.1:80/","http://127.1:49152/"})
                await Fails<StrongSignInException>(()=>{f.Session.BeginStrongSignIn(Needs(),root,new Uri(uri));return Task.CompletedTask;});
            using var request=f.Begin();var valid=f.Callback(request);var state=Query(request.AuthorizationUri)["state"];
            foreach(var uri in new[]{valid.AbsoluteUri.Replace(":49152/",":49153/"),valid.AbsoluteUri.Replace("127.0.0.1","localhost"),
                valid.AbsoluteUri.Replace(state,"wrong-state"),valid.AbsoluteUri+"&state="+state,valid.AbsoluteUri+"&code=other",
                valid.AbsoluteUri+"#fragment",valid.AbsoluteUri+"&iss=https%3A%2F%2Fother.invalid",valid.AbsoluteUri.Replace("/?","/callback?")})
            {
                Require(!request.MatchesCallback(new Uri(uri)));
                await Fails<StrongSignInException>(()=>f.Session.CompleteStrongSignInAsync(request,new Uri(uri)));
            }
            Require(f.CodeCalls==0 && root.IsActive && request.MatchesCallback(valid));
            await f.Session.CompleteStrongSignInAsync(request,valid);Require(f.CodeCalls==1);
        });
        yield return ("Same-person strong verification atomically rotates the grant after protected credential save",async()=>
        {
            using var f=await Fixture.Create();var old=f.Session.AcceptedAuthorization!;var owner=f.Session.RecoveryOwnerKey;
            using var request=f.Begin();var adopted=await f.Session.CompleteStrongSignInAsync(request,f.Callback(request));
            Require(!old.IsActive && adopted.IsActive && ReferenceEquals(adopted,f.Session.AcceptedAuthorization));
            Require(f.Store.Value=="refresh-strong" && owner==f.Session.RecoveryOwnerKey && f.CodeCalls==1);
            var challenge=Query(request.AuthorizationUri)["code_challenge"];
            var verifier=f.CodeFields!["code_verifier"];
            Require(Enc(SHA256.HashData(Encoding.ASCII.GetBytes(verifier)))==challenge && verifier.Length==43);
            Require(f.CodeFields["grant_type"]=="authorization_code" && f.CodeFields["redirect_uri"]==Redirect.AbsoluteUri);
            await Fails<AuthorizationRetiredException>(()=>f.Session.CompleteStrongSignInAsync(request,f.Callback(request,"second-code")));
            Require(f.CodeCalls==1);
        });
        yield return ("A rejected fresh candidate leaves the original account and stored session intact",async()=>
        {
            foreach(var change in new[]{"subject","nonce","method","time","issuer","missing_refresh"})
            {
                using var f=await Fixture.Create();var old=f.Session.AcceptedAuthorization!;var saved=f.Store.Value;
                f.CodeReply=fields=>Task.FromResult(f.StrongReply(fields,change));using var request=f.Begin();
                if(change=="missing_refresh")await Fails<SignInException>(()=>f.Session.CompleteStrongSignInAsync(request,f.Callback(request)));
                else await Fails<StrongSignInException>(()=>f.Session.CompleteStrongSignInAsync(request,f.Callback(request)));
                Require(ReferenceEquals(old,f.Session.AcceptedAuthorization) && old.IsActive && f.Store.Value==saved && f.CodeCalls==1);
            }
        });
        yield return ("Cancellation cannot adopt a noncooperative late verification response",async()=>
        {
            using var f=await Fixture.Create();var old=f.Session.AcceptedAuthorization!;var saved=f.Store.Value;
            var started=Signal();var release=Signal();f.CodeReply=async fields=>{var reply=f.StrongReply(fields);started.SetResult();await release.Task;return reply;};
            using var request=f.Begin();var pending=f.Session.CompleteStrongSignInAsync(request,f.Callback(request));
            await started.Task.WaitAsync(TimeSpan.FromSeconds(5));f.Session.CancelStrongSignIn(request);release.SetResult();
            await Fails<AuthorizationRetiredException>(()=>pending);
            Require(ReferenceEquals(old,f.Session.AcceptedAuthorization) && old.IsActive && f.Store.Value==saved);
        });
        yield return ("Sign-out wins over a delayed fresh token exchange",async()=>
        {
            using var f=await Fixture.Create();var old=f.Session.AcceptedAuthorization!;var started=Signal();var release=Signal();
            f.CodeReply=async fields=>{var reply=f.StrongReply(fields);started.SetResult();await release.Task;return reply;};
            using var request=f.Begin();var pending=f.Session.CompleteStrongSignInAsync(request,f.Callback(request));
            await started.Task.WaitAsync(TimeSpan.FromSeconds(5));await f.Session.SignOutAsync(default);release.SetResult();
            await Fails<AuthorizationRetiredException>(()=>pending);Require(f.Session.AcceptedAuthorization is null && !old.IsActive && f.Store.Value is null);
        });
        yield return ("An old verifier cannot erase a replacement request when it completes",async()=>
        {
            using var f=await Fixture.Create();var started=Signal();var release=Signal();
            f.CodeReply=async fields=>{var reply=f.StrongReply(fields);started.TrySetResult();if(fields["code"]=="old-code")await release.Task;return reply;};
            using var old=f.Begin();var pending=f.Session.CompleteStrongSignInAsync(old,f.Callback(old,"old-code"));await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            using var replacement=f.Begin();release.SetResult();await Fails<AuthorizationRetiredException>(()=>pending);
            var adopted=await f.Session.CompleteStrongSignInAsync(replacement,f.Callback(replacement,"replacement-code"));Require(adopted.IsActive && f.CodeCalls==2);
        });
        yield return ("A late verifier cannot replace a newly accepted login with the same person",async()=>
        {
            using var f=await Fixture.Create();var old=f.Session.AcceptedAuthorization!;var started=Signal();var release=Signal();
            f.CodeReply=async fields=>{var reply=f.StrongReply(fields);started.SetResult();await release.Task;return reply;};
            using var request=f.Begin();var pending=f.Session.CompleteStrongSignInAsync(request,f.Callback(request));await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            Require(await f.Session.RestoreAsync(default));var replacement=f.Session.AcceptedAuthorization!;release.SetResult();
            await Fails<AuthorizationRetiredException>(()=>pending);
            Require(!old.IsActive && replacement.IsActive && ReferenceEquals(replacement,f.Session.AcceptedAuthorization) && f.Store.Value=="refresh-restored");
        });
        yield return ("Duplicate completion cannot cancel the original successful exchange",async()=>
        {
            using var f=await Fixture.Create();var started=Signal();var release=Signal();
            f.CodeReply=async fields=>{var reply=f.StrongReply(fields);started.SetResult();await release.Task;return reply;};
            using var request=f.Begin();var callback=f.Callback(request);var pending=f.Session.CompleteStrongSignInAsync(request,callback);await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            await Fails<StrongSignInException>(()=>f.Session.CompleteStrongSignInAsync(request,callback));release.SetResult();
            var adopted=await pending;Require(adopted.IsActive && f.CodeCalls==1);
        });
        yield return ("Original and later caller retirement prevent verification adoption",async()=>
        {
            foreach(var original in new[]{false,true})
            {
                using var f=await Fixture.Create();var root=f.Session.AcceptedAuthorization!;var caller=root.Child();StrongSignInRequest request;
                using(new AuthorizationContext(original?caller:root))request=f.Begin();
                using(request)
                {
                    var started=Signal();var release=Signal();f.CodeReply=async fields=>{var reply=f.StrongReply(fields);started.SetResult();await release.Task;return reply;};
                    Task<AuthorizationLease> pending;
                    using(new AuthorizationContext(original?root:caller))pending=f.Session.CompleteStrongSignInAsync(request,f.Callback(request));
                    await started.Task.WaitAsync(TimeSpan.FromSeconds(5));caller.Retire();release.SetResult();
                    await Fails<AuthorizationRetiredException>(()=>pending);Require(root.IsActive && ReferenceEquals(root,f.Session.AcceptedAuthorization));
                }
            }
        });
        yield return ("A protected credential-store failure preserves the accepted login",async()=>
        {
            using var f=await Fixture.Create();var root=f.Session.AcceptedAuthorization!;var saved=f.Store.Value;using var request=f.Begin();f.Store.Fail=true;
            await Fails<IOException>(()=>f.Session.CompleteStrongSignInAsync(request,f.Callback(request)));
            Require(root.IsActive && ReferenceEquals(root,f.Session.AcceptedAuthorization) && f.Store.Value==saved);
        });
        yield return ("Expired requests and a browser cancellation keep the existing account",async()=>
        {
            using var f=await Fixture.Create();var root=f.Session.AcceptedAuthorization!;
            using(var expired=f.Begin()){var callback=f.Callback(expired);f.Clock.Advance(601);await Fails<StrongSignInException>(()=>f.Session.CompleteStrongSignInAsync(expired,callback));}
            using var cancelled=f.Begin();var state=Query(cancelled.AuthorizationUri)["state"];
            await Fails<SignInException>(()=>f.Session.CompleteStrongSignInAsync(cancelled,new Uri(Redirect+"?state="+state+"&error=access_denied&error_description=private-fixture")));
            Require(root.IsActive && ReferenceEquals(root,f.Session.AcceptedAuthorization) && f.CodeCalls==0);
        });
        yield return ("Freshness is rechecked after waiting for an ordinary renewal",async()=>
        {
            using var f=await Fixture.Create();var root=f.Session.AcceptedAuthorization!;using var request=f.Begin(0);
            var started=Signal();var release=Signal();f.RenewReply=async()=>{started.SetResult();await release.Task;return f.OrdinaryReply();};
            var renewing=f.Session.RefreshTokenAsync(default);await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            var pending=f.Session.CompleteStrongSignInAsync(request,f.Callback(request));await f.CodeStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));
            // Go beyond the proof validator's explicit 30-second clock allowance.
            f.Clock.Advance(31);release.SetResult();await renewing;await Fails<StrongSignInException>(()=>pending);
            Require(root.IsActive && ReferenceEquals(root,f.Session.AcceptedAuthorization) && f.Store.Value!="refresh-strong");
        });
    }
    private static StrongSignInRequirement Needs(int age=600)=>StrongSignInRequirement.Create(["mfa"],age)!;
    private static Dictionary<string,string> Query(Uri uri)=>uri.Query.TrimStart('?').Split('&').Select(p=>p.Split('=',2))
        .ToDictionary(p=>Uri.UnescapeDataString(p[0]),p=>Uri.UnescapeDataString(p[1].Replace('+',' ')),StringComparer.Ordinal);
    private static string Enc(byte[] bytes)=>Convert.ToBase64String(bytes).TrimEnd('=').Replace('+','-').Replace('/','_');
    private static string Jwt(Dictionary<string,object> claims)=>Enc(Encoding.UTF8.GetBytes("{\"alg\":\"RS256\"}"))+"."+Enc(JsonSerializer.SerializeToUtf8Bytes(claims))+".c2ln";
    private static TaskCompletionSource Signal()=>new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static void Require(bool value){if(!value)throw new Exception("Strong session behavior assertion failed.");}
    private static async Task<T> Fails<T>(Func<Task> action)where T:Exception
    {try{await action().WaitAsync(TimeSpan.FromSeconds(5));}catch(T error){return error;}throw new Exception("Expected "+typeof(T).Name);}
    private sealed class Clock:TimeProvider
    {private long now=1800000000;public override DateTimeOffset GetUtcNow()=>DateTimeOffset.FromUnixTimeSeconds(Interlocked.Read(ref now));public void Advance(int seconds)=>Interlocked.Add(ref now,seconds);}
    private sealed class Store:IRefreshTokenStore
    {public string? Value="refresh-initial";public bool Fail;public string? Load()=>Value;public void Save(string value){if(Fail)throw new IOException("Synthetic vault unavailable");Value=value;}public void Clear()=>Value=null;}
    private sealed class Fixture:IDisposable
    {
        public readonly Clock Clock=new();public readonly Store Store=new();public AccountSession Session=null!;
        public int CodeCalls;public Dictionary<string,string>? CodeFields;public readonly TaskCompletionSource CodeStarted=Signal();
        public Func<Dictionary<string,string>,Task<HttpResponseMessage>>? CodeReply;public Func<Task<HttpResponseMessage>>? RenewReply;
        private readonly Dictionary<string,string> nonces=new();private HttpClient http=null!;
        public static async Task<Fixture> Create()
        {
            var f=new Fixture();f.http=new(new Handler(async(request,ct)=>
            {
                Require(request.RequestUri!.AbsoluteUri==Issuer+"/protocol/openid-connect/token");
                var fields=Query(new Uri("http://127.0.0.1/?"+await request.Content!.ReadAsStringAsync(ct)));
                if(fields["grant_type"]=="authorization_code")
                {
                    f.CodeCalls++;f.CodeFields=fields;
                    var reply=f.CodeReply is null?Task.FromResult(f.StrongReply(fields)):f.CodeReply(fields);
                    f.CodeStarted.TrySetResult();return await reply;
                }
                Require(fields["grant_type"]=="refresh_token");return f.RenewReply is null?f.OrdinaryReply():await f.RenewReply();
            }));
            f.Session=new(f.http,f.Store,new Uri(Issuer+"/"),f.Clock);Require(await f.Session.RestoreAsync(default));return f;
        }
        public StrongSignInRequest Begin(int age=600)=>Session.BeginStrongSignIn(Needs(age),Session.AcceptedAuthorization!,Redirect);
        public Uri Callback(StrongSignInRequest request,string code="synthetic-code")
        {var fields=Query(request.AuthorizationUri);nonces[code]=fields["nonce"];return new(fields["redirect_uri"]+"?code="+code+"&state="+fields["state"]+"&iss="+Uri.EscapeDataString(Issuer));}
        private Dictionary<string,object> Claims()=>new(){{"iss",Issuer},{"sub",Subject},{"azp",AccountSession.ClientId},
            {"exp",Clock.GetUtcNow().ToUnixTimeSeconds()+3600},{"iat",Clock.GetUtcNow().ToUnixTimeSeconds()},{"auth_time",Clock.GetUtcNow().ToUnixTimeSeconds()},{"acr","mfa"}};
        public HttpResponseMessage OrdinaryReply()=>Reply(new{access_token=Jwt(Claims()),refresh_token="refresh-restored",expires_in=3600});
        public HttpResponseMessage StrongReply(Dictionary<string,string> fields,string? change=null)
        {
            var access=Claims();var id=Claims();id["aud"]=AccountSession.ClientId;id["nonce"]=nonces[fields["code"]];
            if(change=="subject")access["sub"]="private-other-person";if(change=="nonce")id["nonce"]="other";
            if(change=="method")access["acr"]="weak";if(change=="time")id["auth_time"]=Clock.GetUtcNow().ToUnixTimeSeconds()-631;
            if(change=="issuer")access["iss"]="https://other.invalid";
            return Reply(new{access_token=Jwt(access),id_token=Jwt(id),refresh_token=change=="missing_refresh"?null:"refresh-strong",expires_in=3600});
        }
        private static HttpResponseMessage Reply(object value)=>new(HttpStatusCode.OK){Content=new StringContent(JsonSerializer.Serialize(value),Encoding.UTF8,"application/json")};
        public void Dispose()=>http.Dispose();
    }
    private sealed class Handler(Func<HttpRequestMessage,CancellationToken,Task<HttpResponseMessage>> send):HttpMessageHandler
    {protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request,CancellationToken ct)=>send(request,ct);}
}
