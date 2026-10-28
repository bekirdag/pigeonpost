using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core.Meetings;

// This device's notes key (docs/meeting-summaries.md §3): a P-256 key pair whose public half is
// registered for the account, so meeting-intel can wrap a brief's content key to it. The private
// half stays on this device, sealed at rest by NotesKeyStore and bound to the person it was made for.
public sealed partial class NotesDeviceKey : IDisposable
{
    [GeneratedRegex("^[A-Za-z0-9_-]{8,64}$")] private static partial Regex KeyIdPattern();
    private byte[] privatePkcs8;
    public string KeyId { get; }
    public string PublicKey { get; } // base64 of the 65-byte uncompressed point
    internal ReadOnlySpan<byte> PrivatePkcs8 => privatePkcs8;

    private NotesDeviceKey(string keyId, string publicKey, byte[] pkcs8) { KeyId = keyId; PublicKey = publicKey; privatePkcs8 = pkcs8; }

    public static bool ValidKeyId(string? id) => id is not null && KeyIdPattern().IsMatch(id);

    public static NotesDeviceKey Create()
    {
        using var ecdh = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
        var id = "nk" + Convert.ToBase64String(RandomNumberGenerator.GetBytes(12)).TrimEnd('=').Replace('+', '-').Replace('/', '_');
        return new(id, Convert.ToBase64String(MeetingNotesCrypto.RawPublic(ecdh)), ecdh.ExportPkcs8PrivateKey());
    }

    // A stored key, checked: the id is well formed and the public half belongs to the private one.
    public static NotesDeviceKey Restore(string keyId, string publicKey, byte[] pkcs8)
    {
        if (!ValidKeyId(keyId)) throw new MeetingNotesException("bad_key");
        using var ecdh = ECDiffieHellman.Create();
        ecdh.ImportPkcs8PrivateKey(pkcs8, out _);
        if (ecdh.KeySize != 256 || Convert.ToBase64String(MeetingNotesCrypto.RawPublic(ecdh)) != publicKey) throw new MeetingNotesException("bad_key");
        return new(keyId, publicKey, pkcs8.ToArray());
    }

    public void Dispose()
    {
        CryptographicOperations.ZeroMemory(privatePkcs8);
        privatePkcs8 = [];
    }

    public override string ToString() => "NotesDeviceKey(" + KeyId + ")";
}

public static class MeetingNotesCrypto
{
    public const string NotesInfo = "pigeonpost meeting notes v1|";
    public const int MaxCiphertextBytes = 4 * 1024 * 1024;

    internal static byte[] RawPublic(ECDiffieHellman key)
    {
        var p = key.ExportParameters(false);
        if (p.Q.X is not { Length: 32 } x || p.Q.Y is not { Length: 32 } y) throw new CryptographicException("Not a P-256 key.");
        return [0x04, .. x, .. y];
    }

    private static ECDiffieHellman ImportPublic(byte[] raw)
    {
        if (raw.Length != 65 || raw[0] != 0x04) throw new MeetingNotesException("bad_brief");
        return ECDiffieHellman.Create(new ECParameters { Curve = ECCurve.NamedCurves.nistP256, Q = new ECPoint { X = raw[1..33], Y = raw[33..] } });
    }

    private static byte[] Base64(string? text, int length = 0, int max = 0)
    {
        if (text is null || max > 0 && text.Length > (max + 2) / 3 * 4) throw new MeetingNotesException("bad_brief");
        byte[] bytes;
        try { bytes = Convert.FromBase64String(text); } catch (FormatException) { throw new MeetingNotesException("bad_brief"); }
        if (length > 0 && bytes.Length != length) throw new MeetingNotesException("bad_brief");
        return bytes;
    }

    private static byte[] Kek(byte[] shared, string meetingId, string keyId) =>
        HKDF.DeriveKey(HashAlgorithmName.SHA256, shared, 32, Encoding.UTF8.GetBytes(meetingId), Encoding.UTF8.GetBytes(NotesInfo + keyId));

    // Opens a ready brief from GET /v1/meetings/{id} with this device's key.
    public static MeetingBrief Open(MeetingRecord meeting, NotesDeviceKey key)
    {
        if (meeting.State != "ready") throw new MeetingNotesException("not_ready");
        if (meeting.Alg != "A256GCM") throw new MeetingNotesException("bad_brief");
        var wrap = meeting.Keys?.FirstOrDefault(k => k.KeyId == key.KeyId) ?? throw new MeetingNotesException("no_key_for_device");
        var epk = Base64(wrap.Epk, 65);
        var wrapIv = Base64(wrap.Iv, 12);
        var wrapped = Base64(wrap.Wrapped, 48);
        var iv = Base64(meeting.Iv, 12);
        var ciphertext = Base64(meeting.Ciphertext, max: MaxCiphertextBytes);
        if (ciphertext.Length < 17) throw new MeetingNotesException("bad_brief");
        byte[] shared = [], kek = [], cek = new byte[32], plain = [];
        try
        {
            using (var mine = ECDiffieHellman.Create())
            using (var peer = ImportPublic(epk))
            {
                mine.ImportPkcs8PrivateKey(key.PrivatePkcs8, out _);
                shared = mine.DeriveRawSecretAgreement(peer.PublicKey);
            }
            kek = Kek(shared, meeting.Id, wrap.KeyId);
            using (var gcm = new AesGcm(kek, 16))
                gcm.Decrypt(wrapIv, wrapped.AsSpan(0, 32), wrapped.AsSpan(32), cek, Encoding.UTF8.GetBytes(meeting.Id + "|" + wrap.KeyId));
            plain = new byte[ciphertext.Length - 16];
            using (var gcm = new AesGcm(cek, 16))
                gcm.Decrypt(iv, ciphertext.AsSpan(0, plain.Length), ciphertext.AsSpan(plain.Length), plain, Encoding.UTF8.GetBytes(meeting.Id));
            return MeetingBrief.Parse(plain, meeting.Id);
        }
        catch (CryptographicException) { throw new MeetingNotesException("bad_brief"); }
        catch (JsonException) { throw new MeetingNotesException("bad_brief"); }
        finally
        {
            CryptographicOperations.ZeroMemory(shared);
            CryptographicOperations.ZeroMemory(kek);
            CryptographicOperations.ZeroMemory(cek);
            CryptographicOperations.ZeroMemory(plain);
        }
    }

    // What meeting-intel does (meeting-intel/src/crypto.js sealBrief), for the stand-in postbox and
    // tests: one content key, wrapped for each notes key given.
    public static (string Iv, string Ciphertext, IReadOnlyList<MeetingWrap> Wraps) Seal(string meetingId, byte[] plaintext, IEnumerable<(string KeyId, string PublicKey)> recipients)
    {
        var cek = RandomNumberGenerator.GetBytes(32);
        try
        {
            var iv = RandomNumberGenerator.GetBytes(12);
            var sealedBytes = new byte[plaintext.Length + 16];
            using (var gcm = new AesGcm(cek, 16))
                gcm.Encrypt(iv, plaintext, sealedBytes.AsSpan(0, plaintext.Length), sealedBytes.AsSpan(plaintext.Length), Encoding.UTF8.GetBytes(meetingId));
            var wraps = new List<MeetingWrap>();
            foreach (var (keyId, publicKey) in recipients)
            {
                using var peer = ImportPublic(Convert.FromBase64String(publicKey));
                using var eph = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
                var shared = eph.DeriveRawSecretAgreement(peer.PublicKey);
                var kek = Kek(shared, meetingId, keyId);
                var wrapIv = RandomNumberGenerator.GetBytes(12);
                var wrapped = new byte[48];
                using (var gcm = new AesGcm(kek, 16))
                    gcm.Encrypt(wrapIv, cek, wrapped.AsSpan(0, 32), wrapped.AsSpan(32), Encoding.UTF8.GetBytes(meetingId + "|" + keyId));
                CryptographicOperations.ZeroMemory(shared); CryptographicOperations.ZeroMemory(kek);
                wraps.Add(new MeetingWrap { KeyId = keyId, Epk = Convert.ToBase64String(RawPublic(eph)), Iv = Convert.ToBase64String(wrapIv), Wrapped = Convert.ToBase64String(wrapped) });
            }
            return (Convert.ToBase64String(iv), Convert.ToBase64String(sealedBytes), wraps);
        }
        finally { CryptographicOperations.ZeroMemory(cek); }
    }
}
