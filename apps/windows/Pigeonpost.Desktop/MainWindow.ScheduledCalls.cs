using System.Diagnostics;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private sealed class ScheduledCallAttempt(CalendarScheduledJoin request, NativeCallSession actor)
    {
        internal readonly Guid Id = Guid.NewGuid();
        internal readonly CalendarScheduledJoin Request = request;
        internal readonly NativeCallSession Actor = actor;
        internal readonly TaskCompletionSource<CallPageMessage> Result = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal CallPageTicket? Page;
    }
    private ScheduledCallAttempt? scheduledCall;
    private bool callConfigured;

    private async Task JoinCalendarMeetingAsync(CalendarActionTarget target, CalendarEventKey key, string code, bool video, Guid context)
    {
#if UI_TESTS
        if (CallsDebug("PAGE") is null) { AccountStatus.Text = "Calling is not configured in this preview."; return; }
#endif
        if (scheduledCall is not null || pendingCall is not null || callMode != CallMode.Hidden) { AccountStatus.Text = "Finish the current call first."; return; }
        using var request = await calendarModel.PrepareScheduledJoinAsync(target, key, code, video, context, lifetime.Token);
        if (request is null) { if (target.IsCurrent) AccountStatus.Text = calendarModel.State.Error ?? "Open the event again to join."; return; }
        await JoinPreparedMeetingAsync(request);
    }
    private async Task JoinPreparedMeetingAsync(CalendarScheduledJoin request)
    {
        ScheduledCallAttempt? attempt = null;
        try
        {
            var provider = CallTokenProvider(); request.RequireCurrent(provider.AcceptedAuthorization, CurrentCallMailbox()?.Address);
            if (scheduledCall is not null || pendingCall is not null || callMode != CallMode.Hidden) { AccountStatus.Text = "Finish the current call first."; return; }
            using var binding = request.BindForService(provider, request.Mailbox);
            if (callSession is not { } initial || !CallCurrent(initial))
                await StartCallsAsync().WaitAsync(TimeSpan.FromSeconds(15), lifetime.Token);
            binding.RequireCurrent();
            if (callSession is not { } actor || !CallCurrent(actor) || !actor.Matches(new(request.Mailbox)) || scheduledCall is not null || callMode != CallMode.Hidden) return;
            attempt = new(request, actor); scheduledCall = attempt; UpdateCallButtons(); AccountStatus.Text = "Joining the meeting…";
            void Check()
            {
                lifetime.Token.ThrowIfCancellationRequested(); binding.RequireCurrent();
                if (!ReferenceEquals(scheduledCall, attempt) || !CallCurrent(actor) || CurrentCallMailbox()?.Address != request.Mailbox) throw new AuthorizationRetiredException();
                if (attempt.Page is { } original && !CallPageCurrent(original)) throw new AuthorizationRetiredException();
            }
            var watch = Stopwatch.StartNew();
            while (!callReady || !callConfigured || CaptureCallPage() is null)
            {
                Check(); if (watch.Elapsed > TimeSpan.FromSeconds(15)) throw new TimeoutException();
                await Task.Delay(75, lifetime.Token);
            }
            Check(); var page = CaptureCallPage();
            if (page is null || !ReferenceEquals(page.Actor, actor)) throw new AuthorizationRetiredException();
            attempt.Page = page; Check();
            if (!await RunCallScriptAsync(page, CallBridge.ScheduledJoinScript(request, attempt.Id))) throw new AuthorizationRetiredException();
            Check(); watch.Restart();
            while (!attempt.Result.Task.IsCompleted)
            {
                Check(); if (watch.Elapsed > TimeSpan.FromSeconds(30)) throw new TimeoutException();
                await Task.WhenAny(attempt.Result.Task, Task.Delay(100, lifetime.Token));
            }
            var result = await attempt.Result.Task; Check();
            DesktopLog.Write("Scheduled call result: " + result.State);
            AccountStatus.Text = result.State switch
            {
                "joined" => "", "waiting" => "Waiting for a host to admit you.", "cancelled" => "Joining was cancelled.",
                "choose_occurrence" => "The meeting changed. Select its occurrence from the calendar again.",
                _ => result.ErrorCode switch
                {
                    "window_closed" => "This meeting is not open yet or its window has ended.",
                    "not_invited" or "declined" => "This inbox cannot join the meeting. Review its invitation.",
                    "busy" => "Finish the current call first.", _ => "Could not join the meeting. Open the event and try again."
                }
            };
        }
        catch (OperationCanceledException) { }
        catch (Exception)
        {
            try
            {
                request.RequireCurrent(CallTokenProvider().AcceptedAuthorization, CurrentCallMailbox()?.Address);
                if (attempt is null || ReferenceEquals(scheduledCall, attempt) && CallCurrent(attempt.Actor))
                    AccountStatus.Text = "Joining was not confirmed. Check the current call before trying again.";
            }
            catch (AuthorizationRetiredException) { }
        }
        finally
        {
            // The page owns its bounded 25-second join promise. Closing/replacing that page
            // already signs it out; a late timeout must never hang up another call on its host.
            if (attempt is not null && ReferenceEquals(scheduledCall, attempt)) { scheduledCall = null; UpdateCallButtons(); }
        }
    }
    private void ScheduledCallResult(CallPageTicket page, CallPageMessage result)
    {
        if (scheduledCall is { Page: { } original } attempt && result.Operation == attempt.Id && result.Type == "scheduled_result"
            && ReferenceEquals(original.View, page.View) && ReferenceEquals(original.Core, page.Core) && original.Document == page.Document
            && ReferenceEquals(original.Actor, page.Actor) && CallPageCurrent(original)) attempt.Result.TrySetResult(result);
    }
    private void RetireScheduledCall()
    {
        var retired = scheduledCall; scheduledCall = null; retired?.Request.Dispose(); retired?.Result.TrySetCanceled();
        callConfigured = false;
    }
}
