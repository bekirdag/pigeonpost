using System.Net;
using System.Net.Sockets;
using System.Text;
using Pigeonpost.Core;

internal static partial class StrongSignInSessionTests
{
    public static IEnumerable<(string Name,Func<Task> Run)> Browser()
    {
        yield return ("The synthetic browser returns through a request-owned loopback socket before verified adoption",async()=>
        {
            using var f=await Fixture.Create();using var broker=new LoopbackSignInReturn();var old=f.Session.AcceptedAuthorization!;
            Require(broker.RedirectUri.Host=="127.0.0.1" && broker.RedirectUri.Port>=1024);
            using var request=f.Session.BeginStrongSignIn(Needs(),old,broker.RedirectUri);var callback=f.Callback(request);
            var pending=broker.WaitForCallbackAsync(request);var response=await BrowserReturn(broker.RedirectUri,callback.PathAndQuery);
            var received=await pending.WaitAsync(TimeSpan.FromSeconds(5));
            Require(received==callback && f.CodeCalls==0 && old.IsActive && response.StartsWith("HTTP/1.1 200",StringComparison.Ordinal));
            Require(response.Contains("Cache-Control: no-store") && response.Contains("Referrer-Policy: no-referrer"));
            Require(!response.Contains(Query(callback)["state"]) && !response.Contains("synthetic-code"));
            var adopted=await f.Session.CompleteStrongSignInAsync(request,received);Require(adopted.IsActive && !old.IsActive && f.CodeCalls==1);
        });
        yield return ("Wrong local callbacks cannot consume the pending verifier before its real browser return",async()=>
        {
            using var f=await Fixture.Create();using var broker=new LoopbackSignInReturn();
            using var request=f.Session.BeginStrongSignIn(Needs(),f.Session.AcceptedAuthorization!,broker.RedirectUri);var callback=f.Callback(request);
            var pending=broker.WaitForCallbackAsync(request);var state=Query(callback)["state"];
            foreach(var path in new[]{callback.PathAndQuery.Replace(state,"wrong-state"),"/favicon.ico",callback.PathAndQuery+"&code=duplicate",callback.PathAndQuery+"#unexpected"})
            {
                var response=await BrowserReturn(broker.RedirectUri,path);Require(response.StartsWith("HTTP/1.1 400",StringComparison.Ordinal) && !pending.IsCompleted);
            }
            await BrowserReturn(broker.RedirectUri,callback.PathAndQuery);Require(await pending.WaitAsync(TimeSpan.FromSeconds(5))==callback && f.CodeCalls==0);
        });
        yield return ("The loopback return refuses substituted Host methods and oversized request targets",async()=>
        {
            using var f=await Fixture.Create();using var broker=new LoopbackSignInReturn();
            using var request=f.Session.BeginStrongSignIn(Needs(),f.Session.AcceptedAuthorization!,broker.RedirectUri);var callback=f.Callback(request);
            var pending=broker.WaitForCallbackAsync(request);
            foreach(var input in new[]{(callback.PathAndQuery,"localhost:"+broker.RedirectUri.Port,"GET"),
                (callback.PathAndQuery,"127.0.0.1:"+broker.RedirectUri.Port,"POST"),("/?code="+new string('x',8300),"127.0.0.1:"+broker.RedirectUri.Port,"GET")})
            {
                var response=await BrowserReturn(broker.RedirectUri,input.Item1,input.Item2,input.Item3);
                Require(response.StartsWith("HTTP/1.1 400",StringComparison.Ordinal) && !pending.IsCompleted);
            }
            await BrowserReturn(broker.RedirectUri,callback.PathAndQuery);Require(await pending.WaitAsync(TimeSpan.FromSeconds(5))==callback);
        });
        yield return ("A verifier cannot use another loopback listener or start a second reader",async()=>
        {
            using var f=await Fixture.Create();using var broker=new LoopbackSignInReturn();using var other=new LoopbackSignInReturn();
            using var request=f.Session.BeginStrongSignIn(Needs(),f.Session.AcceptedAuthorization!,broker.RedirectUri);
            await Fails<StrongSignInException>(()=>other.WaitForCallbackAsync(request));
            var pending=broker.WaitForCallbackAsync(request);await Fails<StrongSignInException>(()=>broker.WaitForCallbackAsync(request));
            var callback=f.Callback(request);await BrowserReturn(broker.RedirectUri,callback.PathAndQuery);Require(await pending.WaitAsync(TimeSpan.FromSeconds(5))==callback);
        });
        yield return ("Closing or cancelling a browser return releases its owned listener",async()=>
        {
            foreach(var cancel in new[]{false,true})
            {
                using var f=await Fixture.Create();using var broker=new LoopbackSignInReturn();using var cancellation=new CancellationTokenSource();
                using var request=f.Session.BeginStrongSignIn(Needs(),f.Session.AcceptedAuthorization!,broker.RedirectUri);
                var pending=broker.WaitForCallbackAsync(request,cancellation.Token);
                if(cancel)cancellation.Cancel();else broker.Dispose();
                await Fails<OperationCanceledException>(()=>pending);
                using var client=new TcpClient(AddressFamily.InterNetwork);
                await Fails<SocketException>(()=>client.ConnectAsync(IPAddress.Loopback,broker.RedirectUri.Port));
                Require(f.Session.AcceptedAuthorization!.IsActive && f.CodeCalls==0);
            }
        });
        yield return ("A retired caller stops a waiting loopback return without any token request",async()=>
        {
            using var f=await Fixture.Create();using var broker=new LoopbackSignInReturn();var root=f.Session.AcceptedAuthorization!;var caller=root.Child();StrongSignInRequest request;
            using(new AuthorizationContext(caller))request=f.Session.BeginStrongSignIn(Needs(),root,broker.RedirectUri);
            using(request)
            {
                var pending=broker.WaitForCallbackAsync(request);caller.Retire();
                await Fails<AuthorizationRetiredException>(()=>pending);Require(root.IsActive && f.CodeCalls==0);
            }
        });
    }
    private static async Task<string> BrowserReturn(Uri endpoint,string target,string? host=null,string method="GET")
    {
        using var timeout=new CancellationTokenSource(TimeSpan.FromSeconds(5));using var client=new TcpClient(AddressFamily.InterNetwork);
        await client.ConnectAsync(IPAddress.Loopback,endpoint.Port,timeout.Token);using var stream=client.GetStream();
        var request=Encoding.ASCII.GetBytes($"{method} {target} HTTP/1.1\r\nHost: {host??("127.0.0.1:"+endpoint.Port)}\r\nConnection: close\r\n\r\n");
        await stream.WriteAsync(request,timeout.Token);using var reader=new StreamReader(stream,Encoding.UTF8);
        return await reader.ReadToEndAsync(timeout.Token);
    }
}
