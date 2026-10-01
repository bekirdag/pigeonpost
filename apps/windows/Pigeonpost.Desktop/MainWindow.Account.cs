using System.Net.Http;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private readonly HttpClient http = new(new HttpClientHandler { AllowAutoRedirect = false }) { Timeout = TimeSpan.FromSeconds(35) };
    private readonly AccountSession session;
    private readonly PostboxClient postbox;
    private readonly CancellationTokenSource lifetime = new();
    private CancellationTokenSource? signInAttempt;
    private DispatcherTimer? refreshTimer;
    private DeviceSignIn? deviceSignIn;

    private async Task RestoreAccountAsync()
    {
        SetSigningIn(true, "Connecting your account…");
        try
        {
            if (await session.RestoreAsync(lifetime.Token)) await OpenInboxAsync();
            else AuthStatus.Text = "Sign in or create a free Pigeonpost account in your browser.";
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { AuthStatus.Text = FriendlyAccountError(ex); }
        finally { SetSigningIn(false); }
    }

    private async void SignIn_Click(object sender, RoutedEventArgs e)
    {
        if (signInAttempt is not null) return;
        using var attempt = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
        signInAttempt = attempt;
        SetSigningIn(true, "Getting your sign-in code…");
        try
        {
            deviceSignIn = await session.BeginSignInAsync(attempt.Token);
            CodeLabel.Text = deviceSignIn.UserCode;
            CodePanel.Visibility = Visibility.Visible;
            AuthStatus.Text = "Confirm this code and sign in in your browser. This window will connect automatically.";
            if (!await Launcher.LaunchUriAsync(deviceSignIn.VerificationUri))
                AuthStatus.Text = "Open auth.pigeonpost.dev/realms/pigeonpost-prod/device in your browser and enter this code.";
            await session.CompleteSignInAsync(deviceSignIn, attempt.Token);
            CodePanel.Visibility = Visibility.Collapsed;
            deviceSignIn = null;
            await OpenInboxAsync();
        }
        catch (OperationCanceledException) { AuthStatus.Text = "Sign-in cancelled. You can start again when ready."; }
        catch (Exception ex) { AuthStatus.Text = FriendlyAccountError(ex); }
        finally
        {
            deviceSignIn = null;
            CodePanel.Visibility = Visibility.Collapsed;
            signInAttempt = null;
            SetSigningIn(false);
        }
    }

    private async Task OpenInboxAsync()
    {
        Welcome.Visibility = Visibility.Collapsed;
        InboxLayout.Visibility = Visibility.Visible;
        AccountBar.Visibility = Visibility.Visible;
        await ViewModel.InitializeAsync();
        EmptyAccount.Visibility = ViewModel.Mailboxes.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        await ViewModel.AcknowledgeSelectedAsync();
        refreshTimer ??= CreateRefreshTimer();
        refreshTimer.Start();
    }

    private DispatcherTimer CreateRefreshTimer()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(10) };
        timer.Tick += async (_, _) =>
        {
            if (dialogOpen || lifetime.IsCancellationRequested || InboxLayout.Visibility != Visibility.Visible) return;
            await ViewModel.RefreshAsync(background: true);
        };
        return timer;
    }

    private void SetSigningIn(bool busy, string? message = null)
    {
        SignInButton.IsEnabled = !busy;
        SignInProgress.IsActive = busy;
        CancelSignIn.Visibility = signInAttempt is not null ? Visibility.Visible : Visibility.Collapsed;
        if (message is not null) AuthStatus.Text = message;
    }

    private void CancelSignIn_Click(object sender, RoutedEventArgs e) => signInAttempt?.Cancel();
    private async void OpenSignIn_Click(object sender, RoutedEventArgs e)
    {
        if (deviceSignIn is { } device) await OpenLinkAsync(device.VerificationUri);
    }
    private void CopyCode_Click(object sender, RoutedEventArgs e) { if (deviceSignIn is { } device) CopyText(device.UserCode); }
    private void CopyAddress_Click(object sender, RoutedEventArgs e) { if (ViewModel.SelectedMailbox is { } mailbox) CopyText(mailbox.Key); }
    private async void ManageAccount_Click(object sender, RoutedEventArgs e) => await OpenLinkAsync(new Uri("https://inbox.pigeonpost.dev"));
    private async void Privacy_Click(object sender, RoutedEventArgs e) => await OpenLinkAsync(new Uri("https://pigeonpost.dev/privacy"));
    private async void ReloadAccount_Click(object sender, RoutedEventArgs e) => await OpenInboxAsync();

    private async void CreateMailbox_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button) return;
        button.IsEnabled = false;
        try
        {
            await postbox.CreateMailboxAsync(lifetime.Token);
            await OpenInboxAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { AccountStatus.Text = ex is PostboxException ? ex.Message : FriendlyAccountError(ex); }
        finally { button.IsEnabled = true; }
    }

    private async void SignOut_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen) return;
        dialogOpen = true;
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = "Sign out of Pigeonpost?", Content = "Unsent drafts in this window will be discarded.",
                PrimaryButtonText = "Sign out", CloseButtonText = "Cancel"
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            refreshTimer?.Stop();
            await session.SignOutAsync(lifetime.Token);
            ViewModel.PropertyChanged -= ViewModel_PropertyChanged;
            ViewModel.Dispose();
            ViewModel = new InboxViewModel(postbox);
            ViewModel.PropertyChanged += ViewModel_PropertyChanged;
            Bindings.Update();
            InboxLayout.Visibility = AccountBar.Visibility = EmptyAccount.Visibility = Visibility.Collapsed;
            Welcome.Visibility = Visibility.Visible;
            AuthStatus.Text = "You’re signed out. Sign in to connect your account.";
        }
        catch (Exception ex)
        {
            AccountStatus.Text = FriendlyAccountError(ex);
            refreshTimer?.Start();
        }
        finally { dialogOpen = false; }
    }

    private async Task OpenLinkAsync(Uri uri)
    {
        try { if (!await Launcher.LaunchUriAsync(uri)) AccountStatus.Text = "Could not open your browser. Please try again."; }
        catch (Exception) { AccountStatus.Text = "Could not open your browser. Please try again."; }
    }
    private void CopyText(string text)
    {
        try
        {
            var data = new DataPackage();
            data.SetText(text);
            Clipboard.SetContent(data);
            AccountStatus.Text = "Copied to clipboard.";
        }
        catch (Exception) { AccountStatus.Text = "Could not copy to the clipboard. Please try again."; }
    }
    private static string FriendlyAccountError(Exception ex) => ex is SignInException ? ex.Message
        : ex is HttpRequestException or TaskCanceledException ? "Could not connect. Check your internet connection and try again."
        : "Could not access your saved account. Please try again.";
}
