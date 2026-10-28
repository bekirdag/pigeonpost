using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Pigeonpost.Core.Meetings;

namespace Pigeonpost.Desktop;

//  Phase 7 on Windows: the Meeting notes screen (docs/meeting-summaries.md, the web reader
//  site-inbox/meeting-notes.js). Notes are decrypted here with this device's own notes key; the
//  postbox holds only ciphertext. Capture and consent happen in the call host (calls-summary.js).
//  Every brief is shown as plain text, never markup.
public sealed partial class MainWindow
{
    private MeetingNotesModel meetingNotes = null!;
    private StackPanel? notesBody;
    private TextBlock? notesHeading;
    private int notesView;

    private FrameworkElement BuildNotesPage()
    {
        var frame = PageFrame("Meeting notes", out var actions, out var body);
        notesHeading = (TextBlock)((Grid)frame.Children[0]).Children[1];
        var refresh = new Button { Content = "Refresh", Style = (Style)Root.Resources["SecondaryActionButton"] };
        AutomationProperties.SetName(refresh, "Refresh meeting notes");
        refresh.Click += async (_, _) => await ShowNotesListAsync();
        actions.Children.Add(refresh);
        notesBody = new StackPanel { Padding = new Thickness(24, 4, 24, 24), Spacing = 10, MaxWidth = 860, HorizontalAlignment = HorizontalAlignment.Left };
        body.Children.Add(new ScrollViewer { Content = notesBody });
        _ = ShowNotesListAsync();
        return frame;
    }

    private bool NotesPageCurrent(int view) => view == notesView && sideView == SideView.Notes && notesBody is not null;

    private static TextBlock NotesText(string text, double size = 14, bool bold = false, double opacity = 1) => new()
    {
        Text = text, TextWrapping = TextWrapping.Wrap, FontSize = size, Opacity = opacity, IsTextSelectionEnabled = true,
        FontWeight = bold ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal,
    };

    private async Task ShowNotesListAsync()
    {
        if (notesBody is null) return;
        var view = ++notesView;
        if (notesHeading is not null) notesHeading.Text = "Meeting notes";
        notesBody.Children.Clear();
        notesBody.Children.Add(NotesText(MeetingNotesModel.Explanation, 12.5, opacity: 0.8));
        var loading = NotesText("Loading…", 13, opacity: 0.8);
        notesBody.Children.Add(loading);
        await meetingNotes.LoadAsync();
        if (!NotesPageCurrent(view)) return;
        notesBody.Children.Remove(loading);
        if (meetingNotes.EmptyText.Length > 0) { notesBody.Children.Add(NotesText(meetingNotes.EmptyText, 13)); return; }
        foreach (var row in meetingNotes.Meetings.ToArray())
        {
            var content = new StackPanel { Spacing = 2 };
            content.Children.Add(new TextBlock { Text = row.When, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            content.Children.Add(new TextBlock { Text = row.Detail, FontSize = 12.5, Opacity = 0.8, TextWrapping = TextWrapping.Wrap });
            var button = new Button { Content = content, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Left, Padding = new Thickness(12, 8, 12, 8) };
            AutomationProperties.SetName(button, row.AccessibleName);
            var id = row.Id;
            button.Click += async (_, _) => await ShowNotesAsync(id);
            notesBody.Children.Add(button);
        }
    }

    private async Task ShowNotesAsync(string id)
    {
        if (notesBody is null) return;
        var view = ++notesView;
        notesBody.Children.Clear();
        var back = new HyperlinkButton { Content = "← All meeting notes" };
        AutomationProperties.SetName(back, "All meeting notes");
        back.Click += async (_, _) => await ShowNotesListAsync();
        notesBody.Children.Add(back);
        notesBody.Children.Add(NotesText("Opening on this device…", 13, opacity: 0.8));
        var result = await meetingNotes.OpenAsync(id);
        if (!NotesPageCurrent(view)) return;
        notesBody.Children.RemoveAt(1);
        if (result is null) { await ShowNotesListAsync(); return; }
        if (result.Brief is not { } brief)
        {
            var message = NotesText(result.Message ?? "This device couldn't open these notes.", 14);
            AutomationProperties.SetName(message, result.Message ?? "This device couldn't open these notes.");
            notesBody.Children.Add(message);
            notesBody.Children.Add(NotesActions(id, null));
            return;
        }
        if (notesHeading is not null) notesHeading.Text = brief.Heading;
        if (brief.MetaLine.Length > 0) notesBody.Children.Add(NotesText(brief.MetaLine, 12.5, opacity: 0.8));
        if (brief.Draft) notesBody.Children.Add(new InfoBar { IsOpen = true, IsClosable = false, Severity = InfoBarSeverity.Warning, Message = "A summary couldn't be generated for this meeting. The transcript is below." });
        void Section(string title, IEnumerable<string> lines)
        {
            var items = lines.ToArray();
            if (items.Length == 0) return;
            var heading = NotesText(title, 16, bold: true);
            AutomationProperties.SetHeadingLevel(heading, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
            notesBody!.Children.Add(heading);
            foreach (var line in items) notesBody.Children.Add(NotesText("•  " + line));
        }
        if (brief.Summary is { } summary)
        {
            var heading = NotesText("Summary", 16, bold: true);
            AutomationProperties.SetHeadingLevel(heading, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
            notesBody.Children.Add(heading);
            notesBody.Children.Add(NotesText(summary));
        }
        Section("Decisions", brief.Decisions);
        Section("Action items", brief.ActionItems.Select(a => a.Text));
        Section("Open questions", brief.OpenQuestions);
        Section("Transcript", brief.Transcript.Select(t => $"{t.At}  {t.Speaker}: {t.Text}"));
        notesBody.Children.Add(NotesText(brief.GeneratedLine, 12, opacity: 0.75));
        notesBody.Children.Add(NotesActions(id, brief));
    }

    private StackPanel NotesActions(string id, MeetingBrief? brief)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 8, 0, 0) };
        if (brief is not null)
        {
            var copy = new Button { Content = "Copy as text" };
            AutomationProperties.SetName(copy, "Copy notes as text");
            copy.Click += (_, _) => CopyText(brief.AsText());
            row.Children.Add(copy);
        }
        var delete = new Button { Content = "Delete my copy" };
        AutomationProperties.SetName(delete, "Delete my copy of these notes");
        delete.Click += async (_, _) =>
        {
            if (!await ConfirmAsync("Delete your copy of these notes?", "Other attendees keep theirs.", "Delete")) return;
            AccountStatus.Text = await meetingNotes.DeleteAsync(id) ? "Deleted." : "Couldn't delete.";
            await ShowNotesListAsync();
        };
        row.Children.Add(delete);
        return row;
    }
}
