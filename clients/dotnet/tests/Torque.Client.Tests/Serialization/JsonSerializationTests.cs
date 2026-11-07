using System.Text.Json;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Tests.Serialization;

public class JsonSerializationTests
{
    [Fact]
    public void SearchResult_Deserialize()
    {
        var json = """
        {
            "found": 42,
            "hits": [{
                "document": {"id": "1", "title": "Test"},
                "text_match": 850000,
                "highlights": [{"field": "title", "snippet": "<mark>Test</mark>", "matched_tokens": ["Test"]}]
            }],
            "facet_counts": [{"field_name": "color", "counts": [{"value": "red", "count": 5}]}],
            "search_time_ms": 10
        }
        """;

        var result = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.SearchResult)!;
        Assert.Equal(42, result.Found);
        Assert.Single(result.Hits);
        Assert.Equal(850000UL, result.Hits[0].TextMatch);
        Assert.Single(result.Hits[0].Highlights!);
        Assert.Equal("title", result.Hits[0].Highlights![0].Field);
        Assert.Single(result.FacetCounts!);
        Assert.Equal(10, result.SearchTimeMs);
    }

    [Fact]
    public void SearchResult_ArrayFieldHighlightAndNameScore_Deserialize()
    {
        // a string[] field's highlight: matched_tokens per element; and name_score's evidence
        var json = """
        {
            "found": 1,
            "hits": [{
                "document": {"id": "un:1", "names": ["Usama bin Laden", "Osama bin Laden"]},
                "text_match": 100,
                "highlights": [{"field": "names", "snippets": ["<mark>Osama</mark> bin Laden"], "indices": [1],
                                "matched_tokens": [["Osama", "bin", "Laden"]]}],
                "name_score": {"score": 0.93, "field": "names", "name": "Usama bin Laden",
                               "pairs": [{"query": "osama", "name": "usama", "kind": "phonetic", "encoder": "double_metaphone", "similarity": 0.6}],
                               "unmatched_query": [], "unmatched_name": ["bin"]}
            }],
            "search_time_ms": 1
        }
        """;

        var result = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.SearchResult)!;
        var hit = result.Hits[0];
        Assert.Equal(["Osama", "bin", "Laden"], hit.Highlights![0].MatchedTokens);
        Assert.Equal([1], hit.Highlights![0].Indices!);
        Assert.Equal(0.93, hit.NameScore!.Score);
        Assert.Equal("double_metaphone", hit.NameScore.Pairs[0].Encoder);
        Assert.Equal(["bin"], hit.NameScore.UnmatchedName);
    }

    [Fact]
    public void GroupedSearchResult_Deserialize()
    {
        var json = """
        {
            "found": 2,
            "found_docs": 50,
            "grouped_hits": [{
                "group_key": ["electronics"],
                "hits": [{"document": {"id": "1"}, "text_match": 100}],
                "found": 25
            }],
            "search_time_ms": 15
        }
        """;

        var result = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.GroupedSearchResult)!;
        Assert.Equal(2, result.Found);
        Assert.Equal(50, result.FoundDocs);
        Assert.Single(result.GroupedHits);
        Assert.Equal(25, result.GroupedHits[0].Found);
    }

    [Fact]
    public void ApiKey_Roundtrip()
    {
        var key = new ApiKey
        {
            Id = 1,
            Value = "abc123",
            Description = "Test key",
            Actions = ["search"],
            Collections = ["products"],
            ExpiresAt = 1700000000,
        };

        var json = JsonSerializer.Serialize(key, TorqueJsonContext.Default.ApiKey);
        var deserialized = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.ApiKey)!;

        Assert.Equal(1UL, deserialized.Id);
        Assert.Equal("abc123", deserialized.Value);
        Assert.Equal(["search"], deserialized.Actions);
    }

    [Fact]
    public void CollectionAlias_Roundtrip()
    {
        var alias = new CollectionAlias { Name = "latest", CollectionName = "products_v2" };
        var json = JsonSerializer.Serialize(alias, TorqueJsonContext.Default.CollectionAlias);
        var deserialized = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.CollectionAlias)!;

        Assert.Equal("latest", deserialized.Name);
        Assert.Equal("products_v2", deserialized.CollectionName);
    }

    [Fact]
    public void SynonymSet_Roundtrip()
    {
        var set = new SynonymSet
        {
            Name = "colors",
            Items = [new SynonymItem { Id = "red", Synonyms = ["red", "scarlet"], Root = "red", Locale = "en" }]
        };

        var json = JsonSerializer.Serialize(set, TorqueJsonContext.Default.SynonymSet);
        var deserialized = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.SynonymSet)!;

        Assert.Equal("colors", deserialized.Name);
        Assert.Single(deserialized.Items);
        Assert.Equal("red", deserialized.Items[0].Root);
    }

    [Fact]
    public void StopwordSet_Roundtrip()
    {
        var set = new StopwordSet
        {
            Id = "en_stops",
            Stopwords = ["the", "a", "an"],
            Locale = "en",
        };

        var json = JsonSerializer.Serialize(set, TorqueJsonContext.Default.StopwordSet);
        var deserialized = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.StopwordSet)!;

        Assert.Equal("en_stops", deserialized.Id);
        Assert.Contains("the", deserialized.Stopwords);
        Assert.Equal(3, deserialized.Stopwords.Count);
        Assert.Equal("en", deserialized.Locale);
    }

    [Fact]
    public void Preset_Deserialize()
    {
        var json = """{"name": "popular", "value": {"sort_by": "rating:desc", "per_page": 20}}""";
        var preset = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.Preset)!;

        Assert.Equal("popular", preset.Name);
        Assert.Equal(JsonValueKind.Object, preset.Value.ValueKind);
    }

    [Fact]
    public void ImportResult_Deserialize()
    {
        var successJson = """{"success": true}""";
        var failJson = """{"success": false, "error": "bad format"}""";

        var success = JsonSerializer.Deserialize(successJson, TorqueJsonContext.Default.ImportResult)!;
        var fail = JsonSerializer.Deserialize(failJson, TorqueJsonContext.Default.ImportResult)!;

        Assert.True(success.Success);
        Assert.Null(success.Error);
        Assert.False(fail.Success);
        Assert.Equal("bad format", fail.Error);
    }

    [Fact]
    public void HealthResponse_Deserialize()
    {
        var json = """{"ok": true}""";
        var health = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.HealthResponse)!;
        Assert.True(health.Ok);
    }

    [Fact]
    public void CreateKeyRequest_Serialize()
    {
        var request = new CreateKeyRequest
        {
            Description = "test",
            Actions = ["search"],
            Collections = ["*"],
            ExpiresAt = 1700000000,
        };

        var json = JsonSerializer.Serialize(request, TorqueJsonContext.Default.CreateKeyRequest);
        Assert.Contains("\"description\"", json);
        Assert.Contains("\"actions\"", json);
        Assert.Contains("\"expires_at\"", json);
    }

    [Fact]
    public void MultiSearchRequest_Serialize()
    {
        var request = new MultiSearchRequest
        {
            Searches =
            [
                new() { Collection = "products", Q = "laptop", QueryBy = "title" },
            ]
        };

        var json = JsonSerializer.Serialize(request, TorqueJsonContext.Default.MultiSearchRequest);
        Assert.Contains("\"searches\"", json);
        Assert.Contains("\"collection\"", json);
    }
}
