using System.Diagnostics;
using System.IO.Compression;
using System.Net.Http.Headers;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace TDrive.WinUI;

public sealed class ReleaseAssetInfo
{
    [JsonPropertyName("name")]
    public string Name { get; set; } = string.Empty;

    [JsonPropertyName("size")]
    public long Size { get; set; }

    [JsonPropertyName("browser_download_url")]
    public string DownloadUrl { get; set; } = string.Empty;
}

public sealed class ReleaseUpdateInfo
{
    public string Version { get; init; } = string.Empty;
    public string TagName { get; init; } = string.Empty;
    public string HtmlUrl { get; init; } = string.Empty;
    public string ReleaseNotes { get; init; } = string.Empty;
    public ReleaseAssetInfo? Asset { get; init; }
    public bool IsInstalledMode { get; init; }
}

public static class AutoUpdater
{
    private const string LatestReleaseUrl = "https://api.github.com/repos/Coobyk/tdrive/releases/latest";
    private const string AllowedDownloadPrefix = "https://github.com/Coobyk/tdrive/releases/download/";

    private static readonly HttpClient Http = CreateClient();

    private static HttpClient CreateClient()
    {
        var client = new HttpClient { Timeout = TimeSpan.FromSeconds(15) };
        client.DefaultRequestHeaders.Accept.Add(new MediaTypeWithQualityHeaderValue("application/vnd.github+json"));
        client.DefaultRequestHeaders.UserAgent.ParseAdd($"TDrive-WinUI/{CurrentVersion()}");
        return client;
    }

    public static string CurrentVersion()
    {
        var v = NativeCore.Version();
        return string.IsNullOrWhiteSpace(v) || v == "unknown" ? "0.0.0" : v.TrimStart('v');
    }

    public static bool IsInstalledCopy() =>
        File.Exists(Path.Combine(AppContext.BaseDirectory, "Uninstall.exe"));

    public static bool IsNewerVersion(string latestRaw, string currentRaw)
    {
        if (!TryParseSemVer(latestRaw, out var latest) || !TryParseSemVer(currentRaw, out var current))
        {
            return false;
        }
        return latest > current;
    }

    private static bool TryParseSemVer(string raw, out Version version)
    {
        var clean = raw.Trim().TrimStart('v');
        var dash = clean.IndexOfAny(['-', '+']);
        if (dash >= 0) clean = clean[..dash];
        return Version.TryParse(clean, out version!);
    }

    public static async Task<ReleaseUpdateInfo?> CheckForUpdateAsync(CancellationToken ct = default)
    {
        using var resp = await Http.GetAsync(LatestReleaseUrl, ct);
        resp.EnsureSuccessStatusCode();
        await using var stream = await resp.Content.ReadAsStreamAsync(ct);
        using var doc = await JsonDocument.ParseAsync(stream, cancellationToken: ct);
        var root = doc.RootElement;

        var tag = root.TryGetProperty("tag_name", out var t) ? (t.GetString() ?? "") : "";
        var latestVer = tag.TrimStart('v');
        if (!IsNewerVersion(latestVer, CurrentVersion()))
        {
            return null;
        }

        var htmlUrl = root.TryGetProperty("html_url", out var h) ? (h.GetString() ?? "") : "";
        var body = root.TryGetProperty("body", out var b) ? (b.GetString() ?? "") : "";
        var assets = root.TryGetProperty("assets", out var a)
            ? (JsonSerializer.Deserialize<List<ReleaseAssetInfo>>(a.GetRawText()) ?? [])
            : [];

        var installed = IsInstalledCopy();
        ReleaseAssetInfo? asset = installed
            ? assets.FirstOrDefault(x => x.Name.StartsWith("tdrive-windows-x86_64-winui-setup-", StringComparison.OrdinalIgnoreCase) && x.Name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
              ?? assets.FirstOrDefault(x => x.Name.StartsWith("tdrive-windows-x86_64-winui-", StringComparison.OrdinalIgnoreCase) && x.Name.EndsWith(".zip", StringComparison.OrdinalIgnoreCase))
            : assets.FirstOrDefault(x => x.Name.StartsWith("tdrive-windows-x86_64-winui-", StringComparison.OrdinalIgnoreCase) && x.Name.EndsWith(".zip", StringComparison.OrdinalIgnoreCase))
              ?? assets.FirstOrDefault(x => x.Name.StartsWith("tdrive-windows-x86_64-winui-setup-", StringComparison.OrdinalIgnoreCase) && x.Name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase));

        return new ReleaseUpdateInfo
        {
            Version = latestVer,
            TagName = tag,
            HtmlUrl = htmlUrl,
            ReleaseNotes = body,
            Asset = asset,
            IsInstalledMode = installed
        };
    }

    public static async Task<string> DownloadUpdateAsync(
        ReleaseUpdateInfo update,
        IProgress<(long Downloaded, long Total)> progress,
        CancellationToken ct)
    {
        if (update.Asset is null)
        {
            throw new InvalidOperationException("No Windows WinUI 3 asset was attached to this release.");
        }
        if (!update.Asset.DownloadUrl.StartsWith(AllowedDownloadPrefix, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException($"Refusing untrusted download URL: {update.Asset.DownloadUrl}");
        }

        var dir = Path.Combine(Path.GetTempPath(), "tdrive-winui-update");
        Directory.CreateDirectory(dir);
        var destPath = Path.Combine(dir, update.Asset.Name);

        using var downloadClient = new HttpClient { Timeout = TimeSpan.FromMinutes(10) };
        downloadClient.DefaultRequestHeaders.UserAgent.ParseAdd($"TDrive-WinUI/{CurrentVersion()}");

        using var resp = await downloadClient.GetAsync(update.Asset.DownloadUrl, HttpCompletionOption.ResponseHeadersRead, ct);
        resp.EnsureSuccessStatusCode();
        var total = resp.Content.Headers.ContentLength ?? update.Asset.Size;

        await using (var remote = await resp.Content.ReadAsStreamAsync(ct))
        await using (var local = new FileStream(destPath, FileMode.Create, FileAccess.Write, FileShare.None, 65536, useAsync: true))
        {
            var buf = new byte[65536];
            long downloaded = 0;
            int read;
            while ((read = await remote.ReadAsync(buf, ct)) > 0)
            {
                await local.WriteAsync(buf.AsMemory(0, read), ct);
                downloaded += read;
                progress.Report((downloaded, total));
            }
        }

        var actualSize = new FileInfo(destPath).Length;
        if (update.Asset.Size > 0 && actualSize != update.Asset.Size)
        {
            File.Delete(destPath);
            throw new InvalidOperationException($"Download size mismatch: expected {update.Asset.Size} bytes, got {actualSize}.");
        }

        return destPath;
    }

    /// <summary>
    /// Spawns a detached updater script that waits for this process to exit, installs the update
    /// (either via NSIS silent upgrade or portable zip extraction), and relaunches TDrive.WinUI.exe.
    /// Caller must dispose NativeCore and exit immediately after calling this method.
    /// </summary>
    public static void LaunchInstallerAndRelaunch(string downloadedPath, ReleaseUpdateInfo update)
    {
        var pid = Environment.ProcessId;
        var appDir = AppContext.BaseDirectory.TrimEnd('\\', '/');
        var exePath = Path.Combine(appDir, "TDrive.WinUI.exe");
        var scriptDir = Path.Combine(Path.GetTempPath(), "tdrive-winui-update");
        Directory.CreateDirectory(scriptDir);
        var scriptPath = Path.Combine(scriptDir, $"apply-update-{pid}.ps1");

        string script;
        if (downloadedPath.EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
        {
            var installExe = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "Programs", "TDrive WinUI", "TDrive.WinUI.exe");
            var relaunchPath = update.IsInstalledMode ? exePath : installExe;
            script = $@"
$ErrorActionPreference = 'SilentlyContinue'
Wait-Process -Id {pid} -Timeout 30
Start-Sleep -Milliseconds 400
Start-Process -FilePath '{EscapePs(downloadedPath)}' -ArgumentList '/S' -Wait
if (Test-Path -LiteralPath '{EscapePs(relaunchPath)}') {{
    Start-Process -FilePath '{EscapePs(relaunchPath)}'
}}
Remove-Item -LiteralPath '{EscapePs(downloadedPath)}' -Force
Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force
";
        }
        else if (downloadedPath.EndsWith(".zip", StringComparison.OrdinalIgnoreCase))
        {
            var extractDir = Path.Combine(scriptDir, $"stage-{update.Version}");
            if (Directory.Exists(extractDir)) Directory.Delete(extractDir, recursive: true);
            ZipFile.ExtractToDirectory(downloadedPath, extractDir, overwriteFiles: true);

            script = $@"
$ErrorActionPreference = 'SilentlyContinue'
Wait-Process -Id {pid} -Timeout 30
Start-Sleep -Milliseconds 500
Copy-Item -Path '{EscapePs(extractDir)}\*' -Destination '{EscapePs(appDir)}' -Recurse -Force
if (Test-Path -LiteralPath '{EscapePs(exePath)}') {{
    Start-Process -FilePath '{EscapePs(exePath)}'
}}
Remove-Item -LiteralPath '{EscapePs(extractDir)}' -Recurse -Force
Remove-Item -LiteralPath '{EscapePs(downloadedPath)}' -Force
Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force
";
        }
        else
        {
            Process.Start(new ProcessStartInfo(update.HtmlUrl) { UseShellExecute = true });
            return;
        }

        File.WriteAllText(scriptPath, script);
        Process.Start(new ProcessStartInfo
        {
            FileName = "powershell.exe",
            Arguments = $"-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{scriptPath}\"",
            CreateNoWindow = true,
            UseShellExecute = false
        });
    }

    private static string EscapePs(string s) => s.Replace("'", "''");
}
