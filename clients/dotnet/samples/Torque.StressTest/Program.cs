using System.Diagnostics;
using System.Text.Json;
using Torque.Http;
using Torque.Http.Models;
using Torque.Http.Search;
using Torque.Http.Tcp;

// ───────────────────────────────────────────────────────────────────
// Torque Stress Test — ingest + concurrent search with monitoring
// Usage: dotnet run [--url http://localhost:8108] [--key test-key]
//          [--tcp-port 8109] [--docs 5000000] [--batch 5000]
//          [--search-threads 8] [--search-duration 30]
//          [--monitor-interval 2]
// ───────────────────────────────────────────────────────────────────

var url = GetArg(args, "--url") ?? "http://localhost:8108";
var apiKey = GetArg(args, "--key") ?? "test-bootstrap-key";
var tcpPort = int.Parse(GetArg(args, "--tcp-port") ?? "8109");
var totalDocs = int.Parse(GetArg(args, "--docs") ?? "5000000");
var batchSize = int.Parse(GetArg(args, "--batch") ?? "5000");
var numSearchThreads = int.Parse(GetArg(args, "--search-threads") ?? "8");
var searchDurationSec = int.Parse(GetArg(args, "--search-duration") ?? "30");
var monitorIntervalSec = int.Parse(GetArg(args, "--monitor-interval") ?? "2");

Console.WriteLine("╔═══════════════════════════════════════════════════════════╗");
Console.WriteLine("║     Torque Stress Test — Ingest + Concurrent Search       ║");
Console.WriteLine("╚═══════════════════════════════════════════════════════════╝");
Console.WriteLine();
Console.WriteLine($"  HTTP:         {url}");
Console.WriteLine($"  TCP:          localhost:{tcpPort}");
Console.WriteLine($"  Docs:         {totalDocs:N0}");
Console.WriteLine($"  Batch:        {batchSize:N0}");
Console.WriteLine($"  Search:       {numSearchThreads} threads x {searchDurationSec}s after commit");
Console.WriteLine($"  Monitor:      every {monitorIntervalSec}s");
Console.WriteLine();

using var httpClient = new HttpClient { BaseAddress = new Uri(url) };
httpClient.DefaultRequestHeaders.Add("X-TYPESENSE-API-KEY", apiKey);
using var client = new TorqueHttpClient(new TorqueHttpClientOptions { BaseUrl = url, ApiKey = apiKey });

// ─── Tracking state ─────────────────────────────────────────────
long docsIngested = 0;
long batchesSent = 0;
long totalBatchMs = 0;
long peakDotnetWs = 0;
long peakServerWs = 0;
var batchLatencies = new List<long>();

// Search tracking
long searchesCompleted = 0;
long searchErrors = 0;
long totalSearchMs = 0;
var searchLatencies = new List<long>();

var globalSw = Stopwatch.StartNew();

// ─── Create collection ──────────────────────────────────────────
const string collName = "stress_products";
Console.Write("Creating collection... ");
try { await client.Collections.DeleteAsync(collName); } catch { }
var schema = StressSchema();
await client.Collections.CreateAsync(schema);
Console.WriteLine($"OK ({schema.Fields!.Count} fields)");
Console.WriteLine();

// ─── Start background monitor ───────────────────────────────────
using var monitorCts = new CancellationTokenSource();
var monitorTask = Task.Run(() => MonitorLoop(monitorCts.Token));

PrintIngestHeader();

// ─── Phase 1: Ingest (single session, max throughput) ───────────
await using var ingest = new TorqueIngestClient(new TorqueIngestOptions
{
    Port = tcpPort,
    ReadTimeoutMs = 1_800_000, // 30 min for large index builds
});
await ingest.ConnectAsync();
await ingest.StartIngestAsync(collName);

var rng = new Random(42);
var categories = new[] { "Electronics", "Books", "Clothing", "Home", "Sports", "Toys", "Food", "Beauty", "Garden", "Automotive" };
var cities = new[] { "Stockholm", "Gothenburg", "Malmö", "Uppsala", "London", "Berlin", "Paris", "Oslo", "Copenhagen", "Helsinki" };
var lats = new[] { 59.33, 57.71, 55.60, 59.86, 51.51, 52.52, 48.86, 59.91, 55.68, 60.17 };
var lngs = new[] { 18.07, 11.97, 13.00, 17.64, -0.13, 13.41, 2.35, 10.75, 12.57, 24.94 };

for (int batch = 0; batch < totalDocs; batch += batchSize)
{
    int count = Math.Min(batchSize, totalDocs - batch);
    var docs = new TorqueDocument[count];
    for (int i = 0; i < count; i++)
    {
        int id = batch + i;
        int cityIdx = id % cities.Length;
        docs[i] = new TorqueDocumentBuilder()
            .SetId($"p-{id}")
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
            .Set("city", cities[cityIdx])
            .Set("embedding", GenerateEmbedding(rng, 128))
            .Build(schema);
    }

    var batchSw = Stopwatch.StartNew();
    await ingest.SendBatchAsync(docs, schema);
    batchSw.Stop();

    Interlocked.Add(ref docsIngested, count);
    Interlocked.Increment(ref batchesSent);
    Interlocked.Add(ref totalBatchMs, batchSw.ElapsedMilliseconds);
    lock (batchLatencies) { batchLatencies.Add(batchSw.ElapsedMilliseconds); }
}

long streamingMs = globalSw.ElapsedMilliseconds;
Console.WriteLine();
Console.WriteLine($"  Streaming done: {totalDocs:N0} docs in {streamingMs / 1000.0:F1}s ({totalDocs * 1000.0 / streamingMs:N0} docs/sec)");
Console.Write("  Committing (index build)... ");

var commitSw = Stopwatch.StartNew();
var commit = await ingest.CommitAsync();
commitSw.Stop();
Console.WriteLine($"OK — {commit.DocsReceived:N0} docs accepted, roundtrip {commitSw.ElapsedMilliseconds}ms (rebuild runs async on server)");

long ingestTotalMs = globalSw.ElapsedMilliseconds;

// ─── Phase 2: Concurrent Search Load ────────────────────────────
Console.WriteLine();
Console.WriteLine("═══ SEARCH LOAD TEST ═══");
Console.WriteLine();

var searchQueries = new SearchParameters[]
{
    new() { Q = "laptop", QueryBy = "title,description", PerPage = 10 },
    new() { Q = "premium wireless", QueryBy = "title,description", PerPage = 10 },
    new() { Q = "*", QueryBy = "title", FilterBy = "price:>= 500 && category:= Electronics", SortBy = "price:asc", PerPage = 10 },
    new() { Q = "*", QueryBy = "title", FacetBy = "category,city", PerPage = 5 },
    new() { Q = "*", QueryBy = "title", FilterBy = "location:(59.33, 18.07, 50 km)", SortBy = "location(59.33, 18.07):asc", PerPage = 10 },
    new() { Q = "*", QueryBy = "title", SortBy = "rating:desc", PerPage = 10 },
    new() { Q = "phone", QueryBy = "title,description", FilterBy = "in_stock:= true", FacetBy = "category", SortBy = "popularity:desc", PerPage = 10 },
    new() { Q = "laptp", QueryBy = "title,description", NumTypos = 2, PerPage = 10 },
    new() { Q = "*", QueryBy = "title", GroupBy = "category", GroupLimit = 3, PerPage = 10 },
    new() { Q = "*", QueryBy = "title", FilterBy = "price:[100..300] && category:= [Books, Toys, Food]", SortBy = "popularity(origin:5000,func:gauss,scale:2000):desc", PerPage = 10 },
    new() { Q = "pre", QueryBy = "title,description", Prefix = true, PerPage = 10 },
    new() { Q = "camera speaker", QueryBy = "title", TextMatchType = "sum_score", PerPage = 10 },
    // Vector search queries
    new() { Q = "*", QueryBy = "title", VectorQuery = $"embedding:([{GenerateQueryVector(42, 128)}], k:10)", PerPage = 10 },
    new() { Q = "*", QueryBy = "title", VectorQuery = $"embedding:([{GenerateQueryVector(99, 128)}], k:50)", PerPage = 10 },
    // Hybrid: text + vector
    new() { Q = "laptop", QueryBy = "title,description", VectorQuery = $"embedding:([{GenerateQueryVector(7, 128)}], k:20)", PerPage = 10 },
    // Vector + filter
    new() { Q = "*", QueryBy = "title", VectorQuery = $"embedding:([{GenerateQueryVector(13, 128)}], k:10)", FilterBy = "category:= Electronics", PerPage = 10 },
};

Console.WriteLine($"  {numSearchThreads} threads x {searchDurationSec}s, {searchQueries.Length} query patterns");
Console.WriteLine();
PrintSearchHeader();

var searchPhaseSw = Stopwatch.StartNew();
using var searchCts = new CancellationTokenSource(TimeSpan.FromSeconds(searchDurationSec));
var searchTasks = new Task[numSearchThreads];

for (int t = 0; t < numSearchThreads; t++)
{
    int threadIdx = t;
    searchTasks[t] = Task.Run(async () =>
    {
        using var sc = new TorqueHttpClient(new TorqueHttpClientOptions { BaseUrl = url, ApiKey = apiKey });
        var localRng = new Random(threadIdx);
        while (!searchCts.IsCancellationRequested)
        {
            var query = searchQueries[localRng.Next(searchQueries.Length)];
            var sw = Stopwatch.StartNew();
            try
            {
                await sc.SearchAsync(collName, query, searchCts.Token);
                sw.Stop();
                Interlocked.Increment(ref searchesCompleted);
                Interlocked.Add(ref totalSearchMs, sw.ElapsedMilliseconds);
                lock (searchLatencies) { searchLatencies.Add(sw.ElapsedMilliseconds); }
            }
            catch (OperationCanceledException) { break; }
            catch { Interlocked.Increment(ref searchErrors); }
        }
    });
}

// Search monitor loop
while (!searchCts.IsCancellationRequested)
{
    try { await Task.Delay(monitorIntervalSec * 1000, searchCts.Token); } catch (OperationCanceledException) { break; }

    long completed = Interlocked.Read(ref searchesCompleted);
    long errors = Interlocked.Read(ref searchErrors);
    long elapsedMs = searchPhaseSw.ElapsedMilliseconds;
    long qps = elapsedMs > 0 ? completed * 1000 / elapsedMs : 0;

    long p50 = 0, p95 = 0, p99 = 0;
    lock (searchLatencies)
    {
        if (searchLatencies.Count > 0)
        {
            var sorted = searchLatencies.ToList();
            sorted.Sort();
            p50 = Percentile(sorted, 50);
            p95 = Percentile(sorted, 95);
            p99 = Percentile(sorted, 99);
        }
    }

    long serverWs = 0;
    var tp = FindTorqueProcess();
    if (tp != null) { try { tp.Refresh(); serverWs = tp.WorkingSet64; } catch { } }

    Console.WriteLine($"  {elapsedMs / 1000.0,5:F0}s │ {completed,10:N0} │ {errors,6:N0} │ {qps,8:N0} │ {p50,5}ms │ {p95,5}ms │ {p99,5}ms │ {serverWs / (1024.0 * 1024),7:F0} MB");
}

await Task.WhenAll(searchTasks);
searchPhaseSw.Stop();

globalSw.Stop();
monitorCts.Cancel();
try { await monitorTask; } catch (OperationCanceledException) { }

// ─── Final summary ──────────────────────────────────────────────
Console.WriteLine();
Console.WriteLine("╔═══════════════════════════════════════════════════════════╗");
Console.WriteLine("║                      FINAL SUMMARY                        ║");
Console.WriteLine("╚═══════════════════════════════════════════════════════════╝");
Console.WriteLine();
Console.WriteLine("  INGEST:");
Console.WriteLine($"    Documents:        {totalDocs:N0}");
Console.WriteLine($"    Batches:          {Interlocked.Read(ref batchesSent):N0} x {batchSize:N0}");
Console.WriteLine($"    Streaming:        {streamingMs / 1000.0:F1}s ({totalDocs * 1000.0 / streamingMs:N0} docs/sec)");
Console.WriteLine($"    Index build:      async (server-side)");
Console.WriteLine($"    Total ingest:     {ingestTotalMs / 1000.0:F1}s");
Console.WriteLine();

lock (batchLatencies)
{
    if (batchLatencies.Count > 0)
    {
        batchLatencies.Sort();
        Console.WriteLine("    Batch latency:");
        Console.WriteLine($"      min:  {batchLatencies[0]}ms");
        Console.WriteLine($"      p50:  {Percentile(batchLatencies, 50)}ms");
        Console.WriteLine($"      p95:  {Percentile(batchLatencies, 95)}ms");
        Console.WriteLine($"      p99:  {Percentile(batchLatencies, 99)}ms");
        Console.WriteLine($"      max:  {batchLatencies[^1]}ms");
        Console.WriteLine($"      avg:  {Interlocked.Read(ref totalBatchMs) / (double)batchLatencies.Count:F1}ms");
        Console.WriteLine();
    }
}

Console.WriteLine("  SEARCH:");
long finalSearches = Interlocked.Read(ref searchesCompleted);
long finalErrors = Interlocked.Read(ref searchErrors);
Console.WriteLine($"    Completed:        {finalSearches:N0}");
Console.WriteLine($"    Errors:           {finalErrors:N0}");
Console.WriteLine($"    Duration:         {searchPhaseSw.Elapsed.TotalSeconds:F1}s");
if (searchPhaseSw.Elapsed.TotalSeconds > 0)
    Console.WriteLine($"    QPS:              {finalSearches / searchPhaseSw.Elapsed.TotalSeconds:N0}");
Console.WriteLine();

lock (searchLatencies)
{
    if (searchLatencies.Count > 0)
    {
        searchLatencies.Sort();
        Console.WriteLine("    Search latency:");
        Console.WriteLine($"      min:  {searchLatencies[0]}ms");
        Console.WriteLine($"      p50:  {Percentile(searchLatencies, 50)}ms");
        Console.WriteLine($"      p95:  {Percentile(searchLatencies, 95)}ms");
        Console.WriteLine($"      p99:  {Percentile(searchLatencies, 99)}ms");
        Console.WriteLine($"      max:  {searchLatencies[^1]}ms");
        Console.WriteLine($"      avg:  {Interlocked.Read(ref totalSearchMs) / (double)searchLatencies.Count:F1}ms");
        Console.WriteLine();
    }
}

Console.WriteLine("  MEMORY:");
Console.WriteLine($"    .NET peak WS:         {Interlocked.Read(ref peakDotnetWs) / (1024.0 * 1024):F1} MB");
Console.WriteLine($"    Server peak WS:       {Interlocked.Read(ref peakServerWs) / (1024.0 * 1024):F1} MB");
Console.WriteLine();

// Cleanup
Console.Write("Cleanup... ");
await client.Collections.DeleteAsync(collName);
Console.WriteLine("OK");
Console.WriteLine();

// ═══════════════════════════════════════════════════════════════════
// Monitor loop (ingest phase)
// ═══════════════════════════════════════════════════════════════════

async Task MonitorLoop(CancellationToken ct)
{
    var dotnetProc = Process.GetCurrentProcess();
    Process? torqueProc = null;

    while (!ct.IsCancellationRequested)
    {
        try { await Task.Delay(monitorIntervalSec * 1000, ct); } catch (OperationCanceledException) { break; }

        dotnetProc.Refresh();
        long dotnetWs = dotnetProc.WorkingSet64;
        long dotnetGc = GC.GetTotalMemory(false);
        int gen0 = GC.CollectionCount(0);
        int gen1 = GC.CollectionCount(1);
        int gen2 = GC.CollectionCount(2);
        UpdatePeak(ref peakDotnetWs, dotnetWs);

        long serverWs = 0;
        torqueProc ??= FindTorqueProcess();
        if (torqueProc != null)
        {
            try { torqueProc.Refresh(); serverWs = torqueProc.WorkingSet64; UpdatePeak(ref peakServerWs, serverWs); }
            catch { torqueProc = null; }
        }

        long docs = Interlocked.Read(ref docsIngested);
        long elapsedMs = globalSw.ElapsedMilliseconds;
        long docsPerSec = elapsedMs > 0 ? docs * 1000 / elapsedMs : 0;

        Console.WriteLine($"  {elapsedMs / 1000.0,6:F0}s │ {docs,12:N0} │ {docsPerSec,10:N0} │ {dotnetWs / (1024.0 * 1024),7:F0} MB │ {dotnetGc / (1024.0 * 1024),6:F0} MB │ {gen0}/{gen1}/{gen2,3} │ {(serverWs > 0 ? $"{serverWs / (1024.0 * 1024),7:F0} MB" : "     N/A")}");
    }
}

void PrintIngestHeader()
{
    Console.WriteLine($"  {"Time",6} │ {"Ingested",12} │ {"Docs/sec",10} │ {".NET WS",7}    │ {"GC Heap",6}    │ {"GC G0/1/2",9} │ {"Srv WS",7}");
    Console.WriteLine($"  ──────┼──────────────┼────────────┼────────────┼───────────┼───────────┼────────────");
}

void PrintSearchHeader()
{
    Console.WriteLine($"  {"Time",5} │ {"Completed",10} │ {"Errors",6} │ {"QPS",8} │ {"p50",5}   │ {"p95",5}   │ {"p99",5}   │ {"Srv WS",7}");
    Console.WriteLine($"  ─────┼────────────┼────────┼──────────┼────────┼────────┼────────┼────────────");
}

static void UpdatePeak(ref long peak, long value)
{
    long prev;
    do { prev = Interlocked.Read(ref peak); }
    while (value > prev && Interlocked.CompareExchange(ref peak, value, prev) != prev);
}

static long Percentile(List<long> sorted, int p)
{
    int idx = (int)Math.Ceiling(p / 100.0 * sorted.Count) - 1;
    return sorted[Math.Clamp(idx, 0, sorted.Count - 1)];
}

static Process? FindTorqueProcess()
{
    try
    {
        var procs = Process.GetProcessesByName("torque-server");
        return procs.Length > 0 ? procs[0] : null;
    }
    catch { return null; }
}

static CollectionSchema StressSchema() => new()
{
    Name = "stress_products",
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
        new() { Name = "embedding", Type = TorqueFieldType.FloatArray, NumDim = 128, VecDist = VecDist.Cosine },
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

static float[] GenerateEmbedding(Random rng, int dim)
{
    var vec = new float[dim];
    float norm = 0;
    for (int i = 0; i < dim; i++)
    {
        vec[i] = (float)(rng.NextDouble() * 2.0 - 1.0);
        norm += vec[i] * vec[i];
    }
    norm = MathF.Sqrt(norm);
    if (norm > 0)
        for (int i = 0; i < dim; i++)
            vec[i] /= norm;
    return vec;
}

static string GenerateQueryVector(int seed, int dim)
{
    var rng = new Random(seed);
    var vec = GenerateEmbedding(rng, dim);
    return string.Join(", ", vec.Select(v => v.ToString("G", System.Globalization.CultureInfo.InvariantCulture)));
}

static string? GetArg(string[] args, string name)
{
    for (int i = 0; i < args.Length - 1; i++)
        if (args[i] == name) return args[i + 1];
    return null;
}
