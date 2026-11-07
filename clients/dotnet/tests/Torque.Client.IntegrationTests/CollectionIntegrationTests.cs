using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class CollectionIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task CreateGetListDeleteCollection()
    {
        using var client = CreateClient();
        var name = $"test_coll_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                Fields =
                [
                    new() { Name = "title", Type = TorqueFieldType.String },
                    new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true, Optional = true },
                ]
            };

            var created = await client.Collections.CreateAsync(schema);
            Assert.Equal(name, created.Name);

            var fetched = await client.Collections.GetAsync(name);
            Assert.Equal(name, fetched.Name);
            Assert.Equal(2, fetched.Fields.Count);

            var all = await client.Collections.ListAsync();
            Assert.Contains(all, c => c.Name == name);

            await client.Collections.DeleteAsync(name);
        }
        finally
        {
            try { await client.Collections.DeleteAsync(name); } catch { }
        }
    }
}
