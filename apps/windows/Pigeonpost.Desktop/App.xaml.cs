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
        AppNotificationManager.Default.NotificationInvoked += (_, activation) =>
            dispatcher.TryEnqueue(async () =>
            {
                var values = activation.Arguments;
                if (values.TryGetValue("mailbox", out var mailbox) && values.TryGetValue("peer", out var peer)
                    && values.TryGetValue("message", out var message))
                    await window.OpenNotificationAsync(new(mailbox, peer, values.TryGetValue("thread", out var thread) ? thread : null, message));
                else window.ShowInbox();
            });
        try { AppNotificationManager.Default.Register(); }
        catch (Exception) { /* Elevated/test sessions may not support alerts; the inbox and badges still work. */ }
        var instance = AppInstance.FindOrRegisterForKey("pigeonpost-desktop");
        if (!instance.IsCurrent)
        {
            await instance.RedirectActivationToAsync(AppInstance.GetCurrent().GetActivatedEventArgs());
            Exit();
            return;
        }
        instance.Activated += (_, _) => dispatcher.TryEnqueue(() => window.ShowInbox());
        window.Closed += (_, _) =>
        {
            try { AppNotificationManager.Default.Unregister(); } catch (Exception) { }
            instance.UnregisterKey();
        };
        window.Activate();
    }
}
