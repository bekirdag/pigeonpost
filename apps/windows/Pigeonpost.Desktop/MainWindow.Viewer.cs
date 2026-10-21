using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Pigeonpost.Core;
using Windows.Data.Pdf;
using Windows.Media.Core;
using Windows.Storage;
using Windows.Storage.Pickers;
using Windows.Storage.Streams;

namespace Pigeonpost.Desktop;

// Received files open inside the app: pictures, video, audio, PDF and plain text, with a Save
// button. Markup (HTML, SVG, XML) and every other type is only ever saved, never rendered.
public sealed partial class MainWindow
{
    private MessageAttachment? viewerAttachment;
    private byte[]? viewerBytes;
    private CancellationTokenSource? viewerCancellation;
    private MediaPlayerElement? viewerMedia;
    private MediaSource? viewerSource;
    private bool viewerOpen;

    private async void OpenAttachment_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: MessageAttachment attachment }) return;
        if (attachment.CanOpen) await OpenViewerAsync(attachment);
        else await SaveAttachmentsAsync([attachment]);
    }

    // `load` supplies the bytes when they don't come from the mailbox's attachments (a group
    // file, decrypted on this device).
    private async Task OpenViewerAsync(MessageAttachment attachment, Func<CancellationToken, Task<byte[]>>? load = null)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox) return;
        var kind = AttachmentViewer.Kind(attachment.Filename, attachment.MediaType);
        var mime = AttachmentViewer.MediaType(attachment.Filename, attachment.MediaType);
        if (kind == AttachmentViewKind.None || mime is null) return;
        CloseViewer();
        dialogOpen = viewerOpen = true; // The viewer is modal: no refresh-driven changes underneath.
        viewerAttachment = attachment;
        var cancellation = viewerCancellation = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
        ViewerTitle.Text = attachment.Filename;
        ViewerDetail.Text = attachment.SizeLabel;
        ViewerSave.IsEnabled = false;
        ViewerContent.Children.Clear();
        ViewerContent.Children.Add(new ProgressRing { IsActive = true, Width = 36, Height = 36 });
        ViewerOverlay.Visibility = Visibility.Visible;
        ViewerClose.Focus(FocusState.Programmatic);
        try
        {
            var bytes = load is not null ? await load(cancellation.Token) : await postbox.DownloadAsync(mailbox.Address, attachment.Id, cancellation.Token);
            cancellation.Token.ThrowIfCancellationRequested();
            if (bytes.Length is 0 or > PostboxClient.MaxAttachmentBytes) throw new IOException("Attachment size limit.");
            viewerBytes = bytes;
            ViewerSave.IsEnabled = true;
            var content = await ViewerContentAsync(kind, mime, attachment.Filename, bytes, cancellation.Token);
            cancellation.Token.ThrowIfCancellationRequested();
            ViewerContent.Children.Clear();
            ViewerContent.Children.Add(content);
        }
        catch (OperationCanceledException) { }
        catch (Exception ex)
        {
            if (cancellation.IsCancellationRequested) return;
            DesktopLog.Error("Attachment viewer", ex);
            ViewerContent.Children.Clear();
            ViewerContent.Children.Add(ViewerMessage(viewerBytes is null
                ? "Could not download this file. Please try again."
                : "This file can't be shown here. Save it to open it with another app."));
        }
    }

    private async Task<FrameworkElement> ViewerContentAsync(AttachmentViewKind kind, string mime, string filename, byte[] bytes, CancellationToken token)
    {
        switch (kind)
        {
            case AttachmentViewKind.Image:
            {
                var image = new Image { Stretch = Stretch.Uniform, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
                AutomationProperties.SetName(image, "Picture " + filename);
                var bitmap = new BitmapImage();
                using (var stream = await Stream(bytes)) await bitmap.SetSourceAsync(stream);
                image.Source = bitmap;
                var scroller = Zoomable(image);
                // Fit the window first; the person can zoom in (Ctrl + wheel, pinch) from there.
                void Fit()
                {
                    if (bitmap.PixelWidth <= 0 || bitmap.PixelHeight <= 0 || scroller.ActualWidth <= 0 || scroller.ActualHeight <= 0) return;
                    var scale = Math.Min(1, Math.Min(scroller.ActualWidth / bitmap.PixelWidth, scroller.ActualHeight / bitmap.PixelHeight));
                    image.Width = bitmap.PixelWidth * scale;
                    image.Height = bitmap.PixelHeight * scale;
                }
                scroller.SizeChanged += (_, _) => Fit();
                return scroller;
            }
            case AttachmentViewKind.Video:
            case AttachmentViewKind.Audio:
            {
                viewerSource = MediaSource.CreateFromStream(await Stream(bytes), mime);
                var player = viewerMedia = new MediaPlayerElement
                {
                    Source = viewerSource, AreTransportControlsEnabled = true, AutoPlay = true,
                    HorizontalAlignment = HorizontalAlignment.Stretch, VerticalAlignment = VerticalAlignment.Stretch
                };
                AutomationProperties.SetName(player, (kind == AttachmentViewKind.Video ? "Video " : "Audio ") + filename);
                if (kind == AttachmentViewKind.Video) return player;
                player.MaxWidth = 640;
                player.Height = 120;
                player.VerticalAlignment = VerticalAlignment.Center;
                var panel = new StackPanel { Spacing = 24, VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Stretch, MaxWidth = 640 };
                panel.Children.Add(new FontIcon { Glyph = "", FontSize = 64, Foreground = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(255, 0x9A, 0xA1, 0xAB)) });
                panel.Children.Add(player);
                return panel;
            }
            case AttachmentViewKind.Pdf:
            {
                using var stream = await Stream(bytes);
                var document = await PdfDocument.LoadFromStreamAsync(stream);
                var pages = new StackPanel { Spacing = 16, HorizontalAlignment = HorizontalAlignment.Center };
                AutomationProperties.SetName(pages, "Document " + filename);
                var count = (int)Math.Min(document.PageCount, 60u);
                for (uint index = 0; index < count; index++)
                {
                    token.ThrowIfCancellationRequested();
                    using var page = document.GetPage(index);
                    using var output = new InMemoryRandomAccessStream();
                    // Sharp at 1.5x on a typical page width; capped for very large pages.
                    var width = Math.Min(1400, Math.Max(1, page.Size.Width * 1.5));
                    await page.RenderToStreamAsync(output, new PdfPageRenderOptions { DestinationWidth = (uint)width });
                    output.Seek(0);
                    var bitmap = new BitmapImage();
                    await bitmap.SetSourceAsync(output);
                    var image = new Image { Source = bitmap, Width = Math.Min(900, page.Size.Width * 1.25), Stretch = Stretch.Uniform };
                    AutomationProperties.SetName(image, $"Page {index + 1} of {document.PageCount}");
                    pages.Children.Add(new Border { Child = image, Background = new SolidColorBrush(Microsoft.UI.Colors.White), CornerRadius = new CornerRadius(4) });
                }
                if (document.PageCount > count)
                    pages.Children.Add(ViewerMessage($"Showing the first {count} of {document.PageCount} pages. Save the file to read the rest."));
                return Zoomable(pages);
            }
            default:
            {
                var text = AttachmentViewer.Text(bytes, out var truncated);
                var block = new TextBlock
                {
                    Text = truncated ? text + "\n\n… (the rest of this file is not shown; save it to read everything)" : text,
                    FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], FontSize = 13, LineHeight = 20, TextWrapping = TextWrapping.Wrap,
                    IsTextSelectionEnabled = true, Foreground = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(255, 0xE8, 0xEA, 0xED)),
                    MaxWidth = 960, HorizontalAlignment = HorizontalAlignment.Left
                };
                AutomationProperties.SetName(block, "Contents of " + filename);
                return new Border
                {
                    Background = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(255, 0x16, 0x19, 0x1E)), CornerRadius = new CornerRadius(10),
                    Padding = new Thickness(20, 16, 20, 16),
                    Child = new ScrollViewer { Content = block, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }
                };
            }
        }
    }

    private static ScrollViewer Zoomable(UIElement content) => new()
    {
        Content = content, ZoomMode = ZoomMode.Enabled, MinZoomFactor = 0.25f, MaxZoomFactor = 8,
        HorizontalScrollBarVisibility = ScrollBarVisibility.Auto, VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        HorizontalScrollMode = ScrollMode.Auto, VerticalScrollMode = ScrollMode.Auto
    };

    private static TextBlock ViewerMessage(string text) => new()
    {
        Text = text, TextWrapping = TextWrapping.Wrap, FontSize = 14, HorizontalAlignment = HorizontalAlignment.Center,
        VerticalAlignment = VerticalAlignment.Center, TextAlignment = TextAlignment.Center, MaxWidth = 520,
        Foreground = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(255, 0xC9, 0xCE, 0xD6))
    };

    private static async Task<IRandomAccessStream> Stream(byte[] bytes)
    {
        var stream = new InMemoryRandomAccessStream();
        using (var writer = new DataWriter(stream.GetOutputStreamAt(0))) { writer.WriteBytes(bytes); await writer.StoreAsync(); await writer.FlushAsync(); writer.DetachStream(); }
        stream.Seek(0);
        return stream;
    }

    private async Task SaveViewerAsync()
    {
        if (!viewerOpen || viewerAttachment is not { } attachment || viewerBytes is not { } bytes) return;
        var name = AttachmentViewer.SafeFileName(attachment.Filename);
        try
        {
            var destination = await SaveDestinationAsync(name, AttachmentViewer.Kind(attachment.Filename, attachment.MediaType) == AttachmentViewKind.Image);
            if (destination is null || !ReferenceEquals(attachment, viewerAttachment)) return;
            await FileIO.WriteBytesAsync(destination, bytes);
            ViewerDetail.Text = $"{attachment.SizeLabel} · Saved as {destination.Name}";
            AccountStatus.Text = $"Saved {name}.";
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            DesktopLog.Error("Attachment save", ex);
            ViewerDetail.Text = "Could not save the file. Please try again.";
        }
    }

    // The Save As dialog (Pictures for images, Downloads for everything else).
    private async Task<StorageFile?> SaveDestinationAsync(string name, bool picture)
    {
#if UI_TESTS
        _ = picture;
        var folder = await StorageFolder.GetFolderFromPathAsync(Environment.GetEnvironmentVariable("PIGEONPOST_UI_DOWNLOAD_DIRECTORY")!);
        return await folder.CreateFileAsync(name, CreationCollisionOption.ReplaceExisting);
#else
        var picker = new FileSavePicker
        {
            SuggestedFileName = name,
            SuggestedStartLocation = picture ? PickerLocationId.PicturesLibrary : PickerLocationId.Downloads
        };
        var extension = Path.GetExtension(name);
        if (extension.Length is < 2 or > 20 || !extension[1..].All(char.IsAsciiLetterOrDigit)) extension = ".bin";
        picker.FileTypeChoices.Add("File", new List<string> { extension });
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        return await picker.PickSaveFileAsync();
#endif
    }

    private void CloseViewer()
    {
        if (!viewerOpen) return;
        viewerCancellation?.Cancel();
        viewerCancellation?.Dispose();
        viewerCancellation = null;
        if (viewerMedia is { } media)
        {
            try { media.MediaPlayer?.Pause(); } catch (Exception) { }
            media.Source = null;
            viewerMedia = null;
        }
        viewerSource?.Dispose();
        viewerSource = null;
        ViewerContent.Children.Clear();
        ViewerOverlay.Visibility = Visibility.Collapsed;
        viewerAttachment = null;
        viewerBytes = null;
        viewerOpen = false;
        dialogOpen = false;
        if (MessagingArea.Visibility == Visibility.Visible && ViewModel.CanCompose) Composer.Focus(FocusState.Programmatic);
    }

    private void ViewerClose_Click(object sender, RoutedEventArgs e) => CloseViewer();
    private async void ViewerSave_Click(object sender, RoutedEventArgs e) => await SaveViewerAsync();
    private void ViewerClose_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { args.Handled = true; CloseViewer(); }
    private async void ViewerSave_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { args.Handled = true; await SaveViewerAsync(); }

#if UI_TESTS
    // Startup self-check of every viewer kind against the shared fixtures (UI-test builds only).
    private async Task CheckViewerContentAsync()
    {
        var wav = new byte[44 + 8000];
        System.Text.Encoding.ASCII.GetBytes("RIFF").CopyTo(wav, 0); BitConverter.GetBytes(wav.Length - 8).CopyTo(wav, 4);
        System.Text.Encoding.ASCII.GetBytes("WAVEfmt ").CopyTo(wav, 8); BitConverter.GetBytes(16).CopyTo(wav, 16);
        BitConverter.GetBytes((short)1).CopyTo(wav, 20); BitConverter.GetBytes((short)1).CopyTo(wav, 22);
        BitConverter.GetBytes(8000).CopyTo(wav, 24); BitConverter.GetBytes(8000).CopyTo(wav, 28);
        BitConverter.GetBytes((short)1).CopyTo(wav, 32); BitConverter.GetBytes((short)8).CopyTo(wav, 34);
        System.Text.Encoding.ASCII.GetBytes("data").CopyTo(wav, 36); BitConverter.GetBytes(8000).CopyTo(wav, 40);
        var fixtures = new List<(string Name, byte[] Bytes)> { ("tone.wav", wav) };
        foreach (var name in new[] { "preview.png", "preview.pdf", "preview.mp4", "preview.txt" })
            fixtures.Add((name, await File.ReadAllBytesAsync(Path.Combine(AppContext.BaseDirectory, "PreviewFixtures", name))));
        foreach (var (name, bytes) in fixtures)
        {
            var kind = AttachmentViewer.Kind(name, "");
            var element = await ViewerContentAsync(kind, AttachmentViewer.MediaType(name, "")!, name, bytes, CancellationToken.None);
            if (element is null) throw new InvalidOperationException("Viewer produced nothing for " + name);
            if (kind == AttachmentViewKind.Pdf && element is ScrollViewer { Content: StackPanel { Children.Count: 0 } })
                throw new InvalidOperationException("Viewer rendered no PDF pages.");
            viewerSource?.Dispose(); viewerSource = null; viewerMedia = null;
        }
        if (AttachmentViewer.Kind("page.html", "text/html") != AttachmentViewKind.None || AttachmentViewer.Kind("logo.svg", "image/svg+xml") != AttachmentViewKind.None)
            throw new InvalidOperationException("The viewer must never render markup.");
        DesktopLog.Write("Viewer self-check passed");
    }
#endif
}
