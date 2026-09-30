using System.Runtime.InteropServices;
using Pigeonpost.Core;
using Windows.Security.Credentials;

namespace Pigeonpost.Desktop;

public sealed class WindowsTokenStore : IRefreshTokenStore
{
    private const string Resource = "Pigeonpost.Windows.RefreshToken";
    private const string User = "account";
    private readonly PasswordVault vault = new();

    public string? Load()
    {
        try
        {
            var credential = vault.Retrieve(Resource, User);
            credential.RetrievePassword();
            return credential.Password;
        }
        catch (COMException ex) when (ex.HResult == unchecked((int)0x80070490)) { return null; }
    }

    public void Save(string token) => vault.Add(new PasswordCredential(Resource, User, token));

    public void Clear()
    {
        try { vault.Remove(vault.Retrieve(Resource, User)); }
        catch (COMException ex) when (ex.HResult == unchecked((int)0x80070490)) { }
    }
}
