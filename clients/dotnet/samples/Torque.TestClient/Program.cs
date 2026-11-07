using System.Diagnostics;
using System.Text.Json;
using Torque.Http;
using Torque.Http.Models;
using Torque.Http.Search;
using Torque.Http.Tcp;

// ───────────────────────────────────────────────────────────────────
// Torque Test Client — exercises every major API feature
// Usage: dotnet run [--url http://localhost:8108] [--key test-key] [--tcp-port 8109] [--docs 5000]
// ───────────────────────────────────────────────────────────────────

var url = GetArg(args, "--url") ?? "http://localhost:8108";
var apiKey = GetArg(args, "--key") ?? "test-bootstrap-key";
var tcpPort = int.Parse(GetArg(args, "--tcp-port") ?? "8109");
var docCount = int.Parse(GetArg(args, "--docs") ?? "5000");

Console.WriteLine($"Torque Test Client");
Console.WriteLine($"  HTTP:  {url}");
Console.WriteLine($"  TCP:   localhost:{tcpPort}");
Console.WriteLine($"  Docs:  {docCount:N0}");
Console.WriteLine();

using var client = new TorqueHttpClient(new TorqueHttpClientOptions { BaseUrl = url, ApiKey = apiKey });

// ─── Health ───────────────────────────────────────────────────────
await Section("Health Check", async () =>
{
    var health = await client.HealthAsync();
    Print($"OK: {health.Ok}");
    var metrics = await client.MetricsAsync();
    Print($"Metrics: {metrics.Length:N0} chars");
});

// ─── Collection Setup ─────────────────────────────────────────────
const string collName = "products";

await Section("Create Collection", async () =>
{
    // Delete if exists (ignore errors)
    try { await client.Collections.DeleteAsync(collName); Print("Deleted existing collection"); }
    catch { /* ignore */ }

    var schema = ProductSchema();
    var created = await client.Collections.CreateAsync(schema);
    Print($"Created '{created.Name}' with {created.Fields?.Count} fields");
});

// ─── TCP Ingest ───────────────────────────────────────────────────
await Section($"TCP Ingest ({docCount:N0} docs)", async () =>
{
    var schema = ProductSchema();
    await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Port = tcpPort });
    await ingest.ConnectAsync();

    var ack = await ingest.StartIngestAsync(collName);
    Print($"Session started (seq={ack.BatchSeq})");

    var sw = Stopwatch.StartNew();
    int batchSize = 500;
    var rng = new Random(42);
    var categories = new[] { "Electronics", "Books", "Clothing", "Home", "Sports", "Toys", "Food", "Beauty", "Garden", "Automotive" };
    var cities = new[] { "Stockholm", "Gothenburg", "Malmö", "Uppsala", "London", "Berlin", "Paris", "Oslo", "Copenhagen", "Helsinki" };
    var lats = new[] { 59.33, 57.71, 55.60, 59.86, 51.51, 52.52, 48.86, 59.91, 55.68, 60.17 };
    var lngs = new[] { 18.07, 11.97, 13.00, 17.64, -0.13, 13.41, 2.35, 10.75, 12.57, 24.94 };

    for (int batch = 0; batch < docCount; batch += batchSize)
    {
        int count = Math.Min(batchSize, docCount - batch);
        var docs = new TorqueDocument[count];
        for (int i = 0; i < count; i++)
        {
            int id = batch + i;
            int cityIdx = id % cities.Length;
            var builder = new TorqueDocumentBuilder()
                .SetId($"prod-{id}")
                .SetLanguage(id % 5 == 0 ? Language.Swedish : Language.English)
                .Set("title", GenerateTitle(rng, id))
                .Set("description", GenerateDescription(rng, id))
                .Set("price", 10 + rng.Next(990))
                .Set("rating", Math.Round(1.0 + rng.NextDouble() * 4.0, 1))
                .Set("in_stock", rng.Next(100) < 80)
                .Set("category", categories[id % categories.Length])
                .Set("tags", GenerateTags(rng))
                .SetGeoPoint("location", lats[cityIdx] + rng.NextDouble() * 0.1, lngs[cityIdx] + rng.NextDouble() * 0.1)
                .Set("popularity", rng.Next(10000))
                .Set("city", cities[cityIdx]);
            docs[i] = builder.Build(schema);
        }

        var batchAck = await ingest.SendBatchAsync(docs, schema);
        if ((batch / batchSize) % 4 == 0)
            Print($"  batch {batch / batchSize + 1}: {batchAck.DocsReceived} docs received");
    }

    var commit = await ingest.CommitAsync();
    sw.Stop();
    Print($"Committed: {commit.DocsReceived} docs accepted, total {sw.ElapsedMilliseconds}ms (rebuild async)");
    Print($"Throughput: {docCount * 1000.0 / sw.ElapsedMilliseconds:N0} docs/sec");
});

// ─── Synonyms ─────────────────────────────────────────────────────
await Section("Synonyms", async () =>
{
    await client.Synonyms.UpsertSetAsync("product-synonyms", new SynonymSet { Name = "product-synonyms" });
    await client.Synonyms.UpsertItemAsync("product-synonyms", "laptop-synonyms", new SynonymItem
    {
        Id = "laptop-synonyms",
        Synonyms = ["laptop", "notebook", "portable computer"],
    });
    await client.Synonyms.UpsertItemAsync("product-synonyms", "phone-synonyms", new SynonymItem
    {
        Id = "phone-synonyms",
        Synonyms = ["phone", "mobile", "smartphone", "cell phone"],
    });
    Print("Created synonym set 'product-synonyms' with 2 items");
});

// ─── Stopwords ────────────────────────────────────────────────────
await Section("Stopwords", async () =>
{
    await client.Stopwords.UpsertAsync("english-stops", new StopwordSet
    {
        Id = "english-stops",
        Stopwords = ["the", "a", "an", "is", "are", "was", "were", "of", "for", "with"],
        Locale = "en",
    });
    Print("Created stopword set 'english-stops'");
});

// ─── Presets ──────────────────────────────────────────────────────
await Section("Presets", async () =>
{
    var presetValue = JsonDocument.Parse("""{"sort_by": "rating:desc", "per_page": "5"}""").RootElement;
    await client.Presets.UpsertAsync("top-rated", new Preset { Name = "top-rated", Value = presetValue });
    Print("Created preset 'top-rated' (sort by rating desc, 5 per page)");
});

// ─── Search Queries ───────────────────────────────────────────────
Console.WriteLine();
Console.WriteLine("═══ SEARCH QUERIES ═══");
Console.WriteLine();

// 1. Basic text search
await SearchTest("1. Basic text search", collName, new SearchParameters
{
    Q = "laptop",
    QueryBy = "title,description",
    PerPage = 5,
});

// 2. Filter by price range + category
await SearchTest("2. Filter: price >= 500 AND category Electronics", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "price:>= 500 && category:= Electronics",
    SortBy = "price:asc",
    PerPage = 5,
});

// 3. Faceted search
await SearchTest("3. Facets on category + price stats", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FacetBy = "category,price",
    PerPage = 3,
});

// 4. Sort by rating desc
await SearchTest("4. Sort by rating descending", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    SortBy = "rating:desc",
    PerPage = 5,
});

// 5. Boolean filter
await SearchTest("5. Filter: in_stock = true, sorted by popularity", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "in_stock:= true",
    SortBy = "popularity:desc",
    PerPage = 5,
});

// 6. Highlighting
await SearchTest("6. Highlighting on title+description", collName, new SearchParameters
{
    Q = "premium wireless",
    QueryBy = "title,description",
    HighlightFields = "title,description",
    PerPage = 5,
}, showHighlights: true);

// 7. Typo tolerance
await SearchTest("7. Typo tolerance: 'laptp' (typo for laptop)", collName, new SearchParameters
{
    Q = "laptp",
    QueryBy = "title,description",
    NumTypos = 2,
    PerPage = 5,
});

// 8. Group by category
await GroupSearchTest("8. Group by category", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    GroupBy = "category",
    GroupLimit = 2,
    PerPage = 5,
});

// 9. Geo search: within 50km of Stockholm
await SearchTest("9. Geo: within 50km of Stockholm (59.33, 18.07)", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "location:(59.33, 18.07, 50 km)",
    SortBy = "location(59.33, 18.07):asc",
    PerPage = 5,
}, showGeo: true);

// 10. Multi-field text match type
await SearchTest("10. TextMatchType = sum_score", collName, new SearchParameters
{
    Q = "premium quality",
    QueryBy = "title,description",
    TextMatchType = "sum_score",
    PerPage = 5,
});

// 11. Synonym expansion
await SearchTest("11. Synonym: 'notebook' → expands via product-synonyms", collName, new SearchParameters
{
    Q = "notebook",
    QueryBy = "title,description",
    SynonymSets = "product-synonyms",
    PerPage = 5,
});

// 12. Stopwords
await SearchTest("12. Stopwords: 'the laptop for the office' → strips stop words", collName, new SearchParameters
{
    Q = "the laptop for the office",
    QueryBy = "title,description",
    Stopwords = "english-stops",
    PerPage = 5,
});

// 13. Preset
await SearchTest("13. Preset 'top-rated' (sort by rating desc, 5 per page)", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    Preset = "top-rated",
});

// 14. IN filter
await SearchTest("14. Filter: category IN [Books, Toys]", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "category:= [Books, Toys]",
    PerPage = 5,
});

// 15. Range filter
await SearchTest("15. Filter: price [100..200]", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "price:[100..200]",
    SortBy = "price:asc",
    PerPage = 5,
});

// 16. Prefix search
await SearchTest("16. Prefix search: 'pre' with prefix=true", collName, new SearchParameters
{
    Q = "pre",
    QueryBy = "title,description",
    Prefix = true,
    PerPage = 5,
});

// 17. Pagination
await SearchTest("17. Pagination: page 3, 10 per page", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    Page = 3,
    PerPage = 10,
});

// 18. Combined: filter + facet + sort + highlight
await SearchTest("18. Combined: filter + facet + sort + highlight", collName, new SearchParameters
{
    Q = "wireless",
    QueryBy = "title,description",
    FilterBy = "price:>= 100 && in_stock:= true",
    SortBy = "rating:desc",
    FacetBy = "category",
    HighlightFields = "title",
    PerPage = 5,
}, showHighlights: true);

// 19. Multi-search
await Section("19. Multi-search (2 queries)", async () =>
{
    var response = await client.MultiSearchAsync(new MultiSearchRequest
    {
        Searches =
        [
            new MultiSearchQuery { Collection = collName, Q = "laptop", QueryBy = "title", PerPage = 3 },
            new MultiSearchQuery { Collection = collName, Q = "garden", QueryBy = "title,description", PerPage = 3 },
        ]
    });

    for (int i = 0; i < response.Results.Count; i++)
    {
        var r = response.Results[i];
        var found = r.GetProperty("found").GetInt32();
        var hits = r.GetProperty("hits");
        var timeMs = r.GetProperty("search_time_ms").GetInt64();
        Print($"  Query {i + 1}: found={found}, hits={hits.GetArrayLength()}, time={timeMs}ms");
        foreach (var hit in hits.EnumerateArray())
        {
            var doc = hit.GetProperty("document");
            var id = doc.TryGetProperty("id", out var idEl) ? idEl.ToString() : "?";
            var title = doc.TryGetProperty("title", out var titleEl) ? titleEl.GetString() : "?";
            Print($"    [{id}] {title}");
        }
    }
});

// 20. Decay sort
await SearchTest("20. Decay sort: popularity gauss(origin=5000, scale=2000)", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    SortBy = "popularity(origin:5000,func:gauss,scale:2000):desc",
    PerPage = 5,
});

// 21. Per-field weights
await SearchTest("21. Per-field weights: title:3, description:1", collName, new SearchParameters
{
    Q = "premium",
    QueryBy = "title:3,description:1",
    PerPage = 5,
});

// 22. Max facet values
await SearchTest("22. max_facet_values=3 on category", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FacetBy = "category",
    MaxFacetValues = 3,
    PerPage = 3,
});

// 23. Facet query (prefix filter)
await SearchTest("23. facet_query: category starts with 'Elec'", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FacetBy = "category",
    FacetQuery = "category:Elec",
    PerPage = 3,
});

// 24. Highlight custom tags + snippet windowing
await SearchTest("24. Highlight: custom tags <b>/<b> + affix 4 tokens", collName, new SearchParameters
{
    Q = "premium wireless",
    QueryBy = "title,description",
    HighlightFields = "title,description",
    HighlightStartTag = "<b>",
    HighlightEndTag = "</b>",
    HighlightAffixNumTokens = 4,
    PerPage = 5,
}, showHighlights: true);

// 25. Preset in multi_search item
await Section("25. Multi-search with per-item preset", async () =>
{
    var response = await client.MultiSearchAsync(new MultiSearchRequest
    {
        Searches =
        [
            new MultiSearchQuery { Collection = collName, Q = "*", QueryBy = "title", Preset = "top-rated" },
            new MultiSearchQuery { Collection = collName, Q = "laptop", QueryBy = "title", PerPage = 3 },
        ]
    });

    for (int i = 0; i < response.Results.Count; i++)
    {
        var r = response.Results[i];
        var found = r.GetProperty("found").GetInt32();
        var hits = r.GetProperty("hits");
        var timeMs = r.GetProperty("search_time_ms").GetInt64();
        Print($"  Query {i + 1}: found={found}, hits={hits.GetArrayLength()}, time={timeMs}ms");
        foreach (var hit in hits.EnumerateArray())
        {
            var doc = hit.GetProperty("document");
            var id = doc.TryGetProperty("id", out var idEl) ? idEl.ToString() : "?";
            var title = doc.TryGetProperty("title", out var titleEl) ? titleEl.GetString() : "?";
            var rating = doc.TryGetProperty("rating", out var ratingEl) ? ratingEl.ToString() : "?";
            Print($"    [{id}] {title}  (rating={rating})");
        }
    }
});

// 26. Multi preset (top-level preset on multi_search)
await Section("26. Multi-search with top-level preset", async () =>
{
    // Create a multi preset that sets sort_by for all sub-queries
    var multiPresetValue = JsonDocument.Parse("""{"sort_by": "price:asc", "per_page": "3"}""").RootElement;
    await client.Presets.UpsertAsync("cheap-first", new Preset { Name = "cheap-first", Value = multiPresetValue });

    var response = await client.MultiSearchAsync(new MultiSearchRequest
    {
        Preset = "cheap-first",
        Searches =
        [
            new MultiSearchQuery { Collection = collName, Q = "*", QueryBy = "title" },
            new MultiSearchQuery { Collection = collName, Q = "laptop", QueryBy = "title,description" },
        ]
    });

    for (int i = 0; i < response.Results.Count; i++)
    {
        var r = response.Results[i];
        var found = r.GetProperty("found").GetInt32();
        var hits = r.GetProperty("hits");
        Print($"  Query {i + 1}: found={found}, hits={hits.GetArrayLength()}");
        foreach (var hit in hits.EnumerateArray())
        {
            var doc = hit.GetProperty("document");
            var id = doc.TryGetProperty("id", out var idEl) ? idEl.ToString() : "?";
            var title = doc.TryGetProperty("title", out var titleEl) ? titleEl.GetString() : "?";
            var price = doc.TryGetProperty("price", out var priceEl) ? priceEl.ToString() : "?";
            Print($"    [{id}] {title}  (price={price})");
        }
    }

    // Cleanup this preset
    await client.Presets.DeleteAsync("cheap-first");
});

// 27. Geo bounding box filter
await SearchTest("27. Geo bounding box: Nordic region (55-60N, 10-20E)", collName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "location:(55.0, 10.0, 60.5, 20.0)",
    PerPage = 5,
}, showGeo: true);

// 28. Scoped key with limit_hits
await Section("28. Scoped key with limit_hits (pagination check)", async () =>
{
    // Create admin key, then scoped key with limit_hits=10
    var adminKey = await client.Keys.CreateAsync(new CreateKeyRequest
    {
        Description = "test-admin-for-scoped",
        Actions = ["*"],
        Collections = ["*"],
    });

    var scopedKey = Torque.Http.Auth.ScopedKeyHelper.GenerateScopedKey(
        adminKey.Value!,
        new Dictionary<string, object>
        {
            ["filter_by"] = "in_stock:= true",
            ["limit_hits"] = 10,
        });

    using var scopedClient = new TorqueHttpClient(new TorqueHttpClientOptions { BaseUrl = url, ApiKey = scopedKey });

    // Page 1: should return up to 5 hits (offset=0, limit_hits=10 → per_page stays 5)
    var p1 = await scopedClient.SearchAsync(collName, new SearchParameters
    {
        Q = "*",
        QueryBy = "title",
        Page = 1,
        PerPage = 5,
    });
    Print($"Page 1: found={p1.Found}, hits={p1.Hits.Count}");

    // Page 2: should return up to 5 hits (offset=5, limit_hits=10 → per_page=min(5,5)=5)
    var p2 = await scopedClient.SearchAsync(collName, new SearchParameters
    {
        Q = "*",
        QueryBy = "title",
        Page = 2,
        PerPage = 5,
    });
    Print($"Page 2: found={p2.Found}, hits={p2.Hits.Count}");

    // Page 3: should return 0 hits (offset=10, limit_hits=10 → per_page=0)
    var p3 = await scopedClient.SearchAsync(collName, new SearchParameters
    {
        Q = "*",
        QueryBy = "title",
        Page = 3,
        PerPage = 5,
    });
    Print($"Page 3: found={p3.Found}, hits={p3.Hits.Count}");

    // Cleanup
    await client.Keys.DeleteAsync(adminKey.Id);
});

// ═══════════════════════════════════════════════════════════════════
// GPU VECTOR SEARCH (queries 29-33)
// Uses 128-dim embeddings to exercise RaBitQ quantization + GPU dispatch
// ═══════════════════════════════════════════════════════════════════
Console.WriteLine();
Console.WriteLine("═══ GPU VECTOR SEARCH ═══");
Console.WriteLine();

const string vecCollName = "articles";
const int vecDim = 128;

await Section("Create vector collection", async () =>
{
    try { await client.Collections.DeleteAsync(vecCollName); } catch { }

    var schema = new CollectionSchema
    {
        Name = vecCollName,
        Fields =
        [
            new() { Name = "title", Type = TorqueFieldType.String },
            new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
            new() { Name = "embedding", Type = TorqueFieldType.FloatArray, NumDim = vecDim, VecDist = VecDist.Cosine, Stem = false },
        ],
    };
    var created = await client.Collections.CreateAsync(schema);
    Print($"Created '{created.Name}' with {created.Fields?.Count} fields (embedding: {vecDim}-dim cosine)");
});

await Section($"TCP Ingest vector docs (1000 docs, {vecDim}-dim)", async () =>
{
    var schema = new CollectionSchema
    {
        Name = vecCollName,
        Fields =
        [
            new() { Name = "title", Type = TorqueFieldType.String },
            new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
            new() { Name = "embedding", Type = TorqueFieldType.FloatArray, NumDim = vecDim, VecDist = VecDist.Cosine, Stem = false },
        ],
    };

    await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Port = tcpPort });
    await ingest.ConnectAsync();
    await ingest.StartIngestAsync(vecCollName);

    var rng = new Random(123);
    var categories = new[] { "science", "sports", "politics", "technology" };
    int batchSize = 200;
    int totalDocs = 1000;

    for (int batch = 0; batch < totalDocs; batch += batchSize)
    {
        int count = Math.Min(batchSize, totalDocs - batch);
        var docs = new TorqueDocument[count];
        for (int i = 0; i < count; i++)
        {
            int id = batch + i;
            // Generate a deterministic 128-dim embedding with category-based clusters
            var embedding = new float[vecDim];
            int catIdx = id % categories.Length;
            for (int d = 0; d < vecDim; d++)
                embedding[d] = (float)(rng.NextDouble() * 0.3 + (d % 4 == catIdx ? 0.7 : 0.0));
            // Normalize to unit vector for cosine similarity
            float norm = 0;
            for (int d = 0; d < vecDim; d++) norm += embedding[d] * embedding[d];
            norm = MathF.Sqrt(norm);
            for (int d = 0; d < vecDim; d++) embedding[d] /= norm;

            var builder = new TorqueDocumentBuilder()
                .SetId($"art-{id}")
                .SetLanguage(Language.English)
                .Set("title", $"{categories[catIdx]} article {id}")
                .Set("category", categories[catIdx])
                .Set("embedding", embedding);
            docs[i] = builder.Build(schema);
        }
        await ingest.SendBatchAsync(docs, schema);
    }

    var commit = await ingest.CommitAsync();
    Print($"Committed: {commit.DocsReceived} docs accepted (rebuild async)");
    Print($"  (128-dim → RaBitQ quantization + GPU vector upload)");
});

// 29. Vector-only search (pure nearest-neighbor)
await SearchTest("29. GPU vector search: nearest to science cluster", vecCollName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    VectorQuery = $"embedding:([{string.Join(",", GenerateQueryVector(vecDim, 0, 42))}], k:5)",
    PerPage = 5,
});

// 30. Hybrid search (text + vector with RRF)
await SearchTest("30. GPU hybrid search: 'technology' + vector", vecCollName, new SearchParameters
{
    Q = "technology",
    QueryBy = "title",
    VectorQuery = $"embedding:([{string.Join(",", GenerateQueryVector(vecDim, 3, 99))}], k:10)",
    PerPage = 5,
});

// 31. Filtered vector search (GPU filtered kernel)
await SearchTest("31. GPU filtered vector search: category=sports + vector", vecCollName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    FilterBy = "category:= sports",
    VectorQuery = $"embedding:([{string.Join(",", GenerateQueryVector(vecDim, 1, 77))}], k:5)",
    PerPage = 5,
});

// 32. Vector search with facets
await SearchTest("32. GPU vector search with facets", vecCollName, new SearchParameters
{
    Q = "*",
    QueryBy = "title",
    VectorQuery = $"embedding:([{string.Join(",", GenerateQueryVector(vecDim, 2, 55))}], k:20)",
    FacetBy = "category",
    PerPage = 5,
});

// 33. Multi-search with vector queries
await Section("33. GPU multi-search with vector queries", async () =>
{
    var response = await client.MultiSearchAsync(new MultiSearchRequest
    {
        Searches =
        [
            new MultiSearchQuery
            {
                Collection = vecCollName, Q = "*", QueryBy = "title", PerPage = 3,
            },
            new MultiSearchQuery
            {
                Collection = vecCollName, Q = "sports", QueryBy = "title", PerPage = 3,
            },
        ]
    });

    for (int i = 0; i < response.Results.Count; i++)
    {
        var r = response.Results[i];
        var found = r.GetProperty("found").GetInt32();
        var hits = r.GetProperty("hits");
        Print($"  Query {i + 1}: found={found}, hits={hits.GetArrayLength()}");
        foreach (var hit in hits.EnumerateArray())
        {
            var doc = hit.GetProperty("document");
            var id = doc.TryGetProperty("id", out var idEl) ? idEl.ToString() : "?";
            var title = doc.TryGetProperty("title", out var titleEl) ? titleEl.GetString() : "?";
            var dist = hit.TryGetProperty("vector_distance", out var distEl) ? distEl.ToString() : "-";
            Print($"    [{id}] {title}  (dist={dist})");
        }
    }
});

// Cleanup vector collection
await Section("Cleanup vector collection", async () =>
{
    await client.Collections.DeleteAsync(vecCollName);
    Print($"Deleted '{vecCollName}'");
});

// ─── Cleanup ──────────────────────────────────────────────────────
await Section("Cleanup", async () =>
{
    await client.Presets.DeleteAsync("top-rated");
    await client.Stopwords.DeleteAsync("english-stops");
    await client.Synonyms.DeleteSetAsync("product-synonyms");
    await client.Collections.DeleteAsync(collName);
    Print("Deleted collection, synonyms, stopwords, preset");
});

Console.WriteLine();
Console.WriteLine("Done.");

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

async Task Section(string name, Func<Task> action)
{
    Console.WriteLine($"─── {name} ───");
    var sw = Stopwatch.StartNew();
    try
    {
        await action();
    }
    catch (Exception ex)
    {
        Console.ForegroundColor = ConsoleColor.Red;
        Console.WriteLine($"  ERROR: {ex.Message}");
        Console.ResetColor();
    }
    Console.WriteLine($"  [{sw.ElapsedMilliseconds}ms]");
    Console.WriteLine();
}

async Task SearchTest(string name, string collection, SearchParameters p, bool showHighlights = false, bool showGeo = false)
{
    Console.WriteLine($"─── {name} ───");
    try
    {
        var sw = Stopwatch.StartNew();
        var result = await client.SearchAsync(collection, p);
        sw.Stop();
        Print($"Found: {result.Found}, Hits: {result.Hits.Count}, Time: {result.SearchTimeMs}ms (roundtrip {sw.ElapsedMilliseconds}ms)");

        if (result.FacetCounts is { Count: > 0 })
        {
            foreach (var fc in result.FacetCounts)
            {
                if (fc.Counts.Count > 0)
                    Print($"  Facet '{fc.FieldName}': {string.Join(", ", fc.Counts.Take(5).Select(c => $"{c.Value}({c.Count})"))}");
                if (fc.Stats is { } s)
                    Print($"  Facet '{fc.FieldName}' stats: min={s.Min}, max={s.Max}, avg={s.Avg:F1}, sum={s.Sum}");
            }
        }

        foreach (var hit in result.Hits)
        {
            PrintHit(hit, showHighlights, showGeo);
        }
    }
    catch (Exception ex)
    {
        Console.ForegroundColor = ConsoleColor.Red;
        Console.WriteLine($"  ERROR: {ex.Message}");
        Console.ResetColor();
    }
    Console.WriteLine();
}

async Task GroupSearchTest(string name, string collection, SearchParameters p)
{
    Console.WriteLine($"─── {name} ───");
    try
    {
        var sw = Stopwatch.StartNew();
        var result = await client.SearchGroupedAsync(collection, p);
        sw.Stop();
        Print($"Groups: {result.Found}, Docs: {result.FoundDocs}, Time: {result.SearchTimeMs}ms");

        foreach (var group in result.GroupedHits.Take(5))
        {
            var key = string.Join(", ", group.GroupKey.Select(k => k.ToString()));
            Print($"  Group [{key}]: {group.Found} docs");
            foreach (var hit in group.Hits.Take(2))
                PrintHit(hit, indent: "    ");
        }
    }
    catch (Exception ex)
    {
        Console.ForegroundColor = ConsoleColor.Red;
        Console.WriteLine($"  ERROR: {ex.Message}");
        Console.ResetColor();
    }
    Console.WriteLine();
}

void PrintHit(Hit hit, bool showHighlights = false, bool showGeo = false, string indent = "  ")
{
    var doc = hit.Document;
    var id = doc.GetValueOrDefault("id") ?? doc.GetValueOrDefault("external_id");
    var title = doc.GetValueOrDefault("title");
    var price = doc.GetValueOrDefault("price");
    var rating = doc.GetValueOrDefault("rating");
    var category = doc.GetValueOrDefault("category");
    Print($"{indent}[{id}] {title}  (price={price}, rating={rating}, cat={category})");

    if (showHighlights && hit.Highlights is { Count: > 0 })
    {
        foreach (var h in hit.Highlights)
            Print($"{indent}  highlight({h.Field}): {h.Snippet?.Substring(0, Math.Min(100, h.Snippet?.Length ?? 0))}...");
    }
    if (showGeo && hit.GeoDistanceMeters is { Count: > 0 })
    {
        foreach (var (field, dist) in hit.GeoDistanceMeters)
            Print($"{indent}  geo({field}): {dist}m");
    }
    if (hit.VectorDistance.HasValue)
        Print($"{indent}  vector_distance: {hit.VectorDistance}");
}

static void Print(string msg) => Console.WriteLine($"  {msg}");

static string? GetArg(string[] args, string name)
{
    for (int i = 0; i < args.Length - 1; i++)
        if (args[i] == name) return args[i + 1];
    return null;
}

static CollectionSchema ProductSchema() => new()
{
    Name = "products",
    Fields =
    [
        new() { Name = "title", Type = TorqueFieldType.String },
        new() { Name = "description", Type = TorqueFieldType.String },
        new() { Name = "price", Type = TorqueFieldType.Int32, Facet = true, Sort = true },
        new() { Name = "rating", Type = TorqueFieldType.Float, Sort = true },
        new() { Name = "in_stock", Type = TorqueFieldType.Bool, Facet = true },
        new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
        new() { Name = "tags", Type = TorqueFieldType.StringArray, Facet = true },
        new() { Name = "location", Type = TorqueFieldType.GeoPoint },
        new() { Name = "popularity", Type = TorqueFieldType.Int32, Sort = true },
        new() { Name = "city", Type = TorqueFieldType.String, Facet = true },
    ]
};

static string GenerateTitle(Random rng, int id)
{
    string[] adjectives = ["Premium", "Ultra", "Pro", "Basic", "Deluxe", "Compact", "Advanced", "Smart", "Wireless", "Portable"];
    string[] nouns = ["Laptop", "Phone", "Headphones", "Speaker", "Camera", "Tablet", "Watch", "Keyboard", "Mouse", "Monitor"];
    string[] suffixes = ["X1", "Max", "Plus", "Elite", "SE", "Air", "Mini", "Go", "Flex", "One"];
    return $"{adjectives[rng.Next(adjectives.Length)]} {nouns[rng.Next(nouns.Length)]} {suffixes[rng.Next(suffixes.Length)]}";
}

static string GenerateDescription(Random rng, int id)
{
    string[] features = [
        "high-quality build with premium materials",
        "wireless connectivity and long battery life",
        "compact design perfect for travel",
        "advanced noise cancellation technology",
        "water resistant and durable construction",
        "fast charging and extended runtime",
        "ergonomic design for comfortable use",
        "crystal clear display with vivid colors",
        "lightweight and portable for on-the-go",
        "smart features with voice assistant support",
    ];
    return $"Product {id}: {features[rng.Next(features.Length)]}. {features[rng.Next(features.Length)]}.";
}

static string[] GenerateTags(Random rng)
{
    string[] allTags = ["sale", "new", "bestseller", "eco-friendly", "trending", "limited", "premium", "budget", "featured", "exclusive"];
    int count = 1 + rng.Next(3);
    return allTags.OrderBy(_ => rng.Next()).Take(count).ToArray();
}

/// Generate a query vector that's similar to documents in a given category cluster.
/// Uses the same pattern as ingest: dimension d % 4 == catIdx gets a high component.
static string[] GenerateQueryVector(int dim, int catIdx, int seed)
{
    var rng = new Random(seed);
    var vec = new float[dim];
    for (int d = 0; d < dim; d++)
        vec[d] = (float)(rng.NextDouble() * 0.3 + (d % 4 == catIdx ? 0.7 : 0.0));
    float norm = 0;
    for (int d = 0; d < dim; d++) norm += vec[d] * vec[d];
    norm = MathF.Sqrt(norm);
    for (int d = 0; d < dim; d++) vec[d] /= norm;
    return vec.Select(v => v.ToString("F6", System.Globalization.CultureInfo.InvariantCulture)).ToArray();
}
