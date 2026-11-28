using System.Collections.ObjectModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics;
using Windows.Storage;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace TDrive.WinUI;

public sealed partial class MainWindow : Window
{
    private readonly NativeCore _core = new();
    private readonly ObservableCollection<DriveEntryViewModel> _visibleEntries = [];
    private readonly ObservableCollection<BreadcrumbSegment> _breadcrumbs = [];
    private readonly ObservableCollection<ChannelDto> _channels = [];
    private readonly ObservableCollection<TrashDto> _trash = [];
    private readonly ObservableCollection<UnadoptedDto> _unadopted = [];
    private readonly ObservableCollection<AccountDto> _accounts = [];
    private readonly ObservableCollection<TransferViewModel> _transfers = [];

    private readonly Dictionary<string, FolderDto> _foldersById = [];
    private readonly Dictionary<string, FileDto> _filesById = [];
    private readonly Dictionary<string, TransferViewModel> _transfersById = [];
    private readonly Dictionary<string, string> _thumbPaths = [];
    private readonly HashSet<string> _thumbRequested = [];

    private AppConfigDto? _config;
    private ChannelDto? _currentChannel;
    private string _cwd = string.Empty;
    private string _searchQuery = string.Empty;
    private bool _gridView;
    private bool _sortBySize;
    private bool _vaultConfigured;
    private bool _vaultUnlocked;
    private string _vaultHint = string.Empty;
    private string? _davUrl;
    private string? _pendingCopyText;

    public MainWindow()
    {
        InitializeComponent();

        ConfigureWindowChrome();

        EntriesListView.ItemsSource = _visibleEntries;
        EntriesGridView.ItemsSource = _visibleEntries;
        PathBreadcrumbs.ItemsSource = _breadcrumbs;
        DrivesListView.ItemsSource = _channels;
        TrashListView.ItemsSource = _trash;
        UnadoptedListView.ItemsSource = _unadopted;
        AccountsListView.ItemsSource = _accounts;
        TransfersListView.ItemsSource = _transfers;

        _core.EventReceived += OnCoreEvent;
        Closed += (_, _) => _core.Dispose();

        BootstrapCore();
    }

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);

    private void ConfigureWindowChrome()
    {
        Title = "TDrive";
        SystemBackdrop = new MicaBackdrop();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);

        var hwnd = WindowNative.GetWindowHandle(this);
        var windowId = Win32Interop.GetWindowIdFromWindow(hwnd);
        var appWindow = AppWindow.GetFromWindowId(windowId);
        var scale = Math.Max(1.0, GetDpiForWindow(hwnd) / 96.0);
        appWindow.Resize(new SizeInt32((int)Math.Round(1140 * scale), (int)Math.Round(740 * scale)));

        var icoPath = Path.Combine(AppContext.BaseDirectory, "Assets", "tdrive.ico");
        if (File.Exists(icoPath))
        {
            appWindow.SetIcon(icoPath);
            TitleBarIcon.Source = new BitmapImage(new Uri(icoPath));
        }
    }

    private void BootstrapCore()
    {
        var (ok, err) = _core.Start();
        if (!ok)
        {
            ShowInfo("Unable to start TDrive core", err ?? "Another TDrive instance may already be running.", InfoBarSeverity.Error);
            ConnectingLabel.Text = err ?? "Failed to start worker.";
            return;
        }

        var (cfg, cfgErr) = NativeCore.LoadConfig();
        if (cfgErr is not null)
        {
            ShowInfo("Configuration error", cfgErr, InfoBarSeverity.Warning);
        }

        _config = cfg;
        if (_config is null || _config.ApiId <= 0 || string.IsNullOrWhiteSpace(_config.ApiHash))
        {
            ShowOnboardingPanel(SetupPanel);
            return;
        }

        _gridView = _config.GridView;
        ApplyViewMode();
        ShowOnboardingPanel(ConnectingPanel, "Connecting to Telegram...");
        _core.SetUploadLimit(Math.Clamp(_config.UploadLimit, 1, 16));
        _core.Connect(_config.ApiId, _config.ApiHash);
        _core.ListAccounts();
    }

    private void ShowOnboardingPanel(UIElement panel, string? connectingText = null)
    {
        NavView.Visibility = Visibility.Collapsed;
        OnboardingGrid.Visibility = Visibility.Visible;
        SetupPanel.Visibility = panel == SetupPanel ? Visibility.Visible : Visibility.Collapsed;
        ConnectingPanel.Visibility = panel == ConnectingPanel ? Visibility.Visible : Visibility.Collapsed;
        AuthPhonePanel.Visibility = panel == AuthPhonePanel ? Visibility.Visible : Visibility.Collapsed;
        AuthCodePanel.Visibility = panel == AuthCodePanel ? Visibility.Visible : Visibility.Collapsed;
        AuthPasswordPanel.Visibility = panel == AuthPasswordPanel ? Visibility.Visible : Visibility.Collapsed;
        if (connectingText is not null)
        {
            ConnectingLabel.Text = connectingText;
        }
    }

    private void ShowMainShell(string navTag)
    {
        OnboardingGrid.Visibility = Visibility.Collapsed;
        NavView.Visibility = Visibility.Visible;
        foreach (var item in NavView.MenuItems.OfType<NavigationViewItem>())
        {
            if (item.Tag as string == navTag)
            {
                NavView.SelectedItem = item;
                break;
            }
        }
        SwitchPage(navTag);
    }

    private void SwitchPage(string tag)
    {
        DrivePage.Visibility = tag == "drive" ? Visibility.Visible : Visibility.Collapsed;
        DrivesPage.Visibility = tag == "drives" ? Visibility.Visible : Visibility.Collapsed;
        TrashPage.Visibility = tag == "trash" ? Visibility.Visible : Visibility.Collapsed;
        UnadoptedPage.Visibility = tag == "unadopted" ? Visibility.Visible : Visibility.Collapsed;
        AccountsPage.Visibility = tag == "accounts" ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ShowInfo(string title, string message, InfoBarSeverity severity, string? copyText = null)
    {
        StatusInfoBar.Title = title;
        StatusInfoBar.Message = message;
        StatusInfoBar.Severity = severity;
        _pendingCopyText = copyText;
        InfoBarActionBtn.Visibility = copyText is not null ? Visibility.Visible : Visibility.Collapsed;
        StatusInfoBar.IsOpen = true;
    }

    private void OnCoreEvent(JsonDocument doc)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            using (doc)
            {
                HandleCoreEvent(doc.RootElement);
            }
        });
    }

    private void HandleCoreEvent(JsonElement root)
    {
        if (!root.TryGetProperty("event", out var evProp)) return;
        var kind = evProp.GetString();

        switch (kind)
        {
            case "Connected":
                var authorized = root.GetProperty("authorized").GetBoolean();
                if (authorized)
                {
                    ConnectingLabel.Text = "Loading drives...";
                    _core.ListChannels();
                }
                else
                {
                    ShowOnboardingPanel(AuthPhonePanel);
                }
                break;

            case "CodeSent":
                ShowOnboardingPanel(AuthCodePanel);
                CodeBox.Focus(FocusState.Programmatic);
                break;

            case "NeedPassword":
                var hint = root.TryGetProperty("hint", out var h) ? h.GetString() : "";
                PasswordHintLabel.Text = string.IsNullOrWhiteSpace(hint)
                    ? "Enter your Telegram cloud password."
                    : $"Enter your Telegram cloud password (hint: {hint}).";
                ShowOnboardingPanel(AuthPasswordPanel);
                PasswordBox.Focus(FocusState.Programmatic);
                break;

            case "SignedIn":
                var name = root.TryGetProperty("name", out var n) ? n.GetString() : "";
                ShowInfo("Signed in", string.IsNullOrWhiteSpace(name) ? "Welcome to TDrive." : $"Signed in as {name}.", InfoBarSeverity.Success);
                ShowOnboardingPanel(ConnectingPanel, "Loading drives...");
                _core.ListAccounts();
                _core.ListChannels();
                break;

            case "Channels":
                _channels.Clear();
                if (root.TryGetProperty("channels", out var chArr))
                {
                    var list = JsonSerializer.Deserialize<List<ChannelDto>>(chArr.GetRawText()) ?? [];
                    foreach (var ch in list) _channels.Add(ch);

                    if (_currentChannel is null && _config?.LastChannelId is long lastId)
                    {
                        var match = list.FirstOrDefault(c => c.Id == lastId);
                        if (match is not null)
                        {
                            OpenChannel(match);
                            break;
                        }
                    }
                }
                if (_currentChannel is null)
                {
                    ShowMainShell("drives");
                }
                break;

            case "ChannelOpened":
                var info = JsonSerializer.Deserialize<ChannelDto>(root.GetProperty("info").GetRawText());
                var pending = root.TryGetProperty("pending_uploads", out var p) ? p.GetInt32() : 0;
                if (info is not null)
                {
                    _currentChannel = info;
                    _cwd = string.Empty;
                    _thumbRequested.Clear();
                    TitleSubtitle.Text = $"· {info.Title}";
                    if (_config is not null && _config.LastChannelId != info.Id)
                    {
                        _config.LastChannelId = info.Id;
                        NativeCore.SaveConfig(_config);
                    }
                    ShowMainShell("drive");
                    RebuildVisibleEntries();
                    if (pending > 0)
                    {
                        ShowInfo("Pending uploads", $"{pending} interrupted upload(s) can be resumed from the toolbar.", InfoBarSeverity.Informational);
                    }
                }
                break;

            case "Namespace":
                ApplyNamespace(root);
                break;

            case "Transfer":
                ApplyTransfer(root);
                break;

            case "VaultState":
                _vaultConfigured = root.GetProperty("configured").GetBoolean();
                _vaultUnlocked = root.GetProperty("unlocked").GetBoolean();
                _vaultHint = root.TryGetProperty("hint", out var vh) ? (vh.GetString() ?? "") : "";
                UpdateVaultBadge();
                break;

            case "SyncState":
                var active = root.GetProperty("active").GetBoolean();
                SyncRing.IsActive = active;
                SyncRing.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
                break;

            case "Status":
                var msg = root.GetProperty("message").GetString() ?? "";
                if (!string.IsNullOrWhiteSpace(msg))
                {
                    ShowInfo("Status", msg, InfoBarSeverity.Informational);
                }
                break;

            case "Error":
                var ctx = root.TryGetProperty("context", out var c) ? c.GetString() : "Error";
                var errMsg = root.TryGetProperty("message", out var m) ? m.GetString() : "Unknown error";
                ShowInfo($"Error ({ctx})", errMsg ?? "", InfoBarSeverity.Error);
                break;

            case "InviteLink":
                var link = root.GetProperty("link").GetString() ?? "";
                ShowInfo("Invite link", link, InfoBarSeverity.Success, copyText: link);
                break;

            case "ThumbnailReady":
                var tid = root.GetProperty("file_id").GetString() ?? "";
                var tpath = root.GetProperty("path").GetString() ?? "";
                if (!string.IsNullOrEmpty(tid) && File.Exists(tpath))
                {
                    _thumbPaths[tid] = tpath;
                    var vm = _visibleEntries.FirstOrDefault(e => e.Id == tid);
                    if (vm is not null)
                    {
                        vm.Thumbnail = new BitmapImage(new Uri(tpath));
                    }
                }
                break;

            case "PreviewReady":
                var ppath = root.GetProperty("path").GetString() ?? "";
                var pid = root.GetProperty("file_id").GetString() ?? "";
                if (File.Exists(ppath))
                {
                    _ = ShowImagePreviewDialogAsync(pid, ppath);
                }
                break;

            case "PreviewFailed":
                ShowInfo("Preview unavailable", "Could not fetch a preview for that file.", InfoBarSeverity.Warning);
                break;

            case "StreamUrl":
                var streamUrl = root.GetProperty("url").GetString() ?? "";
                if (!string.IsNullOrEmpty(streamUrl))
                {
                    ShowInfo("Stream ready", streamUrl, InfoBarSeverity.Success, copyText: streamUrl);
                    try
                    {
                        Process.Start(new ProcessStartInfo(streamUrl) { UseShellExecute = true });
                    }
                    catch { }
                }
                break;

            case "DavUrl":
                _davUrl = root.GetProperty("url").GetString();
                break;

            case "OptimizePlan":
                var dead = root.GetProperty("dead").GetInt32();
                var scanned = root.GetProperty("scanned").GetInt32();
                _ = ShowOptimizeDialogAsync(dead, scanned);
                break;

            case "Accounts":
                _accounts.Clear();
                var activeId = root.TryGetProperty("active", out var a) && a.ValueKind == JsonValueKind.String
                    ? a.GetString()
                    : null;
                if (root.TryGetProperty("accounts", out var accArr))
                {
                    var list = JsonSerializer.Deserialize<List<AccountDto>>(accArr.GetRawText()) ?? [];
                    foreach (var acc in list)
                    {
                        acc.IsActive = acc.Id == activeId;
                        _accounts.Add(acc);
                    }
                }
                break;

            case "SignedOut":
                _currentChannel = null;
                _cwd = string.Empty;
                TitleSubtitle.Text = string.Empty;
                ShowOnboardingPanel(AuthPhonePanel);
                break;
        }
    }

    private void ApplyNamespace(JsonElement root)
    {
        _foldersById.Clear();
        _filesById.Clear();
        _trash.Clear();
        _unadopted.Clear();

        if (root.TryGetProperty("folders", out var fArr))
        {
            foreach (var f in JsonSerializer.Deserialize<List<FolderDto>>(fArr.GetRawText()) ?? [])
            {
                _foldersById[f.Id] = f;
            }
        }
        if (root.TryGetProperty("files", out var fileArr))
        {
            foreach (var f in JsonSerializer.Deserialize<List<FileDto>>(fileArr.GetRawText()) ?? [])
            {
                _filesById[f.Id] = f;
            }
        }
        if (root.TryGetProperty("trash", out var tArr))
        {
            foreach (var t in JsonSerializer.Deserialize<List<TrashDto>>(tArr.GetRawText()) ?? [])
            {
                _trash.Add(t);
            }
        }
        if (root.TryGetProperty("unadopted", out var uArr))
        {
            foreach (var u in JsonSerializer.Deserialize<List<UnadoptedDto>>(uArr.GetRawText()) ?? [])
            {
                _unadopted.Add(u);
            }
        }

        if (!string.IsNullOrEmpty(_cwd) && (!_foldersById.TryGetValue(_cwd, out var cur) || cur.Tombstoned))
        {
            _cwd = string.Empty;
        }

        RebuildVisibleEntries();
    }

    private void ApplyTransfer(JsonElement root)
    {
        var id = root.GetProperty("id").GetString() ?? "";
        var label = root.GetProperty("label").GetString() ?? "";
        var sent = root.GetProperty("sent").GetInt64();
        var total = root.GetProperty("total").GetInt64();
        var state = root.GetProperty("state").GetString() ?? "active";

        if (!_transfersById.TryGetValue(id, out var vm))
        {
            vm = new TransferViewModel { Id = id };
            _transfersById[id] = vm;
            _transfers.Insert(0, vm);
        }
        vm.Label = label;
        vm.Sent = sent;
        vm.Total = total;
        vm.State = state;

        var activeCount = _transfers.Count(t => t.State is "active" or "paused");
        TransfersHeaderText.Text = activeCount > 0
            ? $"Transfers ({activeCount} active)"
            : $"Transfers ({_transfers.Count})";
        TransfersExpander.Visibility = _transfers.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateVaultBadge()
    {
        if (!_vaultConfigured)
        {
            VaultNavItem.Content = "Encrypt drive";
            VaultActionBtn.Label = "Enable encryption";
        }
        else if (_vaultUnlocked)
        {
            VaultNavItem.Content = "Vault unlocked";
            VaultActionBtn.Label = "Lock vault";
        }
        else
        {
            VaultNavItem.Content = "Vault locked";
            VaultActionBtn.Label = "Unlock vault";
        }
    }

    private void RebuildVisibleEntries()
    {
        RebuildBreadcrumbs();

        var q = _searchQuery.Trim();
        var folders = _foldersById.Values
            .Where(f => !f.Tombstoned && f.Parent == _cwd)
            .Where(f => string.IsNullOrEmpty(q) || f.Name.Contains(q, StringComparison.OrdinalIgnoreCase))
            .OrderBy(f => f.Name, StringComparer.OrdinalIgnoreCase)
            .Select(f => new DriveEntryViewModel
            {
                Id = f.Id,
                Name = f.Name,
                Parent = f.Parent,
                IsFolder = true
            });

        var filesQuery = _filesById.Values
            .Where(f => !f.Tombstoned && f.Parent == _cwd)
            .Where(f => string.IsNullOrEmpty(q) || f.Name.Contains(q, StringComparison.OrdinalIgnoreCase));

        filesQuery = _sortBySize
            ? filesQuery.OrderByDescending(f => f.LogicalSize).ThenBy(f => f.Name, StringComparer.OrdinalIgnoreCase)
            : filesQuery.OrderBy(f => f.Name, StringComparer.OrdinalIgnoreCase);

        var files = filesQuery.Select(f =>
        {
            var vm = new DriveEntryViewModel
            {
                Id = f.Id,
                Name = f.Name,
                Parent = f.Parent,
                IsFolder = false,
                LogicalSize = f.LogicalSize,
                UploadTime = f.UploadTime,
                Encrypted = f.Encrypted,
                HasThumb = f.HasThumb,
                HasPreview = f.HasPreview,
                IsImage = f.IsImage,
                IsVideo = f.IsVideo,
                PartCount = f.PartCount
            };
            if (_thumbPaths.TryGetValue(f.Id, out var path) && File.Exists(path))
            {
                vm.Thumbnail = new BitmapImage(new Uri(path));
            }
            else if ((f.HasThumb || f.IsImage) && _thumbRequested.Add(f.Id))
            {
                _core.FetchThumbnail(f.Id);
            }
            return vm;
        });

        _visibleEntries.Clear();
        foreach (var item in folders) _visibleEntries.Add(item);
        foreach (var item in files) _visibleEntries.Add(item);

        var folderCount = _visibleEntries.Count(e => e.IsFolder);
        var fileCount = _visibleEntries.Count - folderCount;
        var totalBytes = _visibleEntries.Where(e => !e.IsFolder).Sum(e => e.LogicalSize);
        FolderStatsLabel.Text = $"{folderCount} folder(s) · {fileCount} file(s) · {UnadoptedDto.FormatBytes(totalBytes)}";

        EmptyFolderPanel.Visibility = _visibleEntries.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        UpFolderBtn.IsEnabled = !string.IsNullOrEmpty(_cwd);
    }

    private void RebuildBreadcrumbs()
    {
        var chain = new List<BreadcrumbSegment>();
        var cursor = _cwd;
        var guard = 0;
        while (!string.IsNullOrEmpty(cursor) && guard++ < 64 && _foldersById.TryGetValue(cursor, out var folder))
        {
            chain.Add(new BreadcrumbSegment { Id = folder.Id, Name = folder.Name });
            cursor = folder.Parent;
        }
        chain.Add(new BreadcrumbSegment { Id = string.Empty, Name = _currentChannel?.Title ?? "My Drive" });
        chain.Reverse();

        _breadcrumbs.Clear();
        foreach (var seg in chain) _breadcrumbs.Add(seg);
    }

    private void OpenChannel(ChannelDto ch)
    {
        _currentChannel = ch;
        _core.OpenChannel(ch);
    }

    private void ApplyViewMode()
    {
        GridToggleBtn.IsChecked = _gridView;
        EntriesListView.Visibility = _gridView ? Visibility.Collapsed : Visibility.Visible;
        EntriesGridView.Visibility = _gridView ? Visibility.Visible : Visibility.Collapsed;
    }

    private IReadOnlyList<DriveEntryViewModel> SelectedEntries()
    {
        var source = _gridView ? EntriesGridView.SelectedItems : EntriesListView.SelectedItems;
        return source.OfType<DriveEntryViewModel>().ToList();
    }

    // ── Onboarding handlers ───────────────────────────────────────────────

    private void SaveSetup_Click(object sender, RoutedEventArgs e)
    {
        if (!int.TryParse(ApiIdBox.Text.Trim(), out var apiId) || apiId <= 0)
        {
            ShowInfo("Invalid API ID", "API ID must be a positive integer from my.telegram.org.", InfoBarSeverity.Warning);
            return;
        }
        var apiHash = ApiHashBox.Text.Trim();
        if (string.IsNullOrWhiteSpace(apiHash))
        {
            ShowInfo("Missing API Hash", "Enter your Telegram API Hash.", InfoBarSeverity.Warning);
            return;
        }

        _config = new AppConfigDto { ApiId = apiId, ApiHash = apiHash, UploadLimit = 3 };
        var (ok, err) = NativeCore.SaveConfig(_config);
        if (!ok)
        {
            ShowInfo("Could not save config", err ?? "Unknown error", InfoBarSeverity.Error);
            return;
        }
        ShowOnboardingPanel(ConnectingPanel, "Connecting to Telegram...");
        _core.Connect(apiId, apiHash);
    }

    private void SetupBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) SaveSetup_Click(sender, e);
    }

    private void RequestCode_Click(object sender, RoutedEventArgs e)
    {
        var phone = PhoneBox.Text.Trim();
        if (string.IsNullOrEmpty(phone)) return;
        ShowOnboardingPanel(ConnectingPanel, "Sending verification code...");
        _core.RequestCode(phone);
    }

    private void PhoneBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) RequestCode_Click(sender, e);
    }

    private void SubmitCode_Click(object sender, RoutedEventArgs e)
    {
        var code = CodeBox.Text.Trim();
        if (string.IsNullOrEmpty(code)) return;
        ShowOnboardingPanel(ConnectingPanel, "Verifying login code...");
        _core.SubmitCode(code);
    }

    private void CodeBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) SubmitCode_Click(sender, e);
    }

    private void SubmitPassword_Click(object sender, RoutedEventArgs e)
    {
        var pw = PasswordBox.Password;
        if (string.IsNullOrEmpty(pw)) return;
        ShowOnboardingPanel(ConnectingPanel, "Checking cloud password...");
        _core.SubmitPassword(pw);
    }

    private void PasswordBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) SubmitPassword_Click(sender, e);
    }

    // ── Navigation & toolbar handlers ─────────────────────────────────────

    private void NavView_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is not NavigationViewItem item) return;
        var tag = item.Tag as string ?? "drive";
        switch (tag)
        {
            case "vault":
                _ = HandleVaultActionAsync();
                RestoreDriveNavSelection();
                break;
            case "settings":
                _ = ShowSettingsDialogAsync();
                RestoreDriveNavSelection();
                break;
            case "accounts":
                _core.ListAccounts();
                SwitchPage("accounts");
                break;
            case "drives":
                _core.ListChannels();
                SwitchPage("drives");
                break;
            default:
                SwitchPage(tag);
                break;
        }
    }

    private void RestoreDriveNavSelection()
    {
        var targetTag = _currentChannel is not null ? "drive" : "drives";
        foreach (var item in NavView.MenuItems.OfType<NavigationViewItem>())
        {
            if (item.Tag as string == targetTag)
            {
                NavView.SelectedItem = item;
                break;
            }
        }
    }

    private void InfoBarActionBtn_Click(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrEmpty(_pendingCopyText)) return;
        var dp = new DataPackage();
        dp.SetText(_pendingCopyText);
        Clipboard.SetContent(dp);
    }

    private void PathBreadcrumbs_ItemClicked(BreadcrumbBar sender, BreadcrumbBarItemClickedEventArgs args)
    {
        if (args.Item is BreadcrumbSegment seg)
        {
            _cwd = seg.Id;
            RebuildVisibleEntries();
        }
    }

    private void UpFolder_Click(object sender, RoutedEventArgs e)
    {
        if (!string.IsNullOrEmpty(_cwd) && _foldersById.TryGetValue(_cwd, out var folder))
        {
            _cwd = folder.Parent;
            RebuildVisibleEntries();
        }
    }

    private void SearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        _searchQuery = sender.Text;
        RebuildVisibleEntries();
    }

    private void ToggleView_Click(object sender, RoutedEventArgs e)
    {
        _gridView = GridToggleBtn.IsChecked == true;
        ApplyViewMode();
        if (_config is not null)
        {
            _config.GridView = _gridView;
            NativeCore.SaveConfig(_config);
        }
    }

    private void ToggleSort_Click(object sender, RoutedEventArgs e)
    {
        _sortBySize = !_sortBySize;
        SortBtn.Label = _sortBySize ? "Sort: Size" : "Sort: Name";
        RebuildVisibleEntries();
    }

    private void Sync_Click(object sender, RoutedEventArgs e) => _core.Sync();
    private void OptimizeDrive_Click(object sender, RoutedEventArgs e) => _core.OptimizeDrive();
    private void ResumeUploads_Click(object sender, RoutedEventArgs e) => _core.ResumeUploads();
    private void VaultAction_Click(object sender, RoutedEventArgs e) => _ = HandleVaultActionAsync();

    // ── File / Folder pickers & Drag-and-Drop ─────────────────────────────

    private async void UploadFiles_Click(object sender, RoutedEventArgs e)
    {
        if (_currentChannel is null) return;
        var picker = new FileOpenPicker();
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(this));
        picker.FileTypeFilter.Add("*");
        var picked = await picker.PickMultipleFilesAsync();
        if (picked is null || picked.Count == 0) return;
        _core.UploadItems(picked.Select(f => f.Path), _cwd);
    }

    private async void UploadFolder_Click(object sender, RoutedEventArgs e)
    {
        if (_currentChannel is null) return;
        var picker = new FolderPicker();
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(this));
        picker.FileTypeFilter.Add("*");
        var folder = await picker.PickSingleFolderAsync();
        if (folder is null) return;
        _core.UploadItems([folder.Path], _cwd);
    }

    private void DrivePage_DragOver(object sender, DragEventArgs e)
    {
        if (_currentChannel is not null && e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.Copy;
            e.DragUIOverride.Caption = "Upload to TDrive";
        }
    }

    private async void DrivePage_Drop(object sender, DragEventArgs e)
    {
        if (_currentChannel is null || !e.DataView.Contains(StandardDataFormats.StorageItems)) return;
        var items = await e.DataView.GetStorageItemsAsync();
        var paths = items.Select(i => i.Path).Where(p => !string.IsNullOrEmpty(p)).ToList();
        if (paths.Count > 0)
        {
            _core.UploadItems(paths, _cwd);
        }
    }

    // ── Entry interaction (double click & context menu) ───────────────────

    private void Entries_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        var item = SelectedEntries().FirstOrDefault();
        if (item is null) return;
        if (item.IsFolder)
        {
            _cwd = item.Id;
            RebuildVisibleEntries();
        }
        else if (item.IsImage || item.HasPreview)
        {
            _core.FetchPreview(item.Id);
        }
        else if (item.IsVideo)
        {
            _core.Stream(item.Id);
        }
        else
        {
            _ = DownloadEntryAsync(item);
        }
    }

    private void Entries_RightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        if ((e.OriginalSource as FrameworkElement)?.DataContext is DriveEntryViewModel clicked)
        {
            var selected = SelectedEntries();
            if (!selected.Any(s => s.Id == clicked.Id))
            {
                if (_gridView) EntriesGridView.SelectedItem = clicked;
                else EntriesListView.SelectedItem = clicked;
            }
        }

        var targets = SelectedEntries();
        if (targets.Count == 0) return;
        var primary = targets[0];

        var menu = new MenuFlyout();
        if (targets.Count == 1 && !primary.IsFolder)
        {
            var dl = new MenuFlyoutItem { Text = "Download...", Icon = new SymbolIcon(Symbol.Download) };
            dl.Click += (_, _) => _ = DownloadEntryAsync(primary);
            menu.Items.Add(dl);

            if (primary.IsImage || primary.HasPreview)
            {
                var prev = new MenuFlyoutItem { Text = "Preview", Icon = new SymbolIcon(Symbol.Pictures) };
                prev.Click += (_, _) => _core.FetchPreview(primary.Id);
                menu.Items.Add(prev);
            }

            var stream = new MenuFlyoutItem { Text = "Stream", Icon = new SymbolIcon(Symbol.Play) };
            stream.Click += (_, _) => _core.Stream(primary.Id);
            menu.Items.Add(stream);
            menu.Items.Add(new MenuFlyoutSeparator());
        }

        if (targets.Count == 1)
        {
            var rename = new MenuFlyoutItem { Text = "Rename", Icon = new SymbolIcon(Symbol.Rename) };
            rename.Click += (_, _) => _ = ShowRenameDialogAsync(primary);
            menu.Items.Add(rename);
        }

        var move = new MenuFlyoutItem { Text = "Move to...", Icon = new SymbolIcon(Symbol.MoveToFolder) };
        move.Click += (_, _) => _ = ShowMoveOrCopyDialogAsync(targets, isCopy: false);
        menu.Items.Add(move);

        var copy = new MenuFlyoutItem { Text = "Copy to...", Icon = new SymbolIcon(Symbol.Copy) };
        copy.Click += (_, _) => _ = ShowMoveOrCopyDialogAsync(targets, isCopy: true);
        menu.Items.Add(copy);

        menu.Items.Add(new MenuFlyoutSeparator());

        var delete = new MenuFlyoutItem { Text = targets.Count == 1 ? "Move to Trash" : $"Move {targets.Count} items to Trash", Icon = new SymbolIcon(Symbol.Delete) };
        delete.Click += (_, _) => _ = ShowDeleteConfirmAsync(targets);
        menu.Items.Add(delete);

        menu.ShowAt(sender as FrameworkElement, e.GetPosition(sender as UIElement));
    }

    private async Task DownloadEntryAsync(DriveEntryViewModel entry)
    {
        var picker = new FileSavePicker();
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(this));
        picker.SuggestedFileName = entry.Name;
        var ext = Path.GetExtension(entry.Name);
        picker.FileTypeChoices.Add("File", [string.IsNullOrEmpty(ext) ? "." : ext]);
        var dest = await picker.PickSaveFileAsync();
        if (dest is null) return;
        _core.Download(entry.Id, dest.Path);
    }

    // ── Dialogs ───────────────────────────────────────────────────────────

    private async void NewFolder_Click(object sender, RoutedEventArgs e)
    {
        var box = new TextBox { Header = "Folder name", PlaceholderText = "New folder" };
        var dialog = new ContentDialog
        {
            Title = "Create folder",
            Content = box,
            PrimaryButtonText = "Create",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(box.Text))
        {
            _core.Mkdir(_cwd, box.Text.Trim());
        }
    }

    private async Task ShowRenameDialogAsync(DriveEntryViewModel entry)
    {
        var box = new TextBox { Header = "New name", Text = entry.Name };
        var dialog = new ContentDialog
        {
            Title = "Rename",
            Content = box,
            PrimaryButtonText = "Rename",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(box.Text))
        {
            _core.Rename(entry.Id, box.Text.Trim());
        }
    }

    private async Task ShowMoveOrCopyDialogAsync(IReadOnlyList<DriveEntryViewModel> entries, bool isCopy)
    {
        var destinations = new List<BreadcrumbSegment>
        {
            new() { Id = string.Empty, Name = "/ (Drive root)" }
        };
        foreach (var f in _foldersById.Values.Where(f => !f.Tombstoned).OrderBy(f => f.Name, StringComparer.OrdinalIgnoreCase))
        {
            destinations.Add(new BreadcrumbSegment { Id = f.Id, Name = f.Name });
        }

        var combo = new ComboBox
        {
            Header = "Destination folder",
            ItemsSource = destinations,
            SelectedIndex = 0,
            HorizontalAlignment = HorizontalAlignment.Stretch
        };
        var dialog = new ContentDialog
        {
            Title = isCopy ? $"Copy {entries.Count} item(s)" : $"Move {entries.Count} item(s)",
            Content = combo,
            PrimaryButtonText = isCopy ? "Copy" : "Move",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary && combo.SelectedItem is BreadcrumbSegment dest)
        {
            foreach (var entry in entries)
            {
                if (isCopy) _core.Copy(entry.Id, dest.Id);
                else _core.Move(entry.Id, dest.Id);
            }
        }
    }

    private async Task ShowDeleteConfirmAsync(IReadOnlyList<DriveEntryViewModel> entries)
    {
        var label = entries.Count == 1 ? $"\"{entries[0].Name}\"" : $"{entries.Count} items";
        var dialog = new ContentDialog
        {
            Title = "Move to Trash?",
            Content = $"Move {label} to Trash? You can restore them later from the Trash tab.",
            PrimaryButtonText = "Move to Trash",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            foreach (var entry in entries) _core.Delete(entry.Id);
        }
    }

    private async Task HandleVaultActionAsync()
    {
        if (!_vaultConfigured)
        {
            var pw = new PasswordBox { Header = "Vault passphrase" };
            var hint = new TextBox { Header = "Hint (optional)", PlaceholderText = "Stored unencrypted in the channel" };
            var stack = new StackPanel { Spacing = 10 };
            stack.Children.Add(new TextBlock
            {
                Text = "Client-side ChaCha20-Poly1305 encryption protects future uploads with a passphrase only you know.",
                TextWrapping = TextWrapping.Wrap
            });
            stack.Children.Add(pw);
            stack.Children.Add(hint);

            var dialog = new ContentDialog
            {
                Title = "Enable drive encryption",
                Content = stack,
                PrimaryButtonText = "Enable",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
                XamlRoot = Content.XamlRoot
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrEmpty(pw.Password))
            {
                _core.EnableEncryption(pw.Password, hint.Text.Trim());
            }
        }
        else if (_vaultUnlocked)
        {
            _core.LockVault();
        }
        else
        {
            var pw = new PasswordBox { Header = "Vault passphrase" };
            var stack = new StackPanel { Spacing = 10 };
            if (!string.IsNullOrWhiteSpace(_vaultHint))
            {
                stack.Children.Add(new TextBlock { Text = $"Hint: {_vaultHint}", TextWrapping = TextWrapping.Wrap });
            }
            stack.Children.Add(pw);

            var dialog = new ContentDialog
            {
                Title = "Unlock encrypted drive",
                Content = stack,
                PrimaryButtonText = "Unlock",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
                XamlRoot = Content.XamlRoot
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrEmpty(pw.Password))
            {
                _core.UnlockVault(pw.Password);
            }
        }
    }

    private async Task ShowOptimizeDialogAsync(int dead, int scanned)
    {
        if (dead == 0)
        {
            ShowInfo("Drive is clean", $"Scanned {scanned} channel message(s); no orphaned messages found.", InfoBarSeverity.Success);
            return;
        }
        var dialog = new ContentDialog
        {
            Title = "Optimize drive",
            Content = $"Scanned {scanned} messages and found {dead} obsolete/orphaned message(s). Permanently delete them from Telegram?",
            PrimaryButtonText = "Delete dead messages",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            _core.OptimizeApply();
        }
    }

    private async Task ShowImagePreviewDialogAsync(string fileId, string path)
    {
        var title = _filesById.TryGetValue(fileId, out var f) ? f.Name : "Preview";
        var img = new Image
        {
            Source = new BitmapImage(new Uri(path)),
            Stretch = Stretch.Uniform,
            MaxHeight = 520,
            MaxWidth = 760
        };
        var dialog = new ContentDialog
        {
            Title = title,
            Content = img,
            CloseButtonText = "Close",
            XamlRoot = Content.XamlRoot
        };
        await dialog.ShowAsync();
    }

    private async Task ShowSettingsDialogAsync()
    {
        var limitBox = new NumberBox
        {
            Header = "Max concurrent uploads (1–16)",
            Minimum = 1,
            Maximum = 16,
            Value = _config?.UploadLimit ?? 3,
            SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline
        };

        var stack = new StackPanel { Spacing = 12 };
        stack.Children.Add(limitBox);
        stack.Children.Add(new TextBlock
        {
            Text = $"Rust core version: {NativeCore.Version()}",
            FontSize = 12
        });
        if (!string.IsNullOrEmpty(_davUrl))
        {
            stack.Children.Add(new TextBox
            {
                Header = "Local WebDAV URL",
                Text = _davUrl,
                IsReadOnly = true
            });
        }

        var dialog = new ContentDialog
        {
            Title = "Settings",
            Content = stack,
            PrimaryButtonText = "Save",
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };

        if (await dialog.ShowAsync() == ContentDialogResult.Primary && _config is not null)
        {
            var n = Math.Clamp((int)Math.Round(limitBox.Value), 1, 16);
            _config.UploadLimit = n;
            NativeCore.SaveConfig(_config);
            _core.SetUploadLimit(n);
        }
    }

    // ── Drives, Trash, Unadopted, Accounts, Transfers handlers ────────────

    private void DrivesListView_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is ChannelDto ch) OpenChannel(ch);
    }

    private void RefreshDrives_Click(object sender, RoutedEventArgs e) => _core.ListChannels();

    private async void CreatePersonalDrive_Click(object sender, RoutedEventArgs e)
    {
        var box = new TextBox { Header = "Drive title", PlaceholderText = "My TDrive" };
        var dialog = new ContentDialog
        {
            Title = "Create personal drive",
            Content = box,
            PrimaryButtonText = "Create",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(box.Text))
        {
            _core.CreateChannel(box.Text.Trim());
        }
    }

    private async void CreateSharedDrive_Click(object sender, RoutedEventArgs e)
    {
        var box = new TextBox { Header = "Shared drive title", PlaceholderText = "Team Drive" };
        var dialog = new ContentDialog
        {
            Title = "Create shared drive",
            Content = box,
            PrimaryButtonText = "Create",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(box.Text))
        {
            _core.CreateSharedChannel(box.Text.Trim());
        }
    }

    private async void JoinInvite_Click(object sender, RoutedEventArgs e)
    {
        var box = new TextBox { Header = "Telegram invite link", PlaceholderText = "https://t.me/+..." };
        var dialog = new ContentDialog
        {
            Title = "Join shared drive",
            Content = box,
            PrimaryButtonText = "Join",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary && !string.IsNullOrWhiteSpace(box.Text))
        {
            _core.JoinInvite(box.Text.Trim());
        }
    }

    private async void EmptyTrash_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new ContentDialog
        {
            Title = "Empty Trash?",
            Content = "Permanently remove all items currently in Trash? This cannot be undone.",
            PrimaryButtonText = "Empty Trash",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = Content.XamlRoot
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary) _core.EmptyTrash();
    }

    private void RestoreTrashItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id }) _core.Restore(id);
    }

    private void PurgeTrashItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id }) _core.Purge(id);
    }

    private void AdoptItem_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: long msgId }) _core.Adopt(msgId, _cwd);
    }

    private void AddAccount_Click(object sender, RoutedEventArgs e)
    {
        _currentChannel = null;
        _core.AddAccount();
        ShowOnboardingPanel(AuthPhonePanel);
    }

    private void SignOut_Click(object sender, RoutedEventArgs e) => _core.SignOut();

    private void SwitchAccount_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id })
        {
            _currentChannel = null;
            ShowOnboardingPanel(ConnectingPanel, "Switching account...");
            _core.SwitchAccount(id);
        }
    }

    private void RemoveAccount_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id }) _core.RemoveAccount(id);
    }

    private void PauseTransfer_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id }) _core.PauseTransfer(id);
    }

    private void ResumeTransfer_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id }) _core.ResumeTransfer(id);
    }

    private void CancelTransfer_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string id }) _core.CancelTransfer(id);
    }

    private void ClearFinishedTransfers_Click(object sender, RoutedEventArgs e)
    {
        var finished = _transfers.Where(t => t.State is "done" or "failed").ToList();
        foreach (var item in finished)
        {
            _transfers.Remove(item);
            _transfersById.Remove(item.Id);
        }
        if (_transfers.Count == 0)
        {
            TransfersExpander.Visibility = Visibility.Collapsed;
        }
    }
}
