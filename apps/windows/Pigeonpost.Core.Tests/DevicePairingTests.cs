using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

internal static class DevicePairingTests
{
    private const string Alice="/k/ec55xq18rbwdnwpsvkht07az40", Bob="/k/rywv63mb6h0hhrpfjewrrttspw";
    private static readonly DevicePairingCode Code=DevicePairingCode.Parse("ABCD-EFGH")!;
    private static DevicePairingPreview Preview()=>new("pair_fixture",new("org_fixture","Example"),"/example/build","123 456",["mailbox"],
        DateTimeOffset.UtcNow.ToUnixTimeSeconds(),DateTimeOffset.UtcNow.ToUnixTimeSeconds()+600,Address:Alice,CanApprove:true,State:"pending");
    public static IEnumerable<(string Name,Func<Task> Run)> All()
    {
        yield return ("Pairing codes normalize ASCII input and keep terminal proofs out of diagnostics",()=>
        {
            foreach(var value in new[]{"abcd-efgh","ABCDEFGH"," ABCD-EFGH\r\n"}) Require(DevicePairingCode.Parse(value)!.Equals(Code));
            foreach(var value in new[]{"ABCD--EFGH","ABC-EFGH","ABCD EFGH","ＡBCD-EFGH","ıBCD-EFGH","ABCD-EFGH-extra","ABCD-ÉFGH",new string('A',129)})
                Require(DevicePairingCode.Parse(value) is null);
            Require(!Code.ToString().Contains("ABCD") && !Preview().ToString().Contains("123"));
            return Task.CompletedTask;
        });
        yield return ("Pairing links require the exact trusted origin path and raw fragment",()=>
        {
            Require(DevicePairingLink.Parse("https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH")!.Equals(Code));
            Require(DevicePairingLink.Parse("HTTPS://INBOX.PIGEONPOST.DEV/pair.html#abcdefgh")!.Equals(Code));
            foreach(var value in new[]{
                "http://inbox.pigeonpost.dev/pair.html#ABCD-EFGH", "https://inbox.pigeonpost.dev:443/pair.html#ABCD-EFGH",
                "https://inbox.pigeonpost.dev.evil.test/pair.html#ABCD-EFGH","https://user@inbox.pigeonpost.dev/pair.html#ABCD-EFGH",
                "https://inbox.pigeonpost.dev/pair.html?code=ABCD-EFGH","https://inbox.pigeonpost.dev/pair.html?x=1#ABCD-EFGH",
                "https://inbox.pigeonpost.dev/Pair.html#ABCD-EFGH","https://inbox.pigeonpost.dev/pair.html#%41BCD-EFGH",
                "https://inbox.pigeonpost.dev/pair.html# ABCD-EFGH","https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH#extra" })
                Require(DevicePairingLink.Parse(value) is null);
            return Task.CompletedTask;
        });
        yield return ("Pairing previews distinguish explicit creation and absent or unsupported authority",()=>
        {
            var p=Preview();var now=DateTimeOffset.UtcNow.ToUnixTimeSeconds();Require(p.IsValid && p.AllowsApproval(now));
            Require((p with{Address=null,CreatesAgent=true}).AllowsApproval(now));
            foreach(var invalid in new[]{p with{Address=null},p with{Address=Alice,CreatesAgent=true},p with{Address="/k/fixture"},
                p with{PairingId="../substitute"},p with{Handle="/k/build"},p with{Confirmation="１２３４５６"},p with{State="unknown"},
                p with{Org=null!},p with{Capabilities=null!},p with{Label="visible\u202eoverride"},p with{ExpiresAt=long.MaxValue}})
                Require(!invalid.IsValid && !invalid.AllowsApproval(now));
            foreach(var refused in new[]{p with{CanApprove=false},p with{Capabilities=["mailbox","ssh"]},p with{Capabilities=["other"]},
                p with{State="approved"},p with{State="denied"},p with{ExpiresAt=now},p with{RequestedAt=now+61,ExpiresAt=now+600}})
                Require(!refused.AllowsApproval(now));
            return Task.CompletedTask;
        });
        yield return ("Pairing HTTP uses fixed code-only POST routes and validates the complete preview",async()=>
        {
            var tokens=new Tokens();var p=Preview();var count=0;
            using var http=new HttpClient(new Handler(async(request,ct)=>
            {
                count++;Require(request.Method==HttpMethod.Post && request.RequestUri!.AbsolutePath=="/v1/device-pairings/resolve");
                Require(!request.RequestUri!.AbsoluteUri.Contains("ABCD") && !request.Headers.Contains("x-pigeonpost-identity"));
                Require(request.Headers.Authorization?.Parameter=="fixture-human");
                using var body=JsonDocument.Parse(await request.Content!.ReadAsStringAsync(ct));
                Require(body.RootElement.EnumerateObject().Count()==1 && body.RootElement.GetProperty("user_code").GetString()==Code.DisplayValue);
                return Json(JsonSerializer.Serialize(p,PostboxJson.Options));
            }));
            using var api=Client(http,tokens);var resolved=await api.ResolveDevicePairingAsync(Code,default);
            Require(resolved.Address==Alice && resolved.PairingId==p.PairingId && count==1);
        });
        yield return ("Pairing approval acknowledgments must match the reviewed canonical inbox and handle",async()=>
        {
            foreach(var body in new[]{
                JsonSerializer.Serialize(new{approved=true,address=Bob,handle="/example/build"}),
                JsonSerializer.Serialize(new{approved=true,address=Alice,handle="/example/other"}),
                "{\"approved\":true,\"address\":\"/k/fixture\",\"handle\":\"/example/build\"}",
                "{\"approved\":true}","{\"approved\":false}","{\"approved\":\"true\"}"})
            {
                var tokens=new Tokens();var count=0;
                using var http=new HttpClient(new Handler((request,_)=>{count++;Require(request.RequestUri!.AbsolutePath=="/v1/device-pairings/pair_fixture/approve");return Task.FromResult(Json(body));}));
                using var api=Client(http,tokens);var error=await Fails<PostboxException>(()=>api.ApproveDevicePairingAsync(Preview(),Code,default));
                Require(error.Code=="bad_response" && count==1);
            }
        });
        yield return ("Approval validates an explicit new agent without borrowing another target",async()=>
        {
            var tokens=new Tokens();var count=0;
            using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json(JsonSerializer.Serialize(new{approved=true,address=Alice,handle="/example/build"})));}));
            using var api=Client(http,tokens);
            await api.ApproveDevicePairingAsync(Preview(),Code,default);
            await api.ApproveDevicePairingAsync(Preview() with{Address=null,CreatesAgent=true},Code,default);Require(count==2);
        });
        yield return ("Missing account and nonactionable previews never send a decision request",async()=>
        {
            var tokens=new Tokens();var count=0;
            using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json("{}"));}));
            using var api=Client(http,tokens);
            foreach(var p in new[]{Preview() with{CanApprove=false},Preview() with{State="approved"},Preview() with{ExpiresAt=1}})
                await Fails<PostboxException>(()=>api.ApproveDevicePairingAsync(p,Code,default));
            tokens.AcceptedAuthorization!.Retire();
            await Fails<AuthorizationRetiredException>(()=>api.ResolveDevicePairingAsync(Code,default));Require(count==0);
        });
        yield return ("Pairing success bodies are bounded and malformed previews fail closed",async()=>
        {
            foreach(var body in new[]{new string('x',1024*1024+1),"{}","null","{\"pairing_id\":\"pair_fixture\",\"pairing_id\":\"other\"}",
                JsonSerializer.Serialize(Preview() with{Org=null!},PostboxJson.Options)})
            {
                var tokens=new Tokens();var count=0;
                using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json(body));}));
                using var api=Client(http,tokens);var error=await Fails<PostboxException>(()=>api.ResolveDevicePairingAsync(Code,default));
                Require(error.Code=="bad_response" && count==1);
            }
        });
        yield return ("Every late pairing response retains its original account and independent caller",async()=>
        {
            foreach(var kind in new[]{"resolve","approve","deny"})foreach(var replace in new[]{false,true})
            {
                var tokens=new Tokens();var caller=tokens.AcceptedAuthorization!.Child();var content=new DeferredContent(Reply(kind));
                using var http=new HttpClient(new Handler((_,_)=>Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK){Content=content})));
                using var api=Client(http,tokens);
                using(new AuthorizationContext(caller))
                {
                    var pending=Invoke(api,kind);await content.Started.Task.WaitAsync(TimeSpan.FromSeconds(5));
                    if(replace)tokens.Replace();else caller.Retire();content.Release.SetResult();
                    await Fails<AuthorizationRetiredException>(()=>pending);
                }
            }
        });
        yield return ("Response disposal cannot return pairing data after account replacement",async()=>
        {
            foreach(var kind in new[]{"resolve","approve","deny"})
            {
                var tokens=new Tokens();
                using var http=new HttpClient(new Handler((_,_)=>Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK){Content=new ReplaceOnDisposeContent(Reply(kind),tokens.Replace)})));
                using var api=Client(http,tokens);await Fails<AuthorizationRetiredException>(()=>Invoke(api,kind));
            }
        });
        yield return ("Unknown decision replies never resend and a denial remains only an acknowledgment",async()=>
        {
            var tokens=new Tokens();var count=0;
            using var http=new HttpClient(new Handler((request,_)=>
            {
                count++;
                if(request.RequestUri!.AbsolutePath.EndsWith("/approve"))throw new HttpRequestException("Synthetic lost reply");
                return Task.FromResult(Json("{\"denied\":true}"));
            }));
            using var api=Client(http,tokens);await Fails<HttpRequestException>(()=>api.ApproveDevicePairingAsync(Preview(),Code,default));
            Require(count==1);await api.DenyDevicePairingAsync(Preview(),Code,default);Require(count==2);
        });
    }
    private static string Reply(string kind)=>kind=="resolve"?JsonSerializer.Serialize(Preview(),PostboxJson.Options):kind=="deny"?"{\"denied\":true}":JsonSerializer.Serialize(new{approved=true,address=Alice,handle="/example/build"});
    private static Task Invoke(PostboxClient api,string kind)=>kind=="resolve"?api.ResolveDevicePairingAsync(Code,default):kind=="approve"?api.ApproveDevicePairingAsync(Preview(),Code,default):api.DenyDevicePairingAsync(Preview(),Code,default);
    private static PostboxClient Client(HttpClient http,Tokens tokens)=>new(http,tokens,new Uri("http://127.0.0.1:1/"));
    private static void Require(bool condition){if(!condition)throw new Exception("Pairing behavior assertion failed.");}
    private static async Task<T> Fails<T>(Func<Task> action)where T:Exception
    {try{await action().WaitAsync(TimeSpan.FromSeconds(5));}catch(T error){return error;}throw new Exception("Expected "+typeof(T).Name);}
    private static HttpResponseMessage Json(string body)=>new(HttpStatusCode.OK){Content=new StringContent(body,Encoding.UTF8,"application/json")};
    private sealed class Handler(Func<HttpRequestMessage,CancellationToken,Task<HttpResponseMessage>> send):HttpMessageHandler
    {protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request,CancellationToken ct)=>send(request,ct);}
    private sealed class Tokens:IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization{get;private set;}=new();
        public Task<string> GetTokenAsync(CancellationToken ct)=>Task.FromResult("fixture-human");
        public Task<string> RefreshTokenAsync(CancellationToken ct)=>throw new Exception("Unexpected pairing renewal");
        public void Replace()=>AcceptedAuthorization=new AuthorizationLease();
    }
    private sealed class DeferredContent(string body):HttpContent
    {
        public TaskCompletionSource Started{get;}=new(TaskCreationOptions.RunContinuationsAsynchronously);
        public TaskCompletionSource Release{get;}=new(TaskCreationOptions.RunContinuationsAsynchronously);
        protected override async Task SerializeToStreamAsync(Stream stream,TransportContext? context)
        {Started.TrySetResult();await Release.Task;await stream.WriteAsync(Encoding.UTF8.GetBytes(body));}
        protected override bool TryComputeLength(out long length){length=0;return false;}
    }
    private sealed class ReplaceOnDisposeContent(string body,Action replace):StringContent(body,Encoding.UTF8,"application/json")
    {protected override void Dispose(bool disposing){if(disposing)replace();base.Dispose(disposing);}}
}
