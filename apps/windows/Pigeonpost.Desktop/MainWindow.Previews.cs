using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Pigeonpost.Core;
using Windows.Data.Pdf;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Storage.FileProperties;
using Windows.Storage.Streams;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private sealed record FilePreview(ImageSource? Image = null, string? Text = null);
    private readonly Dictionary<Border, CancellationTokenSource> previewTasks = [];
    private readonly Dictionary<string, FilePreview> previewCache = [];
    private readonly Queue<string> previewOrder = [];
    private readonly SemaphoreSlim previewSlots = new(2);

#if UI_TESTS
    private static async Task CheckNativePreviewsAsync()
    {
        foreach (var (name, mime) in new[] { ("preview.png", "image/png"), ("preview.pdf", "application/pdf"), ("preview.mp4", "video/mp4") })
        {
            var data = await File.ReadAllBytesAsync(Path.Combine(AppContext.BaseDirectory, "PreviewFixtures", name));
            var result = await RenderFilePreviewAsync(data, name, mime, CancellationToken.None);
            if (result?.Image is not BitmapImage { PixelWidth: > 0, PixelHeight: > 0 })
                throw new InvalidOperationException("Native preview did not render " + name);
        }
        if (PreviewType("photo.png", "text/html") is not null || PreviewType("photo.svg", "image/svg+xml") is not null
            || PreviewType("photo.png", "application/octet-stream") != "image/png")
            throw new InvalidOperationException("Preview MIME boundaries failed.");
    }
#endif

    private static string? PreviewType(string filename, string mediaType)
    {
        var mime = mediaType.Split(';')[0].Trim().ToLowerInvariant();
        if (mime is "" or "application/octet-stream") mime = Path.GetExtension(filename).ToLowerInvariant() switch
        {
            ".png" => "image/png", ".jpg" or ".jpeg" => "image/jpeg", ".gif" => "image/gif",
            ".webp" => "image/webp", ".bmp" => "image/bmp", ".heic" => "image/heic",
            ".mp4" or ".m4v" => "video/mp4", ".mov" => "video/quicktime", ".webm" => "video/webm",
            ".mp3" => "audio/mpeg", ".m4a" => "audio/mp4", ".wav" => "audio/wav",
            ".pdf" => "application/pdf", ".txt" or ".md" or ".csv" or ".json" => "text/plain", _ => ""
        };
        return mime switch
        {
            "image/png" or "image/jpeg" or "image/gif" or "image/webp" or "image/bmp" or "image/heic" => mime,
            "video/mp4" or "video/quicktime" or "video/webm" or "audio/mpeg" or "audio/mp4" or "audio/wav" => mime,
            "application/pdf" or "text/plain" or "text/csv" or "text/markdown" or "application/json" => mime, _ => null
        };
    }

    private async void AttachmentPreview_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not Border border || ViewModel.SelectedMailbox is not { } mailbox) return;
        AttachmentPreview_Unloaded(border, e);
        var draft = border.Tag as DraftAttachment;
        var file = border.Tag as MessageAttachment;
        var name = draft?.Filename ?? file?.Filename ?? "";
        var mime = PreviewType(name, file?.MediaType ?? "");
        var bytes = draft?.Bytes ?? file?.Bytes ?? 0;
        if (mime is null || bytes is <= 0 or > PostboxClient.MaxAttachmentBytes)
        { border.Visibility = Visibility.Collapsed; return; }
        border.Visibility = Visibility.Visible;
        border.Width = draft is null ? 220 : 72;
        border.Height = draft is null ? 150 : 54;
        AutomationProperties.SetName(border, "Preview of " + name);
        border.Child = PreviewLabel("Loading preview…");
        var cancellation = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
        previewTasks[border] = cancellation;
        var token = cancellation.Token;
        var context = ViewModel.ContextVersion;
        var key = mailbox.Address + ":" + (file?.Id ?? "draft:" + System.Runtime.CompilerServices.RuntimeHelpers.GetHashCode(draft!));
        try
        {
            await previewSlots.WaitAsync(token);
            FilePreview? result;
            try
            {
                if (!previewCache.TryGetValue(key, out result))
                {
                    var data = draft is not null ? await draft.ReadAsync(token) : await postbox.DownloadAsync(mailbox.Address, file!.Id, token);
                    token.ThrowIfCancellationRequested();
                    if (data.Length is 0 or > PostboxClient.MaxAttachmentBytes) throw new IOException("Preview size limit.");
                    result = await RenderFilePreviewAsync(data, name, mime, token);
                    token.ThrowIfCancellationRequested();
                    if (result is not null)
                    {
                        while (previewCache.Count >= 32 && previewOrder.TryDequeue(out var oldest)) previewCache.Remove(oldest);
                        previewCache[key] = result;
                        previewOrder.Enqueue(key);
                    }
                }
            }
            finally { previewSlots.Release(); }
            if (token.IsCancellationRequested || context != ViewModel.ContextVersion) return;
            border.Child = result?.Image is { } image
                ? new Image { Source = image, Stretch = Stretch.Uniform }
                : PreviewLabel(result?.Text ?? "Preview unavailable");
        }
        catch (OperationCanceledException) { }
        catch (Exception) { if (!token.IsCancellationRequested) border.Child = PreviewLabel("Preview unavailable"); }
    }

    private static TextBlock PreviewLabel(string text) => new()
    {
        Text = text, TextWrapping = TextWrapping.Wrap, MaxLines = 8, FontSize = 11,
        Margin = new Thickness(6), VerticalAlignment = VerticalAlignment.Center
    };

    private void AttachmentPreview_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is Border border && previewTasks.Remove(border, out var cancellation))
        {
            cancellation.Cancel(); cancellation.Dispose(); border.Child = null;
        }
    }

    private static async Task<FilePreview?> RenderFilePreviewAsync(byte[] bytes, string name, string mime, CancellationToken token)
    {
        if (mime.StartsWith("text/", StringComparison.Ordinal) || mime == "application/json")
            return new FilePreview(Text: System.Text.Encoding.UTF8.GetString(bytes, 0, Math.Min(bytes.Length, 1200)));
        if (mime.StartsWith("image/", StringComparison.Ordinal))
        {
            using var input = new InMemoryRandomAccessStream();
            using (var writer = new DataWriter(input.GetOutputStreamAt(0))) { writer.WriteBytes(bytes); await writer.StoreAsync(); }
            input.Seek(0);
            var decoder = await BitmapDecoder.CreateAsync(input);
            if ((ulong)decoder.PixelWidth * decoder.PixelHeight > 64_000_000) throw new IOException("Preview dimensions too large.");
            var scale = Math.Min(1, Math.Min(440.0 / decoder.OrientedPixelWidth, 300.0 / decoder.OrientedPixelHeight));
            var image = new BitmapImage { DecodePixelWidth = Math.Max(1, (int)(decoder.OrientedPixelWidth * scale)),
                DecodePixelHeight = Math.Max(1, (int)(decoder.OrientedPixelHeight * scale)) };
            input.Seek(0); await image.SetSourceAsync(input);
            return new FilePreview(Image: image);
        }
        var folder = Path.Combine(Path.GetTempPath(), "Pigeonpost-preview-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(folder);
        try
        {
            var extension = mime switch { "application/pdf" => ".pdf", "video/mp4" => ".mp4", "video/quicktime" => ".mov",
                "video/webm" => ".webm", "audio/mpeg" => ".mp3", "audio/mp4" => ".m4a", "audio/wav" => ".wav", _ => ".bin" };
            var path = Path.Combine(folder, "preview" + extension);
            await File.WriteAllBytesAsync(path, bytes, token);
            var file = await StorageFile.GetFileFromPathAsync(path);
            if (mime == "application/pdf")
            {
                var pdf = await PdfDocument.LoadFromFileAsync(file);
                if (pdf.PageCount == 0) return null;
                using var page = pdf.GetPage(0);
                using var output = new InMemoryRandomAccessStream();
                var scale = Math.Min(440 / page.Size.Width, 300 / page.Size.Height);
                await page.RenderToStreamAsync(output, new PdfPageRenderOptions {
                    DestinationWidth = (uint)Math.Max(1, page.Size.Width * scale), DestinationHeight = (uint)Math.Max(1, page.Size.Height * scale) });
                output.Seek(0);
                var image = new BitmapImage(); await image.SetSourceAsync(output);
                return new FilePreview(Image: image);
            }
            using var thumbnail = await file.GetThumbnailAsync(ThumbnailMode.SingleItem, 440, ThumbnailOptions.ResizeThumbnail);
            if (thumbnail is { Type: ThumbnailType.Image })
            {
                var image = new BitmapImage { DecodePixelWidth = 440 }; await image.SetSourceAsync(thumbnail);
                return new FilePreview(Image: image);
            }
            if (mime.StartsWith("audio/", StringComparison.Ordinal)) return new FilePreview(Text: "♫ " + name);
            return null;
        }
        finally { try { Directory.Delete(folder, recursive: true); } catch (IOException) { } }
    }
}
