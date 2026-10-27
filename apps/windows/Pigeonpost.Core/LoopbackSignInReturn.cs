using System.Net;
using System.Net.Sockets;
using System.Text;

namespace Pigeonpost.Core;

// One temporary, request-owned browser return. No public bind, redirects, cookies, logs or disk.
public sealed class LoopbackSignInReturn : IDisposable
{
    private readonly TcpListener listener;
    private int disposed, waiting;
    public Uri RedirectUri { get; }
    public LoopbackSignInReturn()
    {
        listener=new(IPAddress.Loopback,0);listener.Start(4);
        RedirectUri=new($"http://127.0.0.1:{((IPEndPoint)listener.LocalEndpoint).Port}/");
    }
    public async Task<Uri> WaitForCallbackAsync(StrongSignInRequest request,CancellationToken ct=default)
    {
        if(Volatile.Read(ref disposed)!=0 || !request.IsActive || request.Redirect!=RedirectUri
            || Interlocked.CompareExchange(ref waiting,1,0)!=0)throw new StrongSignInException();
        using var deadline=CancellationTokenSource.CreateLinkedTokenSource(ct);deadline.CancelAfter(TimeSpan.FromMinutes(10));
        try
        {
            while(true)
            {
                deadline.Token.ThrowIfCancellationRequested();request.Validate();
                if(!request.IsActive)throw new AuthorizationRetiredException();
                using var accept=CancellationTokenSource.CreateLinkedTokenSource(deadline.Token);accept.CancelAfter(TimeSpan.FromSeconds(2));
                TcpClient client;
                try{client=await listener.AcceptTcpClientAsync(accept.Token).ConfigureAwait(false);}
                catch(OperationCanceledException)when(!deadline.IsCancellationRequested){continue;}
                using(client)
                {
                    using var read=CancellationTokenSource.CreateLinkedTokenSource(deadline.Token);read.CancelAfter(TimeSpan.FromSeconds(2));
                    var stream=client.GetStream();Uri? callback=null;
                    try{callback=await ReadCallbackAsync(stream,read.Token).ConfigureAwait(false);}
                    catch(OperationCanceledException)when(!deadline.IsCancellationRequested){}
                    catch(IOException){}
                    request.Validate();if(!request.IsActive)throw new AuthorizationRetiredException();
                    var matches=callback is not null && request.MatchesCallback(callback);
                    try{await RespondAsync(stream,matches,deadline.Token).ConfigureAwait(false);}catch(IOException){}
                    if(matches){deadline.Token.ThrowIfCancellationRequested();request.Validate();return callback!;}
                }
            }
        }
        catch(OperationCanceledException)when(!ct.IsCancellationRequested && deadline.IsCancellationRequested)
        {throw new SignInException("This verification request expired. Start again when ready.");}
        catch(SocketException)when(Volatile.Read(ref disposed)!=0){throw new OperationCanceledException("Verification was closed.",ct);}
        catch(ObjectDisposedException)when(Volatile.Read(ref disposed)!=0){throw new OperationCanceledException("Verification was closed.",ct);}
        finally{Dispose();}
    }
    private async Task<Uri?> ReadCallbackAsync(NetworkStream stream,CancellationToken ct)
    {
        var bytes=new byte[16384];var length=0;var end=-1;
        while(length<bytes.Length)
        {
            var read=await stream.ReadAsync(bytes.AsMemory(length),ct).ConfigureAwait(false);if(read==0)return null;
            length+=read;
            for(var i=Math.Max(0,length-read-3);i<=length-4;i++)
                if(bytes[i]==13&&bytes[i+1]==10&&bytes[i+2]==13&&bytes[i+3]==10){end=i;break;}
            if(end>=0)break;
        }
        if(end<0)return null;
        for(var i=0;i<end;i++)if(bytes[i]>126 || bytes[i]<32 && bytes[i] is not (9 or 10 or 13))return null;
        var lines=Encoding.ASCII.GetString(bytes,0,end).Split("\r\n");var first=lines[0].Split(' ');
        if(first.Length!=3 || first[0]!="GET" || first[2] is not ("HTTP/1.1" or "HTTP/1.0")
            || !first[1].StartsWith("/?",StringComparison.Ordinal) || first[1].Length>8192)return null;
        var hosts=lines.Skip(1).Where(l=>l.StartsWith("Host:",StringComparison.OrdinalIgnoreCase)).ToArray();
        if(hosts.Length!=1 || hosts[0][5..].Trim()!=$"127.0.0.1:{RedirectUri.Port}")return null;
        return Uri.TryCreate(RedirectUri.GetLeftPart(UriPartial.Authority)+first[1],UriKind.Absolute,out var callback)?callback:null;
    }
    private static async Task RespondAsync(NetworkStream stream,bool accepted,CancellationToken ct)
    {
        var body=Encoding.UTF8.GetBytes(accepted?"<!doctype html><meta charset=utf-8><title>Pigeonpost</title><p>Return to Pigeonpost to finish verifying this request.</p>"
            :"<!doctype html><title>Pigeonpost</title><p>This is not the pending Pigeonpost verification request.</p>");
        var header=Encoding.ASCII.GetBytes($"HTTP/1.1 {(accepted?"200 OK":"400 Bad Request")}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {body.Length}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n");
        await stream.WriteAsync(header,ct).ConfigureAwait(false);await stream.WriteAsync(body,ct).ConfigureAwait(false);
    }
    public void Dispose(){if(Interlocked.Exchange(ref disposed,1)==0)listener.Stop();}
    public override string ToString()=>"LoopbackSignInReturn(redacted)";
}
