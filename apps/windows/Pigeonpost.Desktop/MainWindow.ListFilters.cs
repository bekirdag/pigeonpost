using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls.Primitives;
using Pigeonpost.Core;
using Pigeonpost.Core.Nests;

namespace Pigeonpost.Desktop;

// The quick filters above the conversation list (All · Unread · Agents · People, as in the web
// inbox). They narrow what is shown and change nothing. Agents are the account's own mailboxes and
// its Nests; People is everyone else.
public sealed partial class MainWindow
{
    private bool IsNestPeer(string peer) => NestsOn && NestRules.ForPeer(nests.Snapshot, peer, ViewModel.ResolvePeerKey) is not null;

    // The set of known Nests changed: the conversation header and the Agents/People split follow it.
    private void NestsKnownChanged()
    {
        UpdateNestHeader();
        ViewModel.RefreshListFilter();
    }

    // Called whenever ViewModel is created or replaced.
    private void AttachListFilter()
    {
        ViewModel.AgentPeer = IsNestPeer;
        SyncListFilterChips();
    }

    private void ListFilter_Click(object sender, RoutedEventArgs e)
    {
        if (sender is ToggleButton { Tag: string tag } && Enum.TryParse<ConversationListFilter>(tag, out var filter))
            ViewModel.ListFilter = filter;
        SyncListFilterChips();
    }

    private void SyncListFilterChips()
    {
        if (ListFilterChips is null) return;
        foreach (var child in ListFilterChips.Children)
            if (child is ToggleButton { Tag: string tag } chip)
                chip.IsChecked = tag == ViewModel.ListFilter.ToString();
    }
}
