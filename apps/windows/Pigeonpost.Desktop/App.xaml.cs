using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public partial class App : Application
{
    private MainWindow? window;
    public App() => InitializeComponent();
    protected override async void OnLaunched(LaunchActivatedEventArgs args)
    {
        window = new MainWindow();
        var dispatcher = window.DispatcherQueue;
        void HandleNotification(AppNotificationActivatedEventArgs activation) =>
            dispatcher.TryEnqueue(async () =>
            {
                var values = activation.Arguments;
                if (values.TryGetValue("mailbox", out var mailbox) && values.TryGetValue("peer", out var peer)
                    && values.TryGetValue("message", out var message))
                    await window.OpenNotificationAsync(new(mailbox, peer, values.TryGetValue("thread", out var thread) ? thread : null, message));
                else window.ShowInbox();
            });
        void HandleActivation(AppActivationArguments activation)
        {
            if (activation.Kind == ExtendedActivationKind.AppNotification
                && activation.Data is AppNotificationActivatedEventArgs notification)
                HandleNotification(notification);
            else dispatcher.TryEnqueue(() => window.ShowInbox());
        }
        AppNotificationManager.Default.NotificationInvoked += (_, activation) => HandleNotification(activation);
        try { AppNotificationManager.Default.Register(); }
        catch (Exception) { /* Elevated/test sessions may not support alerts; the inbox and badges still work. */ }
        var instance = AppInstance.FindOrRegisterForKey("pigeonpost-desktop");
        if (!instance.IsCurrent)
        {
            await instance.RedirectActivationToAsync(AppInstance.GetCurrent().GetActivatedEventArgs());
            Exit();
            return;
        }
        instance.Activated += (_, activation) => HandleActivation(activation);
        window.Closed += (_, _) =>
        {
            try { AppNotificationManager.Default.Unregister(); } catch (Exception) { }
            instance.UnregisterKey();
        };
        var initialActivation = AppInstance.GetCurrent().GetActivatedEventArgs();
        // COM launches deliver their target through NotificationInvoked. Wait for it
        // before activating, so the previously selected subject is not acknowledged.
        if (initialActivation.Kind == ExtendedActivationKind.AppNotification
            || !args.Arguments.Contains("----AppNotificationActivated:", StringComparison.Ordinal))
            HandleActivation(initialActivation);
    }
}
