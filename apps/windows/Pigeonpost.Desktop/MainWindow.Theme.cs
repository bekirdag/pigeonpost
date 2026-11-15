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
        ShowNotificationPreview(preferences.LoadNotificationPreview());
    }

    private void ShowNotificationPreview(NotificationPreview preview)
    {
        PreviewMessageItem.IsChecked = preview == NotificationPreview.NameAndMessage;
        PreviewNameItem.IsChecked = preview == NotificationPreview.NameOnly;
        PreviewHiddenItem.IsChecked = preview == NotificationPreview.Hidden;
    }

    // What a new-message toast may show. Windows can still hide toast content on the lock screen.
    private void NotificationPreview_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not RadioMenuFlyoutItem { Tag: string tag }) return;
        var preview = AppPreferences.ParseNotificationPreview(tag);
        ShowNotificationPreview(preview);
        try { preferences.SaveNotificationPreview(preview); }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { AccountStatus.Text = "Could not save the notification preference."; }
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
        // Design kit roles: --pp-bg, --pp-text, --pp-text-faint, --pp-surface-hover, --pp-surface-selected.
        var dark = Root.ActualTheme == ElementTheme.Dark;
        var background = dark ? ColorHelper.FromArgb(255, 0x0B, 0x0E, 0x12) : ColorHelper.FromArgb(255, 0xF4, 0xF6, 0xF8);
        var foreground = dark ? ColorHelper.FromArgb(255, 0xF2, 0xF4, 0xF7) : ColorHelper.FromArgb(255, 0x12, 0x16, 0x1C);
        var muted = dark ? ColorHelper.FromArgb(255, 0x8A, 0x93, 0xA0) : ColorHelper.FromArgb(255, 0x63, 0x6C, 0x78);
        bar.BackgroundColor = bar.InactiveBackgroundColor = background;
        bar.ButtonBackgroundColor = bar.ButtonInactiveBackgroundColor = background;
        bar.ForegroundColor = bar.ButtonForegroundColor = foreground;
        bar.InactiveForegroundColor = bar.ButtonInactiveForegroundColor = muted;
        bar.ButtonHoverBackgroundColor = dark ? ColorHelper.FromArgb(255, 0x1D, 0x23, 0x2D) : ColorHelper.FromArgb(255, 0xEE, 0xF1, 0xF5);
        bar.ButtonPressedBackgroundColor = dark ? ColorHelper.FromArgb(255, 0x1C, 0x28, 0x40) : ColorHelper.FromArgb(255, 0xE4, 0xEB, 0xF7);
        bar.ButtonHoverForegroundColor = bar.ButtonPressedForegroundColor = foreground;
    }

    // Kit composer: the box edge is Signal Blue while the editor has keyboard or pointer focus.
    private void Composer_FocusChanged(object sender, RoutedEventArgs e) =>
        ComposerFocusRing.Visibility = Composer.FocusState == FocusState.Unfocused ? Visibility.Collapsed : Visibility.Visible;
}
