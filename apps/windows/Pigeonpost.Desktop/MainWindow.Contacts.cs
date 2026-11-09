using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace Pigeonpost.Desktop;

// docs/contacts-and-phone.md §4–§5 and §9 (Windows): Phone number, and "Find people I know" from an exported
// address book (.vcf or .csv). Nothing leaves the device without a press: the file is read when chosen, only
// truncated hashes are sent, names stay here, and each invitation is one confirmed press for one number.
public sealed partial class MainWindow
{
    private static string HomeRegion()
    {
        try { return Windows.System.UserProfile.GlobalizationPreferences.HomeGeographicRegion; }
        catch (Exception) { return System.Globalization.RegionInfo.CurrentRegion.TwoLetterISORegionName; }
    }

    private static ComboBox CountryPicker(string name)
    {
        var picker = new ComboBox { Header = "Country", HorizontalAlignment = HorizontalAlignment.Stretch };
        var guess = ContactImport.GuessCallingCode(HomeRegion());
        foreach (var (_, country, code) in ContactImport.Countries)
            picker.Items.Add(new ComboBoxItem { Content = $"{country} (+{code})", Tag = code });
        picker.SelectedIndex = Math.Max(0, ContactImport.Countries.ToList().FindIndex(c => c.Code == guess));
        AutomationProperties.SetName(picker, name);
        return picker;
    }
    private static string SelectedCode(ComboBox picker) => (picker.SelectedItem as ComboBoxItem)?.Tag as string ?? "1";
    private static Button ContactButton(string text, string? automation = null)
    {
        var button = new Button { Content = text };
        AutomationProperties.SetName(button, automation ?? text);
        return button;
    }

    private void UpdateContactMenus()
    {
        PhoneMenuItem.Visibility = ToVisibility(features.Has(ServerFeatures.PhoneNumbers));
        FindPeopleMenuItem.Visibility = ToVisibility(features.Has(ServerFeatures.ContactDiscovery));
    }

    // ---- Phone number ----------------------------------------------------------------------------

    private async void PhoneNumber_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (dialogOpen || !features.Has(ServerFeatures.PhoneNumbers) || model.CaptureNewConversationTarget() is not { } target) return;
        dialogOpen = true;
        try
        {
            var panel = new StackPanel { Spacing = 12, MaxWidth = 460 };
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(),
                Title = "Phone number", Content = new ScrollViewer { Content = panel, MaxHeight = 560 }, CloseButtonText = "Done" };
            var status = PeopleText("");
            Task<T> Run<T>(Func<string, CancellationToken, Task<T>> action) => ViewModel.WithPeopleTargetAsync(target, postbox, action);
            bool Current() => ReferenceEquals(model, ViewModel) && target.IsCurrent && features.Has(ServerFeatures.PhoneNumbers);

            async Task RenderAsync(string? message = null)
            {
                panel.Children.Clear();
                panel.Children.Add(PeopleText("Add a number so people who already have it can find this inbox, and so you can invite people by text message. Your full number is never shown, not even to you."));
                panel.Children.Add(PeopleText(target.Mailbox));
                IReadOnlyList<VerifiedPhone> numbers;
                try { numbers = await Run(postbox.GetPhonesAsync); }
                catch (OperationCanceledException) { dialog.Hide(); return; }
                catch (Exception error) { status.Text = PostboxClient.PeopleRefusal(error); panel.Children.Add(status); return; }
                if (!Current()) { dialog.Hide(); return; }
                foreach (var number in numbers)
                {
                    var row = new StackPanel { Spacing = 6 };
                    var own = number.Identity == target.Mailbox;
                    row.Children.Add(new TextBlock { Text = number.Masked, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] });
                    if (!own) { row.Children.Add(PeopleText("Linked to another of your inboxes.")); panel.Children.Add(row); continue; }
                    var find = new ToggleSwitch { Header = "Let people who have my number find me", IsOn = number.Discoverable };
                    AutomationProperties.SetName(find, "Let people who have my number find me");
                    find.Toggled += async (_, _) =>
                    {
                        if (find.IsOn == number.Discoverable) return;
                        find.IsEnabled = false;
                        try { await Run((m, ct) => postbox.SetPhoneDiscoverableAsync(m, number.Id, find.IsOn, ct)); await RenderAsync(find.IsOn ? "People who have your number can now find you." : "People can no longer find you by your number."); }
                        catch (OperationCanceledException) { dialog.Hide(); }
                        catch (Exception error) { find.IsOn = number.Discoverable; find.IsEnabled = true; status.Text = PostboxClient.PeopleRefusal(error); }
                    };
                    var unlink = ContactButton("Unlink this number");
                    unlink.Click += async (_, _) =>
                    {
                        unlink.IsEnabled = false;
                        try { await Run(async (m, ct) => { await postbox.UnlinkPhoneAsync(m, number.Id, ct); return true; }); await RenderAsync("The number was unlinked."); }
                        catch (OperationCanceledException) { dialog.Hide(); }
                        catch (Exception error) { unlink.IsEnabled = true; status.Text = PostboxClient.PeopleRefusal(error); }
                    };
                    row.Children.Add(find); row.Children.Add(unlink);
                    panel.Children.Add(row);
                }
                if (numbers.All(n => n.Identity != target.Mailbox) && numbers.Count < 3) AddVerification();
                status.Text = message ?? "";
                panel.Children.Add(status);
            }

            void AddVerification()
            {
                var country = CountryPicker("Phone country");
                var number = new TextBox { Header = "Phone number", PlaceholderText = "0532 123 45 67", MaxLength = 32, InputScope = new Microsoft.UI.Xaml.Input.InputScope { Names = { new Microsoft.UI.Xaml.Input.InputScopeName(Microsoft.UI.Xaml.Input.InputScopeNameValue.TelephoneNumber) } } };
                AutomationProperties.SetName(number, "Phone number");
                var send = ContactButton("Send code");
                var code = new TextBox { Header = "6-digit code", MaxLength = 6, Visibility = Visibility.Collapsed };
                AutomationProperties.SetName(code, "Verification code");
                var discoverable = new CheckBox { Content = "Let people who have my number find me", IsChecked = false, Visibility = Visibility.Collapsed };
                AutomationProperties.SetName(discoverable, "Let people find me after verifying");
                var verify = ContactButton("Verify"); verify.Visibility = Visibility.Collapsed;
                string? challenge = null;
                send.Click += async (_, _) =>
                {
                    var e164 = ContactImport.NormalizeE164(number.Text, SelectedCode(country));
                    if (e164 is null) { status.Text = "Enter the number with its area code, or start it with + and the country code."; return; }
                    send.IsEnabled = false;
                    try
                    {
                        var sent = await Run((m, ct) => postbox.SendPhoneCodeAsync(m, e164, ct));
                        if (!Current()) { dialog.Hide(); return; }
                        challenge = sent.Id;
                        code.Visibility = discoverable.Visibility = verify.Visibility = Visibility.Visible;
                        send.Content = "Send a new code";
                        status.Text = "We sent a code by text message. It expires in 10 minutes.";
                        code.Focus(FocusState.Programmatic);
                    }
                    catch (OperationCanceledException) { dialog.Hide(); }
                    catch (Exception error) { status.Text = PostboxClient.PeopleRefusal(error); }
                    finally { send.IsEnabled = true; }
                };
                verify.Click += async (_, _) =>
                {
                    if (challenge is null) return;
                    verify.IsEnabled = false;
                    try
                    {
                        _ = await Run((m, ct) => postbox.VerifyPhoneAsync(m, challenge, code.Text.Trim(), discoverable.IsChecked == true, ct));
                        if (!Current()) { dialog.Hide(); return; }
                        await RenderAsync("Your number is verified.");
                    }
                    catch (OperationCanceledException) { dialog.Hide(); }
                    catch (Exception error) { status.Text = PostboxClient.PeopleRefusal(error); verify.IsEnabled = true; }
                };
                panel.Children.Add(country); panel.Children.Add(number); panel.Children.Add(send);
                panel.Children.Add(code); panel.Children.Add(discoverable); panel.Children.Add(verify);
            }

            await RenderAsync();
            await dialog.ShowAsync();
        }
        catch (OperationCanceledException) { }
        finally { dialogOpen = false; }
    }

    // ---- Find people I know ------------------------------------------------------------------------

    private async Task<(string Name, string Text)?> PickContactsFileAsync()
    {
#if UI_TESTS
        if (Environment.GetEnvironmentVariable("PIGEONPOST_UI_CONTACTS_FILE") is { } fixture)
            return (Path.GetFileName(fixture), await File.ReadAllTextAsync(fixture));
#endif
        var picker = new FileOpenPicker { SuggestedStartLocation = PickerLocationId.DocumentsLibrary };
        picker.FileTypeFilter.Add(".vcf"); picker.FileTypeFilter.Add(".csv");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        if (await picker.PickSingleFileAsync() is not StorageFile file) return null;
        var size = (await file.GetBasicPropertiesAsync()).Size;
        if (size > ContactImport.MaxFileBytes) throw new ArgumentException("This file is too large. Export up to 5 MB of contacts.");
        return (file.Name, await FileIO.ReadTextAsync(file));
    }

    private async void FindPeople_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (dialogOpen || !features.Has(ServerFeatures.ContactDiscovery) || model.CaptureNewConversationTarget() is not { } target) return;
        dialogOpen = true;
        try
        {
            using var stop = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
            var panel = new StackPanel { Spacing = 12, MaxWidth = 500 };
            var results = new StackPanel { Spacing = 10 };
            var status = PeopleText("");
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(),
                Title = "Find people I know", Content = new ScrollViewer { Content = panel, MaxHeight = 600 }, CloseButtonText = "Close" };
            dialog.Closed += (_, _) => stop.Cancel();
            bool Current() => ReferenceEquals(model, ViewModel) && target.IsCurrent && features.Has(ServerFeatures.ContactDiscovery);
            Task<T> Run<T>(Func<string, CancellationToken, Task<T>> action) => ViewModel.WithPeopleTargetAsync(target, postbox, action);

            panel.Children.Add(PeopleText("Choose a contacts export (.vcf or .csv) from Google Contacts, Outlook or your phone. Pigeonpost reads it on this computer and sends only scrambled numbers to check who is here. Names, emails and notes never leave this computer, and the file is not kept."));
            panel.Children.Add(PeopleText("To stop abuse, the server keeps a short-lived record of which scrambled numbers this account checked, for up to 7 days."));
            var country = CountryPicker("Contacts country");
            panel.Children.Add(PeopleText("Numbers without a country code are read as numbers from:"));
            panel.Children.Add(country);
            var choose = ContactButton("Choose a contacts file…", "Choose a contacts file");
            panel.Children.Add(choose); panel.Children.Add(status); panel.Children.Add(results);

            choose.Click += async (_, _) =>
            {
                choose.IsEnabled = false; results.Children.Clear();
                try
                {
                    if (await PickContactsFileAsync() is not { } file) return;
                    var import = ContactImport.Read(file.Name, file.Text, SelectedCode(country));
                    if (import.Numbers.Count == 0)
                    {
                        status.Text = import.People == 0 ? "No contacts were found in this file." : $"No phone numbers could be read from {import.People} contacts.";
                        return;
                    }
                    status.Text = $"Checking {import.Numbers.Count} numbers…";
                    var canInvite = false;
                    if (features.Has(ServerFeatures.SmsInvites) && features.Has(ServerFeatures.PhoneNumbers))
                    {
                        try { canInvite = (await Run(postbox.GetPhonesAsync)).Any(p => p.Identity == target.Mailbox); }
                        catch (Exception ex) when (ex is not OperationCanceledException) { canInvite = false; }
                    }
                    var outcome = await ContactDiscovery.RunAsync(import.Numbers, (hashes, ct) => Run((m, inner) => postbox.DiscoverContactsAsync(m, hashes, inner)), stop.Token);
                    if (!Current()) { dialog.Hide(); return; }
                    var summary = $"{outcome.Found.Count} on Pigeonpost. {outcome.NoMatch.Count} with no match found.";
                    if (import.Unreadable > 0) summary += $" Couldn't read {import.Unreadable}.";
                    if (outcome.Unchecked > 0)
                        summary += outcome.Stopped == "daily" ? $" {outcome.Unchecked} were not checked: today's limit was reached. Try again tomorrow." : $" {outcome.Unchecked} were not checked. {outcome.Stopped}";
                    status.Text = summary;
                    RenderDiscovery(outcome, canInvite);
                }
                catch (OperationCanceledException) { }
                catch (Exception error) { if (Current()) status.Text = PostboxClient.PeopleRefusal(error); }
                finally { choose.IsEnabled = true; }
            };

            void RenderDiscovery(DiscoveryOutcome outcome, bool canInvite)
            {
                if (outcome.Found.Count > 0) results.Children.Add(new TextBlock { Text = "On Pigeonpost", Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"] });
                foreach (var person in outcome.Found.DistinctBy(p => p.Address))
                {
                    var row = new Grid { ColumnSpacing = 12 };
                    row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                    row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
                    var shown = person.Handle ?? (person.DisplayName is { } d ? "~" + d : person.Address);
                    row.Children.Add(PeopleText(person.Name + "\n" + shown));
                    var start = ContactButton("Start chat", $"Start chat with {person.Name}");
                    Grid.SetColumn(start, 1); row.Children.Add(start);
                    start.Click += async (_, _) =>
                    {
                        dialog.Hide(); dialogOpen = false;
                        await OpenStartedConversationAsync(model, target, person.Handle ?? person.Address, "");
                    };
                    results.Children.Add(row);
                }
                if (outcome.NoMatch.Count > 0)
                {
                    results.Children.Add(new TextBlock { Text = "No match found", Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"] });
                    results.Children.Add(PeopleText(canInvite
                        ? "They may not use Pigeonpost yet, or may not let people find them by number. You can invite one person at a time; each invitation is a text message from Pigeonpost."
                        : features.Has(ServerFeatures.SmsInvites)
                            ? "They may not use Pigeonpost yet, or may not let people find them by number. To invite them by text message, first verify your own number in Phone number."
                            : "They may not use Pigeonpost yet, or may not let people find them by number."));
                }
                foreach (var number in outcome.NoMatch)
                {
                    var row = new StackPanel { Spacing = 6 };
                    row.Children.Add(PeopleText(number.Name));
                    if (canInvite)
                    {
                        var invite = ContactButton("Invite by SMS", $"Invite {number.Name} by SMS");
                        invite.Click += (_, _) => ConfirmInvite(row, number);
                        row.Children.Add(invite);
                    }
                    results.Children.Add(row);
                }
            }

            void ConfirmInvite(StackPanel row, ImportedNumber number)
            {
                row.Children.Clear();
                row.Children.Add(PeopleText(number.Name));
                row.Children.Add(PeopleText($"{number.Name} will get this text message from Pigeonpost:"));
                var text = PeopleText(ContactImport.InviteText(null));
                row.Children.Add(text);
                var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                var send = ContactButton("Send invitation", $"Send invitation to {number.Name}");
                var cancel = ContactButton("Cancel", $"Cancel invitation to {number.Name}");
                buttons.Children.Add(send); buttons.Children.Add(cancel); row.Children.Add(buttons);
                _ = LoadSenderNameAsync();
                async Task LoadSenderNameAsync()
                {
                    if (!features.Has(ServerFeatures.DisplayNames)) return;
                    try { if (await Run(postbox.GetProfileNameAsync) is { } own) text.Text = ContactImport.InviteText(own.DisplayName); }
                    catch (Exception) { }
                }
                cancel.Click += (_, _) =>
                {
                    row.Children.Clear(); row.Children.Add(PeopleText(number.Name));
                    var invite = ContactButton("Invite by SMS", $"Invite {number.Name} by SMS");
                    invite.Click += (_, _) => ConfirmInvite(row, number);
                    row.Children.Add(invite);
                };
                send.Click += async (_, _) =>
                {
                    send.IsEnabled = cancel.IsEnabled = false;
                    string outcome;
                    try
                    {
                        var result = await Run((m, ct) => postbox.InviteBySmsAsync(m, number.Phone, ct));
                        outcome = result.Status == "sent" ? "Invitation sent."
                            : result.Reason == "already_invited" ? "You already invited this number in the last 30 days."
                            : "This number can't get an invitation right now.";
                    }
                    catch (OperationCanceledException) { return; }
                    catch (Exception error) { outcome = PostboxClient.PeopleRefusal(error); }
                    if (!Current()) return;
                    row.Children.Clear(); row.Children.Add(PeopleText(number.Name));
                    var done = PeopleText(outcome); AutomationProperties.SetName(done, $"Invitation to {number.Name}: {outcome}");
                    row.Children.Add(done);
                };
            }

            await dialog.ShowAsync();
        }
        catch (OperationCanceledException) { }
        finally { dialogOpen = false; }
    }
}
