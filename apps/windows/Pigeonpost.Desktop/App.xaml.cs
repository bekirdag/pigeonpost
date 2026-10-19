using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public partial class App : Application
{
    private MainWindow? window;
    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) => DesktopLog.Error("Unhandled UI exception", e.Exception);
        AppDomain.CurrentDomain.UnhandledException += (_, e) => { if (e.ExceptionObject is Exception ex) DesktopLog.Error("Unhandled exception", ex); };
        TaskScheduler.UnobservedTaskException += (_, e) => DesktopLog.Error("Unobserved task exception", e.Exception);
    }

    protected override async void OnLaunched(LaunchActivatedEventArgs args)
    {
        DesktopLog.Write($"Launch {typeof(App).Assembly.GetName().Version} as {DesktopLog.PackageIdentity()}");
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
        try
        {
            AppNotificationManager.Default.Register();
            DesktopLog.Write($"Windows notifications registered; setting {AppNotificationManager.Default.Setting}");
        }
        catch (Exception ex)
        {
            // Elevated/test sessions may not support alerts; the inbox and badges still work.
            DesktopLog.Error("Windows notification registration failed", ex);
        }
        var instance = AppInstance.FindOrRegisterForKey("pigeonpost-desktop");
        if (!instance.IsCurrent)
        {
            DesktopLog.Write("Another instance is running; redirecting this activation to it");
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
        var startedAtSignIn = initialActivation.Kind == ExtendedActivationKind.StartupTask;
#if UI_TESTS
        startedAtSignIn |= Environment.GetEnvironmentVariable("PIGEONPOST_UI_STARTUP") == "1";
#endif
        if (startedAtSignIn) window.StartInNotificationArea(); // No window at sign-in; alerts keep running.
        else if (initialActivation.Kind == ExtendedActivationKind.AppNotification
            || !args.Arguments.Contains("----AppNotificationActivated:", StringComparison.Ordinal))
            HandleActivation(initialActivation);
    }
}
