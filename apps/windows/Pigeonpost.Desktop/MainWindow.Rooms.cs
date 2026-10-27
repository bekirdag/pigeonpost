using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

namespace Pigeonpost.Desktop;

// Organization rooms (docs/orgs.md §5) for the selected canonical member inbox. Membership comes
// from the server catalog only. Operated (delegated) agents are listed but not opened here: that
// integration is a release gate. Uncertain creations stay in protected custody until resolved.
public sealed partial class MainWindow
{
    private OrganizationRoomStore roomStore = null!;
    private RoomsView? roomsView;
    private bool roomOpening;
#if UI_TESTS
    private RoomLossHandler? roomLoss;
#endif

    private sealed class RoomsView
    {
        public required StackPanel Organizations, Directory, Review, Creation, Pending, Delegated;
        public required TextBlock Status, OrganizationTitle, ReviewTitle, ReviewDetail;
        public required ComboBox OrganizationPicker;
        public required TextBox Search, Name, Description;
        public required Button Refresh, SearchButton, NewRoom, ReviewAction, CloseReview, Create, CancelCreate, Retry, Check, Discard, OpenCreated;
        public required ComboBox JoinPolicy, PostPolicy;
        public required CheckBox Listed;
        public required ProgressRing Busy;
        public bool Rendering;
        // Rebuild lists only when their content changes, so keyboard and Narrator focus stays put.
        public object? CatalogKey, PickerKey, DirectoryKey;
    }

    private void CreateRoomStore()
    {
        IInboxService inbox = postbox;
        Func<(AuthorizationLease? Grant, string? Owner)> account = () => (session.AcceptedAuthorization, session.RecoveryOwnerKey);
        IRoomCreationStore custody = new FileRoomCreationStore(FileRoomCreationStore.DefaultFolder(), new DpapiProtector());
#if UI_TESTS
        if (approvalAccount is { } fixture)
        {
            inbox = preview; account = () => (fixture.Lease, fixture.Owner);
            custody = Environment.GetEnvironmentVariable("PIGEONPOST_UI_ROOMS_FOLDER") is { } folder
                ? new FileRoomCreationStore(folder, new DpapiProtector()) : new MemoryRoomCreationStore();
        }
#endif
        roomStore = new(postbox, inbox, custody, () =>
        {
            var (grant, owner) = account();
            return grant is { IsActive: true } && owner is not null && ViewModel.SelectedMailbox?.Address is { } mailbox ? new(owner, grant, mailbox) : null;
        });
        roomStore.PropertyChanged += (_, _) => { RenderRooms(); _ = OpenRoomAsync(); };
    }

    private void CloseRooms() { if (!roomStore.State.Busy) roomStore.Close(); }

    private FrameworkElement BuildRoomsPage()
    {
        var frame = PageFrame("Rooms", out var actions, out var body);
        TextBlock Text(string text = "", double size = 14, bool strong = false) => new() { Text = text, FontSize = size, TextWrapping = TextWrapping.Wrap,
            FontWeight = strong ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal };
        Button Action(string label, bool accent = false)
        {
            var button = new Button { Content = label, MinHeight = 32, Padding = new Thickness(12, 5, 12, 5) };
            if (accent) button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            AutomationProperties.SetName(button, label); return button;
        }
        ComboBox Choice(string name, params string[] items)
        {
            var box = new ComboBox { MinWidth = 220 };
            foreach (var item in items) box.Items.Add(item);
            box.SelectedIndex = 0; AutomationProperties.SetName(box, name); return box;
        }
        var refresh = Action("Refresh rooms"); actions.Children.Add(refresh);
        var page = new StackPanel { Spacing = 12, Padding = new Thickness(24, 4, 24, 24), MaxWidth = 720, HorizontalAlignment = HorizontalAlignment.Left };
        var busy = new ProgressRing { IsActive = false, Width = 20, Height = 20, HorizontalAlignment = HorizontalAlignment.Left };
        AutomationProperties.SetName(busy, "Working");
        var status = Text(); AutomationProperties.SetLiveSetting(status, AutomationLiveSetting.Polite);
        var organizations = new StackPanel { Spacing = 8 };
        var picker = new ComboBox { MinWidth = 260 }; AutomationProperties.SetName(picker, "Organization");
        var orgTitle = Text(size: 16, strong: true);
        var delegated = new StackPanel { Spacing = 4 };
        page.Children.Add(busy); page.Children.Add(status); page.Children.Add(organizations); page.Children.Add(orgTitle); page.Children.Add(picker); page.Children.Add(delegated);
        var searchRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        var search = new TextBox { PlaceholderText = "Search rooms", MinWidth = 260, MaxLength = 80 }; AutomationProperties.SetName(search, "Search rooms");
        var searchButton = Action("Search"); var newRoom = Action("New room", accent: true);
        searchRow.Children.Add(search); searchRow.Children.Add(searchButton); searchRow.Children.Add(newRoom);
        page.Children.Add(searchRow);
        var directory = new StackPanel { Spacing = 6 }; AutomationProperties.SetName(directory, "Room directory");
        page.Children.Add(directory);
        var review = new StackPanel { Spacing = 8, Padding = new Thickness(14), CornerRadius = new CornerRadius(8),
            Background = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"] };
        var reviewTitle = Text(size: 18, strong: true); AutomationProperties.SetHeadingLevel(reviewTitle, AutomationHeadingLevel.Level2);
        var reviewDetail = Text();
        var reviewAction = Action("Join room", accent: true); var closeReview = Action("Close room details");
        var reviewButtons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 }; reviewButtons.Children.Add(reviewAction); reviewButtons.Children.Add(closeReview);
        review.Children.Add(reviewTitle); review.Children.Add(reviewDetail); review.Children.Add(reviewButtons);
        page.Children.Add(review);
        var creation = new StackPanel { Spacing = 8 };
        creation.Children.Add(Text("New room", 16, strong: true));
        var name = new TextBox { Header = "Room name", MaxLength = 320 }; AutomationProperties.SetName(name, "Room name");
        var description = new TextBox { Header = "Description (optional)", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MaxLength = 2000, MinHeight = 64 };
        AutomationProperties.SetName(description, "Room description");
        var join = Choice("Who can join", "Any organization member", "Invited people only");
        var post = Choice("Who can post", "Everyone in the room", "Room admins only");
        var listed = new CheckBox { Content = "Show in the organization's room directory", IsChecked = true };
        AutomationProperties.SetName(listed, "Show in the organization's room directory");
        var create = Action("Create room", accent: true); var cancelCreate = Action("Cancel new room");
        var createButtons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 }; createButtons.Children.Add(create); createButtons.Children.Add(cancelCreate);
        creation.Children.Add(name); creation.Children.Add(description); creation.Children.Add(join); creation.Children.Add(post); creation.Children.Add(listed);
        creation.Children.Add(createButtons);
        page.Children.Add(creation);
        var pending = new StackPanel { Spacing = 8 };
        var retry = Action("Retry the same details", accent: true); var check = Action("Check the created room");
        var discard = Action("Discard saved creation…"); var openCreated = Action("Open created room", accent: true);
        var pendingButtons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        pendingButtons.Children.Add(retry); pendingButtons.Children.Add(check); pendingButtons.Children.Add(discard); pendingButtons.Children.Add(openCreated);
        pending.Children.Add(pendingButtons);
        page.Children.Add(pending);
        body.Children.Add(new ScrollViewer { Content = page, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });

        var view = roomsView = new RoomsView
        {
            Organizations = organizations, Directory = directory, Review = review, Creation = creation, Pending = pending, Delegated = delegated,
            Status = status, OrganizationTitle = orgTitle, ReviewTitle = reviewTitle, ReviewDetail = reviewDetail, OrganizationPicker = picker,
            Search = search, Name = name, Description = description, Refresh = refresh, SearchButton = searchButton, NewRoom = newRoom,
            ReviewAction = reviewAction, CloseReview = closeReview, Create = create, CancelCreate = cancelCreate, Retry = retry, Check = check,
            Discard = discard, OpenCreated = openCreated, JoinPolicy = join, PostPolicy = post, Listed = listed, Busy = busy
        };
        string Context() => roomStore.State.ContextId;
        refresh.Click += (_, _) => roomStore.LoadOrganizations(Context(), search.Text);
        searchButton.Click += (_, _) => roomStore.LoadRooms(search.Text, Context());
        search.KeyDown += (_, e) => { if (e.Key == Windows.System.VirtualKey.Enter) { e.Handled = true; roomStore.LoadRooms(search.Text, Context()); } };
        picker.SelectionChanged += (_, _) =>
        {
            if (view.Rendering || picker.SelectedItem is not ComboBoxItem { Tag: string id }) return;
            if (roomStore.SelectOrganization(id, Context())) roomStore.LoadRooms(search.Text, Context());
        };
        newRoom.Click += (_, _) => roomStore.BeginCreation(Context());
        void DraftChanged()
        {
            if (view.Rendering || roomStore.State.Draft is not { } draft) return;
            roomStore.ChangeDraft(draft with { Name = name.Text, Description = description.Text,
                Join = join.SelectedIndex == 1 ? RoomJoinPolicy.Invited : RoomJoinPolicy.Members,
                Post = post.SelectedIndex == 1 ? RoomPostPolicy.Admins : RoomPostPolicy.Everyone, Listed = listed.IsChecked == true }, Context());
        }
        name.TextChanged += (_, _) => DraftChanged(); description.TextChanged += (_, _) => DraftChanged();
        join.SelectionChanged += (_, _) => DraftChanged(); post.SelectionChanged += (_, _) => DraftChanged();
        listed.Checked += (_, _) => DraftChanged(); listed.Unchecked += (_, _) => DraftChanged();
        create.Click += (_, _) => { if (roomStore.State.Draft is { } draft) roomStore.SubmitDraft(draft.Id, Context()); };
        retry.Click += (_, _) => { if (roomStore.State.Draft is { } draft) roomStore.SubmitDraft(draft.Id, Context()); };
        cancelCreate.Click += (_, _) => roomStore.CancelDraft(Context());
        check.Click += (_, _) => roomStore.CheckCreated(Context());
        openCreated.Click += (_, _) => roomStore.CheckCreated(Context());
        discard.Click += async (_, _) => await DiscardRoomCreationAsync();
        closeReview.Click += (_, _) => roomStore.CloseReview(Context());
        reviewAction.Click += (_, _) => roomStore.OpenReviewed(roomStore.State.ReviewProof, Context());
        roomStore.Synchronize();
        if (roomStore.State.Catalog is null && !roomStore.State.Loading) roomStore.LoadOrganizations(Context());
        RenderRooms();
        return frame;
    }

    private void RenderRooms()
    {
        if (roomsView is not { } view || sideView != SideView.Rooms) return;
        var state = roomStore.State; var current = roomStore.IsCurrent(state.ContextId);
        view.Rendering = true;
        try
        {
            view.Busy.IsActive = state.Busy || state.Loading; view.Busy.Visibility = ToVisibility(state.Busy || state.Loading);
            view.Status.Text = state.Message ?? (!current ? "Select an inbox to see its organization rooms."
                : state.Catalog is null ? (state.LoadingCatalog ? "Loading your organizations…" : "")
                : state.Organizations.Count == 0 ? "This inbox is not an organization member inbox." : "");
            AutomationProperties.SetName(view.Status, view.Status.Text);
            view.Status.Visibility = ToVisibility(view.Status.Text.Length > 0);
            var catalogKey = (state.Catalog, state.Mailbox);
            var rebuildCatalog = !Equals(view.CatalogKey, catalogKey); view.CatalogKey = catalogKey;
            if (rebuildCatalog) view.Organizations.Children.Clear();
            if (rebuildCatalog) foreach (var other in state.Organizations.Count == 0 ? state.OtherInboxes : Array.Empty<RoomOrganization>())
            {
                var inbox = ViewModel.Mailboxes.FirstOrDefault(m => m.Address == other.Mailbox!.Address);
                var label = other.Mailbox!.Handle ?? other.Mailbox.Address;
                var row = new StackPanel { Spacing = 4 };
                row.Children.Add(new TextBlock { Text = $"{other.Name} rooms use your {label} inbox.", TextWrapping = TextWrapping.Wrap });
                if (inbox is not null)
                {
                    var button = new Button { Content = "Switch to " + label };
                    AutomationProperties.SetName(button, "Switch to " + label);
                    button.Click += async (_, _) => await SwitchToRoomInboxAsync(inbox);
                    row.Children.Add(button);
                }
                view.Organizations.Children.Add(row);
            }
            if (rebuildCatalog) view.Delegated.Children.Clear();
            if (rebuildCatalog && state.Delegated.Count > 0)
            {
                view.Delegated.Children.Add(new TextBlock { Text = "Agents you operate", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
                foreach (var agent in state.Delegated)
                    view.Delegated.Children.Add(new TextBlock { Text = (agent.Handle ?? agent.Address) + " — opening an operated agent's rooms isn't available in this app yet.", TextWrapping = TextWrapping.Wrap });
            }
            var org = state.Organization;
            view.OrganizationTitle.Text = org is null ? "" : org.Name + (org.Writable ? "" : " (read-only)");
            view.OrganizationTitle.Visibility = ToVisibility(org is not null);
            var pickerKey = (state.Catalog, state.Mailbox, state.SelectedOrganizationId);
            if (!Equals(view.PickerKey, pickerKey)) { view.PickerKey = pickerKey; view.OrganizationPicker.Items.Clear(); }
            if (view.OrganizationPicker.Items.Count == 0) foreach (var option in state.Organizations)
            {
                var item = new ComboBoxItem { Content = option.Name, Tag = option.Id };
                view.OrganizationPicker.Items.Add(item);
                if (option.Id == state.SelectedOrganizationId) view.OrganizationPicker.SelectedItem = item;
            }
            view.OrganizationPicker.Visibility = ToVisibility(state.Organizations.Count > 1);
            var editing = state.Draft is not null && state.CompletedCreation is null;
            view.Search.IsEnabled = view.SearchButton.IsEnabled = current && org is not null && !state.Busy;
            view.NewRoom.IsEnabled = roomStore.CanCreate(state.ContextId) && !editing;
            view.NewRoom.Visibility = ToVisibility(org is not null);
            var directoryKey = (state.DirectoryProof, state.Busy, current, state.LoadingRooms);
            var rebuildDirectory = !Equals(view.DirectoryKey, directoryKey); view.DirectoryKey = directoryKey;
            if (rebuildDirectory) view.Directory.Children.Clear();
            if (rebuildDirectory && org is not null && state.Rooms.Count == 0 && !state.LoadingRooms)
                view.Directory.Children.Add(new TextBlock { Text = "No rooms found.", Opacity = 0.75 });
            if (rebuildDirectory) foreach (var room in state.Rooms)
            {
                var label = room.Action switch { RoomAction.Open => "Open", RoomAction.Join => "Join", RoomAction.AcceptInvitation => "Invitation", _ => "Not available" };
                var members = room.MemberCount == 1 ? "1 member" : room.MemberCount + " members";
                var button = new Button { HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Left,
                    Content = $"{room.Name} · {members} · {label}", IsEnabled = current && !state.Busy };
                AutomationProperties.SetName(button, $"Room {room.Name}, {members}, {label}");
                var id = room.Id; var proof = state.DirectoryProof;
                button.Click += (_, _) => roomStore.Inspect(id, proof, roomStore.State.ContextId);
                view.Directory.Children.Add(button);
            }
            view.Review.Visibility = ToVisibility(state.Preview is not null);
            if (state.Preview is { } preview)
            {
                var action = roomStore.ReviewAction(state.ReviewProof, state.ContextId);
                view.ReviewTitle.Text = preview.Name;
                view.ReviewDetail.Text = (preview.Description is { Length: > 0 } d ? d + "\n" : "")
                    + (preview.MemberCount == 1 ? "1 member" : preview.MemberCount + " members") + " · "
                    + (preview.JoinPolicy == "public" ? "Any organization member can join" : "Invited people only")
                    + (state.MustReviewAgain ? "\nThe last result was not confirmed. Close and review this room again." : "");
                var actionLabel = action switch { RoomAction.Open => "Open room", RoomAction.Join => "Join room", RoomAction.AcceptInvitation => "Accept invitation", _ => "Not available" };
                view.ReviewAction.Content = actionLabel; AutomationProperties.SetName(view.ReviewAction, actionLabel);
                view.ReviewAction.IsEnabled = action != RoomAction.Unavailable && !roomOpening;
            }
            view.Creation.Visibility = ToVisibility(editing && state.PendingCreation is null);
            if (state.Draft is { } draft)
            {
                if (view.Name.Text != draft.Name) view.Name.Text = draft.Name;
                if (view.Description.Text != draft.Description) view.Description.Text = draft.Description;
                view.JoinPolicy.SelectedIndex = draft.Join == RoomJoinPolicy.Invited ? 1 : 0;
                view.PostPolicy.SelectedIndex = draft.Post == RoomPostPolicy.Admins ? 1 : 0;
                view.Listed.IsChecked = draft.Listed;
            }
            view.Create.IsEnabled = current && !state.Busy && state.PendingCreation is null;
            view.Pending.Visibility = ToVisibility(state.PendingCreation is not null || state.CompletedCreation is not null);
            var attempt = state.PendingCreation;
            view.Retry.Visibility = ToVisibility(attempt is { CandidateId: null });
            view.Retry.IsEnabled = current && !state.Busy && attempt?.CanRetry(DateTimeOffset.UtcNow.ToUnixTimeSeconds()) == true;
            view.Check.Visibility = ToVisibility(attempt is { CandidateId: not null });
            view.Discard.Visibility = ToVisibility(attempt is not null);
            view.Discard.IsEnabled = view.Check.IsEnabled = current && !state.Busy;
            view.OpenCreated.Visibility = ToVisibility(attempt is null && state.CompletedCreation is not null);
            view.OpenCreated.IsEnabled = current && !state.Busy && !roomOpening;
        }
        finally { view.Rendering = false; }
    }

    private async Task SwitchToRoomInboxAsync(Mailbox inbox)
    {
        if (callMode != CallMode.Hidden || callRinging || pendingCall is not null) { AccountStatus.Text = "End the call before switching inboxes."; return; }
        // The selected-inbox change rebinds side views; the Rooms page stays and reloads for this inbox.
        await ViewModel.SwitchMailboxAsync(inbox);
        await AcknowledgeVisibleAsync();
    }

    private async Task DiscardRoomCreationAsync()
    {
        if (dialogOpen || roomStore.State.PendingCreation is null) return;
        dialogOpen = true;
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Discard this saved room creation?",
                Content = "Pigeonpost forgets the saved request on this PC. If the server already created the room, it stays; check your rooms first.",
                PrimaryButtonText = "Discard", CloseButtonText = "Keep", DefaultButton = ContentDialogButton.Close
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) roomStore.DiscardCreation(roomStore.State.ContextId);
        }
        finally { dialogOpen = false; }
    }

    // A confirmed room opens in Groups only after a fresh read there confirms its organization and
    // membership; an owned provisioning room is activated with real MLS first.
    private async Task OpenRoomAsync()
    {
        if (roomOpening || roomStore.State.Opening is not { } opening || !roomStore.Allows(opening)) return;
        roomOpening = true;
        try
        {
            // The selected inbox's encrypted group client may still be starting after an inbox switch.
            for (var wait = 0; wait < 60 && roomStore.Allows(opening) && roomStore.State.Mailbox is { } inbox && !groupsModel.IsReadyFor(inbox); wait++)
                await Task.Delay(250);
            if (!roomStore.Allows(opening)) return;
            bool prepared;
            try { prepared = await groupsModel.PrepareOrganizationRoomAsync(opening.GroupId, opening.OrganizationId); }
            catch (Exception ex) when (ex is not OperationCanceledException) { roomStore.OpeningFailed(opening, ex); return; }
            if (!roomStore.Allows(opening)) return;
            if (!prepared) { roomStore.OpeningFailed(opening); return; }
            roomStore.OpeningCompleted(opening);
            ShowSideView(SideView.Groups);
            if (groupList is not null) groupList.SelectedItem = groupsModel.Groups.FirstOrDefault(g => g.Id == opening.GroupId);
            UpdateGroupPage();
        }
        finally { roomOpening = false; RenderRooms(); }
    }
}
