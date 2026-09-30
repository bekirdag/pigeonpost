using System.Net;
using Pigeonpost.Core;

internal static class AccountTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A fresh account does not contact the token endpoint", async () =>
        {
            using var http = new HttpClient(new Handler((_, _) => throw new Exception("Unexpected network request")));
            Require(!await new AccountSession(http, new Vault()).RestoreAsync(default));
        });
        yield return ("Restore persists rotated refresh tokens and coalesces concurrent access", async () =>
        {
            var count = 0;
            var vault = new Vault { Token = "old-refresh" };
            using var http = new HttpClient(new Handler(async (request, ct) =>
            {
                count++;
                var body = await request.Content!.ReadAsStringAsync(ct);
                Require(body.Contains("client_id=pigeonpost-windows") && body.Contains("refresh_token=old-refresh"));
                return Json("{\"access_token\":\"access\",\"refresh_token\":\"rotated\",\"expires_in\":300}");
            }));
            var session = new AccountSession(http, vault);
            Require(await session.RestoreAsync(default));
            var tokens = await Task.WhenAll(Enumerable.Range(0, 8).Select(_ => session.GetTokenAsync(default)));
            Require(tokens.All(x => x == "access") && count == 1 && vault.Token == "rotated");
        });
        yield return ("An invalid refresh grant clears the saved session", async () =>
        {
            var vault = new Vault { Token = "revoked" };
            using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json("{\"error\":\"invalid_grant\"}", HttpStatusCode.BadRequest))));
            var session = new AccountSession(http, vault);
            await Fails<SignInException>(() => session.RestoreAsync(default));
            Require(vault.Token is null);
            await Fails<SignInException>(() => session.GetTokenAsync(default));
        });
        yield return ("A vault write failure never accepts a new access token", async () =>
        {
            var vault = new Vault { Token = "old", FailSave = true };
            using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json("{\"access_token\":\"secret\",\"refresh_token\":\"new\"}"))));
            var session = new AccountSession(http, vault);
            await Fails<IOException>(() => session.RestoreAsync(default));
            await Fails<IOException>(() => session.GetTokenAsync(default));
            Require(vault.Token == "old");
        });
        yield return ("Device authorization refuses foreign browser origins and realm paths", async () =>
        {
            foreach (var uri in new[] { "https://evil.example/device", "http://auth.pigeonpost.dev/realms/pigeonpost-prod/device", "https://auth.pigeonpost.dev/realms/other/device" })
            {
                using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(Device(uri)))));
                await Fails<SignInException>(() => new AccountSession(http, new Vault()).BeginSignInAsync(default));
            }
        });
        yield return ("Successful device authorization persists a session after browser consent", async () =>
        {
            var vault = new Vault();
            var calls = 0;
            using var http = new HttpClient(new Handler((request, _) =>
            {
                calls++;
                return Task.FromResult(Json(request.RequestUri!.AbsolutePath.EndsWith("auth/device")
                    ? Device("https://auth.pigeonpost.dev/realms/pigeonpost-prod/device")
                    : "{\"access_token\":\"access\",\"refresh_token\":\"refresh\",\"expires_in\":300}"));
            }));
            var session = new AccountSession(http, vault);
            var device = await session.BeginSignInAsync(default);
            Require(device.UserCode == "ABCD");
            await session.CompleteSignInAsync(device, default);
            Require(await session.GetTokenAsync(default) == "access" && vault.Token == "refresh" && calls == 2);
        });
        yield return ("Sign-out prevents a delayed device response from restoring the account", async () =>
        {
            var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            var vault = new Vault();
            using var http = new HttpClient(new Handler(async (_, _) =>
            {
                started.SetResult();
                await release.Task;
                return Json("{\"access_token\":\"access\",\"refresh_token\":\"refresh\"}");
            }));
            var session = new AccountSession(http, vault);
            var pending = session.CompleteSignInAsync(new DeviceSignIn("code", "ABCD", new Uri("https://auth.pigeonpost.dev/realms/pigeonpost-prod/device"), 60, 1), default);
            await started.Task;
            await session.SignOutAsync(default);
            release.SetResult();
            await Fails<OperationCanceledException>(() => pending);
            Require(vault.Token is null);
        });
        yield return ("Cancelled device sign-in does not persist credentials", async () =>
        {
            var vault = new Vault();
            using var http = new HttpClient(new Handler((_, _) => throw new Exception("Unexpected token request")));
            var session = new AccountSession(http, vault);
            using var cancel = new CancellationTokenSource();
            cancel.Cancel();
            await Fails<OperationCanceledException>(() => session.CompleteSignInAsync(new DeviceSignIn("d", "c", new Uri("https://auth.pigeonpost.dev/realms/pigeonpost-prod/device"), 60, 1), cancel.Token));
            Require(vault.Token is null);
        });
        yield return ("Authentication errors do not expose service response details", async () =>
        {
            using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json("{\"error\":\"unknown-secret\",\"error_description\":\"sensitive\"}", HttpStatusCode.BadRequest))));
            var ex = await Fails<SignInException>(() => new AccountSession(http, new Vault()).BeginSignInAsync(default));
            Require(!ex.Message.Contains("secret") && !ex.Message.Contains("sensitive"));
        });
    }

    private static string Device(string uri) => System.Text.Json.JsonSerializer.Serialize(new { device_code = "device", user_code = "ABCD", verification_uri = uri, expires_in = 60, interval = 1 });
    private static HttpResponseMessage Json(string body, HttpStatusCode status = HttpStatusCode.OK) => new(status) { Content = new StringContent(body) };
    private static void Require(bool condition) { if (!condition) throw new Exception("Account behavior assertion failed."); }
    private static async Task<T> Fails<T>(Func<Task> action) where T : Exception
    {
        try { await action(); }
        catch (T ex) { return ex; }
        throw new Exception("Expected " + typeof(T).Name);
    }
    private sealed class Vault : IRefreshTokenStore
    {
        public string? Token { get; set; }
        public bool FailSave { get; init; }
        public string? Load() => Token;
        public void Save(string token) { if (FailSave) throw new IOException("Vault unavailable"); Token = token; }
        public void Clear() => Token = null;
    }
}
