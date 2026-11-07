using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class StopwordIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task StopwordSet_CrudCycle()
    {
        using var client = CreateClient();
        var stopId = $"stop_{Guid.NewGuid():N}"[..32];

        try
        {
            var set = new StopwordSet
            {
                Id = stopId,
                Stopwords = ["the", "a", "an"],
                Locale = "en",
            };

            await client.Stopwords.UpsertAsync(stopId, set);
            var fetched = await client.Stopwords.GetAsync(stopId);
            Assert.Equal(stopId, fetched.Id);
            Assert.Contains("the", fetched.Stopwords);

            var all = await client.Stopwords.ListAsync();
            Assert.Contains(all, s => s.Id == stopId);

            await client.Stopwords.DeleteAsync(stopId);
        }
        finally
        {
            try { await client.Stopwords.DeleteAsync(stopId); } catch { }
        }
    }
}
