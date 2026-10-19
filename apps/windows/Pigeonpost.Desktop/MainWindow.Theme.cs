using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private readonly AppPreferences preferences = new(AppPreferences.DefaultPath());

    private void InitializeTheme()
    {
        Root.ActualThemeChanged += (_, _) => UpdateTitleBar();
        ApplyTheme(preferences.LoadTheme());
    }

    // Root carries the choice; dialogs copy Root.ActualTheme when they open. System leaves
    // the window on ElementTheme.Default so it keeps following Windows live.
    private void ApplyTheme(AppTheme theme)
    {
        Root.RequestedTheme = theme switch
        {
            AppTheme.Light => ElementTheme.Light,
            AppTheme.Dark => ElementTheme.Dark,
            _ => ElementTheme.Default
        };
        ThemeSystemItem.IsChecked = theme == AppTheme.System;
        ThemeLightItem.IsChecked = theme == AppTheme.Light;
        ThemeDarkItem.IsChecked = theme == AppTheme.Dark;
        UpdateTitleBar();
    }

    private void Theme_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not RadioMenuFlyoutItem { Tag: string tag }) return;
        var theme = AppPreferences.ParseTheme(tag);
        ApplyTheme(theme);
        try { preferences.SaveTheme(theme); }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { AccountStatus.Text = "Could not save the theme preference."; }
    }

    // The standard caption (title, minimise/maximise/close) follows the window's theme.
    private void UpdateTitleBar()
    {
        if (!AppWindowTitleBar.IsCustomizationSupported()) return;
        var bar = AppWindow.TitleBar;
        if (new Windows.UI.ViewManagement.AccessibilitySettings().HighContrast)
        {
            bar.ResetToDefault();
            return;
        }
        var dark = Root.ActualTheme == ElementTheme.Dark;
        var background = dark ? ColorHelper.FromArgb(255, 0x0D, 0x0F, 0x12) : ColorHelper.FromArgb(255, 0xF4, 0xF5, 0xF7);
        var foreground = dark ? ColorHelper.FromArgb(255, 0xE8, 0xEA, 0xED) : ColorHelper.FromArgb(255, 0x15, 0x18, 0x1D);
        var muted = ColorHelper.FromArgb(255, 0x7D, 0x85, 0x90);
        bar.BackgroundColor = bar.InactiveBackgroundColor = background;
        bar.ButtonBackgroundColor = bar.ButtonInactiveBackgroundColor = background;
        bar.ForegroundColor = bar.ButtonForegroundColor = foreground;
        bar.InactiveForegroundColor = bar.ButtonInactiveForegroundColor = muted;
        bar.ButtonHoverBackgroundColor = dark ? ColorHelper.FromArgb(255, 0x1C, 0x21, 0x29) : ColorHelper.FromArgb(255, 0xE4, 0xE9, 0xF1);
        bar.ButtonPressedBackgroundColor = dark ? ColorHelper.FromArgb(255, 0x25, 0x2B, 0x34) : ColorHelper.FromArgb(255, 0xD8, 0xDF, 0xEA);
        bar.ButtonHoverForegroundColor = bar.ButtonPressedForegroundColor = foreground;
    }
}
