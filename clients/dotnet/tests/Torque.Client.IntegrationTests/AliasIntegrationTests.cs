using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class AliasIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task Alias_UpsertGetListDelete()
    {
        using var client = CreateClient();
        var collName = $"alias_coll_{Guid.NewGuid():N}"[..32];
        var aliasName = $"alias_{Guid.NewGuid():N}"[..32];

        try
        {
            await client.Collections.CreateAsync(new CollectionSchema
            {
                Name = collName,
                Fields = [new() { Name = "title", Type = TorqueFieldType.String }]
            });

            var alias = await client.Aliases.UpsertAsync(aliasName, collName);
            Assert.Equal(aliasName, alias.Name);
            Assert.Equal(collName, alias.CollectionName);

            var fetched = await client.Aliases.GetAsync(aliasName);
            Assert.Equal(collName, fetched.CollectionName);

            var all = await client.Aliases.ListAsync();
            Assert.Contains(all, a => a.Name == aliasName);

            await client.Aliases.DeleteAsync(aliasName);
        }
        finally
        {
            try { await client.Aliases.DeleteAsync(aliasName); } catch { }
            try { await client.Collections.DeleteAsync(collName); } catch { }
        }
    }
}
