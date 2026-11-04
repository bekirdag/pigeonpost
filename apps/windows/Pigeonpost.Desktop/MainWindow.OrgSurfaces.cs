using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

namespace Pigeonpost.Desktop;

// SDS §5.4: the organization inside the normal app. The selected org member inbox shows a badge under the mailbox
// switcher ("Acme Inc. · Member") with Browse rooms, and New conversation suggests organization addresses from the
// org directory. Membership comes only from GET /v1/orgs for the selected canonical inbox; nothing is cached across
// accounts or inboxes.
public sealed partial class MainWindow
{
    private (NewConversationTarget Target, RoomOrganization Org)? currentOrganization;
    private int organizationVersion;

    private void ResetOrganizationBadge()
    {
        organizationVersion++;
        currentOrganization = null;
        OrgBadge.Visibility = Visibility.Collapsed;
    }

    private async Task RefreshOrganizationBadgeAsync()
    {
        var version = ++organizationVersion;
        currentOrganization = null;
        OrgBadge.Visibility = Visibility.Collapsed;
        var model = ViewModel;
        if (!features.Has(ServerFeatures.Organizations) || model.CaptureNewConversationTarget() is not { } target) return;
        try
        {
            var catalog = await model.WithPeopleTargetAsync(target, postbox, postbox.OrganizationsAsync);
            if (version != organizationVersion || !ReferenceEquals(model, ViewModel) || !target.IsCurrent) return;
            if (OrganizationBadge.For(catalog, target.Mailbox) is not { } org) return;
            currentOrganization = (target, org);
            var label = OrganizationBadge.Label(org);
            OrgBadgeInitials.Text = OrganizationBadge.Initials(org.Name);
            OrgBadgeText.Text = label;
            AutomationProperties.SetName(OrgBadge, "Organization: " + label);
            ToolTipService.SetToolTip(OrgBadge, $"This inbox belongs to {org.Name}. Your administrators manage it.");
            OrgBrowseRooms.Visibility = ToVisibility(roomsNav is not null);
            OrgBadge.Visibility = Visibility.Visible;
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { DesktopLog.Write("Organization badge unavailable: " + ex.GetType().Name); }
    }

    private void OrgBrowseRooms_Click(object sender, RoutedEventArgs e)
    {
        if (roomsNav is null) return;
        ShowSideView(SideView.Rooms);
        roomStore.Synchronize(); roomStore.LoadOrganizations(roomStore.State.ContextId);
    }

    // Suggestions under the address box in New conversation, only while composing from an org inbox.
    private ListView? OrganizationSuggestions(TextBox peer, NewConversationTarget target)
    {
        if (currentOrganization is not { } current || !ReferenceEquals(current.Target, target) || !target.IsCurrent) return null;
        var org = current.Org;
        var list = new ListView { MaxHeight = 220, SelectionMode = ListViewSelectionMode.None, IsItemClickEnabled = true, Visibility = Visibility.Collapsed };
        AutomationProperties.SetName(list, $"{org.Name} addresses");
        var directory = new OrganizationDirectoryClient(postbox);
        var generation = 0;
        var shown = new Dictionary<string, DirectoryEntry>(StringComparer.Ordinal);
        list.ItemClick += (_, args) =>
        {
            if (args.ClickedItem is string label && shown.TryGetValue(label, out var entry))
            { peer.Text = entry.Handle; peer.SelectionStart = peer.Text.Length; list.Visibility = Visibility.Collapsed; }
        };
        peer.TextChanged += async (_, _) =>
        {
            var mine = ++generation;
            if (OrganizationBadge.Query(peer.Text, org.Namespace) is not { } query) { list.Items.Clear(); list.Visibility = Visibility.Collapsed; return; }
            await Task.Delay(250);
            if (mine != generation || !target.IsCurrent) return;
            try
            {
                var entries = await ViewModel.WithPeopleTargetAsync(target, postbox, (m, ct) => directory.SearchAsync(m, org.Id, query, ct));
                if (mine != generation || !target.IsCurrent) return;
                list.Items.Clear(); shown.Clear();
                foreach (var entry in entries) if (shown.TryAdd(entry.Label, entry)) list.Items.Add(entry.Label);
                list.Visibility = ToVisibility(entries.Count > 0 && !entries.Any(e => e.Handle == PostAddress.Canonical(peer.Text)));
            }
            catch (OperationCanceledException) { }
            catch (Exception) { if (mine == generation) list.Visibility = Visibility.Collapsed; }
        };
        return list;
    }
}
