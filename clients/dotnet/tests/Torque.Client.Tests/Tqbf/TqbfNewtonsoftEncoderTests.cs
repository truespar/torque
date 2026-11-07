using System.Text;
using Newtonsoft.Json;
using Newtonsoft.Json.Converters;
using Newtonsoft.Json.Linq;
using Newtonsoft.Json.Serialization;
using Torque.Http.Tqbf;

namespace Torque.Http.Tests.Tqbf;

/// <summary>
/// The encoder's record of an object equals the record of the JSON Newtonsoft writes for it, read
/// as Torque's import reads JSON, byte for byte (same path ids).
/// </summary>
public class TqbfNewtonsoftEncoderTests
{
    public enum Status { Active, InLiquidation, Ceased = 7 }

    [Flags]
    public enum Flags : ulong { None = 0, A = 1, B = 2, High = 1UL << 63 }

    public record Name(string NameOrIdentifier, int? Kind, [property: JsonIgnore] int Hidden);

    public class Address
    {
        public string? City { get; set; }
        public int? Zip { get; set; }
        public double[]? Geo { get; set; }
        public List<Address>? Children { get; set; }
    }

    public class Base
    {
        public long BaseValue { get; set; } = 42;
    }

    public class Doc : Base
    {
        public string Id { get; set; } = "1";
        public string? Name { get; set; }
        [JsonProperty("rs_NetSalesK")] public int? NetSales { get; set; }
        [JsonIgnore] public string Secret { get; set; } = "x";
        public bool IsCeased { get; set; }
        public bool? MaybeBool { get; set; }
        public byte B { get; set; } = 255;
        public short S { get; set; } = -3;
        public uint U { get; set; } = 4_000_000_000;
        public ulong Big { get; set; } = ulong.MaxValue;
        public ulong Small { get; set; } = 5;
        public long Min { get; set; } = long.MinValue;
        public double D { get; set; } = 11.952819999999999;
        public double NegZero { get; set; } = -0.0;
        public double Whole { get; set; } = 5.0;
        public double Nan { get; set; } = double.NaN;
        public double Inf { get; set; } = double.NegativeInfinity;
        public float F { get; set; } = 0.1f;
        public float? NF { get; set; }
        public decimal M { get; set; } = 1.10m;
        public decimal? NM { get; set; } = 12m;
        public decimal Tiny { get; set; } = 0.0000001m;
        public Guid? G { get; set; } = Guid.Parse("8c8f1a2e-0d4b-4c7e-9a43-35b1c2d4e5f6");
        public Guid? NoG { get; set; }
        public char C { get; set; } = 'ö';
        public Status State { get; set; } = Status.InLiquidation;
        public Status? NoState { get; set; }
        public Status? Undefined { get; set; } = (Status)99;
        public Flags Bits { get; set; } = Flags.A | Flags.High;
        public Status[]? States { get; set; } = [Status.Active, Status.Ceased];
        public List<Name>? Names { get; set; }
        public Address? Address { get; set; }
        public List<Address?>? Addresses { get; set; }
        public int[]? Ints { get; set; } = [1, -2, int.MaxValue];
        public long?[]? NullableLongs { get; set; } = [1, null, long.MaxValue];
        public List<string?>? Tags { get; set; } = ["b", null, "a"];
        public List<bool>? Bools { get; set; } = [true, false, true];
        public List<bool?>? NullBools { get; set; } = [true, null];
        public List<double>? Doubles { get; set; } = [0.1, 2.5, -0.0];
        public List<List<int>>? Nested { get; set; } = [[1, 2], [], [3]];
        public object?[]? Mixed { get; set; } = [1, "two", null, false, 2.5, new Name("n", 1, 0)];
        public List<int>? Empty { get; set; } = [];
        public HashSet<string>? Set { get; set; } = ["x", "y"];
        public IEnumerable<int> Lazy => Enumerable.Range(1, 3).Select(i => i * i);
        public DateTime When { get; set; } = new(2026, 10, 17, 12, 30, 15, DateTimeKind.Utc);
        public Dictionary<string, int?>? Map { get; set; } = new() { ["z"] = 1, ["a"] = null };
        public object? Data { get; set; } = JArray.Parse("[1, {\"x\": \"y\", \"n\": null}, 2.5]");
        public object? Obj { get; set; } = JObject.Parse("{\"message\": \"hej\"}");
        public byte[]? Bytes { get; set; } = [1, 2, 3];
        [JsonConverter(typeof(IsoDateTimeConverter))] public DateTime Converted { get; set; } = new(2020, 1, 2);
        public string? Conditional { get; set; } = "shown";
        public bool ShouldSerializeConditional() => NetSales != 7;
        public string? Ünicode { get; set; } = "Åkeri ΣΟΦΟΣ \u0000 \"q\" 😀";
    }

    public class Wide
    {
        public string Id { get; set; } = "w";
        public Dictionary<string, int> Unused { get; } = new();
        public int P000 { get; set; } public int P001 { get; set; } public int P002 { get; set; } public int P003 { get; set; } public int P004 { get; set; }
        public int P005 { get; set; } public int P006 { get; set; } public int P007 { get; set; } public int P008 { get; set; } public int P009 { get; set; }
        public int P010 { get; set; } public int P011 { get; set; } public int P012 { get; set; } public int P013 { get; set; } public int P014 { get; set; }
        public int P015 { get; set; } public int P016 { get; set; } public int P017 { get; set; } public int P018 { get; set; } public int P019 { get; set; }
        public int P020 { get; set; } public int P021 { get; set; } public int P022 { get; set; } public int P023 { get; set; } public int P024 { get; set; }
        public int P025 { get; set; } public int P026 { get; set; } public int P027 { get; set; } public int P028 { get; set; } public int P029 { get; set; }
        public int P030 { get; set; } public int P031 { get; set; } public int P032 { get; set; } public int P033 { get; set; } public int P034 { get; set; }
        public int P035 { get; set; } public int P036 { get; set; } public int P037 { get; set; } public int P038 { get; set; } public int P039 { get; set; }
        public int P040 { get; set; } public int P041 { get; set; } public int P042 { get; set; } public int P043 { get; set; } public int P044 { get; set; }
        public int P045 { get; set; } public int P046 { get; set; } public int P047 { get; set; } public int P048 { get; set; } public int P049 { get; set; }
        public int P050 { get; set; } public int P051 { get; set; } public int P052 { get; set; } public int P053 { get; set; } public int P054 { get; set; }
        public int P055 { get; set; } public int P056 { get; set; } public int P057 { get; set; } public int P058 { get; set; } public int P059 { get; set; }
        public int P060 { get; set; } public int P061 { get; set; } public int P062 { get; set; } public int P063 { get; set; } public int P064 { get; set; }
        public int P065 { get; set; } public int P066 { get; set; } public int P067 { get; set; } public int P068 { get; set; } public int P069 { get; set; }
        public int P070 { get; set; } public int P071 { get; set; } public int P072 { get; set; } public int P073 { get; set; } public int P074 { get; set; }
        public int P075 { get; set; } public int P076 { get; set; } public int P077 { get; set; } public int P078 { get; set; } public int P079 { get; set; }
        public int P080 { get; set; } public int P081 { get; set; } public int P082 { get; set; } public int P083 { get; set; } public int P084 { get; set; }
        public int P085 { get; set; } public int P086 { get; set; } public int P087 { get; set; } public int P088 { get; set; } public int P089 { get; set; }
        public int P090 { get; set; } public int P091 { get; set; } public int P092 { get; set; } public int P093 { get; set; } public int P094 { get; set; }
        public int P095 { get; set; } public int P096 { get; set; } public int P097 { get; set; } public int P098 { get; set; } public int P099 { get; set; }
        public int P100 { get; set; } public int P101 { get; set; } public int P102 { get; set; } public int P103 { get; set; } public int P104 { get; set; }
        public int P105 { get; set; } public int P106 { get; set; } public int P107 { get; set; } public int P108 { get; set; } public int P109 { get; set; }
        public int P110 { get; set; } public int P111 { get; set; } public int P112 { get; set; } public int P113 { get; set; } public int P114 { get; set; }
        public int P115 { get; set; } public int P116 { get; set; } public int P117 { get; set; } public int P118 { get; set; } public int P119 { get; set; }
        public int P120 { get; set; } public int P121 { get; set; } public int P122 { get; set; } public int P123 { get; set; } public int P124 { get; set; }
        public int P125 { get; set; } public int P126 { get; set; } public int P127 { get; set; } public int P128 { get; set; } public int P129 { get; set; }
        public string Long { get; set; } = new string('x', 300) + "å";
    }

    private static JsonSerializerSettings Settings(NullValueHandling nulls)
    {
        var s = new JsonSerializerSettings
        {
            NullValueHandling = nulls,
            Formatting = Formatting.None,
            ContractResolver = new DefaultContractResolver { NamingStrategy = new CamelCaseNamingStrategy() },
        };
        s.Converters.Add(new StringEnumConverter { NamingStrategy = new CamelCaseNamingStrategy() });
        return s;
    }

    private static void AssertSameRecord(object doc, JsonSerializerSettings settings)
    {
        var paths = new TqbfFieldRegistry();
        var w = new TqbfRecordWriter();
        new TqbfNewtonsoftEncoder(settings).Encode(doc, paths, w);
        var json = JsonConvert.SerializeObject(doc, settings);
        var reference = new TqbfDocument(paths);
        reference.Reset();
        reference.ReadJson(Encoding.UTF8.GetBytes(json));
        Assert.Equal(Convert.ToHexString(reference.Encode()), Convert.ToHexString(w.Written));
    }

    private static Doc Full()
    {
        var d = new Doc
        {
            Name = "Göteborgs Åkeri AB",
            NetSales = 1234,
            Names = [new("A", 1, 5), new("B", null, 6)],
            Address = new Address { City = "Malmö", Geo = [55.6, 13.0], Children = [new Address { Zip = 21100 }] },
            Addresses = [new Address { City = "Lund" }, null, new Address()],
        };
        return d;
    }

    [Theory]
    [InlineData(NullValueHandling.Ignore)]
    [InlineData(NullValueHandling.Include)]
    public void EveryShape_EqualsTheJsonRecord(NullValueHandling nulls)
    {
        AssertSameRecord(Full(), Settings(nulls));
        AssertSameRecord(new Doc { NetSales = 7, Names = [], Address = new Address() }, Settings(nulls));
        AssertSameRecord(new Wide(), Settings(nulls));
    }

    [Fact]
    public void ManyDocuments_ShareOneEncoderAndRegistry()
    {
        var settings = Settings(NullValueHandling.Ignore);
        var encoder = new TqbfNewtonsoftEncoder(settings);
        var paths = new TqbfFieldRegistry();
        var w = new TqbfRecordWriter();
        var rnd = new Random(5);
        for (int i = 0; i < 300; i++)
        {
            var d = Full();
            d.Id = i.ToString();
            d.D = rnd.NextDouble() * 1e6;
            d.M = (decimal)rnd.Next() / 100;
            d.F = (float)rnd.NextDouble();
            if (i % 3 == 0) d.Names = null;
            if (i % 5 == 0) d.Address = null;
            encoder.Encode(d, paths, w);
            var reference = new TqbfDocument(paths);
            reference.Reset();
            reference.ReadJson(Encoding.UTF8.GetBytes(JsonConvert.SerializeObject(d, settings)));
            Assert.Equal(Convert.ToHexString(reference.Encode()), Convert.ToHexString(w.Written));
        }
    }

    [Fact]
    public void RefusesDocumentsWithoutAStringId()
    {
        var w = new TqbfRecordWriter();
        var e = new TqbfNewtonsoftEncoder(Settings(NullValueHandling.Ignore));
        Assert.Throws<FormatException>(() => e.Encode(new Doc { Id = null! }, new TqbfFieldRegistry(), w));
        Assert.Throws<FormatException>(() => e.Encode(new Address(), new TqbfFieldRegistry(), w));
    }
}
