using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class SynonymIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task SynonymSet_CrudCycle()
    {
        using var client = CreateClient();
        var setName = $"syn_{Guid.NewGuid():N}"[..32];

        try
        {
            var set = new SynonymSet
            {
                Name = setName,
                Items = [new SynonymItem { Id = "item1", Synonyms = ["red", "scarlet", "crimson"] }]
            };

            await client.Synonyms.UpsertSetAsync(setName, set);
            var fetched = await client.Synonyms.GetSetAsync(setName);
            Assert.Equal(setName, fetched.Name);

            var all = await client.Synonyms.ListSetsAsync();
            Assert.Contains(all, s => s.Name == setName);

            await client.Synonyms.DeleteSetAsync(setName);
        }
        finally
        {
            try { await client.Synonyms.DeleteSetAsync(setName); } catch { }
        }
    }
}
