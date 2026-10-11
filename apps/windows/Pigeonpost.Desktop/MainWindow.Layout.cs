using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private bool compactConversation;
    private double senderWidth = 304;
    private double subjectWidth = 208;
    private bool IsCompact => Root.ActualWidth < 760;

    private void Root_SizeChanged(object sender, SizeChangedEventArgs e) => UpdateLayoutMode();

    private void UpdateLayoutMode()
    {
        if (MessagingArea is null || PinSubjectsButton is null || AccountStatus is null || Root.ActualWidth <= 0) return;
        var compact = IsCompact;
        var conversation = !compact || (compactConversation && ViewModel.HasConversation);
        var subjects = !compact && Root.ActualWidth >= 1100 && PinSubjectsButton.IsChecked == true;
        SenderPane.Visibility = ToVisibility(!compact || !conversation);
        MessagingArea.Visibility = ToVisibility(conversation);
        SubjectPane.Visibility = ToVisibility(subjects);
        SenderDivider.Visibility = ToVisibility(!compact);
        SubjectDivider.Visibility = ToVisibility(subjects);
        BackButton.Visibility = ToVisibility(compact);
        PinSubjectsButton.Visibility = ToVisibility(Root.ActualWidth >= 1100);
        SendingAsLabel.Visibility = ToVisibility(!compact);
        AccountStatus.MaxWidth = compact ? 150 : 260;
        SenderColumn.Width = compact
            ? (conversation ? new GridLength(0) : new GridLength(1, GridUnitType.Star))
            : new GridLength(Math.Min(senderWidth, Root.ActualWidth * .4));
        SenderDividerColumn.Width = new GridLength(compact ? 0 : 5);
        SubjectColumn.Width = new GridLength(subjects ? subjectWidth : 0);
        SubjectDividerColumn.Width = new GridLength(subjects ? 5 : 0);
        MessageColumn.Width = conversation ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
    }

    private void ShowConversation()
    {
        compactConversation = true;
        UpdateLayoutMode();
    }

    private void Conversation_ItemClick(object sender, ItemClickEventArgs e) => ShowConversation();
    private void Back_Click(object sender, RoutedEventArgs e)
    {
        compactConversation = false;
        UpdateLayoutMode();
        ConversationList.Focus(FocusState.Programmatic);
    }
    private void PinSubjects_Click(object sender, RoutedEventArgs e) => UpdateLayoutMode();

    private void ArchiveFilter_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox box) ViewModel.ShowArchived = box.SelectedIndex == 1;
    }

    private void ShowFind()
    {
        if (!ViewModel.HasConversation || InboxLayout.Visibility != Visibility.Visible) return;
        ShowConversation();
        FindBar.Visibility = Visibility.Visible;
        FindBox.Focus(FocusState.Programmatic);
    }
    private void Find_Click(object sender, RoutedEventArgs e) => ShowFind();
    private void CloseFind_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.Find = "";
        FindBar.Visibility = Visibility.Collapsed;
        FindButton.Focus(FocusState.Programmatic);
    }
    private void FindBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Escape) return;
        CloseFind_Click(sender, e);
        e.Handled = true;
    }

    private void SenderColumn_DragDelta(object sender, DragDeltaEventArgs e)
    {
        senderWidth = Math.Clamp(senderWidth + e.HorizontalChange, 260, 380);
        UpdateLayoutMode();
    }
    private void SubjectColumn_DragDelta(object sender, DragDeltaEventArgs e)
    {
        subjectWidth = Math.Clamp(subjectWidth + e.HorizontalChange, 180, 260);
        UpdateLayoutMode();
    }
}
