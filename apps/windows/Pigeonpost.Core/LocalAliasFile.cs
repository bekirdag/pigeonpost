using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Core;

// Names never enter the contact-permission API. Desktop injects Windows per-user DPAPI.
public sealed class LocalAliasFile(string folder, IStateProtector protector)
{
    private sealed record Stored(string Scope, Dictionary<string, string> Aliases);
    private static string Scope(string owner, string origin, string mailbox) => Convert.ToHexString(
        SHA256.HashData(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(new[] { owner, origin, mailbox })))).ToLowerInvariant();
    private string PathFor(string scope) => Path.Combine(folder, scope + ".names");
    private static void Check(Func<bool> current) { if (!current()) throw new AuthorizationRetiredException(); }
    private static void Validate(IReadOnlyDictionary<string, string> aliases)
    {
        if (aliases.Count > 1000 || aliases.Any(p => !PeopleNames.Mailbox(p.Key) || PeopleNames.Normalize(p.Value, alias: true) != p.Value))
            throw new ArgumentException("Local names could not be read.");
    }
    public async Task<Dictionary<string, string>> LoadAsync(string owner, string origin, string mailbox, Func<bool> current)
    {
        Check(current);
        var scope = Scope(owner, origin, mailbox); var path = PathFor(scope);
        if (!File.Exists(path)) return new(StringComparer.Ordinal);
        if (new FileInfo(path).Length > 1024 * 1024) throw new IOException("Local names file is too large.");
        var plain = await protector.UnprotectAsync(File.ReadAllBytes(path));
        try
        {
            Check(current);
            if (plain.Length > 512 * 1024) throw new IOException("Local names file is too large.");
            var stored = JsonSerializer.Deserialize<Stored>(plain) ?? throw new IOException("Local names could not be read.");
            if (stored.Scope != scope || stored.Aliases is null) throw new IOException("Local names belong to another inbox.");
            Validate(stored.Aliases);
            return stored.Aliases;
        }
        finally { CryptographicOperations.ZeroMemory(plain); }
    }
    public async Task SaveAsync(string owner, string origin, string mailbox, IReadOnlyDictionary<string, string> aliases, Func<bool> current)
    {
        Check(current); Validate(aliases);
        var scope = Scope(owner, origin, mailbox);
        var plain = JsonSerializer.SerializeToUtf8Bytes(new Stored(scope, new(aliases)));
        byte[] sealedBytes;
        try { sealedBytes = await protector.ProtectAsync(plain); }
        finally { CryptographicOperations.ZeroMemory(plain); }
        Check(current);
        Directory.CreateDirectory(folder);
        var path = PathFor(scope); var temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.WriteAllBytes(temporary, sealedBytes);
            Check(current);
            File.Move(temporary, path, true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }
    public void Clear()
    {
        if (Directory.Exists(folder)) Directory.Delete(folder, true);
    }
}
