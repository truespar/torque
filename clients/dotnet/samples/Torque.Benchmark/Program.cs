using System.Diagnostics;
using Torque.Http;
using Torque.Http.Models;
using Torque.Http.Search;
using Torque.Http.Tcp;

// ───────────────────────────────────────────────────────────────────
// Torque vs Typesense — Comparative Benchmark
// Usage: dotnet run -- --torque-url http://localhost:8108
//                      --typesense-url http://localhost:8108
//                      --torque-key test-bootstrap-key
//                      --typesense-key xyz
//                      --docs 100000 --iterations 50 --batch 1000
//                      [--skip-typesense] [--skip-tcp] [--tcp-port 8109] [--keep]
// ───────────────────────────────────────────────────────────────────

var torqueUrl = GetArg(args, "--torque-url") ?? "http://localhost:8108";
var typesenseUrl = GetArg(args, "--typesense-url") ?? "http://localhost:8109";
var torqueKey = GetArg(args, "--torque-key") ?? "test-bootstrap-key";
var typesenseKey = GetArg(args, "--typesense-key") ?? "xyz";
var totalDocs = int.Parse(GetArg(args, "--docs") ?? "100000");
var iterations = int.Parse(GetArg(args, "--iterations") ?? "50");
var batchSize = int.Parse(GetArg(args, "--batch") ?? "1000");
var skipTypesense = args.Contains("--skip-typesense");
var keep = args.Contains("--keep"); // leave the collections in place (profiling)
var tcpPort = int.Parse(GetArg(args, "--tcp-port") ?? "8109");

Console.WriteLine("╔═══════════════════════════════════════════════════════════════╗");
Console.WriteLine("║       Torque vs Typesense — Comparative Benchmark            ║");
Console.WriteLine("╚═══════════════════════════════════════════════════════════════╝");
Console.WriteLine();
Console.WriteLine($"  Torque:       {torqueUrl}");
Console.WriteLine($"  Typesense:    {(skipTypesense ? "(skipped)" : typesenseUrl)}");
Console.WriteLine($"  Docs:         {totalDocs:N0}");
Console.WriteLine($"  Iterations:   {iterations}");
Console.WriteLine($"  Batch:        {batchSize:N0}");
Console.WriteLine($"  TCP port:     {tcpPort}");
Console.WriteLine();

// ─── Health checks ───────────────────────────────────────────────
Console.Write("Health check Torque... ");
using var torqueClient = new TorqueHttpClient(new TorqueHttpClientOptions
{
    BaseUrl = torqueUrl,
    ApiKey = torqueKey,
    MaxRetries = 0,
});

try
{
    var h = await torqueClient.HealthAsync();
    if (!h.Ok) { Console.WriteLine("NOT OK — aborting"); return; }
    Console.WriteLine("OK");
}
catch (Exception ex)
{
    Console.WriteLine($"FAILED ({ex.Message}) — aborting");
    return;
}

bool typesenseAvailable = !skipTypesense;
TorqueHttpClient? typesenseClient = null;

if (!skipTypesense)
{
    Console.Write("Health check Typesense... ");
    typesenseClient = new TorqueHttpClient(new TorqueHttpClientOptions
    {
        BaseUrl = typesenseUrl,
        ApiKey = typesenseKey,
        MaxRetries = 0,
    });

    try
    {
        var h = await typesenseClient.HealthAsync();
        if (!h.Ok)
        {
            Console.WriteLine("NOT OK — will run Torque-only");
            typesenseAvailable = false;
        }
        else
        {
            Console.WriteLine("OK");
        }
    }
    catch (Exception ex)
    {
        Console.WriteLine($"DOWN ({ex.Message}) — will run Torque-only");
        typesenseAvailable = false;
    }
}
Console.WriteLine();

// ─── Schema ──────────────────────────────────────────────────────
const string collName = "bench_products";

var schema = BenchSchema(collName);

// ─── Create collections ─────────────────────────────────────────
Console.Write("Creating collection on Torque... ");
try { await torqueClient.Collections.DeleteAsync(collName); } catch { }
await torqueClient.Collections.CreateAsync(schema);
Console.WriteLine("OK");

bool torqueIngestOk = true;
bool typesenseIngestOk = typesenseAvailable;

if (typesenseAvailable)
{
    Console.Write("Creating collection on Typesense... ");
    try { await typesenseClient!.Collections.DeleteAsync(collName); } catch { }
    try
    {
        await typesenseClient!.Collections.CreateAsync(BenchSchema(collName));
        Console.WriteLine("OK");
    }
    catch (Exception ex)
    {
        Console.WriteLine($"FAILED ({ex.Message})");
        typesenseIngestOk = false;
    }
}
Console.WriteLine();

// ─── Shared data arrays ─────────────────────────────────────────
var categories = new[] { "Electronics", "Books", "Clothing", "Home", "Sports", "Toys", "Food", "Beauty", "Garden", "Automotive" };
var cities = new[] { "Stockholm", "Gothenburg", "Malmö", "Uppsala", "London", "Berlin", "Paris", "Oslo", "Copenhagen", "Helsinki" };
var lats = new[] { 59.33, 57.71, 55.60, 59.86, 51.51, 52.52, 48.86, 59.91, 55.68, 60.17 };
var lngs = new[] { 18.07, 11.97, 13.00, 17.64, -0.13, 13.41, 2.35, 10.75, 12.57, 24.94 };
var brands = new[] { "TechCorp", "NovaGear", "AlphaWare", "ZenithPro", "CrystalEdge",
                     "PulseTech", "ArcLight", "VeloStar", "TitanForge", "NimbusAI" };
var materials = new[] { "Aluminum", "Carbon Fiber", "Steel", "Plastic", "Titanium",
                        "Glass", "Ceramic", "Wood", "Bamboo", "Silicone" };
var variantColors = new[] { "Black", "Silver", "White", "Blue", "Red",
                            "Gold", "Green", "Gray", "Rose", "Navy" };
var variantSizes = new[] { "XS", "S", "M", "L", "XL" };

// ─── Ingest ──────────────────────────────────────────────────────
Console.WriteLine("═══ INGEST ═══");
Console.WriteLine();

double torqueIngestSec = 0;
double typesenseIngestSec = 0;

// Torque: TCP binary ingest (fastest path)
Console.Write($"  Torque (TCP binary): ingesting {totalDocs:N0} docs... ");
try
{
    var tcpRng = new Random(42);
    await using var ingest = new TorqueIngestClient(new TorqueIngestOptions
    {
        Port = tcpPort,
        ReadTimeoutMs = 600_000,
    });
    await ingest.ConnectAsync();
    await ingest.StartIngestAsync(collName);

    var sw = Stopwatch.StartNew();
    for (int offset = 0; offset < totalDocs; offset += batchSize)
    {
        int count = Math.Min(batchSize, totalDocs - offset);
        var docs = new TorqueDocument[count];
        for (int i = 0; i < count; i++)
        {
            int id = offset + i;
            int cityIdx = id % cities.Length;
            var (vColors, vSizes, vPriceMods) = GenerateVariantArrays(tcpRng, variantColors, variantSizes);
            docs[i] = new TorqueDocumentBuilder()
                .SetId($"p-{id}")
                .SetLanguage(id % 5 == 0 ? Language.Swedish : Language.English)
                .Set("title", GenerateTitle(tcpRng, id))
                .Set("description", GenerateDescription(tcpRng, id))
                .Set("price", 10 + tcpRng.Next(990))
                .Set("rating", Math.Round(1.0 + tcpRng.NextDouble() * 4.0, 1))
                .Set("in_stock", tcpRng.Next(100) < 80)
                .Set("category", categories[id % categories.Length])
                .Set("tags", GenerateTags(tcpRng))
                .SetGeoPoint("location", lats[cityIdx] + tcpRng.NextDouble() * 0.1, lngs[cityIdx] + tcpRng.NextDouble() * 0.1)
                .Set("popularity", tcpRng.Next(10000))
                .Set("city", cities[cityIdx])
                .Set("sku", 1_000_000_000L + id)
                .Set("review_scores", GenerateIntArray(tcpRng, 1 + tcpRng.Next(5)))
                .Set("variant_ids", GenerateLongArray(tcpRng, 1 + tcpRng.Next(3)))
                .Set("metadata.brand", brands[id % brands.Length])
                .Set("metadata.material", materials[tcpRng.Next(materials.Length)])
                .Set("metadata.weight_grams", 100 + tcpRng.Next(4900))
                .Set("variants.color", vColors)
                .Set("variants.size", vSizes)
                .Set("variants.price_modifier", vPriceMods)
                .Build(schema);
        }
        await ingest.SendBatchAsync(docs, schema);
    }
    await ingest.CommitAsync();
    // The commit is acknowledged before the server has built the documents: timed until a search finds them all.
    var all = new SearchParameters { Q = "*", QueryBy = "title", PerPage = 1 };
    while ((await torqueClient.SearchAsync(collName, all)).Found < totalDocs)
    {
        if (sw.Elapsed > TimeSpan.FromMinutes(10)) throw new TimeoutException("not searchable 10 minutes after the ingest started");
        await Task.Delay(20);
    }
    sw.Stop();
    torqueIngestSec = sw.Elapsed.TotalSeconds;
    Console.WriteLine($"{torqueIngestSec:F1}s");
}
catch (Exception ex)
{
    Console.WriteLine($"FAILED ({ex.Message})");
    torqueIngestOk = false;
}

// Typesense: HTTP JSONL batch import (its native bulk path)
if (typesenseIngestOk)
{
    Console.Write($"  Typesense (HTTP JSONL): importing {totalDocs:N0} docs in batches of {batchSize:N0}... ");
    try
    {
        var jsonRng = new Random(42);
        var sw = Stopwatch.StartNew();
        for (int offset = 0; offset < totalDocs; offset += batchSize)
        {
            int count = Math.Min(batchSize, totalDocs - offset);
            var batch = new List<Dictionary<string, object?>>(count);
            for (int i = 0; i < count; i++)
            {
                int id = offset + i;
                int cityIdx = id % cities.Length;
                var (vColors, vSizes, vPriceMods) = GenerateVariantArrays(jsonRng, variantColors, variantSizes);
                batch.Add(new Dictionary<string, object?>
                {
                    ["id"] = $"p-{id}",
                    ["title"] = GenerateTitle(jsonRng, id),
                    ["description"] = GenerateDescription(jsonRng, id),
                    ["price"] = 10 + jsonRng.Next(990),
                    ["rating"] = Math.Round(1.0 + jsonRng.NextDouble() * 4.0, 1),
                    ["in_stock"] = jsonRng.Next(100) < 80,
                    ["category"] = categories[id % categories.Length],
                    ["tags"] = GenerateTags(jsonRng),
                    ["location"] = new[] { lats[cityIdx] + jsonRng.NextDouble() * 0.1, lngs[cityIdx] + jsonRng.NextDouble() * 0.1 },
                    ["popularity"] = jsonRng.Next(10000),
                    ["city"] = cities[cityIdx],
                    ["sku"] = (long)1_000_000_000L + id,
                    ["review_scores"] = GenerateIntArray(jsonRng, 1 + jsonRng.Next(5)),
                    ["variant_ids"] = GenerateLongArray(jsonRng, 1 + jsonRng.Next(3)),
                    ["metadata"] = new Dictionary<string, object?>
                    {
                        ["brand"] = brands[id % brands.Length],
                        ["material"] = materials[jsonRng.Next(materials.Length)],
                        ["weight_grams"] = 100 + jsonRng.Next(4900),
                    },
                    ["variants"] = GenerateVariantObjects(vColors, vSizes, vPriceMods),
                });
            }
            await typesenseClient!.Documents.ImportAsync(collName, batch);
        }
        sw.Stop();
        typesenseIngestSec = sw.Elapsed.TotalSeconds;
        Console.WriteLine($"{typesenseIngestSec:F1}s");
    }
    catch (Exception ex)
    {
        Console.WriteLine($"FAILED ({ex.Message})");
        typesenseIngestOk = false;
    }
}

// ─── Memory stats ────────────────────────────────────────────────
if (torqueIngestOk)
{
    Console.WriteLine("═══ MEMORY (after ingest) ═══");
    Console.WriteLine();
    try
    {
        var stats = await torqueClient.StatsAsync();

        // Process-level jemalloc stats
        var mem = stats.System.Memory;
        Console.WriteLine($"  Process (jemalloc):  allocated={FormatBytes(mem.AllocatedBytes)}  resident={FormatBytes(mem.ResidentBytes)}");

        // Per-collection index breakdown
        foreach (var coll in stats.Collections)
        {
            if (coll.Memory is not { } idx) continue;

            Console.WriteLine($"  Index breakdown ({coll.Name}, {coll.NumDocuments:N0} docs):");
            Console.WriteLine($"    {"total",-20}{FormatBytes(idx.TotalBytes),10}");

            var components = new (string Name, long Bytes)[]
            {
                ("docs", idx.DocsBytes),
                ("inverted", idx.InvertedBytes),
                ("sort", idx.SortBytes),
                ("geo", idx.GeoBytes),
                ("numeric_filter", idx.NumericFilterBytes),
                ("string_filter", idx.StringFilterBytes),
                ("bool_filter", idx.BoolFilterBytes),
                ("vector", idx.VectorBytes),
                ("all_docs_bitmap", idx.AllDocsBitmapBytes),
                ("symspell", idx.SymspellBytes),
                ("prefix", idx.PrefixBytes),
                ("id_map", idx.IdMapBytes),
            };

            foreach (var (name, bytes) in components.OrderByDescending(c => c.Bytes))
            {
                double pct = idx.TotalBytes > 0 ? bytes * 100.0 / idx.TotalBytes : 0;
                Console.WriteLine($"    {name,-20}{FormatBytes(bytes),10}  ({pct:F1}%)");
            }
        }
    }
    catch (Exception ex)
    {
        Console.WriteLine($"  Failed to fetch stats: {ex.Message}");
    }
    Console.WriteLine();
}

// ─── Define query patterns ───────────────────────────────────────
var queryPatterns = new (string Name, SearchParameters Params, bool Grouped, bool TorqueOnly)[]
{
    ("text_search", new() { Q = "laptop", QueryBy = "title,description", PerPage = 10 }, false, false),
    ("multi_term", new() { Q = "premium wireless", QueryBy = "title,description", PerPage = 10 }, false, false),
    ("filter_sort", new() { Q = "*", QueryBy = "title", FilterBy = "price:>=500 && category:=Electronics", SortBy = "price:asc", PerPage = 10 }, false, false),
    ("faceted", new() { Q = "*", QueryBy = "title", FacetBy = "category,city", PerPage = 10 }, false, false),
    ("geo_sort", new() { Q = "*", QueryBy = "title", FilterBy = "location:(59.33, 18.07, 50 km)", SortBy = "location(59.33, 18.07):asc", PerPage = 10 }, false, false),
    ("field_sort", new() { Q = "*", QueryBy = "title", SortBy = "rating:desc", PerPage = 10 }, false, false),
    ("filter_facet_text", new() { Q = "phone", QueryBy = "title,description", FilterBy = "in_stock:=true", FacetBy = "category", SortBy = "popularity:desc", PerPage = 10 }, false, false),
    ("typo", new() { Q = "laptp", QueryBy = "title,description", NumTypos = 2, PerPage = 10 }, false, false),
    ("group_by", new() { Q = "*", QueryBy = "title", GroupBy = "category", GroupLimit = 3, PerPage = 10 }, true, false),
    ("decay_sort", new() { Q = "*", QueryBy = "title", FilterBy = "price:[100..300] && category:=[Books,Toys,Food]", SortBy = "popularity(origin:5000,func:gauss,scale:2000):desc", PerPage = 10 }, false, false),
    ("prefix", new() { Q = "pre", QueryBy = "title,description", Prefix = true, PerPage = 10 }, false, false),
    ("nested_filter", new() { Q = "*", QueryBy = "title", FilterBy = "metadata.brand:=TechCorp && metadata.material:=Aluminum", PerPage = 10 }, false, false),
    ("nested_array", new() { Q = "*", QueryBy = "title", FilterBy = "variants.color:=Black", FacetBy = "variants.color,variants.size", PerPage = 10 }, false, false),
    ("nested_and", new() { Q = "*", QueryBy = "title", FilterBy = "variants:(color:=Black && size:=M)", PerPage = 10 }, false, true),
};

// ─── Warmup + Validation ─────────────────────────────────────────
Console.WriteLine("═══ WARMUP + VALIDATION ═══");
Console.WriteLine();

// Warmup Torque
if (torqueIngestOk)
{
    Console.Write("  Torque warmup... ");
    int warmupOk = 0;
    foreach (var (_, p, grouped, _) in queryPatterns)
    {
        try
        {
            if (grouped) await torqueClient.SearchGroupedAsync(collName, p);
            else await torqueClient.SearchAsync(collName, p);
            warmupOk++;
        }
        catch { }
    }
    Console.WriteLine($"{warmupOk}/{queryPatterns.Length} OK");
}

// Warmup Typesense
if (typesenseIngestOk)
{
    Console.Write("  Typesense warmup... ");
    int warmupOk = 0;
    int tsPatterns = 0;
    foreach (var (_, p, grouped, torqueOnly) in queryPatterns)
    {
        if (torqueOnly) continue;
        tsPatterns++;
        try
        {
            if (grouped) await typesenseClient!.SearchGroupedAsync(collName, p);
            else await typesenseClient!.SearchAsync(collName, p);
            warmupOk++;
        }
        catch { }
    }
    Console.WriteLine($"{warmupOk}/{tsPatterns} OK");
}
Console.WriteLine();

// Validate responses match between engines
if (torqueIngestOk && typesenseIngestOk)
{
    Console.WriteLine("  Validating responses:");
    int passed = 0;
    int warned = 0;
    int failed = 0;

    foreach (var (name, p, grouped, torqueOnly) in queryPatterns)
    {
        if (torqueOnly)
        {
            Console.WriteLine($"    {name,-22} SKIP (Torque-only)");
            passed++;
            continue;
        }
        var issues = new List<string>();
        bool isWildcard = p.Q == "*";
        bool hasFacets = p.FacetBy != null;

        try
        {
            if (grouped)
            {
                var tRes = await torqueClient.SearchGroupedAsync(collName, p);
                var tsRes = await typesenseClient!.SearchGroupedAsync(collName, p);

                // found count
                if (tRes.Found != tsRes.Found)
                    issues.Add($"found: T={tRes.Found} TS={tsRes.Found}");

                // group keys
                var tKeys = tRes.GroupedHits.Select(g => string.Join(",", g.GroupKey)).ToList();
                var tsKeys = tsRes.GroupedHits.Select(g => string.Join(",", g.GroupKey)).ToList();
                if (!tKeys.SequenceEqual(tsKeys))
                    issues.Add($"group keys differ: T=[{string.Join("; ", tKeys.Take(5))}] TS=[{string.Join("; ", tsKeys.Take(5))}]");

                // facets
                if (hasFacets)
                    ValidateFacets(tRes.FacetCounts, tsRes.FacetCounts, issues);
            }
            else
            {
                var tRes = await torqueClient.SearchAsync(collName, p);
                var tsRes = await typesenseClient!.SearchAsync(collName, p);

                // found count
                if (tRes.Found != tsRes.Found)
                {
                    if (isWildcard)
                        issues.Add($"found MISMATCH: T={tRes.Found} TS={tsRes.Found}");
                    else
                        issues.Add($"found differs (text query): T={tRes.Found} TS={tsRes.Found}");
                }

                // document IDs in order
                var tIds = tRes.Hits.Select(h => h.Document["id"]?.ToString()).ToList();
                var tsIds = tsRes.Hits.Select(h => h.Document["id"]?.ToString()).ToList();

                if (isWildcard && !tIds.SequenceEqual(tsIds))
                    issues.Add($"hit IDs differ: T=[{string.Join(",", tIds.Take(5))}] TS=[{string.Join(",", tsIds.Take(5))}]");
                else if (!isWildcard && !tIds.SequenceEqual(tsIds))
                    issues.Add($"hit IDs differ (text query, may be OK): T=[{string.Join(",", tIds.Take(3))}...] TS=[{string.Join(",", tsIds.Take(3))}...]");

                // facets
                if (hasFacets)
                    ValidateFacets(tRes.FacetCounts, tsRes.FacetCounts, issues);

                // validate hits returned > 0
                if (tRes.Hits.Count == 0)
                    issues.Add("Torque returned 0 hits");
                if (tsRes.Hits.Count == 0)
                    issues.Add("Typesense returned 0 hits");
            }
        }
        catch (Exception ex)
        {
            issues.Add($"exception: {ex.Message}");
        }

        if (issues.Count == 0)
        {
            Console.WriteLine($"    {name,-22} PASS");
            passed++;
        }
        else
        {
            // Text query differences are warnings, wildcard mismatches are failures
            bool anyHard = issues.Any(i => i.Contains("MISMATCH") || i.Contains("exception") || i.Contains("returned 0"));
            if (anyHard)
            {
                Console.WriteLine($"    {name,-22} FAIL");
                failed++;
            }
            else
            {
                Console.WriteLine($"    {name,-22} WARN");
                warned++;
            }
            foreach (var issue in issues)
                Console.WriteLine($"      {issue}");
        }
    }

    Console.WriteLine();
    Console.Write($"  Validation: {passed} passed");
    if (warned > 0) Console.Write($", {warned} warnings");
    if (failed > 0) Console.Write($", {failed} FAILED");
    Console.WriteLine();
}
Console.WriteLine();

// ─── Measure ─────────────────────────────────────────────────────
Console.WriteLine($"═══ MEASURING ({iterations} iterations per pattern) ═══");
Console.WriteLine();

var torqueResults = new Dictionary<string, List<double>>();
var typesenseResults = new Dictionary<string, List<double>>();
var torqueErrors = new Dictionary<string, int>();
var typesenseErrors = new Dictionary<string, int>();

foreach (var (name, p, grouped, torqueOnly) in queryPatterns)
{
    torqueResults[name] = [];
    typesenseResults[name] = [];
    torqueErrors[name] = 0;
    typesenseErrors[name] = 0;

    Console.Write($"  {name,-22}");

    // Measure Torque
    if (torqueIngestOk)
    {
        for (int i = 0; i < iterations; i++)
        {
            long start = Stopwatch.GetTimestamp();
            try
            {
                if (grouped) await torqueClient.SearchGroupedAsync(collName, p);
                else await torqueClient.SearchAsync(collName, p);

                long elapsed = Stopwatch.GetTimestamp() - start;
                double ms = elapsed * 1000.0 / Stopwatch.Frequency;
                torqueResults[name].Add(ms);
            }
            catch
            {
                torqueErrors[name]++;
            }
        }
    }

    // Measure Typesense (skip for TorqueOnly patterns)
    if (typesenseIngestOk && !torqueOnly)
    {
        for (int i = 0; i < iterations; i++)
        {
            long start = Stopwatch.GetTimestamp();
            try
            {
                if (grouped) await typesenseClient!.SearchGroupedAsync(collName, p);
                else await typesenseClient!.SearchAsync(collName, p);

                long elapsed = Stopwatch.GetTimestamp() - start;
                double ms = elapsed * 1000.0 / Stopwatch.Frequency;
                typesenseResults[name].Add(ms);
            }
            catch
            {
                typesenseErrors[name]++;
            }
        }
    }

    // Print inline progress
    int tOk = torqueResults[name].Count;
    int tsOk = typesenseResults[name].Count;
    int tErr = torqueErrors[name];
    int tsErr = typesenseErrors[name];
    string torqueP50 = tOk > 0 ? $"{Percentile(torqueResults[name], 50):F1}ms" : "N/A";
    string typesenseP50 = tsOk > 0 ? $"{Percentile(typesenseResults[name], 50):F1}ms" : "--";
    string errInfo = (tErr > 0 || tsErr > 0) ? $"  (errors: T={tErr} TS={tsErr})" : "";
    Console.WriteLine($"  T={torqueP50,-10} TS={typesenseP50,-10}{errInfo}");
}
Console.WriteLine();

// ─── Cleanup ─────────────────────────────────────────────────────
if (!keep)
{
    Console.Write("Cleanup... ");
    try { await torqueClient.Collections.DeleteAsync(collName); } catch { }
    if (typesenseIngestOk) try { await typesenseClient!.Collections.DeleteAsync(collName); } catch { }
    Console.WriteLine("OK");
}
Console.WriteLine();

// ─── Report ──────────────────────────────────────────────────────
Console.WriteLine("╔═══════════════════════════════════════════════════════════════════════════════════════════════════════╗");
Console.WriteLine("║                                          RESULTS                                                     ║");
Console.WriteLine("╚═══════════════════════════════════════════════════════════════════════════════════════════════════════╝");
Console.WriteLine();

// Ingest table
Console.WriteLine("INGEST:");
string torqueStr = torqueIngestOk ? $"{torqueIngestSec:F1}s" : "N/A";
string tsStr = typesenseIngestOk ? $"{typesenseIngestSec:F1}s" : "N/A";
Console.WriteLine($"  Torque (TCP binary):     {torqueStr}");
Console.WriteLine($"  Typesense (HTTP JSONL):  {tsStr}");
Console.WriteLine();

// Query latency table
Console.WriteLine($"QUERY LATENCY ({iterations} iterations, {totalDocs:N0} documents):");
if (typesenseIngestOk)
{
    Console.WriteLine($"  {"Pattern",-22}│{"Torque",29}│{"Typesense",29}│{"Ratio",7}");
    Console.WriteLine($"  {"",22}│{"avg",8}{"p50",8}{"p95",8}{"p99",7}│{"avg",8}{"p50",8}{"p95",8}{"p99",7}│{"(p50)",7}");
    Console.WriteLine($"  ──────────────────────┼─────────────────────────────┼─────────────────────────────┼───────");
}
else
{
    Console.WriteLine($"  {"Pattern",-22}│{"Torque",29}│{"Errors",7}");
    Console.WriteLine($"  {"",22}│{"avg",8}{"p50",8}{"p95",8}{"p99",7}│{"",7}");
    Console.WriteLine($"  ──────────────────────┼─────────────────────────────┼───────");
}

int torqueFaster = 0;
int typesenseFaster = 0;
double logRatioSum = 0;
int ratioCount = 0;

foreach (var (name, _, _, torqueOnly) in queryPatterns)
{
    var tSamples = torqueResults[name];
    var tsSamples = typesenseResults[name];

    string tCol = FormatLatencyBlock(tSamples);

    if (typesenseIngestOk)
    {
        if (torqueOnly)
        {
            // TorqueOnly: show -- for Typesense column, no ratio
            string tsCol = $"{"--",8}{"",8}{"",8}{"",5}";
            Console.WriteLine($"  {name,-22}│{tCol}│{tsCol}│{"   --"}");
        }
        else
        {
            string tsCol = FormatLatencyBlock(tsSamples);
            string ratio = "   --";
            if (tSamples.Count > 0 && tsSamples.Count > 0)
            {
                double tp50 = Percentile(tSamples, 50);
                double tsp50 = Percentile(tsSamples, 50);
                if (tp50 > 0)
                {
                    double r = tsp50 / tp50;
                    ratio = $"{r,5:F1}x";
                    logRatioSum += Math.Log(r);
                    ratioCount++;
                    if (r > 1.0) torqueFaster++;
                    else if (r < 1.0) typesenseFaster++;
                }
            }

            Console.WriteLine($"  {name,-22}│{tCol}│{tsCol}│{ratio}");
        }
    }
    else
    {
        int err = torqueErrors[name];
        Console.WriteLine($"  {name,-22}│{tCol}│{(err > 0 ? $"{err,5}" : ""),7}");
    }
}

Console.WriteLine();

// Summary
Console.WriteLine("SUMMARY:");
if (typesenseIngestOk && ratioCount > 0)
{
    double geoMean = Math.Exp(logRatioSum / ratioCount);
    Console.WriteLine($"  Torque faster:          {torqueFaster} / {queryPatterns.Length} patterns");
    Console.WriteLine($"  Typesense faster:       {typesenseFaster} / {queryPatterns.Length} patterns");
    Console.WriteLine($"  Geometric mean ratio:   {geoMean:F1}x");
}
else
{
    Console.WriteLine("  (Typesense not available — comparison ratios omitted)");
    Console.WriteLine();
    Console.WriteLine("  Torque latency summary:");
    var allTorque = queryPatterns
        .SelectMany(qp => torqueResults[qp.Name])
        .ToList();
    if (allTorque.Count > 0)
    {
        allTorque.Sort();
        Console.WriteLine($"    Overall p50:  {Percentile(allTorque, 50):F1}ms");
        Console.WriteLine($"    Overall p95:  {Percentile(allTorque, 95):F1}ms");
        Console.WriteLine($"    Overall p99:  {Percentile(allTorque, 99):F1}ms");
    }
}

// Error summary
int totalErrors = torqueErrors.Values.Sum() + typesenseErrors.Values.Sum();
if (totalErrors > 0)
{
    Console.WriteLine();
    Console.WriteLine($"  Errors:                 {totalErrors} total");
    foreach (var (name, _, _, _) in queryPatterns)
    {
        int te = torqueErrors[name];
        int tse = typesenseErrors[name];
        if (te > 0 || tse > 0)
            Console.WriteLine($"    {name}: Torque={te}, Typesense={tse}");
    }
}

Console.WriteLine();

// Dispose
typesenseClient?.Dispose();

// ═══════════════════════════════════════════════════════════════════
// Helper functions
// ═══════════════════════════════════════════════════════════════════

string FormatLatencyBlock(List<double> samples)
{
    if (samples.Count == 0)
        return $"{"N/A",8}{"",8}{"",8}{"",5}";

    double avg = samples.Average();
    double p50 = Percentile(samples, 50);
    double p95 = Percentile(samples, 95);
    double p99 = Percentile(samples, 99);
    return $"{FormatMs(avg),8}{FormatMs(p50),8}{FormatMs(p95),8}{FormatMs(p99),5}";
}

static string FormatBytes(long bytes)
{
    const double GB = 1024 * 1024 * 1024;
    const double MB = 1024 * 1024;
    return bytes >= GB ? $"{bytes / GB:F1} GB" : $"{bytes / MB:F1} MB";
}

static string FormatMs(double ms)
{
    return ms >= 1000 ? $"{ms / 1000:F2}s" : $"{ms:F1}ms";
}

static double Percentile(List<double> samples, int p)
{
    var sorted = samples.ToList();
    sorted.Sort();
    int idx = (int)Math.Ceiling(p / 100.0 * sorted.Count) - 1;
    return sorted[Math.Clamp(idx, 0, sorted.Count - 1)];
}

static CollectionSchema BenchSchema(string name) => new()
{
    Name = name,
    EnableNestedFields = true,
    Fields =
    [
        new() { Name = "title", Type = TorqueFieldType.String },
        new() { Name = "description", Type = TorqueFieldType.String },
        new() { Name = "price", Type = TorqueFieldType.Int32, Facet = true, Sort = true, Stem = false },
        new() { Name = "rating", Type = TorqueFieldType.Float, Sort = true, Stem = false },
        new() { Name = "in_stock", Type = TorqueFieldType.Bool, Facet = true, Stem = false },
        new() { Name = "category", Type = TorqueFieldType.String, Facet = true },
        new() { Name = "tags", Type = TorqueFieldType.StringArray, Facet = true },
        new() { Name = "location", Type = TorqueFieldType.GeoPoint, Stem = false },
        new() { Name = "popularity", Type = TorqueFieldType.Int32, Sort = true, Stem = false },
        new() { Name = "city", Type = TorqueFieldType.String, Facet = true },
        new() { Name = "sku", Type = TorqueFieldType.Int64, Sort = true, Stem = false },
        new() { Name = "review_scores", Type = TorqueFieldType.Int32Array, Stem = false },
        new() { Name = "variant_ids", Type = TorqueFieldType.Int64Array, Stem = false },
        // Nested object fields (metadata)
        new() { Name = "metadata.brand", Type = TorqueFieldType.String, Facet = true },
        new() { Name = "metadata.material", Type = TorqueFieldType.String, Facet = true },
        new() { Name = "metadata.weight_grams", Type = TorqueFieldType.Int32, Sort = true, Stem = false },
        // Nested array of objects fields (variants)
        new() { Name = "variants.color", Type = TorqueFieldType.StringArray, Facet = true },
        new() { Name = "variants.size", Type = TorqueFieldType.StringArray, Facet = true },
        new() { Name = "variants.price_modifier", Type = TorqueFieldType.Int32Array, Stem = false },
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

static int[] GenerateIntArray(Random rng, int count)
{
    var arr = new int[count];
    for (int i = 0; i < count; i++)
        arr[i] = 1 + rng.Next(10);
    return arr;
}

static long[] GenerateLongArray(Random rng, int count)
{
    var arr = new long[count];
    for (int i = 0; i < count; i++)
        arr[i] = 100_000_000L + rng.Next(900_000_000);
    return arr;
}

static (string[] colors, string[] sizes, int[] priceMods) GenerateVariantArrays(
    Random rng, string[] variantColors, string[] variantSizes)
{
    int count = 1 + rng.Next(3); // 1–3 variants
    var colors = new string[count];
    var sizes = new string[count];
    var priceMods = new int[count];
    for (int v = 0; v < count; v++)
    {
        colors[v] = variantColors[rng.Next(variantColors.Length)];
        sizes[v] = variantSizes[rng.Next(variantSizes.Length)];
        priceMods[v] = -50 + rng.Next(200);
    }
    return (colors, sizes, priceMods);
}

static List<Dictionary<string, object?>> GenerateVariantObjects(
    string[] colors, string[] sizes, int[] priceMods)
{
    var variants = new List<Dictionary<string, object?>>(colors.Length);
    for (int v = 0; v < colors.Length; v++)
    {
        variants.Add(new Dictionary<string, object?>
        {
            ["color"] = colors[v],
            ["size"] = sizes[v],
            ["price_modifier"] = priceMods[v],
        });
    }
    return variants;
}

static void ValidateFacets(List<FacetResult>? torque, List<FacetResult>? typesense, List<string> issues)
{
    if (torque == null && typesense == null) return;
    if (torque == null || typesense == null)
    {
        issues.Add($"facets: one engine returned null (T={torque != null} TS={typesense != null})");
        return;
    }

    var tFields = torque.Select(f => f.FieldName).OrderBy(f => f).ToList();
    var tsFields = typesense.Select(f => f.FieldName).OrderBy(f => f).ToList();
    if (!tFields.SequenceEqual(tsFields))
    {
        issues.Add($"facet fields differ: T=[{string.Join(",", tFields)}] TS=[{string.Join(",", tsFields)}]");
        return;
    }

    foreach (var field in tFields)
    {
        var tCounts = torque.First(f => f.FieldName == field).Counts
            .OrderBy(c => c.Value).ToDictionary(c => c.Value, c => c.Count);
        var tsCounts = typesense.First(f => f.FieldName == field).Counts
            .OrderBy(c => c.Value).ToDictionary(c => c.Value, c => c.Count);

        foreach (var (val, count) in tCounts)
        {
            if (!tsCounts.TryGetValue(val, out int tsCount))
                issues.Add($"facet {field}: T has '{val}' ({count}) missing from TS");
            else if (count != tsCount)
                issues.Add($"facet {field} '{val}': T={count} TS={tsCount}");
        }
        foreach (var val in tsCounts.Keys.Except(tCounts.Keys))
            issues.Add($"facet {field}: TS has '{val}' ({tsCounts[val]}) missing from T");
    }
}

static string? GetArg(string[] args, string name)
{
    for (int i = 0; i < args.Length - 1; i++)
        if (args[i] == name) return args[i + 1];
    return null;
}
