using Torque.Http.IntegrationTests.Fixtures;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class HealthIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task Health_ReturnsOk()
    {
        using var client = CreateClient();
        var health = await client.HealthAsync();
        Assert.True(health.Ok);
    }

    [Fact]
    public async Task Ready_ReturnsResponse()
    {
        using var client = CreateClient();
        // Ready may return ok=true or ok=false depending on state, but should not throw
        var ready = await client.ReadyAsync();
        Assert.True(ready.CollectionsTotal >= 0);
    }

    [Fact]
    public async Task Metrics_ReturnsText()
    {
        using var client = CreateClient();
        var metrics = await client.MetricsAsync();
        Assert.NotNull(metrics);
        Assert.NotEmpty(metrics);
    }
}
