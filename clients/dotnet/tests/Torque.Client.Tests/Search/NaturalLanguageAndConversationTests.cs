using System.Net;
using System.Text.Json;
using Torque.Http.Exceptions;
using Torque.Http.Models;
using Torque.Http.Search;

namespace Torque.Http.Tests.Search;

public class NaturalLanguageAndConversationTests
{
    [Fact]
    public void ToQueryString_NaturalLanguageAndConversation()
    {
        var p = new SearchParameters
        {
            Q = "red shoes under 50", QueryBy = "title",
            NlQuery = true, NlModelId = "gpt", NlQueryDebug = true, NlQueryPromptCacheTtl = 600,
            Conversation = true, ConversationModelId = "conv", ConversationId = "c1",
        };
        var qs = p.ToQueryString();
        Assert.Contains("nl_query=true", qs);
        Assert.Contains("nl_model_id=gpt", qs);
        Assert.Contains("nl_query_debug=true", qs);
        Assert.Contains("nl_query_prompt_cache_ttl=600", qs);
        Assert.Contains("conversation=true", qs);
        Assert.Contains("conversation_model_id=conv", qs);
        Assert.Contains("conversation_id=c1", qs);
    }

    [Fact]
    public async Task Search_ReadsParsedNlQueryAndConversation()
    {
        var handler = new MockHandler((_, _) => Respond("""
            {"found":1,"hits":[],"search_time_ms":3,
             "parsed_nl_query":{"parse_time_ms":12,"generated_params":{"q":"shoes","filter_by":"price:<50"},
                                "augmented_params":{"q":"shoes","filter_by":"(price:<50)"},"llm_response":{"content":"x"}},
             "conversation":{"answer":"Two pairs.","conversation_id":"c1","query":"red shoes",
                             "conversation_history":[{"user":"red shoes"},{"assistant":"Two pairs."}]}}
            """));
        using var client = Client(handler);
        var result = await client.SearchAsync("products", new SearchParameters { Q = "red shoes", NlQuery = true });

        Assert.NotNull(result.ParsedNlQuery);
        Assert.Equal(12, result.ParsedNlQuery!.ParseTimeMs);
        Assert.Equal("price:<50", result.ParsedNlQuery.GeneratedParams["filter_by"].GetString());
        Assert.Equal("(price:<50)", result.ParsedNlQuery.AugmentedParams["filter_by"].GetString());
        Assert.Null(result.ParsedNlQuery.Error);
        Assert.Equal("x", result.ParsedNlQuery.LlmResponse!.Value.GetProperty("content").GetString());
        Assert.Equal("Two pairs.", result.Conversation!.Answer);
        Assert.Equal("c1", result.Conversation.ConversationId);
        Assert.Equal("Two pairs.", result.Conversation.ConversationHistory![1]["assistant"]);
    }

    [Fact]
    public async Task MultiSearch_ConversationGoesInTheUrl_KeysAndNlInTheSearches()
    {
        string? url = null;
        JsonElement body = default;
        var handler = new MockHandler(async (request, ct) =>
        {
            url = request.RequestUri!.AbsoluteUri;
            body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(ct)).RootElement.Clone();
            return Json("""{"results":[{"found":0,"hits":[]}],"conversation":{"answer":"None.","conversation_id":"c9","query":"any boots?"}}""");
        });
        using var client = Client(handler);
        var response = await client.MultiSearchAsync(new MultiSearchRequest
        {
            Q = "any boots?", Conversation = true, ConversationModelId = "conv",
            Searches = [new MultiSearchQuery { Collection = "products", QueryBy = "title", NlQuery = true, NlModelId = "gpt", ApiKey = "scoped-key" }],
        });

        Assert.Contains("q=any%20boots%3F", url);
        Assert.Contains("conversation=true", url);
        Assert.Contains("conversation_model_id=conv", url);
        var search = body.GetProperty("searches")[0];
        Assert.False(search.TryGetProperty("q", out _));
        Assert.True(search.GetProperty("nl_query").GetBoolean());
        Assert.Equal("gpt", search.GetProperty("nl_model_id").GetString());
        Assert.Equal("scoped-key", search.GetProperty("x-typesense-api-key").GetString());
        Assert.False(body.TryGetProperty("q", out _));
        Assert.Equal("None.", response.Conversation!.Answer);
    }

    [Fact]
    public async Task MultiSearchUnion_ReadsHitsAndThrowsOnTheUnionsError()
    {
        var ok = new MockHandler((_, _) => Respond("""
            {"found":2,"out_of":5,"page":1,"search_time_ms":1,"hits":[
              {"document":{"id":"1"},"search_index":0,"collection":"products"},
              {"document":{"id":"g2"},"search_index":1,"collection":"gadgets"}],
             "facet_counts":[],"union_request_params":[],"search_cutoff":false}
            """));
        using (var client = Client(ok))
        {
            var union = await client.MultiSearchUnionAsync(new MultiSearchRequest { Searches = [new MultiSearchQuery { Collection = "products" }], RemoveDuplicates = false });
            Assert.Equal(2, union.Found);
            Assert.Equal(1, union.Hits[1].SearchIndex);
            Assert.Equal("gadgets", union.Hits[1].Collection);
        }

        var failed = new MockHandler((_, _) => Respond("""{"code":400,"error":"Expected order of `price` sort_by (DESC) at search index `1` to be the same as the order of `price` sort_by (ASC) at search index `0`."}"""));
        using (var client = Client(failed))
        {
            var ex = await Assert.ThrowsAsync<BadRequestException>(() => client.MultiSearchUnionAsync(new MultiSearchRequest { Searches = [new MultiSearchQuery { Collection = "products" }] }));
            Assert.StartsWith("Expected order of `price`", ex.Message);
        }
    }

    [Fact]
    public void MultiSearchRequest_SendsRemoveDuplicates()
    {
        var json = JsonSerializer.Serialize(new MultiSearchRequest { Searches = [], Union = true, RemoveDuplicates = false, Q = "kept out" }, Torque.Http.Serialization.TorqueJsonContext.Default.MultiSearchRequest);
        using var doc = JsonDocument.Parse(json);
        Assert.False(doc.RootElement.GetProperty("remove_duplicates").GetBoolean());
        Assert.False(doc.RootElement.TryGetProperty("q", out _));
        Assert.False(doc.RootElement.TryGetProperty("conversation", out _));
    }

    private static TorqueHttpClient Client(HttpMessageHandler handler) =>
        new(new TorqueHttpClientOptions { ApiKey = "test", Nodes = [new("http://localhost:8108")] }, new HttpClient(handler));

    private static HttpResponseMessage Json(string json) => new(HttpStatusCode.OK) { Content = new StringContent(json) };

    private static Task<HttpResponseMessage> Respond(string json) => Task.FromResult(Json(json));

    private sealed class MockHandler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> handler) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct) => handler(request, ct);
    }
}
