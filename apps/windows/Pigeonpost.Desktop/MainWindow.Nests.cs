using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;
using Pigeonpost.Core.Nests;

namespace Pigeonpost.Desktop;

// Nests on Windows (docs/nests/phase-b.md). Products has three tabs: Buy an Inbox, Buy Nests and
// Services. Windows has no in-app payment: buying, upgrading and managing a package open the website.
// A Nest's conversation shows its status in the header; its page lists usage, files and approvals
// and offers pause, resume, rename, export and delete. Everything here exists only while
// GET /v1/capabilities lists `nests`, and every action asks the postbox again.
//
// Text stays text: buttons and labels take their accessible name from what they show, and context a
// screen reader needs goes in the full description, so a name never replaces visible words.
public sealed partial class MainWindow
{
    private static readonly TimeSpan NestRefresh = TimeSpan.FromSeconds(60);

    private sealed class NestPageState
    {
        public IReadOnlyList<NestPackage>? Packages;
        public NestSnapshot? Snapshot;
        public IReadOnlyList<RoomOrganization>? Orgs;
        public string? Org;
        public OrgNests? OrgData;
        public DateTimeOffset LoadedAt;
        public bool Loading, Busy;
        public string? Message;
        public string? Selected;
        public NestRow? Detail;
        public NestFiles? Files;
        public IReadOnlyList<NestApproval>? Cards;
        public IReadOnlyList<string>? Usage;
    }

    private sealed record NestScope(InboxViewModel Inbox, AuthorizationLease? Authorization, NestPageState State);

    private NestPageState nests = new();
    private StackPanel? productsContent;
    private StackPanel? nestContent;
    private bool NestsOn => NestRules.IsOn(features);
    private NestScope CurrentNestScope() => new(ViewModel, session.AcceptedAuthorization, nests);
    private bool NestCurrent(NestScope scope) => ReferenceEquals(scope.State, nests) && SameAccount(scope.Inbox, scope.Authorization);

    // ---- lifecycle -----------------------------------------------------------------------------

    private void ResetNests()
    {
        nests = new NestPageState();
        UpdateNestHeader();
    }

    private void ClearNestPages() { productsContent = null; nestContent = null; }

    private void NestsFeaturesChanged()
    {
        if (!NestsOn)
        {
            nests = new NestPageState();
            UpdateNestHeader();
            if (sideView is SideView.Products or SideView.Nest) ShowSideView(SideView.Chats);
            return;
        }
        if (nests.Snapshot is null) _ = LoadNestsAsync();
    }

    // Runs one network step for this account. Returns false when it failed or the account moved on.
    private async Task<bool> NestDoAsync(NestScope scope, Func<CancellationToken, Task> work, Action<Exception> failed)
    {
        try
        {
            using var operation = scope.Authorization is null ? null : new AuthorizationContext(scope.Authorization);
            await work(lifetime.Token);
            return NestCurrent(scope);
        }
        catch (OperationCanceledException) when (lifetime.IsCancellationRequested) { return false; }
        catch (Exception ex)
        {
            DesktopLog.Error("Nests request failed", ex);
            if (NestCurrent(scope)) failed(ex);
            return false;
        }
    }

    private void NestFailed(NestPageState state, Exception error)
    {
        state.Loading = state.Busy = false;
        state.Message = NestRules.Explain(error);
        if (NestRules.EndsAccess(error)) { state.Snapshot = null; state.Selected = null; state.Detail = null; UpdateNestHeader(); }
        RenderProducts();
        RenderNest();
    }

    private async Task LoadNestsAsync()
    {
        var scope = CurrentNestScope();
        var state = scope.State;
        if (!NestsOn || state.Loading) return;
        var first = state.Snapshot is null;
        state.Loading = true;
        if (first) RenderProducts();
        IReadOnlyList<NestPackage>? packages = null;
        NestSnapshot? snapshot = null;
        var loaded = await NestDoAsync(scope, async ct =>
        {
            packages = await postbox.NestPackagesAsync(ct);
            snapshot = await postbox.NestSnapshotAsync(ct);
        }, ex => { state.LoadedAt = DateTimeOffset.UtcNow; NestFailed(state, ex); });
        state.Loading = false;
        if (!loaded) return;
        var changed = first || state.Packages is null || !NestRules.SameSnapshot(state.Snapshot, snapshot);
        state.Packages = packages;
        state.Snapshot = snapshot;
        state.LoadedAt = DateTimeOffset.UtcNow;
        if (first) state.Message = null;
        UpdateNestHeader();
        if (changed) RenderProducts();
    }

    private async Task LoadNestOrganizationsAsync()
    {
        var scope = CurrentNestScope();
        var state = scope.State;
        if (!NestsOn || !features.Has(ServerFeatures.Organizations) || scope.Inbox.CaptureNewConversationTarget() is not { } target) return;
        RoomOrganizations? catalog = null;
        if (!await NestDoAsync(scope, async _ => catalog = await scope.Inbox.WithPeopleTargetAsync(target, postbox, postbox.OrganizationsAsync),
            ex => DesktopLog.Write("Nest organizations unavailable: " + ex.GetType().Name)) || catalog is null || !target.IsCurrent) return;
        state.Orgs = catalog.Orgs.Where(o => o.Valid && o.Readable).ToArray();
        if (state.Org is { } org && state.Orgs.All(o => o.Id != org)) { state.Org = null; state.OrgData = null; }
        RenderProducts();
    }

    // Each refresh tick; at most one round trip a minute, and a page is only redrawn when it changed.
    private async Task RefreshNestsAsync()
    {
        var state = nests;
        if (!NestsOn || state.Loading || DateTimeOffset.UtcNow - state.LoadedAt < NestRefresh) return;
        await LoadNestsAsync();
        if (sideView == SideView.Nest && state.Selected is { } id && !state.Busy) await RefreshNestDetailAsync(state, id);
    }

    private NestRow? HeaderNest() => NestsOn ? NestRules.ForPeer(nests.Snapshot, ViewModel.SelectedConversation?.Peer, ViewModel.ResolvePeerKey) : null;

    // ---- conversation header -------------------------------------------------------------------

    private void UpdateNestHeader()
    {
        if (NestHeader is null) return;
        var nest = HeaderNest();
        NestHeader.Visibility = ToVisibility(nest is not null);
        if (nest is null) return;
        var (text, reason) = NestRules.StatusLine(nest);
        NestStatusText.Text = text;
        ToolTipService.SetToolTip(NestStatusPill, reason ?? text);
        AutomationProperties.SetFullDescription(NestStatusText, reason is null ? "Nest status" : "Nest status. " + reason);
    }

    private async void NestPage_Click(object sender, RoutedEventArgs e)
    {
        if (HeaderNest() is { } nest) await OpenNestAsync(nest.Id);
    }

    // ---- shared pieces -------------------------------------------------------------------------

    private static TextBlock NestText(string text, double size = 13, bool strong = false) => new()
    {
        Text = text, FontSize = size, TextWrapping = TextWrapping.Wrap, FontWeight = strong ? FontWeights.SemiBold : FontWeights.Normal,
    };

    private static TextBlock NestHeading(string text, AutomationHeadingLevel level = AutomationHeadingLevel.Level2)
    {
        var heading = NestText(text, level == AutomationHeadingLevel.Level1 ? 18 : 15, strong: true);
        AutomationProperties.SetHeadingLevel(heading, level);
        return heading;
    }

    private static TextBlock NestProblem(string text)
    {
        var block = NestText(text);
        block.Foreground = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];
        AutomationProperties.SetLiveSetting(block, AutomationLiveSetting.Polite);
        return block;
    }

    private Button NestButton(string text, Func<Task> action, bool primary = false, string? description = null)
    {
        var button = new Button { Content = text, Style = (Style)Root.Resources[primary ? "PrimaryActionButton" : "SecondaryActionButton"] };
        if (description is not null) AutomationProperties.SetFullDescription(button, description);
        button.Click += async (_, _) => await action();
        return button;
    }

    private static Border NestCard(params UIElement[] children)
    {
        var stack = new StackPanel { Spacing = 6 };
        foreach (var child in children) stack.Children.Add(child);
        return new Border
        {
            Child = stack, Padding = new Thickness(14, 12, 14, 12), CornerRadius = new CornerRadius(8), BorderThickness = new Thickness(1),
            Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"],
            BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
        };
    }

    private static StackPanel NestInline(params UIElement[] children)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        foreach (var child in children) row.Children.Add(child);
        return row;
    }

    private static Grid NestLine(UIElement text, UIElement action)
    {
        var line = new Grid { ColumnSpacing = 8 };
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        line.Children.Add(text);
        Grid.SetColumn((FrameworkElement)action, 1);
        line.Children.Add(action);
        return line;
    }

    private static ScrollViewer NestScroll(UIElement content) => new() { Content = content, HorizontalScrollMode = ScrollMode.Disabled };

    // ---- Products page -------------------------------------------------------------------------

    private FrameworkElement BuildProductsPage()
    {
        var frame = PageFrame("Products", out var actions, out var body);
        actions.Children.Add(NestButton("Refresh", LoadNestsAsync));
        var tabs = new Pivot();
        var inbox = new StackPanel { Spacing = 12, Padding = new Thickness(24, 12, 24, 24) };
        inbox.Children.Add(NestHeading("Buy an Inbox"));
        inbox.Children.Add(NestText("A name and its inbox, with message storage. Register a name or manage your name plans on the website."));
        inbox.Children.Add(NestButton("Register or manage names on the website", () => OpenLinkAsync(new Uri(NestRules.WebAccount)), primary: true));
        productsContent = new StackPanel { Spacing = 12, Padding = new Thickness(24, 12, 24, 24) };
        var services = new StackPanel { Spacing = 12, Padding = new Thickness(24, 12, 24, 24) };
        services.Children.Add(NestHeading("Services"));
        services.Children.Add(NestText("Ready-to-use applications run by a Nest. Not available yet."));
        foreach (var (name, text) in NestRules.Services)
            services.Children.Add(NestCard(NestText(name, 14, strong: true), NestText(text), NestText("Coming soon", 12)));
        tabs.Items.Add(new PivotItem { Header = "Buy an Inbox", Content = NestScroll(inbox) });
        tabs.Items.Add(new PivotItem { Header = "Buy Nests", Content = NestScroll(productsContent) });
        tabs.Items.Add(new PivotItem { Header = "Services", Content = NestScroll(services) });
        tabs.SelectedIndex = 1;
        body.Children.Add(tabs);
        RenderProducts();
        _ = LoadNestsAsync();
        _ = LoadNestOrganizationsAsync();
        return frame;
    }

    private void RenderProducts()
    {
        if (productsContent is not { } content) return;
        content.Children.Clear();
        var state = nests;
        content.Children.Add(NestHeading("Buy Nests"));
        content.Children.Add(NestText("A Nest is your own AI agent with a private, encrypted workspace. You talk to it in an ordinary conversation. Buying and changing a package happens on the website."));
        if (state.Message is { } message) content.Children.Add(NestProblem(message));
        if (state.Snapshot is not { } snapshot)
        {
            content.Children.Add(NestText(state.Loading ? "Loading…" : "Your Nests could not be loaded."));
            content.Children.Add(NestButton("Refresh", LoadNestsAsync));
            return;
        }
        if (NestContextPicker() is { } picker) content.Children.Add(picker);
        if (state.Org is { } org) { RenderOrganizationNests(content, state, org); return; }
        if (snapshot.Package is not null) content.Children.Add(NestText(NestRules.AllowanceLine(snapshot, state.Packages)));
        if (snapshot.Duplicates > 0) content.Children.Add(NestProblem(NestRules.DuplicateWarning));
        foreach (var setup in snapshot.Setups)
            content.Children.Add(NestLine(NestText(NestRules.SetupLine(setup)), NestButton("Retry", () => RetryNestSetupAsync(setup), description: setup.Label)));
        if (snapshot.Nests.Count > 0)
        {
            content.Children.Add(NestHeading("Your Nests"));
            foreach (var nest in snapshot.Nests)
                content.Children.Add(NestLine(NestText($"{nest.Label} — {NestRules.StatusLine(nest).Text}"), NestButton("Open", () => OpenNestAsync(nest.Id), description: nest.Label)));
        }
        content.Children.Add(NestHeading("Packages"));
        foreach (var package in state.Packages ?? [])
            content.Children.Add(NestCard(NestText(package.Name, 14, strong: true), NestText(NestRules.PackageDetails(package)),
                NestText(snapshot.Package == package.Code ? "Your current package" : "See the price on the website", 12)));
        content.Children.Add(NestButton("Buy, upgrade or manage on the website", () => OpenLinkAsync(new Uri(NestRules.WebProducts)), primary: true));
    }

    private async Task RetryNestSetupAsync(NestSetup setup)
    {
        var scope = CurrentNestScope();
        var state = scope.State;
        if (state.Busy) return;
        state.Busy = true;
        NestSnapshot? snapshot = null;
        var done = await NestDoAsync(scope, async ct => snapshot = await postbox.RetryNestSetupAsync(setup.Id, ct), ex => NestFailed(state, ex));
        state.Busy = false;
        if (!done) return;
        state.Snapshot = snapshot;
        state.Message = null;
        UpdateNestHeader();
        RenderProducts();
    }

    // ---- organization Nests --------------------------------------------------------------------

    private ComboBox? NestContextPicker()
    {
        var state = nests;
        if (state.Orgs is not { Count: > 0 } orgs) return null;
        var picker = new ComboBox { Header = "Show Nests for", MinWidth = 220 };
        picker.Items.Add("Personal");
        foreach (var org in orgs) picker.Items.Add(org.Name);
        var index = orgs.ToList().FindIndex(o => o.Id == state.Org);
        picker.SelectedIndex = index < 0 ? 0 : index + 1;
        picker.SelectionChanged += async (_, _) =>
        {
            var chosen = picker.SelectedIndex > 0 ? orgs[picker.SelectedIndex - 1].Id : null;
            if (chosen == state.Org) return;
            state.Org = chosen;
            state.OrgData = null;
            RenderProducts();
            if (chosen is not null) await LoadOrganizationNestsAsync(state, chosen);
        };
        return picker;
    }

    private async Task LoadOrganizationNestsAsync(NestPageState state, string org)
    {
        var scope = CurrentNestScope();
        if (!ReferenceEquals(scope.State, state)) return;
        OrgNests? data = null;
        if (!await NestDoAsync(scope, async ct => data = await postbox.OrgNestsAsync(org, ct), ex => NestFailed(state, ex)) || state.Org != org) return;
        state.OrgData = data;
        RenderProducts();
    }

    private void RenderOrganizationNests(StackPanel content, NestPageState state, string org)
    {
        if (state.OrgData is not { } data)
        {
            content.Children.Add(NestText("Loading…"));
            return;
        }
        content.Children.Add(NestText(NestRules.OrgLine(data)));
        foreach (var nest in data.Nests)
            content.Children.Add(NestLine(NestText($"{nest.Label} ({NestRules.RoleLabel(nest)})"), NestButton("Open", () => OpenNestAsync(nest.Id), description: nest.Label)));
        if (data.Nests.Count == 0) content.Children.Add(NestText("No Nests you can use in this organization yet."));
        if (state.Orgs?.FirstOrDefault(o => o.Id == org) is { Writable: true })
            content.Children.Add(NestButton("New organization Nest…", () => CreateOrganizationNestAsync(state, org)));
        content.Children.Add(NestButton("Buy a package for the organization on the website", () => OpenLinkAsync(new Uri(NestRules.WebOrgProducts))));
    }

    private async Task CreateOrganizationNestAsync(NestPageState state, string org)
    {
        if (dialogOpen) return;
        var scope = CurrentNestScope();
        if (!ReferenceEquals(scope.State, state)) return;
        dialogOpen = true;
        try
        {
            var handle = new TextBox { Header = "Address in the organization", PlaceholderText = "hr", MaxLength = 128 };
            var name = new TextBox { Header = "Name", MaxLength = 128 };
            var error = NestProblem("");
            var panel = new StackPanel { Spacing = 12 };
            panel.Children.Add(handle);
            panel.Children.Add(name);
            panel.Children.Add(error);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "New organization Nest", Content = panel,
                PrimaryButtonText = "Create", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false,
            };
            void Validate(object _, TextChangedEventArgs __) => dialog.IsPrimaryButtonEnabled = handle.Text.Trim().Length > 0 && NestRules.ValidLabel(name.Text);
            handle.TextChanged += Validate;
            name.TextChanged += Validate;
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                var deferral = args.GetDeferral();
                dialog.IsPrimaryButtonEnabled = false;
                try
                {
                    var (h, n) = (handle.Text.Trim(), name.Text.Trim());
                    var done = await NestDoAsync(scope, ct => postbox.CreateOrgNestAsync(org, h, n, ct), ex => error.Text = NestRules.Explain(ex));
                    args.Cancel = !done;
                }
                finally { dialog.IsPrimaryButtonEnabled = true; deferral.Complete(); }
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary || !NestCurrent(scope)) return;
        }
        finally { dialogOpen = false; }
        AccountStatus.Text = "Nest created.";
        await LoadOrganizationNestsAsync(state, org);
    }

    // ---- a Nest's page -------------------------------------------------------------------------

    private FrameworkElement BuildNestPage()
    {
        var frame = PageFrame("Nest", out var actions, out var body);
        actions.Children.Add(NestButton("Refresh", () => nests.Selected is { } id ? LoadNestDetailAsync(nests, id) : Task.CompletedTask));
        nestContent = new StackPanel { Spacing = 12, Padding = new Thickness(24, 8, 24, 24) };
        body.Children.Add(NestScroll(nestContent));
        RenderNest();
        return frame;
    }

    private async Task OpenNestAsync(string nestId)
    {
        var state = nests;
        if (!NestsOn || !NestRules.ValidNest(nestId)) return;
        state.Selected = nestId;
        state.Detail = null;
        state.Files = null;
        state.Cards = null;
        state.Usage = null;
        state.Message = null;
        if (IsCompact) compactConversation = true;
        ShowSideView(SideView.Nest);
        await LoadNestDetailAsync(state, nestId);
    }

    private async Task LoadNestDetailAsync(NestPageState state, string nestId, bool keepMessage = false)
    {
        var scope = CurrentNestScope();
        if (!ReferenceEquals(scope.State, state)) return;
        NestRow? detail = null;
        NestFiles? files = null;
        IReadOnlyList<NestApproval>? cards = null;
        IReadOnlyList<string>? usage = null;
        var loaded = await NestDoAsync(scope, async ct =>
        {
            detail = await postbox.NestAsync(nestId, ct);
            files = await postbox.NestFilesAsync(nestId, ct);
            cards = await postbox.NestApprovalsAsync(nestId, ct);
            // Usage needs the Nests host; the rest of the page still works without it.
            try { usage = await postbox.NestUsageAsync(nestId, ct); }
            catch (Exception ex) when (!ct.IsCancellationRequested) { DesktopLog.Write("Nest usage unavailable: " + ex.GetType().Name); }
        }, ex => NestFailed(state, ex));
        if (!loaded || state.Selected != nestId) return;
        state.Detail = detail;
        state.Files = files;
        state.Cards = cards;
        state.Usage = usage;
        if (!keepMessage) state.Message = null;
        RenderNest();
    }

    private async Task RefreshNestDetailAsync(NestPageState state, string nestId)
    {
        var scope = CurrentNestScope();
        if (!ReferenceEquals(scope.State, state)) return;
        NestRow? detail = null;
        IReadOnlyList<NestApproval>? cards = null;
        if (!await NestDoAsync(scope, async ct =>
        {
            detail = await postbox.NestAsync(nestId, ct);
            cards = await postbox.NestApprovalsAsync(nestId, ct);
        }, ex => DesktopLog.Write("Nest refresh failed: " + ex.GetType().Name)) || state.Selected != nestId || state.Busy) return;
        if (detail == state.Detail && NestRules.SameCards(cards, state.Cards)) return;
        state.Detail = detail;
        state.Cards = cards;
        RenderNest();
    }

    private void RenderNest()
    {
        if (nestContent is not { } content) return;
        content.Children.Clear();
        var state = nests;
        content.Children.Add(NestInline(NestButton("Back to Products", () => { ShowSideView(SideView.Products); return Task.CompletedTask; })));
        if (state.Message is { } message) content.Children.Add(NestProblem(message));
        if (state.Detail is not { } nest)
        {
            content.Children.Add(NestText(state.Selected is null ? "This Nest is no longer available." : "Loading…"));
            return;
        }
        var (text, reason) = NestRules.StatusLine(nest);
        content.Children.Add(NestHeading(nest.Label, AutomationHeadingLevel.Level1));
        content.Children.Add(NestText("Status: " + text + (reason is null ? "" : " — " + reason)));
        if (nest.Address is { Length: > 0 } address)
            content.Children.Add(NestInline(NestButton("Open conversation", () => OpenPeerAsync(ViewModel.ResolvePeerKey(address)))));
        if (nest.IsOwner) content.Children.Add(NestOwnerActions(nest));
        content.Children.Add(NestHeading("Usage"));
        foreach (var line in state.Usage ?? ["Usage is not available right now."]) content.Children.Add(NestText(line));
        RenderNestFiles(content, nest, state);
        RenderNestApprovals(content, nest, state);
    }

    private StackPanel NestOwnerActions(NestRow nest)
    {
        var row = NestInline();
        if (NestRules.CanPause(nest)) row.Children.Add(NestButton("Pause", () => PauseNestAsync(nest, resume: false)));
        if (NestRules.CanResume(nest)) row.Children.Add(NestButton("Resume", () => PauseNestAsync(nest, resume: true)));
        row.Children.Add(NestButton("Rename…", () => RenameNestAsync(nest)));
        row.Children.Add(NestButton("Export…", () => ExportNestAsync(nest)));
        var delete = NestButton("Delete…", () => DeleteNestAsync(nest));
        delete.Foreground = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];
        row.Children.Add(delete);
        return row;
    }

    private void RenderNestFiles(StackPanel content, NestRow nest, NestPageState state)
    {
        content.Children.Add(NestHeading("Files"));
        if (state.Files is { } files)
        {
            content.Children.Add(NestText(NestRules.StorageLine(files), 12));
            foreach (var file in files.Files)
            {
                var info = new StackPanel { Spacing = 2 };
                info.Children.Add(NestText($"{file.Name} — {NestRules.CoverageText(file)}"));
                foreach (var warning in file.Warnings) info.Children.Add(NestText(warning, 12));
                var delete = new Button { Content = new SymbolIcon(Symbol.Delete), Style = (Style)Root.Resources["QuietIconButton"] };
                AutomationProperties.SetName(delete, "Delete " + file.Name);
                ToolTipService.SetToolTip(delete, "Delete file");
                delete.Click += async (_, _) => await DeleteNestFileAsync(nest, file);
                content.Children.Add(NestLine(info, delete));
            }
        }
        content.Children.Add(NestInline(NestButton("Add a file…", () => AddNestFileAsync(nest))));
    }

    private void RenderNestApprovals(StackPanel content, NestRow nest, NestPageState state)
    {
        content.Children.Add(NestHeading("Approvals"));
        var cards = NestRules.Pending(state.Cards);
        if (cards.Count == 0) content.Children.Add(NestText("Nothing is waiting for your approval."));
        foreach (var card in cards)
        {
            var text = NestRules.CardText(card);
            var approve = NestButton("Approve", () => DecideNestAsync(nest, card, true), primary: true, description: text);
            var reject = NestButton("Reject", () => DecideNestAsync(nest, card, false), description: text);
            var shown = NestText(text);
            shown.IsTextSelectionEnabled = true;
            content.Children.Add(NestCard(shown, NestInline(approve, reject)));
        }
    }

    // ---- actions -------------------------------------------------------------------------------

    // One action at a time, then the page reloads with the server's view of what happened.
    private async Task NestActionAsync(NestRow nest, Func<CancellationToken, Task> work, string done)
    {
        var scope = CurrentNestScope();
        var state = scope.State;
        if (state.Busy || state.Selected != nest.Id) return;
        state.Busy = true;
        var ok = await NestDoAsync(scope, work, ex => state.Message = NestRules.Explain(ex));
        state.Busy = false;
        if (!NestCurrent(scope)) return;
        if (ok) { AccountStatus.Text = done; state.Message = null; }
        else if (state.Message is { } problem) AccountStatus.Text = problem;
        await LoadNestDetailAsync(state, nest.Id, keepMessage: !ok);
    }

    private Task PauseNestAsync(NestRow nest, bool resume) =>
        NestActionAsync(nest, ct => postbox.PauseNestAsync(nest.Id, resume, ct), resume ? "Nest resumed." : "Nest paused.");

    private Task DecideNestAsync(NestRow nest, NestApproval card, bool approve) =>
        NestActionAsync(nest, ct => postbox.DecideNestApprovalAsync(nest.Id, card, approve, ct), approve ? "Approved and done." : "Rejected.");

    private async Task DeleteNestFileAsync(NestRow nest, NestFile file)
    {
        if (await ConfirmAsync($"Delete {file.Name}?", "The file and the text read from it are removed from the Nest.", "Delete file"))
            await NestActionAsync(nest, ct => postbox.DeleteNestFileAsync(nest.Id, file.Id, ct), "File deleted.");
    }

    private async Task RenameNestAsync(NestRow nest)
    {
        if (dialogOpen) return;
        dialogOpen = true;
        string label;
        try
        {
            var name = new TextBox { Header = "Name", Text = nest.Label, MaxLength = 128 };
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Rename Nest", Content = name,
                PrimaryButtonText = "Rename", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = NestRules.ValidLabel(nest.Label),
            };
            name.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = NestRules.ValidLabel(name.Text);
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            label = name.Text.Trim();
        }
        finally { dialogOpen = false; }
        await NestActionAsync(nest, ct => postbox.RenameNestAsync(nest.Id, label, ct), "Nest renamed.");
        nests.LoadedAt = default;
        await LoadNestsAsync();
    }

    private async Task DeleteNestAsync(NestRow nest)
    {
        if (dialogOpen) return;
        var scope = CurrentNestScope();
        var state = scope.State;
        var deleted = false;
        dialogOpen = true;
        try
        {
            var typed = new TextBox { Header = "Type the Nest’s name to confirm", PlaceholderText = nest.Label, MaxLength = 128 };
            var error = NestProblem("");
            var panel = new StackPanel { Spacing = 12 };
            panel.Children.Add(NestText($"This deletes “{nest.Label}”, its files and its conversations for good. Export first if you want to keep them."));
            panel.Children.Add(typed);
            panel.Children.Add(error);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Delete Nest", Content = panel,
                PrimaryButtonText = "Delete Nest", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false,
            };
            typed.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = NestRules.DeleteConfirmed(nest.Label, typed.Text);
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                var deferral = args.GetDeferral();
                dialog.IsPrimaryButtonEnabled = false;
                try
                {
                    deleted = await NestDoAsync(scope, ct => postbox.DeleteNestAsync(nest.Id, nest.Label, typed.Text, ct), ex => error.Text = NestRules.Explain(ex));
                    args.Cancel = !deleted;
                }
                finally { dialog.IsPrimaryButtonEnabled = true; deferral.Complete(); }
            };
            await dialog.ShowAsync();
        }
        finally { dialogOpen = false; }
        if (!deleted || !NestCurrent(scope)) return;
        AccountStatus.Text = "Nest deleted.";
        state.Selected = null;
        state.Detail = null;
        state.LoadedAt = default;
        ShowSideView(SideView.Products);
    }

    private async Task ExportNestAsync(NestRow nest)
    {
        var scope = CurrentNestScope();
        var state = scope.State;
        if (state.Busy || dialogOpen) return;
        Windows.Storage.StorageFile? destination;
        try { destination = await SaveDestinationAsync(NestRules.ExportFileName(nest.Label), picture: false); }
        catch (Exception ex) { DesktopLog.Error("Nest export picker failed", ex); return; }
        if (destination is null || !NestCurrent(scope)) return;
        state.Busy = true;
        AccountStatus.Text = "Exporting the Nest…";
        var saved = await NestDoAsync(scope, async ct =>
        {
            var bytes = await postbox.ExportNestAsync(nest.Id, ct);
            await Windows.Storage.FileIO.WriteBytesAsync(destination, bytes);
        }, ex => { state.Message = NestRules.Explain(ex); RenderNest(); });
        state.Busy = false;
        if (saved) { AccountStatus.Text = "Nest exported."; return; }
        // A failed export must not leave an empty archive that looks like a backup.
        try { await destination.DeleteAsync(); } catch (Exception ex) { DesktopLog.Write("Could not remove the partial export: " + ex.GetType().Name); }
        if (NestCurrent(scope)) AccountStatus.Text = "The Nest was not exported.";
    }

    private async Task AddNestFileAsync(NestRow nest)
    {
        var scope = CurrentNestScope();
        var state = scope.State;
        if (state.Busy || dialogOpen) return;
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        Windows.Storage.StorageFile? file;
        try { file = await picker.PickSingleFileAsync(); }
        catch (Exception ex) { DesktopLog.Error("Nest file picker failed", ex); return; }
        if (file is null || !NestCurrent(scope)) return;
        AccountStatus.Text = "Uploading…";
        await NestActionAsync(nest, async ct =>
        {
            var size = (await file.GetBasicPropertiesAsync()).Size;
            if (size == 0) throw new ArgumentException("The selected file is empty.");
            if (size > (ulong)NestRules.MaxUpload) throw new PostboxException(413, "too_large");
            var buffer = await Windows.Storage.FileIO.ReadBufferAsync(file);
            await postbox.UploadNestFileAsync(nest.Id, file.Name, buffer.ToArray(), ct);
        }, "File added.");
    }
}
