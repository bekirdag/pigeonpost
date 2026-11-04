using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Desktop;

// §14C in groups: the row under each bubble (received: copy, reply; sent: edit while editable,
// copy), the right-click menu, the quote that leads to its parent, the ticks that open message info,
// and the composer's reply/edit bar. Every action starts from the row the page is showing now; the
// model refuses a row rendered for another login, inbox, group or selection.
public sealed partial class MainWindow
{
    private Border? groupModeBar;
    private TextBlock? groupModeTitle, groupModeExcerpt;
    private Button? groupModeCancel, groupAttach;

    private FrameworkElement BuildGroupModeBar()
    {
        // Theme brushes resolve against the window's theme only through markup.
        groupModeBar = (Border)Microsoft.UI.Xaml.Markup.XamlReader.Load(
            "<Border xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation' Padding='12,10' CornerRadius='10' BorderThickness='1' Visibility='Collapsed'"
            + " Background='{ThemeResource ChipSurfaceBrush}' BorderBrush='{ThemeResource ComposerBorderBrush}'>"
            + "<Grid ColumnSpacing='12'><Grid.ColumnDefinitions><ColumnDefinition Width='*' /><ColumnDefinition Width='Auto' /></Grid.ColumnDefinitions>"
            + "<StackPanel Spacing='4'><TextBlock FontWeight='SemiBold' FontSize='13' Foreground='{ThemeResource AccentBrush}' />"
            + "<TextBlock TextWrapping='Wrap' MaxLines='2' TextTrimming='CharacterEllipsis' FontSize='12.5' Foreground='{ThemeResource TextSecondaryBrush}' /></StackPanel>"
            + "</Grid></Border>");
        AutomationProperties.SetName(groupModeBar, "Group composer action");
        var grid = (Grid)groupModeBar.Child;
        var text = (StackPanel)grid.Children[0];
        groupModeTitle = (TextBlock)text.Children[0];
        groupModeExcerpt = (TextBlock)text.Children[1];
        groupModeCancel = new Button { Content = "Cancel", Style = (Style)Root.Resources["SecondaryActionButton"], VerticalAlignment = VerticalAlignment.Top };
        AutomationProperties.SetName(groupModeCancel, "Cancel group message action");
        groupModeCancel.Click += (_, _) => { if (!dialogOpen && groupsModel.CancelComposerMode()) { UpdateGroupPage(); KeepGroupComposerFocus(); } };
        Grid.SetColumn(groupModeCancel, 1);
        grid.Children.Add(groupModeCancel);
        return groupModeBar;
    }

    private void UpdateGroupModeBar()
    {
        var m = groupsModel;
        if (groupModeBar is null) return;
        groupModeBar.Visibility = ToVisibility(m.HasComposerMode && m.ShowsComposer);
        groupModeTitle!.Text = m.ComposerModeTitle;
        groupModeExcerpt!.Text = m.ComposerModeExcerpt;
        groupModeCancel!.IsEnabled = !m.IsSending;
        if (groupSend is not null)
        {
            groupSend.Content = m.SendLabel;
            AutomationProperties.SetName(groupSend, m.IsEditing ? "Save group edit" : "Send to group");
        }
        if (groupDraft is not null) groupDraft.PlaceholderText = m.ComposerPlaceholder;
        if (groupAttach is not null) groupAttach.IsEnabled = m.CanAttach;
    }

    private void BeginGroupAction(GroupLineRow row, GroupComposerKind kind)
    {
        if (dialogOpen) return;
        var started = kind == GroupComposerKind.Edit ? groupsModel.BeginEdit(row) : groupsModel.BeginReply(row);
        if (!started) { DesktopLog.Write("Group message action refused: row changed"); return; }
        UpdateGroupPage();
        KeepGroupComposerFocus();
    }

    private void GroupReply_Click(object sender, RoutedEventArgs e)
    { if (sender is FrameworkElement { Tag: GroupLineRow row }) BeginGroupAction(row, GroupComposerKind.Reply); }

    private void GroupEdit_Click(object sender, RoutedEventArgs e)
    { if (sender is FrameworkElement { Tag: GroupLineRow row }) BeginGroupAction(row, GroupComposerKind.Edit); }

    private void GroupQuote_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: GroupLineRow row } || groupLines is null) return;
        if (groupsModel.ReplyParentRow(row) is not { } parent) { AccountStatus.Text = "The original message isn't available on this device."; return; }
        try
        {
            groupLines.ScrollIntoView(parent, ScrollIntoViewAlignment.Leading);
            DesktopLog.Write("Group quoted message shown");
        }
        catch (Exception ex) { DesktopLog.Error("Group quote navigation failed", ex); }
    }

    private async void GroupReceipt_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: GroupLineRow row }) await ShowGroupMessageInfoAsync(row);
    }

    // The right-click menu keeps the full list: reply, the template's copy, edit, the quote and
    // message info. Items are rebuilt on every opening from the row as it is now.
    private void GroupMessageMenu_Opening(object? sender, object e)
    {
        if (sender is not MenuFlyout menu) return;
        foreach (var stale in menu.Items.Where(i => ReferenceEquals(i.Tag, DynamicMenuItem)).ToArray()) menu.Items.Remove(stale);
        if (dialogOpen || menu.Target is not FrameworkElement { Tag: GroupLineRow row } || !row.IsMessage) return;
        void Add(int index, string text, Action click)
        {
            var item = new MenuFlyoutItem { Text = text, Tag = DynamicMenuItem };
            item.Click += (_, _) => click();
            menu.Items.Insert(Math.Min(index, menu.Items.Count), item);
        }
        if (row.CanReply) Add(0, "Reply", () => BeginGroupAction(row, GroupComposerKind.Reply));
        if (row.CanEdit) Add(menu.Items.Count, "Edit message", () => BeginGroupAction(row, GroupComposerKind.Edit));
        if (row.HasReply) Add(menu.Items.Count, "Show quoted message", () =>
        {
            if (groupsModel.ReplyParentRow(row) is { } parent && groupLines is not null) groupLines.ScrollIntoView(parent, ScrollIntoViewAlignment.Leading);
        });
        Add(menu.Items.Count, "Message info", async () => await ShowGroupMessageInfoAsync(row));
    }

    private async Task ShowGroupMessageInfoAsync(GroupLineRow row)
    {
        if (dialogOpen) return;
        var model = groupsModel;
        if (model.MessageInfoFor(row) is not { } info) { AccountStatus.Text = "Message info isn't available for this message."; return; }
        dialogOpen = true;
        try
        {
            var panel = new StackPanel { Spacing = 10, MaxWidth = 480 };
            void Line(string text, bool strong = false) => panel.Children.Add(new TextBlock
            { Text = text, TextWrapping = TextWrapping.Wrap, FontWeight = strong ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal });
            Line(info.Sender, strong: true);
            Line("Sent " + MessageInfoTime(info.At));
            if (info.MissingEarlier) Line("Earlier versions aren't on this device.");
            if (info.Versions.Count > 1 || info.MissingEarlier)
            {
                Line("Versions", strong: true);
                foreach (var version in info.Versions)
                {
                    Line(version.Label + " · " + MessageInfoTime(version.At));
                    var box = new TextBox { Text = version.Text ?? "This version isn't available on this device.", IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MaxHeight = 160 };
                    AutomationProperties.SetName(box, "Message version");
                    panel.Children.Add(box);
                }
            }
            if (info.ShowsDelivery)
            {
                Line("Delivery", strong: true);
                if (!info.DeliveryKnown) Line("Member delivery information isn't available yet.");
                else if (info.Members.Count == 0) Line("No other active members.");
                else foreach (var member in info.Members) Line($"{member.Name} · {member.State}");
            }
            await new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Message info",
                Content = new ScrollViewer { Content = panel, MaxHeight = 480, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled }, CloseButtonText = "Done",
            }.ShowAsync();
        }
        catch (Exception ex) { DesktopLog.Error("Group message information unavailable", ex); AccountStatus.Text = "Could not show message info. Try again."; }
        finally { dialogOpen = false; KeepGroupComposerFocus(); }
    }
}
