using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace TDrive.WinUI;

public sealed class NativeCore : IDisposable
{
    private const string DllName = "tdrive_core.dll";

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr tdrive_core_version();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr tdrive_core_load_config();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr tdrive_core_save_config(byte[] jsonUtf8);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr tdrive_core_spawn();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr tdrive_core_dispatch(byte[] jsonUtf8);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr tdrive_core_poll(int timeoutMs);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void tdrive_core_shutdown();

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    private static extern void tdrive_core_free_string(IntPtr ptr);

    private readonly CancellationTokenSource _cts = new();
    private Task? _pollTask;
    private bool _disposed;

    public event Action<JsonDocument>? EventReceived;

    private static byte[] ToUtf8NullTerminated(string s)
    {
        var bytes = Encoding.UTF8.GetBytes(s);
        var buf = new byte[bytes.Length + 1];
        Buffer.BlockCopy(bytes, 0, buf, 0, bytes.Length);
        return buf;
    }

    private static string? TakeOwnedString(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return null;
        try
        {
            return Marshal.PtrToStringUTF8(ptr);
        }
        finally
        {
            tdrive_core_free_string(ptr);
        }
    }

    public static string Version()
    {
        var ptr = tdrive_core_version();
        return ptr == IntPtr.Zero ? "unknown" : (Marshal.PtrToStringUTF8(ptr) ?? "unknown");
    }

    public static (AppConfigDto? Config, string? Error) LoadConfig()
    {
        var raw = TakeOwnedString(tdrive_core_load_config());
        if (string.IsNullOrEmpty(raw)) return (null, "empty response");
        using var doc = JsonDocument.Parse(raw);
        var root = doc.RootElement;
        if (!root.GetProperty("ok").GetBoolean())
        {
            return (null, root.TryGetProperty("err", out var err) ? err.GetString() : "unknown error");
        }
        if (root.TryGetProperty("config", out var cfgEl) && cfgEl.ValueKind == JsonValueKind.Object)
        {
            var cfg = JsonSerializer.Deserialize<AppConfigDto>(cfgEl.GetRawText());
            return (cfg, null);
        }
        return (null, null);
    }

    public static (bool Ok, string? Error) SaveConfig(AppConfigDto config)
    {
        var json = JsonSerializer.Serialize(config);
        var raw = TakeOwnedString(tdrive_core_save_config(ToUtf8NullTerminated(json)));
        return ParseStatus(raw);
    }

    public (bool Ok, string? Error) Start()
    {
        var raw = TakeOwnedString(tdrive_core_spawn());
        var status = ParseStatus(raw);
        if (!status.Ok) return status;

        _pollTask ??= Task.Run(PollLoop, _cts.Token);
        return (true, null);
    }

    private void PollLoop()
    {
        while (!_cts.IsCancellationRequested)
        {
            var raw = TakeOwnedString(tdrive_core_poll(100));
            if (string.IsNullOrEmpty(raw)) continue;
            try
            {
                var doc = JsonDocument.Parse(raw);
                EventReceived?.Invoke(doc);
            }
            catch
            {
                // Ignore malformed event payloads rather than killing the pump.
            }
        }
    }

    public (bool Ok, string? Error) Dispatch(object payload)
    {
        var json = JsonSerializer.Serialize(payload);
        var raw = TakeOwnedString(tdrive_core_dispatch(ToUtf8NullTerminated(json)));
        return ParseStatus(raw);
    }

    private static (bool Ok, string? Error) ParseStatus(string? raw)
    {
        if (string.IsNullOrEmpty(raw)) return (false, "empty response from tdrive_core");
        using var doc = JsonDocument.Parse(raw);
        var ok = doc.RootElement.TryGetProperty("ok", out var okProp) && okProp.GetBoolean();
        var err = doc.RootElement.TryGetProperty("err", out var errProp) ? errProp.GetString() : null;
        return (ok, err);
    }

    public void Connect(int apiId, string apiHash) =>
        Dispatch(new { cmd = "Connect", api_id = apiId, api_hash = apiHash });

    public void RequestCode(string phone) => Dispatch(new { cmd = "RequestCode", phone });
    public void SubmitCode(string code) => Dispatch(new { cmd = "SubmitCode", code });
    public void SubmitPassword(string password) => Dispatch(new { cmd = "SubmitPassword", password });
    public void ListChannels() => Dispatch(new { cmd = "ListChannels" });
    public void CreateChannel(string title) => Dispatch(new { cmd = "CreateChannel", title });
    public void CreateSharedChannel(string title) => Dispatch(new { cmd = "CreateSharedChannel", title });

    public void OpenChannel(ChannelDto ch) => Dispatch(new
    {
        cmd = "OpenChannel",
        id = ch.Id,
        access_hash = ch.AccessHash,
        title = ch.Title,
        kind = ch.Kind
    });

    public void Sync() => Dispatch(new { cmd = "Sync" });
    public void UploadItems(IEnumerable<string> items, string parent, string? wrap = null) =>
        Dispatch(new { cmd = "UploadItems", items = items.ToArray(), parent, wrap });
    public void Download(string fileId, string dest) =>
        Dispatch(new { cmd = "Download", file_id = fileId, dest });
    public void Mkdir(string parent, string name) => Dispatch(new { cmd = "Mkdir", parent, name });
    public void Rename(string obj, string name) => Dispatch(new { cmd = "Rename", obj, name });
    public void Move(string obj, string parent) => Dispatch(new { cmd = "Move", obj, parent });
    public void Copy(string obj, string parent) => Dispatch(new { cmd = "Copy", obj, parent });
    public void Delete(string obj) => Dispatch(new { cmd = "Delete", obj });
    public void Restore(string obj) => Dispatch(new { cmd = "Restore", obj });
    public void Purge(string obj) => Dispatch(new { cmd = "Purge", obj });
    public void EmptyTrash() => Dispatch(new { cmd = "EmptyTrash" });
    public void OptimizeDrive() => Dispatch(new { cmd = "OptimizeDrive" });
    public void OptimizeApply() => Dispatch(new { cmd = "OptimizeApply" });
    public void CancelTransfer(string id) => Dispatch(new { cmd = "Cancel", id });
    public void PauseTransfer(string id) => Dispatch(new { cmd = "Pause", id });
    public void ResumeTransfer(string id) => Dispatch(new { cmd = "Resume", id });
    public void ResumeUploads() => Dispatch(new { cmd = "ResumeUploads" });
    public void SetUploadLimit(int n) => Dispatch(new { cmd = "SetUploadLimit", n });
    public void UnlockVault(string password) => Dispatch(new { cmd = "Unlock", password });
    public void LockVault() => Dispatch(new { cmd = "Lock" });
    public void EnableEncryption(string password, string hint) =>
        Dispatch(new { cmd = "EnableEncryption", password, hint });
    public void JoinInvite(string link) => Dispatch(new { cmd = "JoinInvite", link });
    public void FetchThumbnail(string fileId) => Dispatch(new { cmd = "FetchThumbnail", file_id = fileId });
    public void FetchPreview(string fileId) => Dispatch(new { cmd = "FetchPreview", file_id = fileId });
    public void Adopt(long msgId, string parent) => Dispatch(new { cmd = "Adopt", msg_id = msgId, parent });
    public void Stream(string obj) => Dispatch(new { cmd = "Stream", obj });
    public void ListAccounts() => Dispatch(new { cmd = "ListAccounts" });
    public void SwitchAccount(string id) => Dispatch(new { cmd = "SwitchAccount", id });
    public void AddAccount() => Dispatch(new { cmd = "AddAccount" });
    public void RemoveAccount(string id) => Dispatch(new { cmd = "RemoveAccount", id });
    public void SignOut() => Dispatch(new { cmd = "SignOut" });

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _cts.Cancel();
        try { _pollTask?.Wait(TimeSpan.FromMilliseconds(300)); } catch { }
        tdrive_core_shutdown();
        _cts.Dispose();
    }
}
