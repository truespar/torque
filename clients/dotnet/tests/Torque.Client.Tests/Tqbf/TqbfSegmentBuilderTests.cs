using System.Buffers.Binary;
using System.Text;
using Torque.Http.Tqbf;

namespace Torque.Http.Tests.Tqbf;

/// <summary>The native builder through the .NET API (its output is checked against the import
/// path by the Rust tests in src/build).</summary>
public class TqbfSegmentBuilderTests
{
    private const string Schema = """
        {"name":"t","enable_nested_fields":true,"fields":[
          {"name":"name","type":"string"},
          {"name":"n","type":"int32","facet":true},
          {"name":"tags","type":"string[]","facet":true,"optional":true},
          {"name":"a.city","type":"string","facet":true,"optional":true}]}
        """;

    private static string Doc(int i) => i % 50 == 7
        ? $$$"""{"id":"{{{i}}}","name":"x","n":"not a number"}"""
        : $$$"""{"id":"{{{i}}}","name":"Företag {{{i % 13}}} AB","n":{{{i % 9}}},"tags":["t{{{i % 4}}}",null],"a":{"city":"Stad{{{i % 3}}}"},"extra":{"k":[1,2.5]}}""";

    [Fact]
    public void RunsFromManyThreads_BuildOneSegment()
    {
        var dir = Directory.CreateTempSubdirectory();
        try
        {
            var path = Path.Combine(dir.FullName, "seg.tqbf");
            using var b = new TqbfSegmentBuilder(Schema, new TqbfSegmentBuildOptions { DocsLevel = 3, PieceBytes = 1 });
            var rejected = new List<TqbfRejected>();
            Parallel.For(0, 8, r =>
            {
                using var run = b.BeginRun(r);
                for (int i = r * 1000; i < (r + 1) * 1000; i++)
                    run.AddJson(Doc(i));
                var refused = run.End();
                lock (rejected) rejected.AddRange(refused);
            });
            var report = b.Finish(path);

            Assert.Equal(8000 - 160, (int)report.Documents);
            Assert.Equal(160, rejected.Count);
            Assert.Equal(160, report.Rejected.Count);
            Assert.All(rejected, r => Assert.Equal("Field 'n': expected int32, got string", r.Error));
            Assert.Contains(rejected, r => r.Run == 3 && r.Id == "3007" && r.Index == 7);
            Assert.True(report.Pieces > 8);

            var file = File.ReadAllBytes(path);
            Assert.Equal("TQBF"u8.ToArray(), file[..4]);
            Assert.Equal(report.Documents, BinaryPrimitives.ReadUInt32LittleEndian(file.AsSpan(8)));
            Assert.Equal(report.Bytes, file.Length);
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }

    [Fact]
    public void Errors_ReachTheCaller()
    {
        Assert.Throws<TqbfBuildException>(() => new TqbfSegmentBuilder("""{"name":"t","fields":[{"name":"x","type":"nope"}]}"""));
        using var b = new TqbfSegmentBuilder(Schema);
        var run = b.BeginRun(0);
        run.AddJson("""{"id":"1","name":"a","n":1}""");
        run.AddJson("""{"id":"1","name":"b","n":2}""");
        var e = Assert.Throws<TqbfBuildException>(() => run.End());
        Assert.Contains("document id `1` is added twice", e.Message);
        Assert.Throws<InvalidOperationException>(() => run.AddJson("""{"id":"2","name":"a","n":1}"""));
        var open = b.BeginRun(1);
        Assert.Throws<InvalidOperationException>(() => b.Finish(Path.Combine(Path.GetTempPath(), "never.tqbf")));
        open.Dispose();
    }
}
