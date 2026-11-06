using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

// docs/contacts-and-phone.md §2, §3.1, §5: the same tables as site-inbox/test/contacts.test.mjs. Every number and
// name is fictional (+1 555 01xx, +44 7700 900xxx, +61 491 570 xxx).
internal static class ContactImportTests
{
    private const string Box = "/k/abc123";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Contact discovery hashes match the contract's test vectors", () =>
        {
            Require(ContactImport.Hash("+15555550100") == "7c0dc19d3eae376a630cee358d0f8912", "vector 1");
            Require(ContactImport.Hash("+447700900123") == "5c4fd5bb0f19345b15863f865cb76cee", "vector 2");
            Require(ContactImport.Hash("+61491570156") == "511ebc743c8a1d1ea8be5541f2fee95d", "vector 3");
            Fails<ArgumentException>(() => ContactImport.Hash("5555550100"));
            return Task.CompletedTask;
        });
        yield return ("Phone numbers normalize to E.164 exactly like the web inbox", () =>
        {
            (string? Raw, string Cc, string? Want)[] table =
            [
                ("0532 123 45 67", "90", "+905321234567"), ("(555) 555-0100", "1", "+15555550100"),
                ("0044 7700 900123", "44", "+447700900123"), ("0044 7700 900123", "90", "+447700900123"),
                ("+1 555 555 0100 x12", "90", "+15555550100"), ("+1 (555) 555-0100 ext. 7", "1", "+15555550100"),
                ("tel:+1-555-555-0100;ext=5", "", "+15555550100"), ("1-555-555-0100", "1", "+15555550100"),
                ("555.555.0100", "+1", "+15555550100"), ("011 44 7700 900123", "1", "+447700900123"),
                ("07700 900123", "44", "+447700900123"), ("0491 570 156", "61", "+61491570156"),
                ("+44 (0)7700 900123", "1", "+447700900123"), ("555 0100", "", null), ("112", "44", null),
                ("0800-FLOWERS", "1", null), ("garbage", "1", null), ("", "1", null), (null, "1", null),
                ("+0123456789", "1", null), ("+1234567890123456", "1", null),
            ];
            foreach (var (raw, cc, want) in table)
                Require(ContactImport.NormalizeE164(raw, cc) == want, $"{raw} with {cc}: got {ContactImport.NormalizeE164(raw, cc)}");
            return Task.CompletedTask;
        });
        yield return ("vCard import reads folded lines, several numbers, 2.1 quoted-printable, grouped TELs and tel: URIs", () =>
        {
            var vcf = string.Join("\r\n",
                "BEGIN:VCARD", "VERSION:3.0", "FN:Avery Ex", " ample", "N:Example;Avery;;;",
                "TEL;TYPE=cell:+1 555 555 0100", "TEL;TYPE=work,voice:(555) 555-0101", "END:VCARD",
                "BEGIN:VCARD", "VERSION:2.1", "N;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:M=C3=BCller;J=C3=B6rg;;;",
                "FN;ENCODING=QUOTED-PRINTABLE;CHARSET=UTF-8:J=C3=B6rg =", "M=C3=BCller",
                "TEL;CELL:+44 7700 900123", "END:VCARD",
                "BEGIN:VCARD", "VERSION:4.0", "N:Tester;Casey;;;", "item1.TEL;type=pref:+61 491 570 156",
                "item1.X-ABLabel:mobile", "TEL;VALUE=uri;TYPE=home:tel:+1-555-555-0102", "END:VCARD",
                "BEGIN:VCARD", "VERSION:3.0", "FN:Nobody\\, No Phone", "ORG:Example Org", "END:VCARD");
            Same(ContactImport.ParseVcard(vcf),
                ("Avery Example", ["+1 555 555 0100", "(555) 555-0101"]), ("Jörg Müller", ["+44 7700 900123"]),
                ("Casey Tester", ["+61 491 570 156", "tel:+1-555-555-0102"]), ("Nobody, No Phone", []));
            Require(ContactImport.ParseVcard("BEGIN:VCARD\nFN:Dana\n\tSample\nTEL:+15555550103\nEND:VCARD\n")[0].Name == "DanaSample", "tab folding");
            return Task.CompletedTask;
        });
        yield return ("CSV import reads Google, newer Google, Outlook and generic exports", () =>
        {
            var google = string.Join("\r\n",
                "Name,Given Name,Family Name,Phone 1 - Type,Phone 1 - Value,Phone 2 - Type,Phone 2 - Value",
                "\"Example, Avery\",Avery,Example,Mobile,+1 555 555 0100 ::: +1 555 555 0101,Work,(555) 555-0102",
                "Blake Sample,Blake,Sample,Home,\"+44 7700 900123\",,");
            Same(ContactImport.ParseCsv(google), ("Example, Avery", ["+1 555 555 0100", "+1 555 555 0101", "(555) 555-0102"]),
                ("Blake Sample", ["+44 7700 900123"]));
            Same(ContactImport.ParseCsv("First Name,Middle Name,Last Name,Phone 1 - Label,Phone 1 - Value\nCasey,J,Tester,Mobile,+61 491 570 156\n"),
                ("Casey J Tester", ["+61 491 570 156"]));
            var outlook = string.Join("\n",
                "First Name,Last Name,Business Phone,Home Phone,Mobile Phone,Primary Phone,E-mail Address",
                "Dana,Sample,(555) 555-0104,,\"555-555-0105\",,dana@example.invalid",
                "\"Multi \"\"Line\"\"\",Person,,\"\",+44 7700 900124,,");
            Same(ContactImport.ParseCsv(outlook), ("Dana Sample", ["(555) 555-0104", "555-555-0105"]), ("Multi \"Line\" Person", ["+44 7700 900124"]));
            Same(ContactImport.ParseCsv("﻿contact,Cell,Tel (work),Phone Type\n\"Eli\nSample\",+1 555 555 0106,+1 555 555 0107,mobile\n"),
                ("", ["+1 555 555 0106", "+1 555 555 0107"]));
            return Task.CompletedTask;
        });
        yield return ("An import lists each readable number once, counts unreadable ones and keeps names local", () =>
        {
            var result = ContactImport.Read("people.csv", "Name,Mobile Phone,Home Phone\nAvery,555-555-0100,garbage\nBlake,+1 555 555 0100,0800-FLOWERS\n", "1");
            Require(result.People == 2 && result.Unreadable == 2, $"counts {result.People}/{result.Unreadable}");
            Require(result.Numbers.Count == 1 && result.Numbers[0] is { Name: "Avery", Phone: "+15555550100", Hash: "7c0dc19d3eae376a630cee358d0f8912" }, "dedupe");
            Require(ContactImport.Read("x.txt", "BEGIN:VCARD\nFN:Casey\nTEL:+61491570156\nEND:VCARD\n", "90").Numbers.Single().Phone == "+61491570156", "vCard sniffing");
            Require(ContactImport.GuessCallingCode("GB") == "44" && ContactImport.GuessCallingCode("CA") == "1" && ContactImport.GuessCallingCode("ZZ") == "1", "region guess");
            return Task.CompletedTask;
        });
        yield return ("The invitation text carries only a reduced sender name", () =>
        {
            Require(ContactImport.InviteName("Bekir 🚀 <Dağ>") == "Bekir Dağ", ContactImport.InviteName("Bekir 🚀 <Dağ>"));
            Require(ContactImport.InviteName("🚀") == "A friend" && ContactImport.InviteName(null) == "A friend", "fallback");
            Require(ContactImport.InviteName(new string('x', 30)).Length == 24, "24 characters");
            Require(ContactImport.InviteText("Bekir") == "Pigeonpost: Bekir invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=… Reply STOP to opt out.", "text");
            return Task.CompletedTask;
        });
        yield return ("Discovery sends batches of 200 hashes, stops at the daily allowance and never lists unchecked numbers as unmatched", async () =>
        {
            var numbers = Enumerable.Range(0, 450).Select(i => { var p = "+1555555" + (1000 + i); return new ImportedNumber("P" + i, p, ContactImport.Hash(p)); }).ToArray();
            var sizes = new List<int>();
            var outcome = await ContactDiscovery.RunAsync(numbers, (hashes, _) =>
            {
                sizes.Add(hashes.Count);
                var matches = hashes.Take(1).Select(h => new DiscoveryMatch(h, "/k/match" + sizes.Count, null, "Match")).ToArray();
                return Task.FromResult(new DiscoveryResult(matches, hashes.Count, hashes.Count, sizes.Count == 1 ? 50 : 0));
            }, default);
            Require(sizes.SequenceEqual([200, 50]), string.Join(",", sizes));
            Require(outcome.Found.Count == 2 && outcome.NoMatch.Count == 248 && outcome.Unchecked == 200 && outcome.Stopped == "daily", "partial results kept");
            Require(outcome.Found[0].Name == "P0" && outcome.Found[0].Address == "/k/match1", "names are joined locally");
            var failed = await ContactDiscovery.RunAsync(numbers, (hashes, _) => hashes.Count == 200 && failedOnce++ == 0
                ? Task.FromResult(new DiscoveryResult([], 200, 200, 900))
                : throw new PostboxException(429, "rate_limited", retryAfter: 60), default);
            Require(failed.NoMatch.Count == 200 && failed.Unchecked == 250 && failed.Stopped!.Contains("60 seconds"), "a refusal mid-way keeps what was checked");
        });
        yield return ("Phone, discovery and invite calls carry the acting inbox and check the server's answers", async () =>
        {
            using var tokens = new Tokens(); using var handler = new Handler(); using var http = new HttpClient(handler);
            using var client = new PostboxClient(http, tokens, new("http://127.0.0.1:9"));
            handler.Body = new { numbers = new[] { new { id = "ph_1", identity = Box, country_code = "44", masked = "+44 ••• ••23", discoverable = false, verified_at = 5 } }, max_numbers = 1 };
            var phones = await client.GetPhonesAsync(Box, default);
            Require(phones.Single().Masked == "+44 ••• ••23" && handler.Path == "/v1/phone" && handler.Identity == Box, "list");
            Require(!phones[0].ToString().Contains("44"), "a phone never prints its digits");
            handler.Body = new { challenge_id = "ch_1", expires_at = 10, resend_after = 60, code_length = 6 };
            Require((await client.SendPhoneCodeAsync(Box, "+447700900123", default)).Id == "ch_1" && handler.Method == HttpMethod.Post, "send");
            await FailsAsync<ArgumentException>(() => client.SendPhoneCodeAsync(Box, "07700 900123", default));
            handler.Body = new { number = new { id = "ph_1", identity = Box, country_code = "44", masked = "+44 ••• ••23", discoverable = true, verified_at = 5 } };
            Require((await client.VerifyPhoneAsync(Box, "ch_1", "123456", true, default)).Discoverable && handler.Path == "/v1/phone/verify/ch_1", "verify");
            using (var body = JsonDocument.Parse(handler.RequestBody!)) Require(body.RootElement.GetProperty("discoverable").GetBoolean(), "discoverable is sent as chosen");
            await FailsAsync<ArgumentException>(() => client.VerifyPhoneAsync(Box, "ch_1", "12345", false, default));
            handler.Body = new { number = new { id = "ph_1", identity = "/k/other", country_code = "44", masked = "+44", discoverable = true, verified_at = 5 } };
            await FailsAsync<PostboxException>(() => client.VerifyPhoneAsync(Box, "ch_1", "123456", true, default));

            var hash = ContactImport.Hash("+15555550100");
            handler.Body = new { matches = new[] { new { hash, address = "/k/peer1", handle = "/avery/main", display_name = "Avery" } }, @checked = 1, @new = 1, remaining_today = 999 };
            var found = await client.DiscoverContactsAsync(Box, [hash], default);
            Require(found.Matches.Single().Handle == "/avery/main" && found.Remaining == 999 && handler.Path == "/v1/contacts/discover", "discover");
            Require(!handler.RequestBody!.Contains("+1555") && handler.RequestBody.Contains(hash), "only hashes leave the device");
            handler.Body = new { matches = new[] { new { hash = ContactImport.Hash("+447700900123"), address = "/k/peer1", handle = (string?)null, display_name = (string?)null } }, @checked = 1, @new = 1, remaining_today = 999 };
            await FailsAsync<PostboxException>(() => client.DiscoverContactsAsync(Box, [hash], default));
            await FailsAsync<ArgumentException>(() => client.DiscoverContactsAsync(Box, Enumerable.Repeat(hash, 2).ToArray(), default));

            handler.Body = new { status = "sent", invite_id = "inv_1" };
            Require((await client.InviteBySmsAsync(Box, "+15555550100", default)).Status == "sent" && handler.Path == "/v1/contacts/invites", "invite");
            using (var body = JsonDocument.Parse(handler.RequestBody!)) Require(body.RootElement.GetProperty("confirm").GetBoolean(), "confirm is explicit");
            handler.Body = new { status = "not_sent", reason = "unavailable" };
            Require((await client.InviteBySmsAsync(Box, "+15555550100", default)).Reason == "unavailable", "not sent");
            handler.Body = new { status = "not_sent", reason = "opted_out" };
            await FailsAsync<PostboxException>(() => client.InviteBySmsAsync(Box, "+15555550100", default));
            Require(PostboxClient.PeopleRefusal(new PostboxException(403, "phone_required")).Contains("Verify your own phone number"), "refusal words");
        });
    }

    public static IEnumerable<(string Name, Func<Task> Run)> StandIn()
    {
        yield return ("Verifying a number, finding a discoverable contact and inviting one person work against the stand-in", async () =>
        {
            var server = new Pigeonpost.Core.Preview.PreviewPostbox();
            server.Features.UnionWith(["phone_numbers", "contact_discovery", "sms_invites"]);
            var me = server.AddMailbox(Pigeonpost.Core.Preview.PreviewPostbox.Seed(81), "/example/me");
            var friend = server.AddMailbox(Pigeonpost.Core.Preview.PreviewPostbox.Seed(82), "/example/friend");
            server.AddDiscoverableNumber(friend, "+447700900123");
            using var tokens = new Tokens();
            using var client = new PostboxClient(new HttpClient(server), tokens, new Uri("http://127.0.0.1:9"));
            var import = ContactImport.Read("contacts.vcf", "BEGIN:VCARD\nFN:Friend\nTEL:07700 900123\nEND:VCARD\nBEGIN:VCARD\nFN:Stranger\nTEL:+1 555 555 0100\nEND:VCARD\n", "44");
            var outcome = await ContactDiscovery.RunAsync(import.Numbers, (h, ct) => client.DiscoverContactsAsync(me, h, ct), default);
            Require(outcome.Found.Single() is { Name: "Friend", Handle: "/example/friend" } && outcome.NoMatch.Single().Name == "Stranger", "matches");
            try { await client.InviteBySmsAsync(me, "+15555550100", default); throw new Exception("invite without a number"); }
            catch (PostboxException e) when (e.Code == "phone_required") { }
            var challenge = await client.SendPhoneCodeAsync(me, "+905321234567", default);
            try { await client.VerifyPhoneAsync(me, challenge.Id, "000000", false, default); throw new Exception("wrong code accepted"); }
            catch (PostboxException e) when (e.Code == "invalid_code") { Require(PostboxClient.PeopleRefusal(e).Contains("4 tries left"), PostboxClient.PeopleRefusal(e)); }
            var mine = await client.VerifyPhoneAsync(me, challenge.Id, "123456", false, default);
            Require(!mine.Discoverable && (await client.GetPhonesAsync(me, default)).Single().Id == mine.Id, "verified off by default");
            Require((await client.SetPhoneDiscoverableAsync(me, mine.Id, true, default)).Discoverable, "discoverable on");
            Require((await client.InviteBySmsAsync(me, "+15555550100", default)).Status == "sent", "sent");
            Require((await client.InviteBySmsAsync(me, "+15555550100", default)).Reason == "already_invited", "deduplicated");
            await client.UnlinkPhoneAsync(me, mine.Id, default);
            Require((await client.GetPhonesAsync(me, default)).Count == 0 && server.Invites.Count == 1, "unlinked");
        });
    }

    private static int failedOnce;
    private static void Same(IReadOnlyList<ImportedContact> actual, params (string Name, string[] Phones)[] expected)
    {
        Require(actual.Count == expected.Length, $"expected {expected.Length} contacts, got {actual.Count}");
        for (var i = 0; i < expected.Length; i++)
            Require(actual[i].Name == expected[i].Name && actual[i].Phones.SequenceEqual(expected[i].Phones),
                $"contact {i}: {actual[i].Name} [{string.Join("|", actual[i].Phones)}]");
    }
    private static void Require(bool condition, string why) { if (!condition) throw new Exception("Contact import: " + why); }
    private static void Fails<T>(Action action) where T : Exception { try { action(); } catch (T) { return; } throw new Exception("Expected refusal."); }
    private static async Task FailsAsync<T>(Func<Task> action) where T : Exception { try { await action(); } catch (T) { return; } throw new Exception("Expected refusal."); }
    private sealed class Tokens : IAccessTokenProvider, IDisposable
    {
        internal readonly AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult("synthetic-contacts-test-token");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
        public void Dispose() => Root.Retire();
    }
    private sealed class Handler : HttpMessageHandler
    {
        internal object Body = new { };
        internal string? Path, Identity, RequestBody;
        internal HttpMethod? Method;
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            Path = request.RequestUri!.AbsolutePath; Method = request.Method;
            Identity = request.Headers.GetValues("x-pigeonpost-identity").Single();
            RequestBody = request.Content is null ? null : await request.Content.ReadAsStringAsync(ct);
            return new(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(Body), Encoding.UTF8, "application/json") };
        }
    }
}
