using System.Text.Json;
using Torque.Http.Documents;
using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;
using Torque.Http.Search;
using Torque.Http.Tcp;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class NewFeatureIntegrationTests : IAsyncLifetime
{
    private readonly TorqueFixture _fixture;
    private TorqueHttpClient _client = null!;
    private string _sharedCollection = null!;

    public NewFeatureIntegrationTests(TorqueFixture fixture)
    {
        _fixture = fixture;
    }

    private TorqueHttpClient CreateHttpClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = _fixture.HttpUrl,
        ApiKey = _fixture.ApiKey,
    });

    private TorqueHttpClient CreateMultiNodeClient() => new(new TorqueHttpClientOptions
    {
        ApiKey = _fixture.ApiKey,
        Nodes = [new(_fixture.HttpUrl)],
    });

    public async Task InitializeAsync()
    {
        _client = CreateHttpClient();
        var name = $"nf_{Guid.NewGuid():N}"[..32];
        var schema = new CollectionSchema
        {
            Name = name,
            Fields =
            [
                new() { Name = "title", Type = TorqueFieldType.String },
                new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
                new() { Name = "brand", Type = TorqueFieldType.String, Facet = true },
                new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true },
                new() { Name = "in_stock", Type = TorqueFieldType.Bool },
            ]
        };
        await _client.Collections.CreateAsync(schema);
        _sharedCollection = name;

        // Ingest test data via TCP
        await IngestTestData(_sharedCollection, 20);
    }

    public async Task DisposeAsync()
    {
        try { await _client.Collections.DeleteAsync(_sharedCollection); } catch { }
        _client.Dispose();
    }

    private async Task IngestTestData(string collection, int docCount)
    {
        await using var ingest = new TorqueIngestClient(new TorqueIngestOptions
        {
            Host = _fixture.TcpHost,
            Port = _fixture.TcpPort,
        });
        await ingest.ConnectAsync();
        await ingest.StartIngestAsync(collection);

        var schema = await _client.Collections.GetAsync(collection);
        var collSchema = new CollectionSchema
        {
            Name = schema.Name,
            Fields = schema.Fields,
        };

        var brands = new[] { "Nike", "Adidas", "Puma", "Reebok" };
        var categories = new[] { "shoes", "clothing", "accessories" };
        var docs = new List<TorqueDocument>();
        for (int i = 0; i < docCount; i++)
        {
            docs.Add(new TorqueDocumentBuilder()
                .SetId($"doc-{i}")
                .Set("title", $"Product {i} {brands[i % brands.Length]} {categories[i % categories.Length]}")
                .Set("category", categories[i % categories.Length])
                .Set("brand", brands[i % brands.Length])
                .Set("price", 10 + i * 5)
                .Set("in_stock", i % 3 != 0)
                .Build(collSchema));
        }
        await ingest.SendBatchAsync(docs, collSchema);
        await ingest.CommitAsync();

        // Wait for index to be searchable
        await Task.Delay(500);
    }

    private async Task<string> CreateTempCollection(string prefix = "tmp")
    {
        var name = $"{prefix}_{Guid.NewGuid():N}"[..32];
        var schema = new CollectionSchema
        {
            Name = name,
            Fields =
            [
                new() { Name = "title", Type = TorqueFieldType.String },
                new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
                new() { Name = "brand", Type = TorqueFieldType.String, Facet = true },
                new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true },
                new() { Name = "in_stock", Type = TorqueFieldType.Bool },
            ]
        };
        await _client.Collections.CreateAsync(schema);
        return name;
    }

    // --- SearchBuilder tests (read-only, use shared collection) ---

    [Fact]
    public async Task SearchBuilder_BasicSearch()
    {
        var result = await _client.SearchAsync(_sharedCollection,
            SearchBuilder.Create("title")
                .Query("Nike")
                .Page(1, 10));

        Assert.True(result.Found > 0);
        Assert.True(result.Hits.Count <= 10);
    }

    [Fact]
    public async Task SearchBuilder_WithFilterAndSort()
    {
        var result = await _client.SearchAsync(_sharedCollection,
            SearchBuilder.Create("title")
                .Query("*")
                .FilterBy("price:>=50")
                .SortBy("price:asc")
                .Page(1, 10));

        Assert.True(result.Found > 0);
        for (int i = 1; i < result.Hits.Count; i++)
        {
            var prev = Convert.ToInt32(result.Hits[i - 1].Document["price"]?.ToString());
            var curr = Convert.ToInt32(result.Hits[i].Document["price"]?.ToString());
            Assert.True(prev <= curr, $"Sort order violated: {prev} > {curr}");
        }
    }

    [Fact]
    public async Task SearchBuilder_WithFacets()
    {
        var result = await _client.SearchAsync(_sharedCollection,
            SearchBuilder.Create("title")
                .Query("*")
                .FacetBy("category,brand")
                .MaxFacetValues(10));

        Assert.True(result.Found > 0);
        Assert.NotNull(result.FacetCounts);
        Assert.True(result.FacetCounts.Count >= 2);

        var categoryFacet = result.FacetCounts.FirstOrDefault(f => f.FieldName == "category");
        Assert.NotNull(categoryFacet);
        Assert.True(categoryFacet.Counts.Count > 0);
    }

    [Fact]
    public async Task SearchBuilder_GroupBy()
    {
        var result = await _client.SearchGroupedAsync(_sharedCollection,
            SearchBuilder.Create("title")
                .Query("*")
                .GroupBy("category", limit: 3));

        Assert.True(result.Found > 0);
        Assert.NotNull(result.GroupedHits);
        Assert.True(result.GroupedHits.Count > 0);

        foreach (var group in result.GroupedHits)
            Assert.True(group.Hits.Count <= 3);
    }

    [Fact]
    public async Task SearchBuilder_IncludeFields()
    {
        var result = await _client.SearchAsync(_sharedCollection,
            SearchBuilder.Create("title")
                .Query("*")
                .IncludeFields("title", "price")
                .Page(1, 5));

        Assert.True(result.Found > 0);
        foreach (var hit in result.Hits)
        {
            Assert.True(hit.Document.ContainsKey("title"));
            Assert.True(hit.Document.ContainsKey("price"));
            Assert.False(hit.Document.ContainsKey("category"));
        }
    }

    [Fact]
    public async Task SearchBuilder_TypoTolerance()
    {
        var result = await _client.SearchAsync(_sharedCollection,
            SearchBuilder.Create("title")
                .Query("Reebk")
                .Typos(1));

        Assert.True(result.Found > 0);
    }

    // --- Multi-node client ---

    [Fact]
    public async Task MultiNodeClient_BasicOperations()
    {
        using var client = CreateMultiNodeClient();
        var health = await client.HealthAsync();
        Assert.True(health.Ok);
    }

    // --- TorqueHttpClient.Create factory ---

    [Fact]
    public async Task CreateFactory_Works()
    {
        using var client = TorqueHttpClient.Create(new TorqueHttpClientOptions
        {
            ApiKey = _fixture.ApiKey,
            BaseUrl = _fixture.HttpUrl,
        });
        var health = await client.HealthAsync();
        Assert.True(health.Ok);
    }

    // --- Collection truncate (needs temp collection) ---

    [Fact]
    public async Task Collection_Truncate()
    {
        string? tempColl = null;
        try
        {
            tempColl = await CreateTempCollection("trunc");
            await IngestTestData(tempColl, docCount: 10);

            var before = await _client.SearchAsync(tempColl, new SearchParameters { Q = "*", QueryBy = "title" });
            Assert.True(before.Found > 0, "Expected docs after ingest");

            await _client.Collections.TruncateAsync(tempColl);

            var after = await _client.SearchAsync(tempColl, new SearchParameters { Q = "*", QueryBy = "title" });
            Assert.Equal(0, after.Found);
        }
        finally
        {
            if (tempColl != null)
                try { await _client.Collections.DeleteAsync(tempColl); } catch { }
        }
    }

    // --- Document import with action types (needs temp collection) ---

    [Fact]
    public async Task Document_Import_WithUpsert()
    {
        string? tempColl = null;
        try
        {
            tempColl = await CreateTempCollection("upsert");

            var docs = new List<Dictionary<string, object?>>
            {
                new() { ["id"] = "imp-1", ["title"] = "First", ["category"] = "a", ["brand"] = "x", ["price"] = 10, ["in_stock"] = true },
                new() { ["id"] = "imp-2", ["title"] = "Second", ["category"] = "b", ["brand"] = "y", ["price"] = 20, ["in_stock"] = false },
            };

            var results = await _client.Documents.ImportAsync(tempColl, docs, ImportAction.Upsert);
            Assert.Equal(2, results.Count);
            Assert.All(results, r => Assert.True(r.Success));

            docs[0]["title"] = "Updated First";
            var results2 = await _client.Documents.ImportAsync(tempColl, docs, ImportAction.Upsert);
            Assert.All(results2, r => Assert.True(r.Success));

            var doc = await _client.Documents.GetAsync(tempColl, "imp-1");
            Assert.Equal("Updated First", doc.GetProperty("title").GetString());
        }
        finally
        {
            if (tempColl != null)
                try { await _client.Collections.DeleteAsync(tempColl); } catch { }
        }
    }

    // --- Streaming export (read-only, use shared collection) ---

    [Fact]
    public async Task Document_ExportStreaming()
    {
        var docs = new List<JsonElement>();
        await foreach (var doc in _client.Documents.ExportStreamingAsync(_sharedCollection))
        {
            docs.Add(doc);
        }

        Assert.Equal(20, docs.Count);
        foreach (var doc in docs)
            Assert.True(doc.TryGetProperty("title", out _));
    }

    [Fact]
    public async Task Document_ExportStreaming_WithFilter()
    {
        var docs = new List<JsonElement>();
        await foreach (var doc in _client.Documents.ExportStreamingAsync(_sharedCollection, filterBy: "brand:=Nike"))
        {
            docs.Add(doc);
        }

        Assert.True(docs.Count > 0);
        Assert.True(docs.Count < 20);
    }

    [Fact]
    public async Task Document_ExportStreaming_IncludeFields()
    {
        var docs = new List<JsonElement>();
        await foreach (var doc in _client.Documents.ExportStreamingAsync(_sharedCollection, includeFields: "title,price"))
        {
            docs.Add(doc);
        }

        Assert.Equal(20, docs.Count);
        foreach (var doc in docs)
        {
            Assert.True(doc.TryGetProperty("title", out _));
            Assert.True(doc.TryGetProperty("price", out _));
        }
    }

    // --- Delete by filter (needs temp collection) ---

    [Fact]
    public async Task Document_DeleteByFilter()
    {
        string? tempColl = null;
        try
        {
            tempColl = await CreateTempCollection("delfilt");
            await IngestTestData(tempColl, docCount: 20);

            await _client.Documents.DeleteByFilterAsync(tempColl, "brand:=Nike");

            var result = await _client.SearchAsync(tempColl, new SearchParameters
            {
                Q = "*",
                QueryBy = "title",
                FilterBy = "brand:=Nike",
            });
            Assert.Equal(0, result.Found);

            var remaining = await _client.SearchAsync(tempColl, new SearchParameters { Q = "*", QueryBy = "title" });
            Assert.True(remaining.Found > 0);
            Assert.True(remaining.Found < 20);
        }
        finally
        {
            if (tempColl != null)
                try { await _client.Collections.DeleteAsync(tempColl); } catch { }
        }
    }

    // --- SearchParameters (read-only, use shared collection) ---

    [Fact]
    public async Task SearchParameters_HighlightFields()
    {
        var result = await _client.SearchAsync(_sharedCollection, new SearchParameters
        {
            Q = "Nike",
            QueryBy = "title",
            HighlightFields = "title",
            HighlightStartTag = "<b>",
            HighlightEndTag = "</b>",
        });

        Assert.True(result.Found > 0);
        Assert.NotNull(result.Hits[0].Highlights);
        Assert.True(result.Hits[0].Highlights!.Count > 0);
    }

    [Fact]
    public async Task SearchParameters_Prefix()
    {
        var result = await _client.SearchAsync(_sharedCollection, new SearchParameters
        {
            Q = "Nik",
            QueryBy = "title",
            Prefix = true,
        });

        Assert.True(result.Found > 0);
    }

    [Fact]
    public async Task SearchParameters_ExcludeFields()
    {
        var result = await _client.SearchAsync(_sharedCollection, new SearchParameters
        {
            Q = "*",
            QueryBy = "title",
            ExcludeFields = "category,brand,in_stock",
            PerPage = 5,
        });

        Assert.True(result.Found > 0);
        foreach (var hit in result.Hits)
        {
            Assert.False(hit.Document.ContainsKey("category"));
            Assert.False(hit.Document.ContainsKey("brand"));
        }
    }

    // --- Admin endpoints ---

    [Fact]
    public async Task Stats_ReturnsData()
    {
        var stats = await _client.StatsAsync();
        Assert.NotNull(stats);
    }

    [Fact]
    public async Task Metrics_ReturnsText()
    {
        var metrics = await _client.MetricsAsync();
        Assert.NotEmpty(metrics);
    }

    [Fact]
    public async Task Ready_ReturnsResponse()
    {
        var ready = await _client.ReadyAsync();
        Assert.True(ready.Ok);
    }
}
