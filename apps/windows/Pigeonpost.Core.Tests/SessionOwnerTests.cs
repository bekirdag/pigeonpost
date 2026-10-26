using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class SessionOwnerTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Trusted token renewal preserves a stable owner partition and its accepted grant", async () =>
        {
            var vault = new Vault(); using var http = new HttpClient(new Replies(Grant(Token("alice"), "refresh_one"), Grant(Token("alice", marker: "renewed"), "refresh_two")));
            var session = new AccountSession(http, vault); await session.RestoreAsync(default);
            var owner = session.RecoveryOwnerKey; var accepted = session.AcceptedAuthorization;
            Require(owner is { Length: 64 } && !owner.Contains("alice", StringComparison.Ordinal));
            await session.RefreshTokenAsync(default);
            Require(session.RecoveryOwnerKey == owner && ReferenceEquals(session.AcceptedAuthorization, accepted) && vault.Value == "refresh_two");
        });
        yield return ("Renewal cannot change a known principal or persist its replacement refresh token", async () =>
        {
            var originalToken = Token("alice"); var vault = new Vault();
            using var http = new HttpClient(new Replies(Grant(originalToken, "refresh_one"), Grant(Token("bob"), "refresh_wrong")));
            var session = new AccountSession(http, vault); await session.RestoreAsync(default);
            var owner = session.RecoveryOwnerKey; var accepted = session.AcceptedAuthorization;
            await Refused(() => session.RefreshTokenAsync(default));
            Require(session.RecoveryOwnerKey == owner && ReferenceEquals(session.AcceptedAuthorization, accepted)
                && accepted?.IsActive == true && vault.Value == "refresh_one" && await session.GetTokenAsync(default) == originalToken);
        });
        yield return ("Renewal cannot downgrade a known owner to an opaque or malformed token", async () =>
        {
            var vault = new Vault(); using var http = new HttpClient(new Replies(Grant(Token("alice"), "refresh_one"), Grant("opaque-fixture", "refresh_wrong")));
            var session = new AccountSession(http, vault); await session.RestoreAsync(default); var owner = session.RecoveryOwnerKey;
            await Refused(() => session.RefreshTokenAsync(default)); Require(session.RecoveryOwnerKey == owner && vault.Value == "refresh_one");
        });
        yield return ("Recovery owner hints reject foreign issuer/client, unsigned, duplicate and unbounded subjects", async () =>
        {
            foreach (var raw in new[] { Token("alice", issuer: "https://foreign.invalid"), Token("alice", client: "other-client"), Token("alice", algorithm: "none"),
                Token(""), Token(new string('ğ', 129)), Token("alice\nprivate"), Token("alice", duplicateSubject: true), "not-a-jwt", new string('x', 32769) })
            {
                using var http = new HttpClient(new Replies(Grant(raw, "fixture_refresh")));
                var session = new AccountSession(http, new Vault()); await session.RestoreAsync(default);
                Require(session.RecoveryOwnerKey is null); // unknown owner is not a recovery grant
            }
        });
        yield return ("Explicit sign-in changes owner only after vault commit and retires the old grant", async () =>
        {
            var vault = new Vault(); using var http = new HttpClient(new Replies(Grant(Token("alice"), "refresh_one"), Device(), Grant(Token("bob"), "refresh_two")));
            var session = new AccountSession(http, vault); await session.RestoreAsync(default);
            var owner = session.RecoveryOwnerKey; var accepted = session.AcceptedAuthorization!;
            var device = await session.BeginSignInAsync(default); await session.CompleteSignInAsync(device, default);
            Require(session.RecoveryOwnerKey is not null && session.RecoveryOwnerKey != owner && !accepted.IsActive);
        });
        yield return ("A failed vault commit preserves the previous recovery owner and accepted grant", async () =>
        {
            var vault = new Vault(); using var http = new HttpClient(new Replies(Grant(Token("alice"), "refresh_one"), Device(), Grant(Token("bob"), "refresh_two")));
            var session = new AccountSession(http, vault); await session.RestoreAsync(default);
            var owner = session.RecoveryOwnerKey; var accepted = session.AcceptedAuthorization;
            var device = await session.BeginSignInAsync(default); vault.FailSave = true;
            try { await session.CompleteSignInAsync(device, default); throw new InvalidOperationException(); } catch (IOException) { }
            Require(session.RecoveryOwnerKey == owner && ReferenceEquals(session.AcceptedAuthorization, accepted) && accepted?.IsActive == true && vault.Value == "refresh_one");
        });
        yield return ("Learning an owner from a legacy unknown session starts a new local authorization", async () =>
        {
            using var http = new HttpClient(new Replies(Grant("opaque-fixture", "refresh_one"), Grant(Token("alice"), "refresh_two")));
            var session = new AccountSession(http, new Vault()); await session.RestoreAsync(default); var accepted = session.AcceptedAuthorization!;
            Require(session.RecoveryOwnerKey is null); await session.RefreshTokenAsync(default);
            Require(session.RecoveryOwnerKey is not null && !accepted.IsActive && !ReferenceEquals(session.AcceptedAuthorization, accepted));
        });
        yield return ("Successful sign-out clears the owner hint while a failed vault clear preserves it", async () =>
        {
            var vault = new Vault(); using var http = new HttpClient(new Replies(Grant(Token("alice"), "refresh_one")));
            var session = new AccountSession(http, vault); await session.RestoreAsync(default); var owner = session.RecoveryOwnerKey;
            vault.FailClear = true;
            try { await session.SignOutAsync(default); throw new InvalidOperationException(); } catch (IOException) { }
            Require(session.RecoveryOwnerKey == owner);
            vault.FailClear = false; await session.SignOutAsync(default);
            Require(session.RecoveryOwnerKey is null && session.AcceptedAuthorization is null);
        });
    }
    private const string Issuer = "https://auth.pigeonpost.dev/realms/pigeonpost-prod";
    private static string Token(string subject, string issuer = Issuer, string client = AccountSession.ClientId, string algorithm = "RS256", string marker = "initial", bool duplicateSubject = false)
    {
        static string Enc(string value) => Convert.ToBase64String(Encoding.UTF8.GetBytes(value)).TrimEnd('=').Replace('+', '-').Replace('/', '_');
        var header = JsonSerializer.Serialize(new { alg = algorithm, typ = "JWT" });
        var claims = JsonSerializer.Serialize(new { iss = issuer, sub = subject, azp = client, jti = marker });
        if (duplicateSubject) claims = claims[..^1] + ",\"sub\":\"different-fixture\"}";
        return Enc(header) + "." + Enc(claims) + ".c3ludGhldGlj";
    }
    private static object Grant(string token, string refresh) => new { access_token = token, refresh_token = refresh, expires_in = 300 };
    private static object Device() => new { device_code = "fixture_device", user_code = "fixture_code", verification_uri = Issuer + "/device", expires_in = 60, interval = 1 };
    private static void Require(bool value) { if (!value) throw new Exception("Session owner assertion failed."); }
    private static async Task Refused(Func<Task> action)
    {
        try { await action(); } catch (SignInException ex) { Require(!ex.Message.Contains("refresh_wrong", StringComparison.Ordinal)); return; }
        throw new Exception("Different session owner accepted.");
    }
    private sealed class Vault : IRefreshTokenStore
    {
        internal string? Value = "fixture_initial_refresh";
        internal bool FailSave, FailClear;
        public string? Load() => Value;
        public void Save(string value) { if (FailSave) throw new IOException("Synthetic vault failure."); Value = value; }
        public void Clear() { if (FailClear) throw new IOException("Synthetic vault failure."); Value = null; }
    }
    private sealed class Replies(params object[] values) : HttpMessageHandler
    {
        private readonly Queue<object> replies = new(values);
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct) => Task.FromResult(
            new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(replies.Dequeue()), Encoding.UTF8, "application/json") });
    }
}
