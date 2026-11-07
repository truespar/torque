using System.Net;
using Torque.Http.Exceptions;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Tests.Http;

public class HttpTransportTests
{
    [Fact]
    public async Task Get_SetsApiKeyHeader()
    {
        string? capturedApiKey = null;
        var handler = new MockHandler((request, ct) =>
        {
            capturedApiKey = request.Headers.TryGetValues("X-TYPESENSE-API-KEY", out var values)
                ? values.FirstOrDefault() : null;
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("""{"ok": true}""")
            });
        });

        var options = new TorqueHttpClientOptions { ApiKey = "test-key-123", Nodes = [new("http://localhost:8108")] };
        using var client = new TorqueHttpClient(options, new HttpClient(handler));
        var result = await client.HealthAsync();

        Assert.Equal("test-key-123", capturedApiKey);
        Assert.True(result.Ok);
    }

    [Fact]
    public async Task Get_404_ThrowsApiException()
    {
        var handler = new MockHandler((_, _) =>
            Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound)
            {
                Content = new StringContent("""{"message": "Collection `foo` not found."}""")
            }));

        var options = new TorqueHttpClientOptions { ApiKey = "test", Nodes = [new("http://localhost:8108")] };
        using var client = new TorqueHttpClient(options, new HttpClient(handler));

        var ex = await Assert.ThrowsAsync<ObjectNotFoundException>(() =>
            client.Collections.GetAsync("foo"));

        Assert.Equal(HttpStatusCode.NotFound, ex.StatusCode);
        Assert.Contains("not found", ex.Message);
    }

    [Fact]
    public async Task Get_CorrectUrlConstruction()
    {
        string? capturedUrl = null;
        var handler = new MockHandler((request, _) =>
        {
            capturedUrl = request.RequestUri?.ToString();
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("[]")
            });
        });

        var options = new TorqueHttpClientOptions { ApiKey = "test", Nodes = [new("http://localhost:8108")] };
        using var client = new TorqueHttpClient(options, new HttpClient(handler));
        await client.Collections.ListAsync();

        Assert.Equal("http://localhost:8108/collections", capturedUrl);
    }

    private sealed class MockHandler(
        Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> handler)
        : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken ct) => handler(request, ct);
    }
}
