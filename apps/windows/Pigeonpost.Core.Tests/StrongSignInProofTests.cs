using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class StrongSignInProofTests
{
    private const long Now=1800000000;
    private const string Issuer="https://issuer.invalid/realms/synthetic",Client="pigeonpost-windows",Subject="synthetic-member",Nonce="request-nonce";
    private static Dictionary<string,object> Access()=>new(){{"iss",Issuer},{"sub",Subject},{"azp",Client},{"exp",Now+3600},{"iat",Now},{"auth_time",Now},{"acr","mfa"}};
    private static Dictionary<string,object> Id(){var value=Access();value["aud"]=Client;value["nonce"]=Nonce;return value;}
    private static string Enc(string text)=>Convert.ToBase64String(Encoding.UTF8.GetBytes(text)).TrimEnd('=').Replace('+','-').Replace('/','_');
    private static string Jwt(Dictionary<string,object> claims,string algorithm="RS256")=>Enc(JsonSerializer.Serialize(new{alg=algorithm}))+"."+Enc(JsonSerializer.Serialize(claims))+".c2ln";
    private static void Verify(string? id=null,string? access=null,int age=600,string subject=Subject,string nonce=Nonce,long started=Now,long now=Now)=>
        StrongSignInProof.ValidateTrustedExchange(id??Jwt(Id()),access??Jwt(Access()),subject,nonce,StrongSignInRequirement.Create(["mfa"],age)!,Issuer,Client,started,now);
    private static void Refuses(Dictionary<string,object>? id=null,Dictionary<string,object>? access=null,StrongSignInProblem problem=StrongSignInProblem.Unverified)
    {var error=Fails(()=>Verify(Jwt(id??Id()),Jwt(access??Access())));Require(error.Problem==problem);}
    public static IEnumerable<(string Name,Func<Task> Run)> All()
    {
        yield return ("Fresh trusted sign-in proof binds both tokens to the same current person",()=>
        {
            Verify();var id=Id();id["aud"]=new[]{Client};Verify(Jwt(id));id.Remove("azp");Verify(Jwt(id));
            id=Id();id["sub"]="another";Refuses(id,problem:StrongSignInProblem.DifferentAccount);
            var access=Access();access["sub"]="another";Refuses(access:access,problem:StrongSignInProblem.DifferentAccount);
            var error=Fails(()=>Verify(subject:"private-fixture-subject"));Require(!error.ToString().Contains("private-fixture-subject"));
            return Task.CompletedTask;
        });
        yield return ("Strong proof binds issuer nonce audience and authorized party to the request",()=>
        {
            foreach(var change in new[]{("iss",(object)"https://other.invalid"),("nonce","different"),("aud","other-client"),("aud",new[]{Client,"other-client"}),("azp","other-client")})
            {var id=Id();id[change.Item1]=change.Item2;Refuses(id);}
            foreach(var name in new[]{"iss","azp"}){var access=Access();access[name]="other";Refuses(access:access);}
            var missing=Id();missing.Remove("nonce");Refuses(missing);return Task.CompletedTask;
        });
        yield return ("Strong proof refuses stale expired future and malformed authentication times",()=>
        {
            foreach(var change in new[]{("exp",(object)Now),("exp",Now-10),("iat",Now-31),("iat",Now+31),("auth_time",Now-631),("auth_time",Now+31),("auth_time",Now.ToString())})
            {var id=Id();id[change.Item1]=change.Item2;Refuses(id);var access=Access();access[change.Item1]=change.Item2;Refuses(access:access);}
            var boundary=Id();boundary["iat"]=Now-30;boundary["auth_time"]=Now-630;Verify(Jwt(boundary));
            Fails(()=>Verify(started:Now+31));Fails(()=>Verify(now:long.MaxValue));return Task.CompletedTask;
        });
        yield return ("Zero-age proof requires authentication during the current interaction",()=>
        {
            Verify(age:0);var id=Id();id["auth_time"]=Now-6;Fails(()=>Verify(Jwt(id),age:0));
            var access=Access();access["auth_time"]=Now-6;Fails(()=>Verify(access:Jwt(access),age:0));return Task.CompletedTask;
        });
        yield return ("Both ID and bearer proofs require the requested method and authentication time",()=>
        {
            foreach(var claim in new[]{"acr","auth_time"})
            {var id=Id();id.Remove(claim);Refuses(id);var access=Access();access.Remove(claim);Refuses(access:access);}
            var weak=Access();weak["acr"]="weak";Refuses(access:weak);var wrong=Id();wrong["acr"]=2;Refuses(wrong);return Task.CompletedTask;
        });
        yield return ("A returned access hash binds the exact bearer bytes",()=>
        {
            var access=Jwt(Access());var digest=SHA256.HashData(Encoding.UTF8.GetBytes(access));
            var id=Id();id["at_hash"]=Convert.ToBase64String(digest,0,16).TrimEnd('=').Replace('+','-').Replace('/','_');Verify(Jwt(id),access);
            var changed=Access();changed["extra"]="different";Fails(()=>Verify(Jwt(id),Jwt(changed)));
            id["at_hash"]="wrong";Refuses(id);id["at_hash"]=123;Refuses(id);return Task.CompletedTask;
        });
        yield return ("Strong proof rejects duplicate claims unsupported algorithms and oversized candidates",()=>
        {
            foreach(var algorithm in new[]{"none","HS256",""}){Fails(()=>Verify(Jwt(Id(),algorithm)));Fails(()=>Verify(access:Jwt(Access(),algorithm)));}
            foreach(var token in new[]{"private-token","a.b.c.d","a..c","a.b.c=",new string('a',32769),"!!!!.aaaa.sig",
                Enc("{\"alg\":\"RS256\"}")+"."+Enc("{\"sub\":\"one\",\"sub\":\"two\"}")+".c2ln"})
            {var error=Fails(()=>Verify(token));Require(!error.ToString().Contains("private-token"));}
            Fails(()=>StrongSignInProof.ValidateTrustedExchange(null,Jwt(Access()),Subject,Nonce,StrongSignInRequirement.Create(["mfa"],600)!,Issuer,Client,Now,Now));
            Fails(()=>Verify(nonce:""));Fails(()=>Verify(nonce:new string('x',129)));return Task.CompletedTask;
        });
        yield return ("The expected person comes only from a protected token for the trusted issuer and client",()=>
        {
            Require(StrongSignInProof.SubjectFromTrustedExchange(Jwt(Access()),Issuer,Client)==Subject);
            Require(StrongSignInProof.SubjectFromTrustedExchange(Jwt(Access()),"https://other.invalid",Client) is null);
            Require(StrongSignInProof.SubjectFromTrustedExchange(Jwt(Access()),Issuer,"other-client") is null);
            foreach(var subject in new[]{"",new string('x',257),"hidden\nvalue"})
            {var access=Access();access["sub"]=subject;Require(StrongSignInProof.SubjectFromTrustedExchange(Jwt(access),Issuer,Client) is null);}
            return Task.CompletedTask;
        });
    }
    private static StrongSignInException Fails(Action action)
    {try{action();}catch(StrongSignInException error){return error;}throw new Exception("Expected a fixed strong sign-in refusal.");}
    private static void Require(bool value){if(!value)throw new Exception("Strong proof behavior assertion failed.");}
}
