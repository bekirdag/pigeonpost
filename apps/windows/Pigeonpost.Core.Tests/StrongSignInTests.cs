using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class StrongSignInTests
{
    private const string Challenge = "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", max_age=600";
    private const string Problem = "{\"error\":\"step_up_required\",\"acr_values\":[\"mfa\"],\"max_age\":600}";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Strong requirements accept bounded methods and recency without mutable caller storage", () =>
        {
            var methods = new[] { "mfa", "urn:example:strong" };
            var requirement = StrongSignInRequirement.Create(methods, 0)!; methods[0] = "changed";
            Require(requirement.RequestedAcr == "mfa urn:example:strong" && requirement.MaxAge == 0);
            Require(!requirement.ToString().Contains("urn:example"));
            foreach (var age in new[] { -1, 601, int.MaxValue }) Require(StrongSignInRequirement.Create(["mfa"], age) is null);
            foreach (var values in new[] { Array.Empty<string>(), new[] { "mfa", "mfa" }, new[] { "mfa other" }, new[] { "mfa\n" }, new[] { "ｍfa" }, Enumerable.Range(0,9).Select(i=>"m"+i).ToArray() })
                Require(StrongSignInRequirement.Create(values, 600) is null);
            return Task.CompletedTask;
        });
        yield return ("Strong challenge parser preserves quoted boundaries and rejects duplicate or injected parameters", () =>
        {
            var valid = StrongSignInRequirement.Parse(null, Challenge)!;
            Require(valid.RequestedAcr == "mfa" && valid.MaxAge == 600);
            foreach (var header in new[] {
                "Bearer error=\"insufficient_user_authentication\", error_description=\"acr_values=mfa, max_age=600\"",
                Challenge + ", ACR_VALUES=\"weak\"", Challenge + ", max_age=0", Challenge + ", extra=\"unterminated",
                "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", max_age=601",
                "Bearer error=\"insufficient_user_authentication\", acr_values=\"mfa\", max_age=\"600x\"",
                Challenge + "\r\n", "Basic " + Challenge, new string('x',2049) })
                Require(StrongSignInRequirement.Parse(null,header) is null);
            return Task.CompletedTask;
        });
        yield return ("Strong body requirements reject wrong JSON types and duplicate fields", () =>
        {
            using var good = JsonDocument.Parse(Problem);
            Require(StrongSignInRequirement.Parse(good.RootElement,null)?.RequestedAcr == "mfa");
            foreach (var body in new[] {
                "{\"acr_values\":[\"mfa\"],\"max_age\":\"600\"}", "{\"acr_values\":[1],\"max_age\":600}",
                "{\"acr_values\":[\"mfa\"],\"max_age\":null}", "{\"acr_values\":[\"mfa\"],\"max_age\":600.5}",
                "{\"acr_values\":[\"mfa\"],\"max_age\":600,\"max_age\":0}", "{\"acr_values\":[],\"max_age\":600}" })
            { using var document=JsonDocument.Parse(body); Require(StrongSignInRequirement.Parse(document.RootElement,null) is null); }
            return Task.CompletedTask;
        });
        yield return ("An MFA refusal stops the first write before renewal or automatic replay", async () =>
        {
            foreach (var header in new string?[] { null, Challenge })
            {
                var tokens=new Tokens(); var root=tokens.AcceptedAuthorization; var count=0;
                using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json(Problem,HttpStatusCode.Unauthorized,header));}));
                using var api=Client(http,tokens);
                var error=await Fails<PostboxException>(()=>api.SendAsync("/k/fixture","/example/peer","Synthetic",null,default));
                Require(error.Code=="step_up_required" && error.StrongSignIn?.RequestedAcr=="mfa" && count==1 && tokens.Renewals==0);
                Require(ReferenceEquals(root,tokens.AcceptedAuthorization) && root!.IsActive);
            }
        });
        yield return ("An MFA refusal after ordinary renewal stops the exact second write", async () =>
        {
            var tokens=new Tokens(); var bodies=new List<string>(); var auth=new List<string?>();
            using var http=new HttpClient(new Handler(async (request,ct)=>
            {
                bodies.Add(await request.Content!.ReadAsStringAsync(ct)); auth.Add(request.Headers.Authorization?.Parameter);
                return bodies.Count==1 ? Json("{\"error\":\"unauthorized\"}",HttpStatusCode.Unauthorized) : Json(Problem,HttpStatusCode.Unauthorized,Challenge);
            }));
            using var api=Client(http,tokens);
            var error=await Fails<PostboxException>(()=>api.SendAsync("/k/fixture","/example/peer","Synthetic exact write",null,default));
            Require(error.Code=="step_up_required" && bodies.Count==2 && bodies[0]==bodies[1] && tokens.Renewals==1);
            Require(auth.SequenceEqual(new[]{"fixture-old","fixture-new"}));
        });
        yield return ("Ordinary expired credentials retain one renewal and the original request", async () =>
        {
            var tokens=new Tokens(); var count=0;
            using var http=new HttpClient(new Handler((_,_)=>Task.FromResult(++count==1
                ? Json("{\"error\":\"unauthorized\"}",HttpStatusCode.Unauthorized) : Json("{\"messages\":[]}"))));
            using var api=Client(http,tokens); var result=await api.GetInboxAsync("/k/fixture");
            Require(result.Messages!.Count==0 && count==2 && tokens.Renewals==1);
        });
        yield return ("Malformed strong challenges stop replay without inventing a verification method", async () =>
        {
            foreach (var pair in new[] {
                ("{}",Challenge.Replace("acr_values=\"mfa\"","error_description=\"acr_values=mfa\"")),
                ("not-json",Challenge.Replace("max_age=600","max_age=999")),
                ("{\"error\":\"step_up_required\",\"acr_values\":[\"mfa\"],\"max_age\":\"600\"}",(string?)null),
                ("{}",new string('x',2049)) })
            {
                var tokens=new Tokens();var count=0;
                using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json(pair.Item1,HttpStatusCode.Unauthorized,pair.Item2));}));
                using var api=Client(http,tokens);var error=await Fails<PostboxException>(()=>api.GetInboxAsync("/k/fixture"));
                Require(error.Code=="step_up_required" && error.StrongSignIn is null && count==1 && tokens.Renewals==0);
            }
        });
        yield return ("Unbounded or duplicate error bodies cannot trigger token renewal or expose raw diagnostics", async () =>
        {
            foreach (var body in new[]{new string('x',65537),"{\"error\":\"step_up_required\",\"error\":\"unauthorized\"}","private-fixture-value"})
            {
                var tokens=new Tokens();var count=0;
                using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json(body,HttpStatusCode.Unauthorized));}));
                using var api=Client(http,tokens);var error=await Fails<PostboxException>(()=>api.GetInboxAsync("/k/fixture"));
                Require(error.Code=="bad_response" && tokens.Renewals==0 && count==1 && !error.ToString().Contains("private-fixture-value"));
            }
        });
        yield return ("A late challenge body retains its original accepted account and every caller", async () =>
        {
            foreach (var changeAccount in new[]{false,true})
            {
                var tokens=new Tokens();var caller=tokens.AcceptedAuthorization!.Child();var content=new DeferredContent(Problem);
                using var http=new HttpClient(new Handler((_,_)=>Task.FromResult(new HttpResponseMessage(HttpStatusCode.Unauthorized){Content=content})));
                using var api=Client(http,tokens);
                using(new AuthorizationContext(caller))
                {
                    var pending=api.GetInboxAsync("/k/fixture");await content.Started.Task.WaitAsync(TimeSpan.FromSeconds(5));
                    if(changeAccount)tokens.Replace();else caller.Retire();
                    content.Release.SetResult();await Fails<AuthorizationRetiredException>(()=>pending);
                }
                Require(tokens.Renewals==0);
            }
        });
        yield return ("The account event stream stops on MFA instead of renewing or reconnecting", async () =>
        {
            var tokens=new Tokens();var count=0;var callbacks=0;
            using var http=new HttpClient(new Handler((_,_)=>{count++;return Task.FromResult(Json(Problem,HttpStatusCode.Unauthorized,Challenge));}));
            using var api=Client(http,tokens);var error=await Fails<PostboxException>(()=>api.WatchMailAsync(()=>{callbacks++;return Task.CompletedTask;},default));
            Require(error.Code=="step_up_required" && tokens.Renewals==0 && count==1 && callbacks==0);
        });
    }
    private static PostboxClient Client(HttpClient http,Tokens tokens)=>new(http,tokens,new Uri("http://127.0.0.1:1/"));
    private static void Require(bool condition){if(!condition)throw new Exception("Strong sign-in behavior assertion failed.");}
    private static async Task<T> Fails<T>(Func<Task> action) where T:Exception
    {try{await action().WaitAsync(TimeSpan.FromSeconds(5));}catch(T error){return error;}throw new Exception("Expected "+typeof(T).Name);}
    private static HttpResponseMessage Json(string body,HttpStatusCode status=HttpStatusCode.OK,string? header=null)
    {var result=new HttpResponseMessage(status){Content=new StringContent(body,Encoding.UTF8,"application/json")};if(header is not null)result.Headers.TryAddWithoutValidation("WWW-Authenticate",header);return result;}
    private sealed class Handler(Func<HttpRequestMessage,CancellationToken,Task<HttpResponseMessage>> send):HttpMessageHandler
    {protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request,CancellationToken cancellationToken)=>send(request,cancellationToken);}
    private sealed class Tokens:IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization{get;private set;}=new();
        public int Renewals;
        public Task<string> GetTokenAsync(CancellationToken cancellationToken)=>Task.FromResult(Renewals==0?"fixture-old":"fixture-new");
        public Task<string> RefreshTokenAsync(CancellationToken cancellationToken){Renewals++;return Task.FromResult("fixture-new");}
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
}
