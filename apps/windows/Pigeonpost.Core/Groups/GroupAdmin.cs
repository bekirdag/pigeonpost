using System.Text.RegularExpressions;

namespace Pigeonpost.Core.Groups;

// Group administration on Windows (audit 2026-11-03 P1-14, P3-6): which member actions to offer,
// invite links and joining by link. The rules mirror the postbox (groups.rs patch_member, transfer,
// link, unban) and the Android, Linux and Apple clients; the postbox still decides every change.
public enum GroupMemberAction
{
    MakeAdmin, RemoveAdmin, Restrict, Unrestrict, Remove, Ban, RevokeInvitation, StepDown, Unban,
}

public static partial class GroupAdmin
{
    public static string Label(GroupMemberAction action) => action switch
    {
        GroupMemberAction.MakeAdmin => "Make admin",
        GroupMemberAction.RemoveAdmin => "Remove admin role",
        GroupMemberAction.Restrict => "Stop from posting",
        GroupMemberAction.Unrestrict => "Allow posting",
        GroupMemberAction.Remove => "Remove from group",
        GroupMemberAction.Ban => "Remove and ban",
        GroupMemberAction.RevokeInvitation => "Revoke invitation",
        GroupMemberAction.StepDown => "Step down as admin",
        GroupMemberAction.Unban => "Unban",
        _ => "",
    };

    // The (role, state) a PATCH /members/{address} sends for an action; Unban is a DELETE instead.
    public static (string? Role, string? State) Change(GroupMemberAction action) => action switch
    {
        GroupMemberAction.MakeAdmin => ("admin", null),
        GroupMemberAction.RemoveAdmin or GroupMemberAction.Unrestrict or GroupMemberAction.StepDown => ("member", null),
        GroupMemberAction.Restrict => ("restricted", null),
        GroupMemberAction.Remove or GroupMemberAction.RevokeInvitation => (null, "removed"),
        GroupMemberAction.Ban => (null, "banned"),
        _ => (null, null),
    };

    public static bool IsDestructive(GroupMemberAction action) => action is GroupMemberAction.Remove or GroupMemberAction.Ban;

    // What this person may do to one member row (the acting mailbox's own row has IsMe set).
    public static IReadOnlyList<GroupMemberAction> ActionsFor(GroupInfo group, GroupMemberRow member)
    {
        var actions = new List<GroupMemberAction>();
        if (!group.IsActive) return actions;
        bool admin = group.IsAdmin, owner = group.IsOwner;
        var inGroup = member.State is "active" or "keying";
        if (member.IsMe)
        {
            // An admin may resign; the owner hands the group over with a transfer instead (D-07).
            if (inGroup && member.Role == "admin") actions.Add(GroupMemberAction.StepDown);
            return actions;
        }
        if (member.State == "banned") { if (admin) actions.Add(GroupMemberAction.Unban); return actions; }
        if (member.State == "invited") { if (admin) actions.Add(GroupMemberAction.RevokeInvitation); return actions; }
        if (!inGroup || member.Role == "owner") return actions;
        if (admin && member.Role != "admin") actions.Add(GroupMemberAction.MakeAdmin);
        if (owner && member.Role == "admin") actions.Add(GroupMemberAction.RemoveAdmin);
        if (admin && member.Role == "member") actions.Add(GroupMemberAction.Restrict);
        if (admin && member.Role == "restricted") actions.Add(GroupMemberAction.Unrestrict);
        if (admin && (member.Role != "admin" || owner)) { actions.Add(GroupMemberAction.Remove); actions.Add(GroupMemberAction.Ban); }
        return actions;
    }

    // Ownership goes to an active admin only (groups.rs transfer).
    public static IReadOnlyList<GroupMemberRow> TransferCandidates(GroupInfo group, IReadOnlyList<GroupMemberRow> members) =>
        group.IsOwner && group.IsActive ? members.Where(m => !m.IsMe && m.Role == "admin" && m.State == "active").ToArray() : [];

    // An owner with other members must transfer ownership before leaving.
    public static bool MayLeave(GroupInfo group) => !group.IsOwner || (group.MemberCount ?? 0) <= 1;

    // Only public groups have a join link, and only admins manage it.
    public static bool ManagesLink(GroupInfo group) => group.IsActive && group.IsAdmin && group.JoinPolicy == "public";

    // The links the postbox makes: https://inbox.pigeonpost.dev/g/<group id>#<token>. A link
    // without its token still names a listed group. Anything else is refused.
    public static (string GroupId, string? Token)? ParseInviteLink(string? text)
    {
        var match = InviteLink().Match(text ?? "");
        if (!match.Success) return null;
        return (match.Groups[1].Value, match.Groups[2].Success ? match.Groups[2].Value : null);
    }

    [GeneratedRegex(@"^\s*https://inbox\.pigeonpost\.dev/g/(grp_[A-Za-z0-9]{1,60})/?(?:#([A-Za-z0-9_-]{16,128}))?\s*$")]
    private static partial Regex InviteLink();

    // The words for a /v1/groups/resolve answer, before anything is joined.
    public static string PreviewText(GroupPreview preview, string actingAs)
    {
        var count = preview.MemberCount ?? 0;
        var lines = new List<string>();
        if (!string.IsNullOrWhiteSpace(preview.Description)) lines.Add(preview.Description!.Trim());
        lines.Add($"{count} member{(count == 1 ? "" : "s")} · joining as {actingAs}.");
        lines.Add("Group messages are end-to-end encrypted. You will see messages from when you join, not before.");
        if (preview.MyState is "active" or "keying") lines.Add("You are already a member.");
        else if (preview.MyState == "banned") lines.Add("An admin removed you from this group, so you cannot join it.");
        else if (preview.CanJoin != true)
            lines.Add(preview.Needs == "invitation" ? "You have an invitation to this group: accept it from Invitations in your group list." : "This group is invite only.");
        return string.Join("\n\n", lines);
    }

    public static string ExplainLinkFailure(PostboxException ex) => ex.Code switch
    {
        "invalid_link" or "not_found" => "This invite link is not valid any more.",
        "banned" => "An admin removed you from this group, so you cannot join it.",
        "invite_required" => "This group is invite only.",
        "rate_limited" => "Too many tries. Wait a little and try again.",
        _ => ex.Message,
    };
}

// POST /v1/groups/resolve: what a link or listing shows before joining.
public sealed record GroupPreview(string Id, string? Name, string? Description, int? MemberCount, string? JoinPolicy,
    string? Visibility, string? MyState, bool? CanJoin, string? Needs)
{
    public string DisplayName => string.IsNullOrWhiteSpace(Name) ? "Group" : Name!;
}
