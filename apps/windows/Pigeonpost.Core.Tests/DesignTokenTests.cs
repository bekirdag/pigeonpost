using System.Xml.Linq;

// The desktop app's colours come from the Pigeonpost Design Kit: Themes/PPTokens.xaml is the kit's
// generated WinUI dictionary (tools/build_tokens.py), and App.xaml's own brushes must carry the same
// values in light and dark. These checks read the XAML files, so they run on any OS.
internal static class DesignTokenTests
{
    private static readonly XNamespace X = "http://schemas.microsoft.com/winfx/2006/xaml";
    private static readonly XNamespace Ui = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";

    // App brush -> kit role (PP<Role>Brush), with an optional opacity applied to the role's colour.
    private static readonly (string App, string Role, double Alpha)[] Map =
    [
        ("DialogSurfaceBrush", "SurfaceRaised", 1), ("DialogBorderBrush", "Border", 1),
        ("AppBackgroundBrush", "Bg", 1), ("NavigationSurfaceBrush", "BgSubtle", 1), ("ThreadsSurfaceBrush", "BgSubtle", 1),
        ("ConversationSurfaceBrush", "Bg", 1), ("HeaderSurfaceBrush", "Surface", 1), ("DividerBrush", "Border", 1),
        ("RowHoverBrush", "SurfaceHover", 1), ("RowSelectedBrush", "SurfaceSelected", 1),
        ("FieldSurfaceBrush", "Surface", 1), ("FieldBorderBrush", "BorderStrong", 1),
        ("IconButtonSurfaceBrush", "Surface", 1), ("IconButtonBorderBrush", "Border", 1),
        ("TextPrimaryBrush", "Text", 1), ("TextSecondaryBrush", "TextMuted", 1), ("TextTertiaryBrush", "TextFaint", 1),
        ("AccentBrush", "TextLink", 1), ("AccentFillBrush", "Accent", 1), ("OnAccentBrush", "OnAccent", 1),
        ("IncomingMessageBrush", "BubbleTheirs", 1), ("IncomingBorderBrush", "BubbleTheirsBorder", 1), ("IncomingTextBrush", "BubbleTheirsText", 1),
        ("IncomingFooterBorderBrush", "BorderSubtle", 1), ("IncomingFooterTextBrush", "TextFaint", 1),
        ("OutgoingMessageBrush", "BubbleMine", 1), ("OutgoingBorderBrush", "BubbleMineBorder", 1), ("OutgoingTextBrush", "BubbleMineText", 1),
        ("OutgoingFooterBorderBrush", "BubbleMineBorder", 1), ("OutgoingFooterTextBrush", "BubbleMineText", 0.62),
        ("TickSentBrush", "TickSent", 1), ("TickSeenBrush", "TickSeen", 1), ("TickWorkingBrush", "TickWorking", 1), ("TickFailedBrush", "Danger", 1),
        ("SuccessBrush", "Success", 1), ("DangerBrush", "Danger", 1), ("DangerFillBrush", "DangerFill", 1),
        ("ChipSurfaceBrush", "SurfaceHover", 1), ("ChipTextBrush", "TextBody", 1),
        ("AttachmentSurfaceBrush", "Surface", 1), ("AttachmentBorderBrush", "Border", 1),
        ("AvatarSurfaceBrush", "AccentSubtle", 1), ("AvatarTextBrush", "TextLink", 1),
        ("ReviewSurfaceBrush", "WarningFill", 1), ("ReviewTextBrush", "Warning", 1),
        ("ComposerSurfaceBrush", "Surface", 1), ("ComposerBorderBrush", "Border", 1), ("ComposerFocusBorderBrush", "Accent", 1),
        ("ComposerAreaBrush", "Bg", 1),
    ];

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Desktop brushes carry the design kit's light and dark values", () =>
        {
            var app = XDocument.Load(DesktopPath("App.xaml"));
            var kit = XDocument.Load(DesktopPath("Themes", "PPTokens.xaml"));
            foreach (var theme in new[] { "Light", "Default" })
            {
                var appBrushes = Brushes(app, theme);
                var kitBrushes = Brushes(kit, theme);
                foreach (var (key, role, alpha) in Map)
                {
                    Require(appBrushes.TryGetValue(key, out var actual), $"{theme}: App.xaml has no {key}.");
                    Require(kitBrushes.TryGetValue("PP" + role + "Brush", out var token), $"{theme}: the kit has no PP{role}Brush.");
                    var expected = alpha >= 1 ? token : $"#{(int)Math.Round(alpha * 255):X2}{token![^6..]}";
                    Require(string.Equals(actual, expected, StringComparison.OrdinalIgnoreCase), $"{theme}: {key} is {actual}, the kit's {role} is {expected}.");
                }
            }
            return Task.CompletedTask;
        });
        yield return ("Every app brush has a high-contrast form and the accent is Signal Blue", () =>
        {
            var app = XDocument.Load(DesktopPath("App.xaml"));
            var light = Brushes(app, "Light").Keys.ToHashSet();
            var contrast = Brushes(app, "HighContrast").Keys.ToHashSet();
            Require(light.SetEquals(Brushes(app, "Default").Keys), "Light and dark define different brushes.");
            Require(light.SetEquals(contrast), "High contrast misses: " + string.Join(", ", light.Except(contrast)));
            var accent = app.Descendants(Ui + "Color").Single(c => (string?)c.Attribute(X + "Key") == "SystemAccentColor");
            Require(accent.Value.Trim().Equals("#086CFF", StringComparison.OrdinalIgnoreCase), "SystemAccentColor is not Signal Blue.");
            var merged = app.Descendants(Ui + "ResourceDictionary").Select(d => (string?)d.Attribute("Source"));
            Require(merged.Contains("ms-appx:///Themes/PPTokens.xaml"), "App.xaml does not merge the kit tokens.");
            return Task.CompletedTask;
        });
    }

    // The SolidColorBrush values of one theme dictionary (Light, Default = dark, HighContrast).
    private static Dictionary<string, string> Brushes(XDocument document, string theme)
    {
        var dictionary = document.Descendants(Ui + "ResourceDictionary").Single(d => (string?)d.Attribute(X + "Key") == theme);
        return dictionary.Elements(Ui + "SolidColorBrush").ToDictionary(b => (string)b.Attribute(X + "Key")!, b => (string)b.Attribute("Color")!);
    }

    private static string DesktopPath(string first, string? second = null, [System.Runtime.CompilerServices.CallerFilePath] string here = "")
    {
        var root = Path.Combine(Path.GetDirectoryName(here)!, "..", "Pigeonpost.Desktop", first);
        return second is null ? root : Path.Combine(root, second);
    }

    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }
}
