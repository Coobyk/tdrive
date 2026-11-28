using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Text.Json.Serialization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media.Imaging;

namespace TDrive.WinUI;

public sealed class AppConfigDto
{
    [JsonPropertyName("api_id")]
    public int ApiId { get; set; }

    [JsonPropertyName("api_hash")]
    public string ApiHash { get; set; } = string.Empty;

    [JsonPropertyName("last_channel_id")]
    public long? LastChannelId { get; set; }

    [JsonPropertyName("download_dir")]
    public string? DownloadDir { get; set; }

    [JsonPropertyName("upload_limit")]
    public int UploadLimit { get; set; } = 3;

    [JsonPropertyName("grid_view")]
    public bool GridView { get; set; }
}

public sealed class ChannelDto
{
    [JsonPropertyName("id")]
    public long Id { get; set; }

    [JsonPropertyName("access_hash")]
    public long AccessHash { get; set; }

    [JsonPropertyName("title")]
    public string Title { get; set; } = string.Empty;

    [JsonPropertyName("kind")]
    public string Kind { get; set; } = "broadcast";

    [JsonPropertyName("is_personal")]
    public bool IsPersonal { get; set; }

    public string KindLabel => IsPersonal ? "Personal Drive" : "Shared Drive";
    public string IconGlyph => IsPersonal ? "\uE753" : "\uE902";
}

public sealed class AccountDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = string.Empty;

    [JsonPropertyName("label")]
    public string Label { get; set; } = string.Empty;

    [JsonPropertyName("drives")]
    public List<string> Drives { get; set; } = [];

    [JsonIgnore]
    public bool IsActive { get; set; }

    public string StatusBadge => IsActive ? "Active" : string.Empty;
    public string DrivesSummary => Drives.Count == 0
        ? "No cached drives"
        : string.Join(", ", Drives);
}

public sealed class FolderDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = string.Empty;

    [JsonPropertyName("name")]
    public string Name { get; set; } = string.Empty;

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = string.Empty;

    [JsonPropertyName("tombstoned")]
    public bool Tombstoned { get; set; }

    [JsonPropertyName("revision")]
    public long Revision { get; set; }
}

public sealed class FileDto
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = string.Empty;

    [JsonPropertyName("msg_id")]
    public long MsgId { get; set; }

    [JsonPropertyName("name")]
    public string Name { get; set; } = string.Empty;

    [JsonPropertyName("size")]
    public long Size { get; set; }

    [JsonPropertyName("logical_size")]
    public long LogicalSize { get; set; }

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = string.Empty;

    [JsonPropertyName("upload_time")]
    public long UploadTime { get; set; }

    [JsonPropertyName("tombstoned")]
    public bool Tombstoned { get; set; }

    [JsonPropertyName("encrypted")]
    public bool Encrypted { get; set; }

    [JsonPropertyName("part_count")]
    public int PartCount { get; set; }

    [JsonPropertyName("revision")]
    public long Revision { get; set; }

    [JsonPropertyName("has_thumb")]
    public bool HasThumb { get; set; }

    [JsonPropertyName("has_preview")]
    public bool HasPreview { get; set; }

    [JsonPropertyName("is_image")]
    public bool IsImage { get; set; }

    [JsonPropertyName("is_video")]
    public bool IsVideo { get; set; }
}

public sealed class TrashDto
{
    [JsonPropertyName("object_id")]
    public string ObjectId { get; set; } = string.Empty;

    [JsonPropertyName("is_folder")]
    public bool IsFolder { get; set; }

    [JsonPropertyName("original_parent")]
    public string OriginalParent { get; set; } = string.Empty;

    [JsonPropertyName("original_name")]
    public string OriginalName { get; set; } = string.Empty;

    [JsonPropertyName("deleted_at")]
    public long DeletedAt { get; set; }

    [JsonPropertyName("purge_after")]
    public long PurgeAfter { get; set; }

    public string IconGlyph => IsFolder ? "\uE8B7" : "\uE8A5";
    public string KindLabel => IsFolder ? "Folder" : "File";
    public string DeletedLabel => DeletedAt > 0
        ? DateTimeOffset.FromUnixTimeSeconds(DeletedAt).ToLocalTime().ToString("yyyy-MM-dd HH:mm")
        : "";
}

public sealed class UnadoptedDto
{
    [JsonPropertyName("msg_id")]
    public long MsgId { get; set; }

    [JsonPropertyName("name")]
    public string Name { get; set; } = string.Empty;

    [JsonPropertyName("size")]
    public long Size { get; set; }

    [JsonPropertyName("sender_id")]
    public long SenderId { get; set; }

    [JsonPropertyName("date")]
    public long Date { get; set; }

    public string SizeText => FormatBytes(Size);

    public static string FormatBytes(long bytes)
    {
        if (bytes < 0) return "0 B";
        string[] units = ["B", "KiB", "MiB", "GiB", "TiB"];
        double value = bytes;
        int idx = 0;
        while (value >= 1024.0 && idx < units.Length - 1)
        {
            value /= 1024.0;
            idx++;
        }
        return idx == 0 ? $"{bytes} B" : $"{value:0.#} {units[idx]}";
    }
}

public sealed class BreadcrumbSegment
{
    public string Id { get; init; } = string.Empty;
    public string Name { get; init; } = string.Empty;
    public override string ToString() => Name;
}

public sealed class DriveEntryViewModel : INotifyPropertyChanged
{
    private BitmapImage? _thumbnail;

    public string Id { get; init; } = string.Empty;
    public string Name { get; init; } = string.Empty;
    public string Parent { get; init; } = string.Empty;
    public bool IsFolder { get; init; }
    public long LogicalSize { get; init; }
    public long UploadTime { get; init; }
    public bool Encrypted { get; init; }
    public bool HasThumb { get; init; }
    public bool HasPreview { get; init; }
    public bool IsImage { get; init; }
    public bool IsVideo { get; init; }
    public int PartCount { get; init; }

    public string SizeText => IsFolder ? "Folder" : UnadoptedDto.FormatBytes(LogicalSize);

    public string DateText => UploadTime > 0
        ? DateTimeOffset.FromUnixTimeSeconds(UploadTime).ToLocalTime().ToString("yyyy-MM-dd HH:mm")
        : string.Empty;

    public string BadgesText
    {
        get
        {
            var parts = new List<string>();
            if (Encrypted) parts.Add("Encrypted");
            if (PartCount > 1) parts.Add($"{PartCount} parts");
            return string.Join(" · ", parts);
        }
    }

    public string IconGlyph
    {
        get
        {
            if (IsFolder) return "\uE8B7";
            if (IsImage) return "\uE91B";
            if (IsVideo) return "\uE714";
            var ext = Path.GetExtension(Name).ToLowerInvariant();
            return ext switch
            {
                ".mp3" or ".flac" or ".wav" or ".ogg" or ".m4a" => "\uE8D6",
                ".zip" or ".7z" or ".tar" or ".gz" or ".rar" => "\uF012",
                ".pdf" or ".doc" or ".docx" or ".txt" or ".md" => "\uE8A5",
                _ => "\uE7C3"
            };
        }
    }

    public BitmapImage? Thumbnail
    {
        get => _thumbnail;
        set
        {
            _thumbnail = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(ThumbnailVisibility));
            OnPropertyChanged(nameof(GlyphVisibility));
        }
    }

    public Visibility ThumbnailVisibility => _thumbnail is not null ? Visibility.Visible : Visibility.Collapsed;
    public Visibility GlyphVisibility => _thumbnail is null ? Visibility.Visible : Visibility.Collapsed;

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

public sealed class TransferViewModel : INotifyPropertyChanged
{
    private string _label = string.Empty;
    private long _sent;
    private long _total;
    private string _state = "active";

    public string Id { get; init; } = string.Empty;

    public string Label
    {
        get => _label;
        set { _label = value; OnPropertyChanged(); }
    }

    public long Sent
    {
        get => _sent;
        set { _sent = value; NotifyProgress(); }
    }

    public long Total
    {
        get => _total;
        set { _total = value; NotifyProgress(); }
    }

    public string State
    {
        get => _state;
        set
        {
            _state = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(CanPause));
            OnPropertyChanged(nameof(CanResume));
            OnPropertyChanged(nameof(CanCancel));
        }
    }

    public double ProgressPercent => Total > 0 ? Math.Clamp(Sent * 100.0 / Total, 0.0, 100.0) : (State == "done" ? 100.0 : 0.0);

    public string StatusText => State switch
    {
        "done" => $"Done · {UnadoptedDto.FormatBytes(Total)}",
        "paused" => $"Paused · {UnadoptedDto.FormatBytes(Sent)} / {UnadoptedDto.FormatBytes(Total)}",
        "failed" => "Failed",
        _ => $"{UnadoptedDto.FormatBytes(Sent)} / {UnadoptedDto.FormatBytes(Total)} ({ProgressPercent:0}%)"
    };

    public Visibility CanPause => State == "active" ? Visibility.Visible : Visibility.Collapsed;
    public Visibility CanResume => State == "paused" ? Visibility.Visible : Visibility.Collapsed;
    public Visibility CanCancel => State is "active" or "paused" ? Visibility.Visible : Visibility.Collapsed;

    private void NotifyProgress()
    {
        OnPropertyChanged(nameof(Sent));
        OnPropertyChanged(nameof(Total));
        OnPropertyChanged(nameof(ProgressPercent));
        OnPropertyChanged(nameof(StatusText));
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
