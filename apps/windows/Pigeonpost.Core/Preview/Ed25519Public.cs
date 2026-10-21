using System.Numerics;
using System.Security.Cryptography;

namespace Pigeonpost.Core.Preview;

// The ed25519 public key of a 32-byte seed (RFC 8032 §5.1.5), for the stand-in postbox only: it
// signs device attestations for its demonstration mailboxes as the real postbox does for hosted
// ones, and needs the mailbox public key to hand back. Affine arithmetic on BigInteger: slow and
// not constant-time, which is fine for a test fixture and never used with a real key.
internal static class Ed25519Public
{
    private static readonly BigInteger Q = BigInteger.Pow(2, 255) - 19;
    private static readonly BigInteger D = Mod(-121665 * Inv(121666));
    private static readonly BigInteger I = BigInteger.ModPow(2, (Q - 1) / 4, Q);
    private static readonly (BigInteger X, BigInteger Y) B = Base();

    public static byte[] FromSeed(byte[] seed)
    {
        if (seed.Length != 32) throw new ArgumentException("seed must be 32 bytes", nameof(seed));
        var h = SHA512.HashData(seed);
        var a = h[..32];
        a[0] &= 248; a[31] &= 127; a[31] |= 64;
        var scalar = new BigInteger(a, isUnsigned: true, isBigEndian: false);
        var (x, y) = Multiply(B, scalar);
        var encoded = y.ToByteArray(isUnsigned: true, isBigEndian: false);
        var output = new byte[32];
        encoded.CopyTo(output, 0);
        if (!x.IsEven) output[31] |= 0x80;
        return output;
    }

    private static BigInteger Mod(BigInteger value) => (value % Q + Q) % Q;
    private static BigInteger Inv(BigInteger value) => BigInteger.ModPow(Mod(value), Q - 2, Q);

    private static (BigInteger, BigInteger) Base()
    {
        var y = Mod(4 * Inv(5));
        var xx = Mod((y * y - 1) * Inv(D * y * y + 1));
        var x = BigInteger.ModPow(xx, (Q + 3) / 8, Q);
        if (Mod(x * x - xx) != 0) x = Mod(x * I);
        if (!x.IsEven) x = Q - x;
        return (x, y);
    }

    private static (BigInteger, BigInteger) Add((BigInteger X, BigInteger Y) p, (BigInteger X, BigInteger Y) q)
    {
        var t = Mod(D * p.X * q.X * p.Y * q.Y);
        var x = Mod((p.X * q.Y + q.X * p.Y) * Inv(1 + t));
        var y = Mod((p.Y * q.Y + p.X * q.X) * Inv(1 - t));
        return (x, y);
    }

    private static (BigInteger, BigInteger) Multiply((BigInteger, BigInteger) point, BigInteger scalar)
    {
        (BigInteger, BigInteger) result = (0, 1);
        var addend = point;
        while (scalar > 0)
        {
            if (!scalar.IsEven) result = Add(result, addend);
            addend = Add(addend, addend);
            scalar >>= 1;
        }
        return result;
    }
}
