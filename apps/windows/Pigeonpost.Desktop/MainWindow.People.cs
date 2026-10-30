using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private readonly LocalAliasFile localNames = new(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Pigeonpost", "local-names"), new DpapiProtector());
    private sealed class PeopleBinding(NewConversationTarget target, string owner)
    {
        internal readonly NewConversationTarget Target = target;
        internal readonly string Owner = owner;
        internal Dictionary<string, string> Aliases = new(StringComparer.Ordinal);
        internal readonly Dictionary<string, PeerProfile?> Profiles = new(StringComparer.Ordinal);
        internal DateTimeOffset Refreshed;
        internal bool Loaded, Busy;
    }
    private PeopleBinding? peopleBinding;
    private bool PeopleCurrent(PeopleBinding binding) => ReferenceEquals(peopleBinding, binding) && binding.Target.IsCurrent
        && session.RecoveryOwnerKey == binding.Owner && features.Has(ServerFeatures.DisplayNames) && !lifetime.IsCancellationRequested;
    private PeopleBinding? PeopleScope()
    {
        if (!features.Has(ServerFeatures.DisplayNames) || session.RecoveryOwnerKey is not { } owner) return null;
        if (peopleBinding is { } current && PeopleCurrent(current)) return current;
        if (ViewModel.CaptureNewConversationTarget() is not { } target) return null;
        return peopleBinding = new(target, owner);
    }
    private string NamesOrigin => postbox.Endpoint.GetLeftPart(UriPartial.Authority);
    private async Task RefreshPeopleNamesAsync(bool force = false)
    {
        if (PeopleScope() is not { } binding || binding.Busy) return;
        var addresses = ViewModel.AllConversations.Select(c => c.Peer).Where(PostAddress.IsValid).Distinct().Take(300).ToArray();
        if (!force && DateTimeOffset.UtcNow - binding.Refreshed < TimeSpan.FromMinutes(2) && addresses.All(binding.Profiles.ContainsKey)) return;
        binding.Busy = true;
        try
        {
            if (!binding.Loaded)
            {
                binding.Aliases = await localNames.LoadAsync(binding.Owner, NamesOrigin, binding.Target.Mailbox, () => PeopleCurrent(binding));
                if (!PeopleCurrent(binding)) return;
                binding.Loaded = true;
            }
            // Remove former visibility grants before refreshing, including profiles which now return null.
            binding.Profiles.Clear();
            ApplyPeopleNames(binding);
            foreach (var batch in addresses.Chunk(100))
            {
                var profiles = await ViewModel.WithPeopleTargetAsync(binding.Target, postbox,
                    (mailbox, ct) => postbox.GetPeerProfilesAsync(mailbox, batch, ct));
                if (!PeopleCurrent(binding)) return;
                foreach (var key in batch) binding.Profiles[key] = profiles.GetValueOrDefault(key);
            }
            binding.Refreshed = DateTimeOffset.UtcNow;
            ApplyPeopleNames(binding);
        }
        catch (OperationCanceledException) { }
        catch (Exception) { if (PeopleCurrent(binding)) ApplyPeopleNames(binding); }
        finally { binding.Busy = false; }
    }
    private void ApplyPeopleNames(PeopleBinding binding)
    {
        if (!PeopleCurrent(binding)) return;
        var names = new Dictionary<string, PeerName>(StringComparer.Ordinal);
        foreach (var row in ViewModel.AllConversations)
        {
            var profile = binding.Profiles.GetValueOrDefault(row.Peer);
            var canonical = profile?.Address ?? (PeopleNames.Mailbox(row.Peer) ? row.Peer : null);
            names[row.Peer] = new(canonical is null ? null : binding.Aliases.GetValueOrDefault(canonical), profile?.DisplayName);
        }
        ViewModel.ApplyPeerNames(binding.Target, names);
    }
    private static TextBlock PeopleText(string text) => new() { Text = text, TextWrapping = TextWrapping.Wrap };
    private async void ProfileName_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || PeopleScope() is not { } binding) return;
        dialogOpen = true;
        try
        {
            var current = await ViewModel.WithPeopleTargetAsync(binding.Target, postbox, postbox.GetProfileNameAsync);
            if (!PeopleCurrent(binding)) return;
            var name = new TextBox { Header = "Your name", Text = current?.DisplayName ?? "", MaxLength = 160 };
            var visibility = new ComboBox { Header = "Who can see your name", HorizontalAlignment = HorizontalAlignment.Stretch };
            foreach (var (value, label) in new[] { ("known", "People you know"), ("everyone", "Everyone"), ("nobody", "Nobody") })
                visibility.Items.Add(new ComboBoxItem { Content = label, Tag = value });
            visibility.SelectedIndex = current?.Visibility switch { "everyone" => 1, "nobody" => 2, _ => 0 };
            var error = PeopleText("");
            var panel = new StackPanel { Spacing = 12, MaxWidth = 440 };
            panel.Children.Add(PeopleText("Your name and profile picture are free. A name does not reserve a handle. People see self-set names with ~; your address stays visible."));
            panel.Children.Add(PeopleText(binding.Target.Mailbox));
            panel.Children.Add(name); panel.Children.Add(visibility);
            panel.Children.Add(PeopleText("Leave the name empty and save to remove it.")); panel.Children.Add(error);
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, Title = "Your name", Content = panel,
                PrimaryButtonText = "Save", CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Close };
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                args.Cancel = true; var deferral = args.GetDeferral(); dialog.IsPrimaryButtonEnabled = false;
                try
                {
                    if (!PeopleCurrent(binding)) throw new AuthorizationRetiredException();
                    var input = name.Text.Trim(); var selected = (visibility.SelectedItem as ComboBoxItem)?.Tag as string ?? "known";
                    await ViewModel.WithPeopleTargetAsync(binding.Target, postbox, async (mailbox, ct) =>
                    {
                        if (input.Length == 0) await postbox.RemoveProfileNameAsync(mailbox, ct);
                        else await postbox.SetProfileNameAsync(mailbox, input, selected, ct);
                        return true;
                    });
                    if (!PeopleCurrent(binding)) throw new AuthorizationRetiredException();
                    dialog.Hide(); binding.Refreshed = default;
                    AccountStatus.Text = input.Length == 0 ? "Your name was removed." : "Your name was saved.";
                }
                catch (OperationCanceledException) { error.Text = "Your inbox changed. Close this window and try again."; }
                catch (Exception ex) { error.Text = ex is ArgumentException ? ex.Message : "Could not confirm the change. Reopen your name to check before trying again."; }
                finally { dialog.IsPrimaryButtonEnabled = PeopleCurrent(binding); deferral.Complete(); }
            };
            await dialog.ShowAsync();
            await RefreshPeopleNamesAsync(force: true);
        }
        catch (OperationCanceledException) { }
        catch (Exception) { if (PeopleCurrent(binding)) AccountStatus.Text = "Your name could not be loaded. Please try again."; }
        finally { dialogOpen = false; }
    }
    private async void LocalName_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || ViewModel.SelectedConversation is not { } conversation || PeopleScope() is not { } binding) return;
        dialogOpen = true;
        try
        {
            await RefreshPeopleNamesAsync(force: true);
            if (!PeopleCurrent(binding)) return;
            if (binding.Busy || !binding.Loaded) { AccountStatus.Text = "Contact names are still loading. Please try again in a moment."; return; }
            var peer = conversation.Peer;
            var profile = binding.Profiles.GetValueOrDefault(peer);
            if (profile is null && !PeopleNames.Mailbox(peer))
            {
                var found = await ViewModel.WithPeopleTargetAsync(binding.Target, postbox,
                    (mailbox, ct) => postbox.GetPeerProfilesAsync(mailbox, new[] { peer }, ct));
                profile = found.GetValueOrDefault(peer);
                if (!PeopleCurrent(binding)) return;
                binding.Profiles[peer] = profile;
            }
            var address = profile?.Address ?? (PeopleNames.Mailbox(peer) ? peer : null);
            if (address is null) { AccountStatus.Text = "This person's inbox could not be confirmed. Please try again later."; return; }
            var name = new TextBox { Header = "Local name", Text = binding.Aliases.GetValueOrDefault(address) ?? "", MaxLength = 256 };
            var error = PeopleText(""); var panel = new StackPanel { Spacing = 12, MaxWidth = 440 };
            panel.Children.Add(PeopleText("Only you see this name, on this device and in this inbox. It does not change their handle or permissions. Leave it empty to remove it."));
            panel.Children.Add(PeopleText(peer)); panel.Children.Add(PeopleText(address)); panel.Children.Add(name); panel.Children.Add(error);
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, Title = "Local name", Content = panel,
                PrimaryButtonText = "Save", CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Close };
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                args.Cancel = true; var deferral = args.GetDeferral(); dialog.IsPrimaryButtonEnabled = false;
                try
                {
                    if (!PeopleCurrent(binding)) throw new AuthorizationRetiredException();
                    var updated = new Dictionary<string, string>(binding.Aliases, StringComparer.Ordinal);
                    var input = name.Text.Trim();
                    if (input.Length == 0) updated.Remove(address); else updated[address] = PeopleNames.Normalize(input, alias: true);
                    await localNames.SaveAsync(binding.Owner, NamesOrigin, binding.Target.Mailbox, updated, () => PeopleCurrent(binding));
                    if (!PeopleCurrent(binding)) throw new AuthorizationRetiredException();
                    binding.Aliases = updated; ApplyPeopleNames(binding); dialog.Hide();
                }
                catch (OperationCanceledException) { error.Text = "Your inbox changed. Close this window and try again."; }
                catch (Exception ex) { error.Text = ex is ArgumentException ? ex.Message : "The local name could not be saved."; }
                finally { dialog.IsPrimaryButtonEnabled = PeopleCurrent(binding); deferral.Complete(); }
            };
            await dialog.ShowAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception) { if (PeopleCurrent(binding)) AccountStatus.Text = "Local names could not be loaded. Please try again."; }
        finally { dialogOpen = false; }
    }
    private void ResetPeopleNames(bool signOut = false)
    {
        peopleBinding = null;
        if (ViewModel.CaptureNewConversationTarget() is { } target)
            ViewModel.ApplyPeerNames(target, new Dictionary<string, PeerName>());
        if (signOut)
        {
            try { localNames.Clear(); }
            catch (Exception) { DesktopLog.Write("Encrypted local names could not be removed."); }
        }
    }
}
