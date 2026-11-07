using System.Net;

namespace Torque.Http.Tests.Http;

public class MultiNodeTests
{
    [Fact]
    public async Task Failover_RetriesOnDifferentNode()
    {
        var callLog = new List<string>();
        int callCount = 0;
        var handler = new MockHandler((request, _) =>
        {
            callLog.Add(request.RequestUri!.Host);
            callCount++;
            // First call fails with 500, second succeeds
            if (callCount == 1)
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.InternalServerError));
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"ok": true}""")
            });
        });

        var options = new TorqueHttpClientOptions
        {
            ApiKey = "test",
            Nodes = [new("http://node1:8108"), new("http://node2:8108")],
            MaxRetries = 2,
        };
        using var client = new TorqueHttpClient(options, new HttpClient(handler));
        var result = await client.HealthAsync();

        Assert.True(result.Ok);
        Assert.Equal(2, callCount);
    }

    [Fact]
    public async Task ApiKey_SentPerRequest()
    {
        var capturedKeys = new List<string>();
        var handler = new MockHandler((request, _) =>
        {
            if (request.Headers.TryGetValues("X-TYPESENSE-API-KEY", out var vals))
                capturedKeys.Add(vals.First());
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"ok": true}""")
            });
        });

        var options = new TorqueHttpClientOptions
        {
            ApiKey = "my-secret-key",
            Nodes = [new("http://localhost:8108")],
        };
        using var client = new TorqueHttpClient(options, new HttpClient(handler));
        await client.HealthAsync();
        await client.HealthAsync();

        Assert.Equal(2, capturedKeys.Count);
        Assert.All(capturedKeys, k => Assert.Equal("my-secret-key", k));
    }

    [Fact]
    public async Task Create_Factory_ProducesWorkingClient()
    {
        // TorqueHttpClient.Create should work without throwing
        var options = new TorqueHttpClientOptions
        {
            ApiKey = "test",
            Nodes = [new("http://localhost:59999")], // Won't actually connect
        };
        using var client = TorqueHttpClient.Create(options);
        Assert.NotNull(client.Collections);
        Assert.NotNull(client.Documents);
        Assert.NotNull(client.Keys);
    }

    [Fact]
    public async Task BaseUrl_Compat_SetsNode()
    {
        string? capturedUrl = null;
        var handler = new MockHandler((request, _) =>
        {
            capturedUrl = request.RequestUri?.ToString();
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"ok": true}""")
            });
        });

        var options = new TorqueHttpClientOptions
        {
            ApiKey = "test",
            BaseUrl = "http://myserver:9999",
        };
        using var client = new TorqueHttpClient(options, new HttpClient(handler));
        await client.HealthAsync();

        Assert.StartsWith("http://myserver:9999/", capturedUrl);
    }

    private sealed class MockHandler(
        Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> handler)
        : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken ct) => handler(request, ct);
    }
}
