// ws_idle_holder .NET comparator — raw ASP.NET Core Kestrel WebSocket
// middleware over TLS that holds N idle connections so the shared bench
// harness can measure per-connection memory (density), connect latency, and
// churn against Kāra.
//
// This is the lean .NET prod default: Kestrel + app.UseWebSockets() + a raw
// middleware that accepts and echoes. NOT SignalR (that is the framework-tier
// stretch comparator #74). Mirrors the Kāra demo + the Go/Netty comparators:
// bind 127.0.0.1:0, print BOUND_PORT for the harness --server-bin contract,
// accept WS-over-TLS at "/", echo, hold idle.

using System.Net;
using System.Net.WebSockets;
using System.Security.Authentication;
using System.Security.Cryptography.X509Certificates;

var builder = WebApplication.CreateSlimBuilder(args);

builder.WebHost.ConfigureKestrel(options =>
{
    // TLS 1.2 + 1.3, no client auth, single self-signed cert — the same
    // tests/fixtures/tls fixture (CN=localhost), resolved next to the binary
    // (AppContext.BaseDirectory) so cwd doesn't matter. On Linux Kestrel
    // terminates TLS via OpenSSL; this is the apples-to-apples in-process TLS
    // (every comparator terminates TLS in-process).
    var baseDir = AppContext.BaseDirectory;
    var pem = X509Certificate2.CreateFromPemFile(
        Path.Combine(baseDir, "cert.pem"),
        Path.Combine(baseDir, "key.pem"));
    // Re-import as PFX: a PEM-loaded cert carries an ephemeral key that
    // SslStream cannot always use directly; the export/reimport round-trip
    // yields a key handle Kestrel's TLS accepts on every platform.
    var cert = new X509Certificate2(pem.Export(X509ContentType.Pfx));

    options.Listen(IPAddress.Loopback, 0, listenOptions =>
    {
        listenOptions.UseHttps(cert, https =>
        {
            https.SslProtocols = SslProtocols.Tls12 | SslProtocols.Tls13;
        });
    });
});

var app = builder.Build();

app.UseWebSockets();

// Raw WebSocket middleware. The harness sends `GET / HTTP/1.1` with the RFC
// 6455 Upgrade headers; any WS-upgrade request is accepted and echoed. Idle
// connections never send, so the echo loop is dormant during the density
// hold (it only fires in the harness's active-traffic phase).
app.Use(async (context, next) =>
{
    if (!context.WebSockets.IsWebSocketRequest)
    {
        await next();
        return;
    }

    using var ws = await context.WebSockets.AcceptWebSocketAsync();
    var buffer = new byte[4096];
    while (ws.State == WebSocketState.Open)
    {
        WebSocketReceiveResult result;
        try
        {
            result = await ws.ReceiveAsync(buffer, CancellationToken.None);
        }
        catch (WebSocketException)
        {
            break;
        }

        if (result.MessageType == WebSocketMessageType.Close)
        {
            break;
        }

        await ws.SendAsync(
            new ArraySegment<byte>(buffer, 0, result.Count),
            result.MessageType,
            result.EndOfMessage,
            CancellationToken.None);
    }
});

// Start, then report the ephemeral port the harness reads from stdout.
app.Start();
var address = app.Urls.First(); // https://127.0.0.1:<port>
var port = new Uri(address).Port;
Console.WriteLine($"BOUND_PORT={port}");
Console.Out.Flush();
Console.Error.WriteLine($"[dotnet] up on {address}");

app.WaitForShutdown();
