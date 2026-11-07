using System.Text.Json;
using BenchmarkDotNet.Attributes;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Benchmarks;

[MemoryDiagnoser]
public class JsonSerializationBenchmark
{
    private string _searchResultJson = null!;

    [GlobalSetup]
    public void Setup()
    {
        var result = new SearchResult
        {
            Found = 1000,
            SearchTimeMs = 5,
            Hits = Enumerable.Range(0, 100).Select(i => new Hit
            {
                Document = new Dictionary<string, object?>
                {
                    ["id"] = $"doc-{i}",
                    ["title"] = $"Search result document {i}",
                    ["price"] = i * 10,
                },
                TextMatch = (ulong)(1000000 - i * 100),
                Highlights =
                [
                    new Highlight
                    {
                        Field = "title",
                        Snippet = $"<mark>Search</mark> result document {i}",
                        MatchedTokens = ["Search"],
                    }
                ],
            }).ToList(),
            FacetCounts =
            [
                new FacetResult
                {
                    FieldName = "category",
                    Counts = Enumerable.Range(0, 10).Select(i => new FacetCount
                    {
                        Value = $"cat-{i}",
                        Count = 100 - i * 5,
                    }).ToList(),
                }
            ],
        };

        _searchResultJson = JsonSerializer.Serialize(result, TorqueJsonContext.Default.SearchResult);
    }

    [Benchmark]
    public SearchResult? DeserializeSearchResult() =>
        JsonSerializer.Deserialize(_searchResultJson, TorqueJsonContext.Default.SearchResult);

    [Benchmark]
    public string SerializeSearchResult()
    {
        var result = JsonSerializer.Deserialize(_searchResultJson, TorqueJsonContext.Default.SearchResult)!;
        return JsonSerializer.Serialize(result, TorqueJsonContext.Default.SearchResult);
    }
}
