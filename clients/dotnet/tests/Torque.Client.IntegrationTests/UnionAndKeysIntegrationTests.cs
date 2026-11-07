using System.Text.Json;
using Torque.Http.Auth;
using Torque.Http.Exceptions;
using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

/// <summary>
/// A union of searches ordered by their sort_by, its duplicates and errors, and a key per search,
/// against a running server.
/// </summary>
[Collection("Torque")]
public class UnionAndKeysIntegrationTests(TorqueFixture fixture) : IAsyncLifetime
{
    private TorqueHttpClient _client = null!;
    private string _products = null!;
    private string _gadgets = null!;

    public async Task InitializeAsync()
    {
        _client = new TorqueHttpClient(new TorqueHttpClientOptions { BaseUrl = fixture.HttpUrl, ApiKey = fixture.ApiKey });
        _products = await Create("up", [("1", "Laptop", 999, "Acme"), ("2", "Phone", 499, "Beta"), ("3", "Laptop Stand", 49, "Beta")]);
        _gadgets = await Create("ug", [("g1", "Laptop Bag", 79, "Acme"), ("g2", "Tablet", 599, "Gamma")]);
    }

    public async Task DisposeAsync()
    {
        try { await _client.Collections.DeleteAsync(_products); } catch { }
        try { await _client.Collections.DeleteAsync(_gadgets); } catch { }
        _client.Dispose();
    }

    private async Task<string> Create(string prefix, (string Id, string Title, int Price, string Brand)[] docs)
    {
        var name = $"{prefix}_{Guid.NewGuid():N}"[..32];
        await _client.Collections.CreateAsync(new CollectionSchema
        {
            Name = name,
            Fields =
            [
                new() { Name = "title", Type = TorqueFieldType.String },
                new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true },
                new() { Name = "brand", Type = TorqueFieldType.String, Facet = true },
            ],
        });
        foreach (var (id, title, price, brand) in docs)
            await _client.Documents.CreateAsync(name, new Dictionary<string, object?> { ["id"] = id, ["title"] = title, ["price"] = price, ["brand"] = brand });
        return name;
    }

    private static List<string> Ids(SearchResult r) => r.Hits.Select(h => ((JsonElement)h.Document["id"]!).GetString()!).ToList();

    [Fact]
    public async Task Union_OrdersBySortBy_AcrossCollections()
    {
        var union = await _client.MultiSearchUnionAsync(new MultiSearchRequest
        {
            Searches =
            [
                new MultiSearchQuery { Collection = _products, Q = "*", SortBy = "price:desc" },
                new MultiSearchQuery { Collection = _gadgets, Q = "*", SortBy = "price:desc" },
            ],
        }, new Dictionary<string, string> { ["per_page"] = "10" });

        Assert.Equal(["1", "g2", "2", "g1", "3"], Ids(union));
        Assert.Equal(5, union.Found);
        Assert.Equal(1, union.Hits[1].SearchIndex);
        Assert.Equal(_gadgets, union.Hits[1].Collection);
    }

    [Fact]
    public async Task Union_CountsADocumentOnce_UnlessAskedNotTo()
    {
        MultiSearchRequest Overlap(bool? dupes) => new()
        {
            RemoveDuplicates = dupes,
            Searches =
            [
                new MultiSearchQuery { Collection = _products, Q = "laptop", QueryBy = "title", FacetBy = "brand" },
                new MultiSearchQuery { Collection = _products, Q = "stand", QueryBy = "title", FacetBy = "brand" },
            ],
        };
        var once = await _client.MultiSearchUnionAsync(Overlap(null));
        Assert.Equal(2, once.Found);
        Assert.Equal(2, once.Hits.Count);
        Assert.Equal(1, once.FacetCounts!.Single().Counts.Single(c => c.Value == "Beta").Count);

        var each = await _client.MultiSearchUnionAsync(Overlap(false));
        Assert.Equal(3, each.Found);
        Assert.Equal(2, each.FacetCounts!.Single().Counts.Single(c => c.Value == "Beta").Count);
    }

    [Fact]
    public async Task Union_WithSortsThatDiffer_Throws()
    {
        var ex = await Assert.ThrowsAsync<BadRequestException>(() => _client.MultiSearchUnionAsync(new MultiSearchRequest
        {
            Searches =
            [
                new MultiSearchQuery { Collection = _products, Q = "*", SortBy = "price:asc" },
                new MultiSearchQuery { Collection = _gadgets, Q = "*", SortBy = "price:desc" },
            ],
        }));
        Assert.Equal("Expected order of `price` sort_by (DESC) at search index `1` to be the same as the order of `price` sort_by (ASC) at search index `0`.", ex.Message);
    }

    [Fact]
    public async Task MultiSearch_SearchWithItsOwnKey()
    {
        var parent = await _client.Keys.CreateAsync(new CreateKeyRequest { Description = "union test", Actions = ["documents:search"], Collections = [_products] });
        try
        {
            var beta = ScopedKeyHelper.GenerateScopedKey(parent.Value!, new Dictionary<string, object> { ["filter_by"] = "brand:=Beta" });
            var response = await _client.MultiSearchAsync(new MultiSearchRequest
            {
                Searches =
                [
                    new MultiSearchQuery { Collection = _products, Q = "*", ApiKey = beta },
                    new MultiSearchQuery { Collection = _products, Q = "*" },
                ],
            });
            Assert.Equal(2, response.Results[0].GetProperty("found").GetInt32());
            Assert.Equal(3, response.Results[1].GetProperty("found").GetInt32());

            // the scoped key's parent may not search the other collection
            await Assert.ThrowsAsync<ForbiddenException>(() => _client.MultiSearchAsync(new MultiSearchRequest
            {
                Searches = [new MultiSearchQuery { Collection = _gadgets, Q = "*", ApiKey = beta }],
            }));
        }
        finally
        {
            await _client.Keys.DeleteAsync(parent.Id);
        }
    }
}
