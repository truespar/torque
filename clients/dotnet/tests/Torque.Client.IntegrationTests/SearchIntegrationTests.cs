using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;
using Torque.Http.Search;
using Torque.Http.Tcp;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class SearchIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateHttpClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task Search_WithFiltersAndFacets()
    {
        using var httpClient = CreateHttpClient();
        var name = $"search_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                Fields =
                [
                    new() { Name = "title", Type = TorqueFieldType.String },
                    new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
                    new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true },
                ]
            };

            await httpClient.Collections.CreateAsync(schema);

            await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await ingest.ConnectAsync();
            await ingest.StartIngestAsync(name);

            var docs = new List<TorqueDocument>();
            for (int i = 0; i < 20; i++)
            {
                docs.Add(new TorqueDocumentBuilder()
                    .SetId($"p-{i}")
                    .Set("title", i < 10 ? "Laptop Pro" : "Tablet Mini")
                    .Set("category", i < 10 ? "electronics" : "tablets")
                    .Set("price", 100 + i * 10)
                    .Build(schema));
            }
            await ingest.SendBatchAsync(docs, schema);
            await ingest.CommitAsync();
            await Task.Delay(500);

            var result = await httpClient.SearchAsync(name, new SearchParameters
            {
                Q = "Laptop",
                QueryBy = "title",
                FilterBy = "price:>= 100",
                SortBy = "price:asc",
                FacetBy = "category",
            });

            Assert.True(result.Found > 0);
            Assert.NotNull(result.FacetCounts);
        }
        finally
        {
            try { await httpClient.Collections.DeleteAsync(name); } catch { }
        }
    }
}
