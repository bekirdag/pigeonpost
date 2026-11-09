using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;
using Pigeonpost.Core.Preview;

// SDS §5.4 on Windows: the org badge beside the mailbox switcher and organization address suggestions.
internal static class OrganizationSurfaceTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("The organization badge names the org and the strongest role, and says when it is read-only", () =>
        {
            var A = PreviewPostbox.AddressFor(PreviewPostbox.Seed(61)); var B = PreviewPostbox.AddressFor(PreviewPostbox.Seed(62));
            RoomOrganization Org(string status, params string[] roles) => new("org_a", "Acme Inc.", status, "mem_1", "acme", "everyone", roles, new(A, "/acme/john"));
            Require(OrganizationBadge.Label(Org("active")) == "Acme Inc. · Member", "member");
            Require(OrganizationBadge.Label(Org("past_due", "people_admin", "admin")) == "Acme Inc. · Admin", "admin wins");
            Require(OrganizationBadge.Label(Org("active", "owner", "admin")) == "Acme Inc. · Owner", "owner wins");
            Require(OrganizationBadge.Label(Org("read_only", "billing_admin")) == "Acme Inc. · Billing admin · read-only", "read-only");
            var catalog = new RoomOrganizations([Org("active"), new("org_b", "Old Co", "suspended", "mem_2", "old", "everyone", [], new(B, "/old/john"))], []);
            Require(OrganizationBadge.For(catalog, A)?.Id == "org_a", "own org inbox");
            Require(OrganizationBadge.For(catalog, B) is null && OrganizationBadge.For(catalog, "/k/zz1111v2h90vnwefj7g7ezvbh9") is null && OrganizationBadge.For(catalog, null) is null, "suspended or personal inboxes get no badge");
            return Task.CompletedTask;
        });
        yield return ("Directory queries drop the leading slash and the org namespace and need two characters", () =>
        {
            Require(OrganizationBadge.Query("/acme/ay", "acme") == "ay", "namespace dropped");
            Require(OrganizationBadge.Query("/a", "acme") is null && OrganizationBadge.Query("/", "acme") is null, "too short");
            Require(OrganizationBadge.Query("/other/ay", "acme") == "other/ay", "other namespaces stay");
            Require(OrganizationBadge.Query("50%_off", null) == "50off", "LIKE wildcards are removed");
            Require(new DirectoryEntry("/acme/deploy", "/k/dddd4444", "agent", "Deploy bot").Label == "Deploy bot · agent · /acme/deploy", "agent label");
            Require(new DirectoryEntry("/acme/ayse", "/k/eeee5555", "member", null).Label == "/acme/ayse", "member label");
            Require(!new DirectoryEntry("/acme/x", "/k/eeee5555", "admin", null).Valid && !new DirectoryEntry("acme", "/k/eeee5555", "member", null).Valid, "unknown kinds and bad handles are refused");
            return Task.CompletedTask;
        });
        yield return ("Composing from an org inbox suggests the organization's members and agents, never the sender itself", async () =>
        {
            var server = new PreviewPostbox(); server.Features.Add(ServerFeatures.Organizations);
            var me = server.AddMailbox(PreviewPostbox.Seed(71), "/acme/me");
            var ayse = server.AddMailbox(PreviewPostbox.Seed(72), "/acme/ayse");
            server.AddOrganization("org_acme", "Acme Inc.", [me, ayse], [("/k/cz6900v2h90vnwefj7g7ezvbh4", "/acme/agent")]);
            var outsider = server.AddMailbox(PreviewPostbox.Seed(73), "/person/main");
            using var client = new PostboxClient(new HttpClient(server), new Tokens(), new Uri("http://127.0.0.1:9"));
            var directory = new OrganizationDirectoryClient(client);
            var found = await directory.SearchAsync(me, "org_acme", "acme", default);
            Require(found.Select(e => e.Handle).SequenceEqual(["/acme/agent", "/acme/ayse"]), string.Join(",", found.Select(e => e.Handle)));
            Require(found[0].Kind == "agent" && server.DirectoryQueries.Single() == "acme", "kinds and the sent query");
            try { await directory.SearchAsync(outsider, "org_acme", "acme", default); throw new Exception("A non-member read the directory."); }
            catch (PostboxException e) when (e.StatusCode == 403) { }
        });
    }
    private static void Require(bool value, string why) { if (!value) throw new Exception("Organization surfaces: " + why); }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult("fixture-org-surfaces");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => throw new Exception("Unexpected renewal");
    }
}
