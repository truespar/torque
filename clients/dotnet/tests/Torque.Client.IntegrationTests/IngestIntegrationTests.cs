using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;
using Torque.Http.Search;
using Torque.Http.Tcp;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class IngestIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateHttpClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    private async Task WaitForRebuildAsync(TorqueHttpClient httpClient, string collection, uint expectedDocs, int timeoutMs = 10_000)
    {
        var sw = System.Diagnostics.Stopwatch.StartNew();
        while (sw.ElapsedMilliseconds < timeoutMs)
        {
            var coll = await httpClient.Collections.GetAsync(collection);
            if (coll.NumDocuments == expectedDocs)
                return;
            await Task.Delay(100);
        }
        throw new TimeoutException($"Collection {collection} did not reach {expectedDocs} docs within {timeoutMs}ms");
    }

    [Fact]
    public async Task TcpIngest_1000Docs_CommitAndSearch()
    {
        using var httpClient = CreateHttpClient();
        var name = $"ingest_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                Fields =
                [
                    new() { Name = "title", Type = TorqueFieldType.String },
                    new() { Name = "seq", Type = TorqueFieldType.Int32, Sort = true },
                ]
            };

            await httpClient.Collections.CreateAsync(schema);

            await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await ingest.ConnectAsync();
            await ingest.StartIngestAsync(name);

            for (int batch = 0; batch < 10; batch++)
            {
                var docs = new List<TorqueDocument>();
                for (int i = 0; i < 100; i++)
                {
                    int seq = batch * 100 + i;
                    docs.Add(new TorqueDocumentBuilder()
                        .SetId($"doc-{seq}")
                        .Set("title", $"Document number {seq}")
                        .Set("seq", seq)
                        .Build(schema));
                }
                var ack = await ingest.SendBatchAsync(docs, schema);
                Assert.Equal((ulong)((batch + 1) * 100), ack.DocsReceived);
            }

            var commitAck = await ingest.CommitAsync();
            Assert.Equal(1000UL, commitAck.DocsReceived);

            await WaitForRebuildAsync(httpClient, name, 1000);

            var search = await httpClient.SearchAsync(name, new SearchParameters
            {
                Q = "Document",
                QueryBy = "title",
                PerPage = 1,
            });
            Assert.Equal(1000, search.Found);
        }
        finally
        {
            try { await CreateHttpClient().Collections.DeleteAsync(name); } catch { }
        }
    }

    [Fact]
    public async Task TcpIngest_Abort_DiscardsDocuments()
    {
        using var httpClient = CreateHttpClient();
        var name = $"abort_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                Fields = [new() { Name = "title", Type = TorqueFieldType.String }]
            };

            await httpClient.Collections.CreateAsync(schema);

            await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await ingest.ConnectAsync();
            await ingest.StartIngestAsync(name);

            var docs = new List<TorqueDocument>
            {
                new TorqueDocumentBuilder().SetId("1").Set("title", "test").Build(schema)
            };
            await ingest.SendBatchAsync(docs, schema);
            await ingest.AbortAsync();

            var coll = await httpClient.Collections.GetAsync(name);
            Assert.Equal(0u, coll.NumDocuments);
        }
        finally
        {
            try { await CreateHttpClient().Collections.DeleteAsync(name); } catch { }
        }
    }

    [Fact]
    public async Task RealtimeUpsert_AddsDocumentsToExistingCollection()
    {
        using var httpClient = CreateHttpClient();
        var name = $"upsert_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                IndexMode = "realtime",
                Fields =
                [
                    new() { Name = "title", Type = TorqueFieldType.String },
                    new() { Name = "seq", Type = TorqueFieldType.Int32, Sort = true },
                ]
            };

            await httpClient.Collections.CreateAsync(schema);

            await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await ingest.ConnectAsync();
            await ingest.StartIngestAsync(name);

            var docs = new List<TorqueDocument>();
            for (int i = 0; i < 100; i++)
            {
                docs.Add(new TorqueDocumentBuilder()
                    .SetId($"doc-{i}")
                    .Set("title", $"Document {i}")
                    .Set("seq", i)
                    .Build(schema));
            }
            await ingest.SendBatchAsync(docs, schema);
            await ingest.CommitAsync();
            await WaitForRebuildAsync(httpClient, name, 100);

            await using var rtClient = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await rtClient.ConnectAsync();

            var upsertDocs = new List<TorqueDocument>();
            for (int i = 100; i < 105; i++)
            {
                upsertDocs.Add(new TorqueDocumentBuilder()
                    .SetId($"doc-{i}")
                    .Set("title", $"Document {i}")
                    .Set("seq", i)
                    .Build(schema));
            }
            var ack = await rtClient.UpsertBatchAsync(name, upsertDocs, schema);
            Assert.Equal(5UL, ack.DocsReceived);

            await Task.Delay(500);

            var search = await httpClient.SearchAsync(name, new SearchParameters
            {
                Q = "Document",
                QueryBy = "title",
                PerPage = 1,
            });
            Assert.Equal(105, search.Found);
        }
        finally
        {
            try { await CreateHttpClient().Collections.DeleteAsync(name); } catch { }
        }
    }

    [Fact]
    public async Task RealtimeDelete_RemovesDocumentsFromCollection()
    {
        using var httpClient = CreateHttpClient();
        var name = $"delete_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                IndexMode = "realtime",
                Fields =
                [
                    new() { Name = "title", Type = TorqueFieldType.String },
                    new() { Name = "seq", Type = TorqueFieldType.Int32, Sort = true },
                ]
            };

            await httpClient.Collections.CreateAsync(schema);

            await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await ingest.ConnectAsync();
            await ingest.StartIngestAsync(name);

            var docs = new List<TorqueDocument>();
            for (int i = 0; i < 100; i++)
            {
                docs.Add(new TorqueDocumentBuilder()
                    .SetId($"doc-{i}")
                    .Set("title", $"Document {i}")
                    .Set("seq", i)
                    .Build(schema));
            }
            await ingest.SendBatchAsync(docs, schema);
            await ingest.CommitAsync();
            await WaitForRebuildAsync(httpClient, name, 100);

            await using var rtClient = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await rtClient.ConnectAsync();
            var ack = await rtClient.DeleteBatchAsync(name, ["doc-0", "doc-1", "doc-2"]);
            Assert.Equal(3UL, ack.DocsReceived);

            await Task.Delay(500);

            var search = await httpClient.SearchAsync(name, new SearchParameters
            {
                Q = "Document",
                QueryBy = "title",
                PerPage = 1,
            });
            Assert.Equal(97, search.Found);
        }
        finally
        {
            try { await CreateHttpClient().Collections.DeleteAsync(name); } catch { }
        }
    }

    [Fact]
    public async Task RealtimeUpsert_UpdatesExistingDocument()
    {
        using var httpClient = CreateHttpClient();
        var name = $"update_{Guid.NewGuid():N}"[..32];

        try
        {
            var schema = new CollectionSchema
            {
                Name = name,
                IndexMode = "realtime",
                Fields =
                [
                    new() { Name = "title", Type = TorqueFieldType.String },
                    new() { Name = "seq", Type = TorqueFieldType.Int32, Sort = true },
                ]
            };

            await httpClient.Collections.CreateAsync(schema);

            await using var ingest = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await ingest.ConnectAsync();
            await ingest.StartIngestAsync(name);

            var docs = new List<TorqueDocument>();
            for (int i = 0; i < 10; i++)
            {
                docs.Add(new TorqueDocumentBuilder()
                    .SetId($"doc-{i}")
                    .Set("title", $"Original {i}")
                    .Set("seq", i)
                    .Build(schema));
            }
            await ingest.SendBatchAsync(docs, schema);
            await ingest.CommitAsync();
            await WaitForRebuildAsync(httpClient, name, 10);

            await using var rtClient = new TorqueIngestClient(new TorqueIngestOptions { Host = fixture.TcpHost, Port = fixture.TcpPort });
            await rtClient.ConnectAsync();
            var updated = new TorqueDocumentBuilder()
                .SetId("doc-5")
                .Set("title", "Updated unique marker xyzzy")
                .Set("seq", 5)
                .Build(schema);
            await rtClient.UpsertAsync(name, updated, schema);

            await Task.Delay(500);

            var searchAll = await httpClient.SearchAsync(name, new SearchParameters
            {
                Q = "*",
                QueryBy = "title",
                PerPage = 1,
            });
            Assert.Equal(10, searchAll.Found);

            var searchUpdated = await httpClient.SearchAsync(name, new SearchParameters
            {
                Q = "xyzzy",
                QueryBy = "title",
                PerPage = 1,
            });
            Assert.Equal(1, searchUpdated.Found);
        }
        finally
        {
            try { await CreateHttpClient().Collections.DeleteAsync(name); } catch { }
        }
    }
}
