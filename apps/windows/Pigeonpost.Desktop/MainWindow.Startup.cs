using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel;

namespace Pigeonpost.Desktop;

// Opt-in start at sign-in through the package's StartupTask (Package.appxmanifest). Windows owns
// the state: Task Manager and policy can change it, so the menu re-reads it every time it opens.
public sealed partial class MainWindow
{
    private const string StartupTaskId = "PigeonpostStartup";
    private static bool Packaged => DesktopLog.PackageIdentity() != "unpackaged";

    private void AccountMenu_Opening(object? sender, object e) => _ = RefreshStartupToggleAsync(log: false);

    private async Task RefreshStartupToggleAsync(bool log = true)
    {
        if (!Packaged)
        {
            // ZIP and test builds have no package, so Windows cannot start them at sign-in.
            StartupItem.IsEnabled = false;
            StartupItem.IsChecked = false;
            ToolTipService.SetToolTip(StartupItem, "Available in the Microsoft Store version of Pigeonpost.");
            if (log) DesktopLog.Write("Start at sign-in unavailable: not installed as a package");
            return;
        }
        try
        {
            if (log) DesktopLog.Write("Reading the sign-in startup setting");
            var state = await Task.Run(async () => (await StartupTask.GetAsync(StartupTaskId)).State);
            ApplyStartupState(state);
            if (log) DesktopLog.Write($"Start at sign-in: {state}");
        }
        catch (Exception ex)
        {
            StartupItem.IsEnabled = false;
            DesktopLog.Error("Reading the sign-in startup setting failed", ex);
        }
    }

    private void ApplyStartupState(StartupTaskState state)
    {
        StartupItem.IsChecked = state is StartupTaskState.Enabled or StartupTaskState.EnabledByPolicy;
        // Policy decides both ways; a person's Task Manager choice stays clickable so we can explain it.
        StartupItem.IsEnabled = state is not (StartupTaskState.DisabledByPolicy or StartupTaskState.EnabledByPolicy);
        ToolTipService.SetToolTip(StartupItem, state switch
        {
            StartupTaskState.DisabledByUser => "Turned off in Task Manager > Startup apps.",
            StartupTaskState.DisabledByPolicy => "Turned off by your organization.",
            StartupTaskState.EnabledByPolicy => "Turned on by your organization.",
            _ => null
        });
    }

    private async void Startup_Click(object sender, RoutedEventArgs e)
    {
        if (!Packaged) return;
        try
        {
            // StartupTask ended the packaged process when called on the UI thread (CI evidence), so
            // every call runs on a worker thread and only the result returns to the menu.
            var state = await Task.Run(async () =>
            {
                var task = await StartupTask.GetAsync(StartupTaskId);
                if (task.State == StartupTaskState.Disabled) return await task.RequestEnableAsync();
                if (task.State == StartupTaskState.Enabled) task.Disable();
                return task.State;
            });
            ApplyStartupState(state);
            AccountStatus.Text = state switch
            {
                StartupTaskState.Enabled => "Pigeonpost will start in the notification area when you sign in.",
                StartupTaskState.Disabled => "Pigeonpost won't start when you sign in.",
                StartupTaskState.DisabledByUser => "Starting at sign-in is turned off in Task Manager. Turn Pigeonpost on under Task Manager > Startup apps.",
                StartupTaskState.DisabledByPolicy => "Your organization doesn't allow Pigeonpost to start when you sign in.",
                StartupTaskState.EnabledByPolicy => "Your organization starts Pigeonpost when you sign in.",
                _ => AccountStatus.Text
            };
            DesktopLog.Write($"Start at sign-in changed to {state}");
        }
        catch (Exception ex)
        {
            DesktopLog.Error("Changing the sign-in startup setting failed", ex);
            AccountStatus.Text = "Couldn't change the sign-in setting. Please try again.";
            await RefreshStartupToggleAsync(log: false);
        }
    }
}
