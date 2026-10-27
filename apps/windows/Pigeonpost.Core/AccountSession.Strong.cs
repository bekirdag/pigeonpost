using System.Security.Cryptography;
using System.Text;

namespace Pigeonpost.Core;

public sealed class StrongSignInRequest : IDisposable
{
    internal readonly AuthorizationLease Accepted;
    internal readonly int Generation;
    internal readonly long StartedAt;
    internal readonly string Subject, Nonce, Verifier, State;
    internal readonly StrongSignInRequirement Requirement;
    internal readonly Func<AuthorizationContext> Bind;
    internal readonly Action Validate;
    internal readonly Uri Redirect;
    private readonly string expectedIssuer;
    private int active = 1, exchanging;
    public Uri AuthorizationUri { get; }
    internal bool IsActive => Volatile.Read(ref active) != 0;
    internal bool TryBeginExchange() => Interlocked.CompareExchange(ref exchanging, 1, 0) == 0;
    internal StrongSignInRequest(AuthorizationLease accepted, int generation, long started, string subject,
        StrongSignInRequirement requirement, Uri issuer, Uri redirect, Func<AuthorizationContext> bind, Action validate)
    {
        Accepted=accepted;Generation=generation;StartedAt=started;Subject=subject;Requirement=requirement;Redirect=redirect;Bind=bind;Validate=validate;
        expectedIssuer=issuer.AbsoluteUri.TrimEnd('/');
        Nonce=Random();State=Random();Verifier=Random();
        var fields=new Dictionary<string,string>{{"client_id",AccountSession.ClientId},{"response_type","code"},
            {"redirect_uri",redirect.AbsoluteUri},{"scope","openid profile offline_access"},{"state",State},{"nonce",Nonce},
            {"code_challenge",Base64Url(SHA256.HashData(Encoding.ASCII.GetBytes(Verifier)))},{"code_challenge_method","S256"},
            {"acr_values",requirement.RequestedAcr},{"max_age",requirement.MaxAge.ToString(System.Globalization.CultureInfo.InvariantCulture)}};
        AuthorizationUri=new Uri(new Uri(issuer,"protocol/openid-connect/auth").AbsoluteUri+"?"+
            string.Join('&',fields.Select(p=>Uri.EscapeDataString(p.Key)+"="+Uri.EscapeDataString(p.Value))));
    }
    public bool MatchesCallback(Uri callback)
    {try{ReadCallback(callback);return true;}catch(StrongSignInException){return false;}}
    internal (string? Code,bool Denied) ReadCallback(Uri callback)
    {
        if (!IsActive || !callback.IsAbsoluteUri || callback.OriginalString.Length>8192 || callback.Fragment.Length!=0
            || !callback.OriginalString.StartsWith(Redirect.AbsoluteUri+"?",StringComparison.Ordinal)
            || callback.GetLeftPart(UriPartial.Path)!=Redirect.AbsoluteUri) throw new StrongSignInException();
        var fields=new Dictionary<string,string>(StringComparer.Ordinal);
        foreach(var field in callback.Query[1..].Split('&'))
        {
            var split=field.IndexOf('=');if(split<=0)throw new StrongSignInException();
            var name=Decode(field[..split]);var value=Decode(field[(split+1)..]);
            if(name is not ("state" or "code" or "error" or "error_description" or "session_state" or "iss")
                || !fields.TryAdd(name,value))throw new StrongSignInException();
        }
        if(!fields.TryGetValue("state",out var state)||!CryptographicOperations.FixedTimeEquals(Encoding.UTF8.GetBytes(state),Encoding.UTF8.GetBytes(State)))
            throw new StrongSignInException();
        if(fields.TryGetValue("iss",out var issuer)&&issuer!=expectedIssuer)
            throw new StrongSignInException();
        if(fields.TryGetValue("error",out _))
        {if(fields.ContainsKey("code"))throw new StrongSignInException();return(null,true);}
        if(!fields.TryGetValue("code",out var code)||code.Length is <1 or >4096||code.Any(c=>c<32||c>126))throw new StrongSignInException();
        return(code,false);
    }
    private static string Decode(string value)
    {
        for(var i=0;i<value.Length;i++)if(value[i]=='%'&&(i+2>=value.Length||!Uri.IsHexDigit(value[i+1])||!Uri.IsHexDigit(value[i+2])))throw new StrongSignInException();
        try{return Uri.UnescapeDataString(value.Replace('+',' '));}catch(UriFormatException){throw new StrongSignInException();}
    }
    private static string Random()=>Base64Url(RandomNumberGenerator.GetBytes(32));
    private static string Base64Url(byte[] value)=>Convert.ToBase64String(value).TrimEnd('=').Replace('+','-').Replace('/','_');
    public void Dispose()=>Interlocked.Exchange(ref active,0);
    public override string ToString()=>"StrongSignInRequest(redacted)";
}

public sealed partial class AccountSession
{
    private StrongSignInRequest? pendingStrong;
    public StrongSignInRequest BeginStrongSignIn(StrongSignInRequirement requirement, AuthorizationLease expected,
        Uri ownedLoopbackRedirect, CancellationToken ct=default)
    {
        if(!ownedLoopbackRedirect.IsAbsoluteUri || ownedLoopbackRedirect.Port is <1024 or >65535
            || ownedLoopbackRedirect.OriginalString!=$"http://127.0.0.1:{ownedLoopbackRedirect.Port}/")throw new StrongSignInException();
        using var context=AuthorizationContext.Capture(expected);
        var validate=AuthorizationContext.CaptureValidation(this);validate();
        lock(stateLock)
        {
            ct.ThrowIfCancellationRequested();validate();
            if(!ReferenceEquals(authorization,expected)||!expected.IsActive)throw new AuthorizationRetiredException();
            var subject=StrongSignInProof.SubjectFromTrustedExchange(accessToken,issuer.AbsoluteUri.TrimEnd('/'),ClientId)
                ??throw new StrongSignInException();
            RetireStrongLocked();pendingSignIn=null;
            return pendingStrong=new(expected,++generation,clock.GetUtcNow().ToUnixTimeSeconds(),subject,requirement,issuer,
                ownedLoopbackRedirect,AuthorizationContext.CaptureBinding(),validate);
        }
    }
    public void CancelStrongSignIn(StrongSignInRequest request)
    {lock(stateLock){if(!ReferenceEquals(pendingStrong,request))return;++generation;RetireStrongLocked();}}

    public async Task<AuthorizationLease> CompleteStrongSignInAsync(StrongSignInRequest request,Uri callback,CancellationToken ct=default)
    {
        using var context=request.Bind();var validate=AuthorizationContext.CaptureValidation(this);
        var commit=AuthorizationContext.CaptureWriteBarrier(()=>AcceptedAuthorization);
        var returned=request.ReadCallback(callback);
        lock(stateLock){RequireStrong(request,ct);validate();if(!request.TryBeginExchange())throw new StrongSignInException();}
        try
        {
            if(returned.Denied)throw new SignInException("Verification was cancelled. Your current sign-in is unchanged.");
            using var result=await PostAsync("token",new(){{"client_id",ClientId},{"grant_type","authorization_code"},
                {"code",returned.Code!},{"redirect_uri",request.Redirect.AbsoluteUri},{"code_verifier",request.Verifier}},ct).ConfigureAwait(false);
            validate();lock(stateLock)RequireStrong(request,ct);CheckError(result.RootElement);
            await gate.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                lock(stateLock)
                {
                    RequireStrong(request,ct);validate();
                    commit(()=>
                    {
                        StrongSignInProof.ValidateTrustedExchange(Text(result.RootElement,"id_token"),Required(result.RootElement,"access_token"),
                            request.Subject,request.Nonce,request.Requirement,issuer.AbsoluteUri.TrimEnd('/'),ClientId,request.StartedAt,clock.GetUtcNow().ToUnixTimeSeconds());
                        AcceptUnderLock(result.RootElement,ct,request.Generation,newAuthorization:true,requireRefresh:true);
                    });
                    pendingStrong=null;request.Dispose();return authorization!;
                }
            }
            finally{gate.Release();}
        }
        finally{lock(stateLock){if(ReferenceEquals(pendingStrong,request))RetireStrongLocked();}}
    }
    private void RequireStrong(StrongSignInRequest request,CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();request.Validate();
        if(!ReferenceEquals(pendingStrong,request)||!request.IsActive||request.Generation!=generation
            ||!ReferenceEquals(authorization,request.Accepted))throw new AuthorizationRetiredException();
        if(clock.GetUtcNow().ToUnixTimeSeconds()>=request.StartedAt+600){RetireStrongLocked();throw new StrongSignInException();}
    }
    private void RetireStrongLocked(){pendingStrong?.Dispose();pendingStrong=null;}
}
