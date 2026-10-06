using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private async void Contact_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || !ViewModel.CanCompose || ViewModel.SelectedConversation is not { } conversation) return;
        dialogOpen = true;
        Mailbox? switchTo = null;
        try
        {
            var original = conversation.Contact;
            var own = ViewModel.OwnMailbox(conversation.Peer);
            var alias = new TextBox { Header = "Display name", Text = original?.Alias ?? "", MaxLength = 200 };
            var known = new CheckBox { Content = "Known sender", IsChecked = original is not null };
            var blocked = new CheckBox { Content = "Block this sender", IsChecked = original?.Admission == "block" };
            var full = new CheckBox { Content = "Full permissions" };
            var choices = ViewModel.GrantableVerbs.Select(verb => new CheckBox
            {
                Content = verb, Tag = verb, IsChecked = original?.Autonomy == "auto" && original.AllowedVerbs?.Contains(verb) == true
            }).ToArray();
            var panel = new StackPanel { Spacing = 12, MaxWidth = 480 };
            panel.Children.Add(new TextBlock { Text = conversation.Peer, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            var copy = new Button { Content = "Copy address" };
            copy.Click += (_, _) => CopyText(conversation.Peer);
            panel.Children.Add(copy);
            if (own is not null) panel.Children.Add(new TextBlock { Text = "This is one of your mailboxes.", TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(alias);
            panel.Children.Add(known);
            panel.Children.Add(full);
            panel.Children.Add(new TextBlock { Text = "Choose which requests can run automatically:", TextWrapping = TextWrapping.Wrap });
            foreach (var choice in choices) panel.Children.Add(choice);
            if (choices.Length == 0) panel.Children.Add(new TextBlock { Text = "No grantable requests were returned by the postbox.", TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(blocked);
            panel.Children.Add(new TextBlock { Text = "Known senders still need permission to run requests. Blocking clears all automatic permissions.", TextWrapping = TextWrapping.Wrap });
            if (ViewModel.NeverAutoVerbs.Count > 0) panel.Children.Add(new TextBlock
            {
                Text = "Always held for approval: " + string.Join(", ", ViewModel.NeverAutoVerbs), TextWrapping = TextWrapping.Wrap
            });
            panel.Children.Add(new TextBlock
            {
                Text = original is null ? "No contact rule exists for this sender."
                    : original.IsWildcard ? $"Inherited from {original.Peer}. Saving creates an exact rule for this address. Removing an exact rule falls back to the namespace rule."
                    : "Exact address rule. Removing it falls back to any namespace rule.", TextWrapping = TextWrapping.Wrap
            });
            if (conversation.Messages.LastOrDefault(m => !m.IsOutgoing) is { } incoming)
                panel.Children.Add(new TextBlock { Text = $"Latest incoming request: {incoming.Autonomy ?? "review"}" +
                    (incoming.HeldBecause is { } reason ? $" ({reason})" : ""), TextWrapping = TextWrapping.Wrap });
            var error = new TextBlock { TextWrapping = TextWrapping.Wrap };
            panel.Children.Add(error);
            var changing = false;
            void UpdatePermissions()
            {
                if (changing) return;
                changing = true;
                var enabled = known.IsChecked == true && blocked.IsChecked != true;
                full.IsEnabled = enabled && choices.Length > 0;
                foreach (var choice in choices) choice.IsEnabled = enabled;
                full.IsChecked = enabled && choices.Length > 0 && choices.All(c => c.IsChecked == true);
                changing = false;
            }
            full.Click += (_, _) =>
            {
                changing = true;
                foreach (var choice in choices) choice.IsChecked = full.IsChecked == true;
                changing = false;
                UpdatePermissions();
            };
            known.Click += (_, _) => UpdatePermissions();
            blocked.Click += (_, _) => UpdatePermissions();
            foreach (var choice in choices) choice.Click += (_, _) => UpdatePermissions();
            UpdatePermissions();
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = "Sender details", Content = new ScrollViewer { Content = panel, MaxHeight = 440 },
                PrimaryButtonText = "Save", CloseButtonText = "Cancel",
                SecondaryButtonText = own is not null && own.Address != ViewModel.SelectedMailbox?.Address ? "Open this mailbox" : ""
            };
            while (true)
            {
                var result = await dialog.ShowAsync();
                if (result == ContentDialogResult.Secondary) { switchTo = own; break; }
                if (result != ContentDialogResult.Primary) break;
                if (blocked.IsChecked == true && original?.Admission != "block")
                {
                    var confirm = new ContentDialog { XamlRoot = Root.XamlRoot, Title = "Block this sender?",
                        Content = "Future mail will be refused and all automatic request permissions will be removed.",
                        PrimaryButtonText = "Block sender", CloseButtonText = "Cancel" };
                    if (await confirm.ShowAsync() != ContentDialogResult.Primary) continue;
                }
                if (await ViewModel.SaveContactAsync(conversation.Peer, known.IsChecked == true, alias.Text,
                    blocked.IsChecked == true, choices.Where(c => c.IsChecked == true).Select(c => (string)c.Tag))) break;
                error.Text = ViewModel.Error ?? "Could not update this sender. Please try again.";
            }
        }
        finally { dialogOpen = false; }
        // Complete the native dialog teardown before replacing its mailbox-bound content.
        if (switchTo is not null)
        {
            await ViewModel.SwitchMailboxAsync(switchTo);
            await ViewModel.AcknowledgeSelectedAsync();
        }
    }

    private async void Attach_Click(object sender, RoutedEventArgs e) => await AttachFilesAsync(async () =>
    {
#if UI_TESTS
        return [await StorageFile.GetFileFromPathAsync(Environment.GetEnvironmentVariable("PIGEONPOST_UI_ATTACHMENT_FILE")
            ?? throw new InvalidOperationException("Attachment fixture file was not provided."))];
#else
        var picker = new FileOpenPicker();
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        return (await picker.PickMultipleFilesAsync()).Cast<IStorageItem>().ToArray();
#endif
    });

    private void Files_DragOver(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems)) return;
        e.Handled = true;
        e.AcceptedOperation = !dialogOpen && ViewModel.CanCompose ? DataPackageOperation.Copy : DataPackageOperation.None;
        e.DragUIOverride.Caption = "Attach files to this conversation";
        e.DragUIOverride.IsCaptionVisible = true;
    }

    private async void Files_Drop(object sender, DragEventArgs e)
    {
        if (dialogOpen || !ViewModel.CanCompose || !e.DataView.Contains(StandardDataFormats.StorageItems)) return;
        e.Handled = true;
        e.AcceptedOperation = DataPackageOperation.Copy;
        var deferral = e.GetDeferral();
        var context = ViewModel.ContextVersion;
        IReadOnlyList<IStorageItem>? items = null;
        dialogOpen = true;
        try { items = await e.DataView.GetStorageItemsAsync(); }
        catch (Exception) { AccountStatus.Text = "Could not read the dropped files. Please try again."; }
        finally { dialogOpen = false; deferral.Complete(); }
        if (items is null || lifetime.IsCancellationRequested) return;
        if (context != ViewModel.ContextVersion)
        { AccountStatus.Text = "The conversation changed. Drop the files again to send them here."; return; }
        await AttachFilesAsync(() => Task.FromResult(items));
    }

    private async Task AttachFilesAsync(Func<Task<IReadOnlyList<IStorageItem>>> chooseFiles)
    {
        if (dialogOpen || !ViewModel.CanCompose || ViewModel.SelectedMailbox is not { } mailbox || ViewModel.SelectedConversation is not { } conversation) return;
        var context = ViewModel.ContextVersion;
        dialogOpen = true;
        try
        {
            var items = await chooseFiles();
            if (items.Count == 0) return;
            if (items.Any(item => item is not StorageFile))
            { AccountStatus.Text = "Choose files, not folders. No files were attached."; return; }
            var selected = new List<DraftAttachment>();
            foreach (var file in items.Cast<StorageFile>())
            {
                var size = (await file.GetBasicPropertiesAsync()).Size;
                if (size is 0 or > PostboxClient.MaxAttachmentBytes)
                { AccountStatus.Text = $"{file.Name}: choose a file between 1 byte and 25 MB. No files were attached."; return; }
                var id = string.IsNullOrEmpty(file.Path) ? Guid.NewGuid().ToString("N")
                    : Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(file.Path.ToUpperInvariant())));
                selected.Add(new DraftAttachment(id, file.Name, (long)size, token => ReadAttachmentAsync(file, token)));
            }
            if (context != ViewModel.ContextVersion || lifetime.IsCancellationRequested)
            { AccountStatus.Text = "The conversation changed. Select the files again to attach them here."; return; }
            ViewModel.AddAttachments(selected);
            AccountStatus.Text = ViewModel.DraftAttachments.Count == 1 ? "1 file attached." : $"{ViewModel.DraftAttachments.Count} files attached.";
            Composer.Focus(FocusState.Programmatic);
        }
        catch (OperationCanceledException) { }
        catch (Exception) { AccountStatus.Text = "Could not read the selected files. Please try again."; }
        finally { dialogOpen = false; }
    }

    private static async Task<byte[]> ReadAttachmentAsync(StorageFile file, CancellationToken token)
    {
        using var stream = await file.OpenStreamForReadAsync();
        if (stream.Length is 0 or > PostboxClient.MaxAttachmentBytes) throw new IOException("Attachment size changed.");
        using var buffer = new MemoryStream();
        var chunk = new byte[64 * 1024];
        int count;
        while ((count = await stream.ReadAsync(chunk, token)) > 0)
        {
            if (buffer.Length + count > PostboxClient.MaxAttachmentBytes) throw new IOException("File grew beyond attachment limit.");
            buffer.Write(chunk, 0, count);
        }
        return buffer.ToArray();
    }

    private void RemoveAttachment_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: DraftAttachment attachment }) ViewModel.RemoveAttachment(attachment);
    }

    private async void SaveAttachment_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: MessageAttachment attachment }) await SaveAttachmentsAsync([attachment]);
    }

    private async void SaveFiles_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { Tag: ThreadMessage message }) await SaveAttachmentsAsync(message.Attachments ?? []);
    }

    private async Task SaveAttachmentsAsync(IReadOnlyList<MessageAttachment> attachments)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        try
        {
            foreach (var attachment in attachments)
            {
                var safeName = Path.GetFileName(attachment.Filename.Replace('\\', '/'));
                foreach (var c in Path.GetInvalidFileNameChars()) safeName = safeName.Replace(c, '_');
                if (string.IsNullOrWhiteSpace(safeName)) safeName = "attachment";
#if UI_TESTS
                var folder = await StorageFolder.GetFolderFromPathAsync(Environment.GetEnvironmentVariable("PIGEONPOST_UI_DOWNLOAD_DIRECTORY")!);
                var destination = await folder.CreateFileAsync(safeName, CreationCollisionOption.ReplaceExisting);
#else
                var picker = new FileSavePicker { SuggestedFileName = safeName };
                var extension = Path.GetExtension(safeName);
                if (extension.Length is < 2 or > 20 || !extension[1..].All(char.IsAsciiLetterOrDigit)) extension = ".bin";
                picker.FileTypeChoices.Add("File", new List<string> { extension });
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
                var destination = await picker.PickSaveFileAsync();
#endif
                if (destination is null) break;
                AccountStatus.Text = $"Downloading {safeName}…";
                var bytes = await postbox.DownloadAsync(mailbox.Address, attachment.Id, lifetime.Token);
                await FileIO.WriteBytesAsync(destination, bytes);
                AccountStatus.Text = $"Saved {safeName}.";
            }
        }
        catch (OperationCanceledException) { }
        catch (Exception) { AccountStatus.Text = "Could not save the attachment. Please try again."; }
        finally { dialogOpen = false; }
    }
}
