using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private async void Contact_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox || ViewModel.SelectedConversation is not { } conversation) return;
        dialogOpen = true;
        try
        {
            var original = conversation.Contact ?? new Contact(conversation.Peer, null, "allow", "review", []);
            var alias = new TextBox { Header = "Display name", Text = original.Alias ?? "", MaxLength = 200 };
            var blocked = new CheckBox { Content = "Block this sender", IsChecked = original.Admission == "block" };
            var panel = new StackPanel { Spacing = 16 };
            panel.Children.Add(new TextBlock { Text = conversation.Peer, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(alias);
            panel.Children.Add(blocked);
            panel.Children.Add(new TextBlock { Text = "Blocking refuses future mail from this sender. Existing request permissions stay unchanged. Manage detailed permissions in the web inbox.", TextWrapping = TextWrapping.Wrap });
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, Title = "Contact settings", Content = panel, PrimaryButtonText = "Save", CloseButtonText = "Cancel" };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            await postbox.SetContactAsync(mailbox.Address, original with { Peer = conversation.Peer, Alias = alias.Text.Trim(), Admission = blocked.IsChecked == true ? "block" : "allow" }, lifetime.Token);
            if (ViewModel.SelectedMailbox?.Address == mailbox.Address) await ViewModel.RefreshAsync();
            AccountStatus.Text = "Contact updated.";
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { AccountStatus.Text = ex is PostboxException ? ex.Message : "Could not update this contact. Please try again."; }
        finally { dialogOpen = false; }
    }

    private async void Attach_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || !ViewModel.CanCompose || ViewModel.SelectedMailbox is not { } mailbox || ViewModel.SelectedConversation is not { } conversation) return;
        var thread = ViewModel.SelectedSubject?.Id;
        dialogOpen = true;
        try
        {
            var picker = new FileOpenPicker();
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
            var file = await picker.PickSingleFileAsync();
            if (file is null) return;
            var size = (await file.GetBasicPropertiesAsync()).Size;
            if (size is 0 or > PostboxClient.MaxAttachmentBytes) { AccountStatus.Text = "Choose a file between 1 byte and 25 MB."; return; }
            var caption = new TextBox { Header = "Message (optional)", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MaxLength = 20000, MinHeight = 80 };
            var panel = new StackPanel { Spacing = 12 };
            panel.Children.Add(new TextBlock { Text = $"Send {file.Name} to {conversation.Peer}", TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(caption);
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, Title = "Send a file", Content = panel, PrimaryButtonText = "Send file", CloseButtonText = "Cancel" };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            if (ViewModel.SelectedMailbox?.Address != mailbox.Address || ViewModel.SelectedConversation?.Peer != conversation.Peer || ViewModel.SelectedSubject?.Id != thread)
            { AccountStatus.Text = "The conversation changed. Select the file again to send it here."; return; }
            AccountStatus.Text = "Sending file…";
            using var stream = await file.OpenStreamForReadAsync();
            if (stream.Length > PostboxClient.MaxAttachmentBytes) throw new IOException("File grew beyond attachment limit.");
            using var buffer = new MemoryStream();
            var chunk = new byte[64 * 1024];
            int count;
            while ((count = await stream.ReadAsync(chunk, lifetime.Token)) > 0)
            {
                if (buffer.Length + count > PostboxClient.MaxAttachmentBytes) throw new IOException("File grew beyond attachment limit.");
                buffer.Write(chunk, 0, count);
            }
            var uploaded = await postbox.UploadAsync(mailbox.Address, file.Name, buffer.ToArray(), lifetime.Token);
            await postbox.SendAttachmentAsync(mailbox.Address, conversation.Peer, RequestEnvelope.Work(string.IsNullOrWhiteSpace(caption.Text) ? file.Name : caption.Text.Trim()), string.IsNullOrEmpty(thread) ? null : thread, uploaded.Id, lifetime.Token);
            if (ViewModel.SelectedMailbox?.Address == mailbox.Address) await ViewModel.RefreshAsync();
            AccountStatus.Text = "File sent.";
        }
        catch (OperationCanceledException) { }
        catch (Exception) { AccountStatus.Text = "File delivery could not be confirmed. Check this conversation before trying again."; }
        finally { dialogOpen = false; }
    }

    private async void SaveFiles_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || sender is not MenuFlyoutItem { Tag: ThreadMessage message } || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        try
        {
            foreach (var attachment in message.Attachments ?? [])
            {
                var safeName = Path.GetFileName(attachment.Filename.Replace('\\', '/'));
                foreach (var c in Path.GetInvalidFileNameChars()) safeName = safeName.Replace(c, '_');
                if (string.IsNullOrWhiteSpace(safeName)) safeName = "attachment";
                var picker = new FileSavePicker { SuggestedFileName = safeName };
                var extension = Path.GetExtension(safeName);
                if (extension.Length is < 2 or > 20 || !extension[1..].All(char.IsAsciiLetterOrDigit)) extension = ".bin";
                picker.FileTypeChoices.Add("File", new List<string> { extension });
                WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
                var destination = await picker.PickSaveFileAsync();
                if (destination is null) break;
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
