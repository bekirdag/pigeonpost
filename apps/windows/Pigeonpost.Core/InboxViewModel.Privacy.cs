namespace Pigeonpost.Core;

// Optional per-conversation settings a service may offer (the postbox does; the preview too).
public interface IConversationSettings
{
    AuthorizationLease? AcceptedAuthorization => null;
    Task SetThreadTimerAsync(string identity, string threadId, int? seconds, CancellationToken cancellationToken);
    Task ReportMessageAsync(string identity, string messageId, CancellationToken cancellationToken);
}

// Phase 1b in the conversation view: blocking with its banner, missed-call lines and the
// disappearing-message timer of the selected subject (SDS §12.1, §15.2, §17).
public sealed partial class InboxViewModel
{
    private IReadOnlyList<CallLogEntry> callEntries = [];
    private IConversationSettings? Settings => service as IConversationSettings ?? attachmentService as IConversationSettings;

    public bool IsSelectedBlocked => SelectedConversation is { } c && BlockRules.IsBlocked(c.Peer, snapshot.Contacts);
    public string BlockedBanner => IsSelectedBlocked ? "You blocked this person. They can't message or call you." : "";
    public IReadOnlyList<Contact> Contacts => ActiveAccount is null ? [] : snapshot.Contacts;
    public bool HasTimer => SelectedSubject?.EphemeralSecs is > 0;
    public string TimerBanner => Timers.Banner(SelectedSubject?.EphemeralSecs);
    public bool CanSetTimer => CanCompose && Settings is not null && !string.IsNullOrEmpty(SelectedSubject?.Id);

    // Missed incoming calls become lines in their 1:1 conversations.
    public void SetCallEntries(IReadOnlyList<CallLogEntry> entries)
    {
        if (ActiveAccount is null || entries.SequenceEqual(callEntries)) return;
        callEntries = entries;
        Rebuild();
    }

    // The newest received message from a peer, for "Also report as spam".
    public string? LastReceivedFrom(string peer) => SelectedMailbox is null ? null
        : allConversations.FirstOrDefault(c => c.Peer == peer)?.Messages.LastOrDefault(m => !m.IsOutgoing && !m.IsSystem)?.Id;

    public async Task<bool> BlockAsync(string peer, string? alias, bool report, string? messageId = null)
    {
        if (SelectedMailbox is not { } mailbox) return false;
        using var operation = BeginOperation();
        if (operation is null) return false;
        var reportedMessage = messageId ?? LastReceivedFrom(peer);
        var generation = mailboxVersion;
        Error = null;
        try
        {
            await service.SetContactAsync(mailbox.Address, new Contact(peer, string.IsNullOrWhiteSpace(alias) ? null : alias.Trim(), "block", "review", []), mailboxScope.Token);
            operation.Check();
            if (report && reportedMessage is { } id && Settings is { } settings)
            {
                try
                {
                    operation.RequireService(settings.AcceptedAuthorization);
                    await settings.ReportMessageAsync(mailbox.Address, id, mailboxScope.Token);
                    operation.Check();
                }
                catch (Exception ex) when (ex is not OperationCanceledException) { if (operation.IsMailbox(generation)) Error = "Blocked, but the spam report could not be sent."; }
            }
            if (operation.IsMailbox(generation)) await RefreshAsync();
            return operation.IsCurrent;
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = "Could not block. " + Describe(ex); }
        return false;
    }

    public async Task<bool> UnblockAsync(string peer)
    {
        if (SelectedMailbox is not { } mailbox) return false;
        using var operation = BeginOperation();
        if (operation is null) return false;
        var generation = mailboxVersion;
        var rule = snapshot.Contacts.FirstOrDefault(c => c.Peer == peer && c.Admission == "block")
            ?? ConversationBuilder.FindContact(peer, snapshot.Contacts);
        try
        {
            await service.RemoveContactAsync(mailbox.Address, rule?.Peer ?? peer, mailboxScope.Token);
            operation.Check();
            if (operation.IsMailbox(generation)) await RefreshAsync();
            return operation.IsCurrent;
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
        return false;
    }

    public async Task<bool> SetTimerAsync(int? seconds)
    {
        if (!CanSetTimer || SelectedMailbox is not { } mailbox || SelectedSubject is not { } subject || Settings is not { } settings) return false;
        using var operation = BeginOperation();
        if (operation is null) return false;
        if (seconds is not null && !Timers.Choices.Contains(seconds)) return false;
        var generation = mailboxVersion;
        try
        {
            operation.RequireService(settings.AcceptedAuthorization);
            await settings.SetThreadTimerAsync(mailbox.Address, subject.Id, seconds, mailboxScope.Token);
            operation.Check();
            if (operation.IsMailbox(generation)) await RefreshAsync();
            return operation.IsCurrent;
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
        return false;
    }

    // Drop messages whose timer ran out, by this device's clock, between server refreshes.
    public void PruneExpired()
    {
        if (SelectedMailbox is null || ActiveAccount is null) return;
        var now = MessageNow;
        if (snapshot.Messages.Any(m => Timers.Expired(m.ExpiresAt, now)))
        {
            snapshot = snapshot with { Messages = snapshot.Messages.Where(m => !Timers.Expired(m.ExpiresAt, now)).ToArray() };
            Rebuild();
        }
    }
}
