using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core.Nests;

// Nests (hosted agents) on Windows (docs/nests/phase-b.md): the records and rules, no UI.
// Everything here is a display hint over the postbox's own answers; each action asks again.
// Desktop has no in-app payment: buying, upgrading and managing a package open the website.
// Two safety rules: a decision echoes the card's own args_hash (never one computed here), and a
// Nest is deleted only when its name is typed back exactly (the server checks it again).

public sealed record NestPackage(string Code, string Name, int Slots, long StorageBytes, int ActiveHours, long InputTokens, long OutputTokens, bool ManagedAi);
public sealed record NestSetup(string Id, string Label, bool Failed, string? LastError);
public sealed record NestRow(string Id, string? Address, string Label, string State, string Status, string? StatusReason, bool IsOwner);
public sealed record NestSnapshot(int Slots, int Used, string? Package, int Duplicates, IReadOnlyList<NestRow> Nests, IReadOnlyList<NestSetup> Setups);
public sealed record NestFile(string Id, string Name, long Bytes, string Status, string? Reason, int? UnitsTotal, int? UnitsText, IReadOnlyList<string> Warnings);
public sealed record NestFiles(IReadOnlyList<NestFile> Files, long UsedBytes, long QuotaBytes);
public sealed record NestApproval(string Id, string Tool, string ArgsHash, string Status, string? To, string? Body);
public sealed record OrgNests(IReadOnlyList<NestRow> Nests, int Slots, int Used);

public sealed class NestWireException() : Exception("Pigeonpost returned an unexpected answer about Nests.");

public static partial class NestRules
{
    public const string Feature = "nests";
    public const string WebProducts = "https://pigeonpost.dev/account#nests";
    public const string WebAccount = "https://pigeonpost.dev/account";
    public const string WebOrgProducts = "https://enterprise.pigeonpost.dev/";
    public const int MaxLabel = 64;
    public const int MaxUpload = 25 * 1024 * 1024;

    [GeneratedRegex("^nst_[0-9a-f]{1,60}$")] private static partial Regex NestId();
    [GeneratedRegex("^nfl_[0-9a-f]{24}$")] private static partial Regex FileId();
    [GeneratedRegex("^nap_[0-9a-f]{24}$")] private static partial Regex ApprovalId();
    [GeneratedRegex("^nsu_[0-9a-f]{1,60}$")] private static partial Regex SetupId();
    [GeneratedRegex("^[0-9a-f]{64}$")] private static partial Regex Hash();

    public static bool ValidNest(string? id) => id is not null && NestId().IsMatch(id);
    public static bool ValidFile(string? id) => id is not null && FileId().IsMatch(id);
    public static bool ValidSetup(string? id) => id is not null && SetupId().IsMatch(id);

    public static bool IsOn(ServerFeatures features) => features.Has(Feature);

    /// <summary>Length as the server counts it: Unicode scalar values, not UTF-16 units.</summary>
    public static int ScalarLength(string value) => value.EnumerateRunes().Count();

    /// <summary>A Nest name the server will take: 1 to 64 printable characters after trimming.</summary>
    public static bool ValidLabel(string? label)
    {
        if (string.IsNullOrWhiteSpace(label)) return false;
        var trimmed = label.Trim();
        return ScalarLength(trimmed) <= MaxLabel && !trimmed.EnumerateRunes().Any(Rune.IsControl);
    }

    // The buttons follow the server's rules; a paused Nest with a funding reason comes back only
    // when it is paid for, so offering Resume would just fail.
    public static bool CanPause(NestRow nest) => nest.IsOwner && nest.Status is "ready" or "working";
    public static bool CanResume(NestRow nest) => nest.IsOwner && nest.Status == "paused" && nest.StatusReason is null;

    /// <summary>The Nest a conversation is with, given how the inbox names that address.</summary>
    public static NestRow? ForPeer(NestSnapshot? snapshot, string? peer, Func<string, string> resolve)
    {
        if (snapshot is null || string.IsNullOrEmpty(peer)) return null;
        var canonical = PostAddress.Canonical(peer);
        return snapshot.Nests.FirstOrDefault(n => n.Address is { Length: > 0 } a
            && (PostAddress.Canonical(a) == canonical || resolve(a) == canonical));
    }

    public static IReadOnlyList<NestApproval> Pending(IEnumerable<NestApproval>? cards) => (cards ?? []).Where(c => c.Status == "pending").ToArray();
    public static bool SameSnapshot(NestSnapshot? a, NestSnapshot? b) => ReferenceEquals(a, b) || a is not null && b is not null
        && (a.Slots, a.Used, a.Package, a.Duplicates) == (b.Slots, b.Used, b.Package, b.Duplicates) && a.Nests.SequenceEqual(b.Nests) && a.Setups.SequenceEqual(b.Setups);
    public static bool SameCards(IReadOnlyList<NestApproval>? a, IReadOnlyList<NestApproval>? b) => ReferenceEquals(a, b) || a is not null && b is not null && a.SequenceEqual(b);

    public static string PackageName(IEnumerable<NestPackage>? packages, string code) => packages?.FirstOrDefault(p => p.Code == code)?.Name ?? code;
    private static string Count(int n, string one) => n == 1 ? $"1 {one}" : $"{n:N0} {one}s";

    public static string PackageDetails(NestPackage p)
    {
        var ai = p.ManagedAi ? $"AI included: {p.InputTokens:N0} input and {p.OutputTokens:N0} output tokens a month" : "No AI included: connect a model later";
        return $"{Count(p.Slots, "Nest")} · {p.StorageBytes / (1L << 30)} GiB files · {p.ActiveHours} hours a month · {ai}. Shared by the package’s Nests.";
    }

    public static string AllowanceLine(NestSnapshot s, IEnumerable<NestPackage>? packages) =>
        $"Your package: {PackageName(packages, s.Package ?? "")}. {s.Used} of {Count(s.Slots, "Nest")} in use.";
    public static string DuplicateWarning => "You pay for more than one Nest package. Cancel the extra one where you bought it; it does not add Nests.";
    public static string SetupLine(NestSetup s) => $"{s.Label}: paid, setting up" + (s.Failed ? " failed" : "…");
    public static string OrgLine(OrgNests o) => $"{o.Used} of {Count(o.Slots, "organization Nest")} in use.";
    public static string RoleLabel(NestRow n) => n.IsOwner ? "admin" : "operator";
    public static string StorageLine(NestFiles f) => $"{f.UsedBytes / 1048576.0:0.0} MiB of {f.QuotaBytes / 1073741824.0:0} GiB used";

    /// <summary>A safe suggested name for the export; the file dialog still lets the person change it.</summary>
    public static string ExportFileName(string label)
    {
        var kept = new string(label.Where(c => char.IsLetterOrDigit(c) || c is ' ' or '-' or '_').Take(40).ToArray()).Trim();
        return (kept.Length == 0 ? "Nest export" : kept) + ".zip";
    }

    public static readonly IReadOnlyList<(string Name, string Text)> Services =
    [
        ("Personal assistant", "Tasks, reminders and scheduling drafts from your own documents."),
        ("AI CRM", "Contacts, pipeline stages and follow-up drafts."),
        ("AI HR", "Job descriptions, candidate intake and interview briefs."),
        ("Shopping assistant", "Wish lists and comparisons from sources you approve."),
        ("Marketing assistant", "Brand knowledge, campaign plans and drafts for review."),
    ];

    public static (string Text, string? Reason) StatusLine(NestRow nest)
    {
        var text = nest.Status switch
        {
            "ready" => "Ready", "working" => "Working…", "needs_you" => "Needs you", "paused" => "Paused", _ => "Unavailable",
        };
        var reason = nest.StatusReason switch
        {
            "payment" => "Its package has ended. Renew it on the website to bring the Nest back.",
            "over_capacity" => "Your package has fewer Nests than you have. Delete one or upgrade.",
            "setting_up" => "Setting up…",
            "failed" => "Setup failed. Retry from Products.",
            _ => null,
        };
        return (text, reason);
    }

    /// <summary>The display name as the x-file-name header: percent-encoded UTF-8 without control characters.</summary>
    public static string UploadHeader(string name)
    {
        // Counted in scalars so a long name never ends in half of a surrogate pair.
        var clean = string.Concat((name ?? "").EnumerateRunes().Where(r => !Rune.IsControl(r)).Take(200).Select(r => r.ToString())).Trim();
        if (clean.Length == 0) throw new ArgumentException("Choose a file with a name.");
        var sb = new StringBuilder();
        foreach (var b in Encoding.UTF8.GetBytes(clean))
            sb.Append(b is >= (byte)'A' and <= (byte)'Z' or >= (byte)'a' and <= (byte)'z' or >= (byte)'0' and <= (byte)'9' or (byte)'-' or (byte)'_' or (byte)'.' or (byte)'~'
                ? ((char)b).ToString() : "%" + b.ToString("X2"));
        return sb.ToString();
    }

    public static bool DeleteConfirmed(string label, string? typed) => typed is not null && string.Equals(label, typed, StringComparison.Ordinal);

    /// <summary>The body of a decision: exactly the hash of the call the person saw.</summary>
    public static string DecisionJson(NestApproval card) => JsonSerializer.Serialize(new Dictionary<string, string> { ["args_hash"] = card.ArgsHash });

    public static string CardText(NestApproval card) => card.Tool == "send_message" ? $"Send a message to {card.To}:\n{card.Body}" : card.Tool;

    public static string CoverageText(NestFile f) => f.Status switch
    {
        "rejected" => f.Reason ?? "Could not be read",
        _ when f.UnitsTotal is > 0 => $"Read {f.UnitsText ?? 0} of {f.UnitsTotal}",
        "parsed" => "Read",
        "partial" => "Partly read",
        "uploading" => "Reading…",
        "failed" => f.Reason ?? "Could not be read",
        _ => "Not read yet",
    };

    public static string Explain(PostboxException error) => error.Code switch
    {
        "feature_disabled" => "Nests are not available for this account yet.",
        "no_nest_package" => "Buy a Nest package on the website first.",
        "nest_slots_full" => "Every Nest in your package is in use.",
        "approval_changed" => "The action changed since you saw it. Review it again.",
        "approval_closed" => "This approval was already decided, replaced or expired.",
        "recipient_not_allowed" => "A Nest may only message you or people you have allowed.",
        "needs_payment" => "This Nest is paused until its package is paid for.",
        "over_capacity" => "Your package has fewer Nests than you have. Delete one or upgrade.",
        "storage_full" => "This Nest’s storage is full.",
        "too_large" => "Choose a file smaller than 25 MB.",
        "confirm_required" => "Type the Nest’s name exactly to delete it.",
        "export_too_large" => "This Nest is larger than one download. Contact support.",
        "no_nest_slot" => "The organization has no free Nest slot. Buy or upgrade a package on the website.",
        "bad_label" => "Use 1 to 64 printable characters for the name.",
        "bad_name" => "Choose a file with a name.",
        "too_many_files" => "This Nest holds its maximum number of files. Delete one first.",
        "too_many_pending" => "Too many approvals are waiting. Decide some first.",
        "rate_limited" => "Too many requests. Please wait a moment and try again.",
        "no_nest" or "not_found" => "This Nest is no longer available.",
        "no_approval" => "This approval no longer exists.",
        "no_file" => "This file no longer exists.",
        "no_setup" => "This setup no longer exists.",
        "org_inactive" => "This organization is not active.",
        "tool_failed" => "The approved action could not be carried out.",
        "nests_unavailable" or "nestd_failed" => "Nests are temporarily unavailable. Please try again later.",
        "human_required" or "credential_not_accepted" => "Only you can do this, signed in on this device.",
        _ => "Pigeonpost could not complete this. Try again.",
    };

    public static string Explain(Exception error) => error switch
    {
        PostboxException p => Explain(p),
        HttpRequestException or TaskCanceledException => "Could not connect. Check your internet connection and try again.",
        NestWireException => error.Message,
        ArgumentException => error.Message,
        _ => "Pigeonpost could not complete this. Try again.",
    };

    /// <summary>The page should forget the account's Nests when the server says they are not for this person.</summary>
    public static bool EndsAccess(Exception error) => error is PostboxException { StatusCode: 401 or 403 } or PostboxException { Code: "feature_disabled" };

    // ---- wire ------------------------------------------------------------------------------

    private static string Clean(string s) => new(s.Where(c => c >= ' ' || c is '\n' or '\t').ToArray());

    // Limits are the server's own, which count scalars: a name of 64 emoji is 128 UTF-16 units.
    private static string? Str(JsonElement o, string name, int max = 300)
    {
        if (!o.TryGetProperty(name, out var v) || v.ValueKind == JsonValueKind.Null) return null;
        if (v.ValueKind != JsonValueKind.String) throw new NestWireException();
        var s = v.GetString()!;
        if (ScalarLength(s) > max) throw new NestWireException();
        return Clean(s);
    }

    // Display-only explanations: too long is shortened, and never costs the person the whole page.
    private static string? Clip(JsonElement o, string name, int max = 300)
    {
        if (!o.TryGetProperty(name, out var v) || v.ValueKind != JsonValueKind.String) return null;
        var s = Clean(v.GetString()!);
        return ScalarLength(s) <= max ? s : string.Concat(s.EnumerateRunes().Take(max).Select(r => r.ToString())) + "…";
    }
    private static string Need(JsonElement o, string name, int max = 300) => Str(o, name, max) ?? throw new NestWireException();
    private static long Num(JsonElement o, string name) => o.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt64(out var n) ? n : 0;
    private static int? OptInt(JsonElement o, string name) => o.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt32(out var n) ? n : null;
    private static IEnumerable<JsonElement> Items(JsonElement o, string name) =>
        o.ValueKind == JsonValueKind.Object && o.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Array ? v.EnumerateArray().ToArray() : [];
    private static JsonElement Obj(JsonElement o, string name) =>
        o.ValueKind == JsonValueKind.Object && o.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Object ? v : default;

    public static IReadOnlyList<NestPackage> Packages(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("packages", out var list) || list.ValueKind != JsonValueKind.Array) throw new NestWireException();
        return list.EnumerateArray()
            .Where(p => p.ValueKind == JsonValueKind.Object && p.TryGetProperty("sellable", out var s) && s.ValueKind == JsonValueKind.True)
            .Select(p => new NestPackage(Need(p, "code", 64), Need(p, "name", 80), (int)Num(p, "slots"), Num(p, "storage_bytes"), (int)Num(p, "active_hours"),
                Num(p, "input_tokens"), Num(p, "output_tokens"), Str(p, "model", 16) == "managed"))
            .ToArray();
    }

    public static NestRow Nest(JsonElement n)
    {
        var id = Need(n, "nest_id", 64);
        if (!ValidNest(id)) throw new NestWireException();
        var status = Str(n, "status", 16);
        return new NestRow(id, Str(n, "address", 256), Need(n, "label", 64), Str(n, "state", 32) ?? "unknown",
            status is "ready" or "working" or "needs_you" or "paused" ? status : "unavailable", Clip(n, "status_reason", 64), Str(n, "role", 16) != "operator");
    }

    public static NestSnapshot Snapshot(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) throw new NestWireException();
        var allowance = Obj(root, "allowance");
        var funding = Obj(allowance, "funding");
        var setups = Items(root, "setups").Where(s => s.ValueKind == JsonValueKind.Object && ValidSetup(Str(s, "setup_id", 64)))
            .Select(s => new NestSetup(Need(s, "setup_id", 64), Str(s, "label", 64) ?? "Nest", Str(s, "status", 16) == "failed", Clip(s, "last_error", 200))).ToArray();
        return new NestSnapshot(allowance.ValueKind == JsonValueKind.Object ? (int)Num(allowance, "slots") : 0,
            allowance.ValueKind == JsonValueKind.Object ? (int)Num(allowance, "used") : 0,
            funding.ValueKind == JsonValueKind.Object ? Str(funding, "package", 64) : null,
            Items(allowance, "duplicates").Count(), Items(root, "nests").Select(Nest).ToArray(), setups);
    }

    public static NestFiles Files(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("files", out var list) || list.ValueKind != JsonValueKind.Array) throw new NestWireException();
        var files = list.EnumerateArray().Select(f =>
        {
            var id = Need(f, "file_id", 40);
            if (!ValidFile(id)) throw new NestWireException();
            var cov = Obj(f, "coverage");
            return new NestFile(id, Need(f, "name", 200), Num(f, "bytes"), Str(f, "status", 16) ?? "failed", Clip(f, "reason"),
                cov.ValueKind == JsonValueKind.Object ? OptInt(cov, "units_total") : null, cov.ValueKind == JsonValueKind.Object ? OptInt(cov, "units_text") : null,
                Items(f, "warnings").Where(w => w.ValueKind == JsonValueKind.String).Take(20).Select(w => Clean(w.GetString()!)).ToArray());
        }).ToArray();
        return new NestFiles(files, Num(root, "used_bytes"), Num(root, "quota_bytes"));
    }

    public static IReadOnlyList<NestApproval> Approvals(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("approvals", out var list) || list.ValueKind != JsonValueKind.Array) throw new NestWireException();
        return list.EnumerateArray().Select(c =>
        {
            var id = Need(c, "approval_id", 40);
            var hash = Need(c, "args_hash", 64);
            if (!ApprovalId().IsMatch(id) || !Hash().IsMatch(hash)) throw new NestWireException();
            var args = Obj(Obj(c, "call"), "args");
            return new NestApproval(id, Need(c, "tool", 64), hash, Str(c, "status", 16) ?? "pending",
                args.ValueKind == JsonValueKind.Object ? Str(args, "to", 200) : null, args.ValueKind == JsonValueKind.Object ? Str(args, "body", 4000) : null);
        }).ToArray();
    }

    public static IReadOnlyList<string> UsageLines(JsonElement root)
    {
        var tin = Obj(root, "input_tokens");
        var tout = Obj(root, "output_tokens");
        long V(JsonElement o, string n) => o.ValueKind == JsonValueKind.Object ? Num(o, n) : 0;
        var secs = root.ValueKind == JsonValueKind.Object ? Num(root, "running_secs") : 0;
        return
        [
            $"AI this month: {V(tin, "used"):N0} of {V(tin, "limit"):N0} input tokens, {V(tout, "used"):N0} of {V(tout, "limit"):N0} output tokens",
            $"Running time this month: {secs / 3600} h {secs % 3600 / 60} min",
            $"Files: {(root.ValueKind == JsonValueKind.Object ? Num(root, "files_bytes") : 0) / 1048576.0:0.0} MiB",
        ];
    }

    public static OrgNests OrgNestsOf(JsonElement root)
    {
        var allowance = Obj(root, "allowance");
        var nests = Items(root, "nests").Where(n => n.ValueKind == JsonValueKind.Object && ValidNest(Str(n, "nest_id", 64)))
            .Select(n => new NestRow(Need(n, "nest_id", 64), Str(n, "address", 256), Str(n, "label", 64) ?? "Nest", Str(n, "state", 32) ?? "unknown", "ready", null, Str(n, "role", 16) == "owner")).ToArray();
        return new OrgNests(nests, allowance.ValueKind == JsonValueKind.Object ? (int)Num(allowance, "slots") : 0, allowance.ValueKind == JsonValueKind.Object ? (int)Num(allowance, "used") : 0);
    }
}

/// <summary>The Nest routes, implemented by <see cref="PostboxClient"/> and by test doubles.</summary>
public interface INestApi
{
    Task<IReadOnlyList<NestPackage>> NestPackagesAsync(CancellationToken ct);
    Task<NestSnapshot> NestSnapshotAsync(CancellationToken ct);
    Task<NestRow> NestAsync(string nestId, CancellationToken ct);
    Task<NestSnapshot> RetryNestSetupAsync(string setupId, CancellationToken ct);
    Task<NestFiles> NestFilesAsync(string nestId, CancellationToken ct);
    Task UploadNestFileAsync(string nestId, string name, byte[] bytes, CancellationToken ct);
    Task DeleteNestFileAsync(string nestId, string fileId, CancellationToken ct);
    Task<IReadOnlyList<NestApproval>> NestApprovalsAsync(string nestId, CancellationToken ct);
    Task DecideNestApprovalAsync(string nestId, NestApproval card, bool approve, CancellationToken ct);
    Task<IReadOnlyList<string>> NestUsageAsync(string nestId, CancellationToken ct);
    Task PauseNestAsync(string nestId, bool resume, CancellationToken ct);
    Task RenameNestAsync(string nestId, string label, CancellationToken ct);
    Task<byte[]> ExportNestAsync(string nestId, CancellationToken ct);
    Task DeleteNestAsync(string nestId, string label, string typed, CancellationToken ct);
    Task<OrgNests> OrgNestsAsync(string orgId, CancellationToken ct);
    Task CreateOrgNestAsync(string orgId, string handle, string label, CancellationToken ct);
}
