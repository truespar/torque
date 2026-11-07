using System.Collections;
using System.Collections.Concurrent;
using System.Globalization;
using System.Linq.Expressions;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using Newtonsoft.Json;
using Newtonsoft.Json.Converters;
using Newtonsoft.Json.Serialization;
using JsonSerializer = Newtonsoft.Json.JsonSerializer;
using JsonProperty = Newtonsoft.Json.Serialization.JsonProperty;
using Tag = Torque.Http.Tqbf.TqbfRecordWriter;

namespace Torque.Http.Tqbf;

/// <summary>
/// Encodes objects as TQBF records exactly as <c>JsonConvert.SerializeObject(document, settings)</c>
/// followed by Torque's JSON import would, without writing JSON. Members, names, ignored and
/// conditional members and null handling come from the settings' own contract resolver; values are
/// written as the record values their JSON text parses to (integers, floats with their exact bits,
/// <c>decimal</c> and <c>float</c> through their Newtonsoft text, enums as the settings' converters
/// write them). Values Newtonsoft writes through a converter (and dictionaries, <c>JToken</c>s,
/// dates and other rarely used shapes) are serialized by Newtonsoft and read back, which is exact
/// but slower. Thread-safe: share one encoder between the threads of a build.
/// </summary>
/// <example>
/// <code>
/// var encoder = new TqbfNewtonsoftEncoder(settings);
/// using var run = builder.BeginRun(rangeIndex);
/// foreach (var document in documents) encoder.Add(run, document);
/// run.End();
/// </code>
/// </example>
public sealed class TqbfNewtonsoftEncoder
{
    private const ushort Unset = ushort.MaxValue;
    private const int MaxDepth = 64;

    private readonly JsonSerializerSettings _settings;
    private readonly IContractResolver _resolver;
    private readonly JsonConverter[] _converters;
    private readonly NullValueHandling _nulls;
    private readonly FloatFormatHandling _floats;
    private readonly ThreadLocal<JsonSerializer> _serializer;
    private readonly ConcurrentDictionary<Type, Plan> _plans = new();
    private readonly ConcurrentDictionary<Type, ConcurrentDictionary<long, Fixed>> _enums = new();
    private readonly ConditionalWeakTable<ITqbfPaths, ConcurrentDictionary<(ObjectPlan, ushort), ushort[]>> _fids = new();
    /// <summary>double, long and int are written without a converter, so their arrays take the typed path.</summary>
    private readonly bool _typedArrays;

    [ThreadStatic] private static Ctx? t_ctx;

    private static readonly PropertyInfo? InternalConverterProperty =
        typeof(JsonContract).GetProperty("InternalConverter", BindingFlags.Instance | BindingFlags.NonPublic);

    /// <param name="settings">The settings documents are serialized with (as <c>JsonConvert.SerializeObject</c>
    /// takes them; <c>JsonConvert.DefaultSettings</c> apply as there).</param>
    /// <exception cref="NotSupportedException">Settings whose JSON depends on more than the values
    /// (type names, preserved references, skipped default values).</exception>
    public TqbfNewtonsoftEncoder(JsonSerializerSettings settings)
    {
        ArgumentNullException.ThrowIfNull(settings);
        var s = JsonSerializer.CreateDefault(settings);
        if (s.TypeNameHandling != TypeNameHandling.None)
            throw new NotSupportedException("TypeNameHandling other than None is not supported.");
        if (s.PreserveReferencesHandling != PreserveReferencesHandling.None)
            throw new NotSupportedException("PreserveReferencesHandling other than None is not supported.");
        if ((s.DefaultValueHandling & DefaultValueHandling.Ignore) != 0)
            throw new NotSupportedException("DefaultValueHandling.Ignore is not supported.");
        if (s.FloatFormatHandling == FloatFormatHandling.Symbol)
            throw new NotSupportedException("FloatFormatHandling.Symbol writes NaN and Infinity as invalid JSON.");
        _settings = settings;
        _resolver = s.ContractResolver;
        _converters = s.Converters.ToArray();
        _nulls = s.NullValueHandling;
        _floats = s.FloatFormatHandling;
        _serializer = new ThreadLocal<JsonSerializer>(() => JsonSerializer.CreateDefault(_settings));
        _typedArrays = PlanOf(typeof(double)) is PrimitivePlan { Kind: Kind.Double }
            && PlanOf(typeof(long)) is PrimitivePlan { Kind: Kind.Int64 }
            && PlanOf(typeof(int)) is PrimitivePlan { Kind: Kind.Int64 };
    }

    /// <summary>Encode <paramref name="document"/> and add it to <paramref name="run"/>.</summary>
    /// <exception cref="FormatException">The document has no string <c>id</c>.</exception>
    public void Add(TqbfRun run, object document)
    {
        ArgumentNullException.ThrowIfNull(run);
        Encode(document, run.Paths, run.Writer);
        run.AddWritten();
    }

    /// <summary>Encode <paramref name="document"/> into <paramref name="w"/> with ids from <paramref name="paths"/>.</summary>
    internal void Encode(object document, ITqbfPaths paths, TqbfRecordWriter w)
    {
        ArgumentNullException.ThrowIfNull(document);
        var c = t_ctx ??= new Ctx();
        c.Enc = this;
        c.W = w;
        c.Paths = paths;
        c.Fids = _fids.GetValue(paths, _ => new ConcurrentDictionary<(ObjectPlan, ushort), ushort[]>());
        c.Depth = 0;
        c.Starts.Clear();
        if (PlanOf(document.GetType()) is ObjectPlan op)
        {
            if (op.IdIndex < 0 || op.Props[op.IdIndex].Get(document) is not { } id)
                throw new FormatException("The document has no `id`.");
            w.Begin(id as string ?? throw new FormatException("The document `id` must be a string."));
            WriteObject(c, document, op, TqbfFieldRegistry.NoParent, top: true);
            return;
        }
        using var doc = JsonDocument.Parse(SerializeFallback(document, null));
        WriteJsonDocument(c, doc.RootElement);
    }

    // -- plans --------------------------------------------------------------------------------------

    private abstract class Plan;

    /// <summary>Written by Newtonsoft (with <see cref="Converter"/> when set) and read back.</summary>
    private sealed class FallbackPlan(JsonConverter? converter) : Plan
    {
        public readonly JsonConverter? Converter = converter;
    }

    private enum Kind : byte { String, Char, Bool, Int64, UInt64, Double, Single, Decimal, Guid, Enum }

    private sealed class PrimitivePlan(Kind kind, Type type) : Plan
    {
        public readonly Kind Kind = kind;
        public readonly Type Type = type;
    }

    private sealed class ArrayPlan : Plan
    {
        public static readonly ArrayPlan Instance = new();
    }

    private sealed class Prop
    {
        public required string Name;
        public required byte[] Utf8;
        public required Func<object, object?> Get;
        public required Func<object, Ctx, ushort, bool> Write;
        public Predicate<object>? ShouldSerialize;
        public Predicate<object>? IsSpecified;
    }

    private sealed class ObjectPlan : Plan
    {
        /// <summary>Members in UTF-8 byte order of their names (the record's key order).</summary>
        public Prop[] Props = [];
        public int IdIndex = -1;
    }

    /// <summary>A value whose record encoding is fixed (enums).</summary>
    private readonly record struct Fixed(byte Tag, long Int, string? Str);

    private Plan PlanOf(Type t) => _plans.TryGetValue(t, out var p) ? p : _plans.GetOrAdd(t, BuildPlan);

    private JsonConverter? MatchingConverter(Type t)
    {
        foreach (var c in _converters)
            if (c.CanConvert(t))
                return c;
        return null;
    }

    private static Kind? KindOf(Type u)
    {
        if (u.IsEnum) return Kind.Enum;
        return Type.GetTypeCode(u) switch
        {
            TypeCode.String => Kind.String,
            TypeCode.Char => Kind.Char,
            TypeCode.Boolean => Kind.Bool,
            TypeCode.SByte or TypeCode.Byte or TypeCode.Int16 or TypeCode.UInt16 or TypeCode.Int32 or TypeCode.UInt32 or TypeCode.Int64 => Kind.Int64,
            TypeCode.UInt64 => Kind.UInt64,
            TypeCode.Double => Kind.Double,
            TypeCode.Single => Kind.Single,
            TypeCode.Decimal => Kind.Decimal,
            _ => u == typeof(Guid) ? Kind.Guid : null,
        };
    }

    private Plan BuildPlan(Type t)
    {
        var contract = _resolver.ResolveContract(t);
        var u = Nullable.GetUnderlyingType(t) ?? t;
        // Newtonsoft's order: the contract's converter, the settings' converters, the internal one.
        var conv = contract.Converter ?? MatchingConverter(contract.UnderlyingType) ?? InternalConverterProperty?.GetValue(contract) as JsonConverter;
        if (conv != null)
            return conv is StringEnumConverter && u.IsEnum ? new PrimitivePlan(Kind.Enum, u) : new FallbackPlan(conv);
        switch (contract)
        {
            case JsonPrimitiveContract:
                return KindOf(u) is { } k ? new PrimitivePlan(k, u) : new FallbackPlan(null);
            case JsonArrayContract ac:
                return ac.ItemConverter != null || ac.ItemIsReference == true || (ac.ItemTypeNameHandling ?? TypeNameHandling.None) != TypeNameHandling.None
                    ? new FallbackPlan(null)
                    : ArrayPlan.Instance;
            case JsonObjectContract oc:
                return BuildObject(oc);
            default:
                return new FallbackPlan(null);
        }
    }

    private Plan BuildObject(JsonObjectContract oc)
    {
        if (oc.ExtensionDataGetter != null || oc.OnSerializingCallbacks.Count > 0 || oc.OnSerializedCallbacks.Count > 0 || oc.IsReference == true)
            return new FallbackPlan(null);
        var props = new List<Prop>();
        foreach (var p in oc.Properties)
        {
            if (p.Ignored || !p.Readable)
                continue;
            if (p.ItemConverter != null || p.IsReference == true || p.ItemIsReference == true
                || (p.TypeNameHandling ?? TypeNameHandling.None) != TypeNameHandling.None
                || (p.ItemTypeNameHandling ?? TypeNameHandling.None) != TypeNameHandling.None
                || ((p.DefaultValueHandling ?? DefaultValueHandling.Include) & DefaultValueHandling.Ignore) != 0)
                return new FallbackPlan(null);
            var vp = p.ValueProvider!;
            bool ignoreNull = (p.NullValueHandling ?? _nulls) == NullValueHandling.Ignore;
            props.Add(new Prop
            {
                Name = p.PropertyName!,
                Utf8 = Encoding.UTF8.GetBytes(p.PropertyName!),
                Get = vp.GetValue,
                Write = CompileWriter(oc.UnderlyingType, p, ignoreNull),
                ShouldSerialize = p.ShouldSerialize,
                IsSpecified = p.GetIsSpecified,
            });
        }
        props.Sort((a, b) => a.Utf8.AsSpan().SequenceCompareTo(b.Utf8));
        var plan = new ObjectPlan { Props = props.ToArray() };
        plan.IdIndex = Array.FindIndex(plan.Props, x => x.Name == "id");
        return plan;
    }

    /// <summary>A writer for one member: reads it typed (no boxing) where its type is a primitive
    /// Newtonsoft writes without a converter, else as an object.</summary>
    private Func<object, Ctx, ushort, bool> CompileWriter(Type declaring, JsonProperty p, bool ignoreNull)
    {
        if (p.Converter != null)
        {
            var get = p.ValueProvider!;
            var conv = p.Converter;
            return (o, c, fid) => c.Enc.WriteValue(c, fid, get.GetValue(o), ignoreNull, element: false, conv);
        }
        MemberInfo? member = (MemberInfo?)p.DeclaringType?.GetProperty(p.UnderlyingName!, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic, null, p.PropertyType!, Type.EmptyTypes, null)
            ?? p.DeclaringType?.GetField(p.UnderlyingName!, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
        if (member is PropertyInfo { GetMethod: null })
            member = null;
        if (member == null)
        {
            var get = p.ValueProvider!;
            return (o, c, fid) => c.Enc.WriteValue(c, fid, get.GetValue(o), ignoreNull, element: false, null);
        }
        Type mt = member is PropertyInfo pi ? pi.PropertyType : ((FieldInfo)member).FieldType;
        var o = Expression.Parameter(typeof(object), "o");
        var c = Expression.Parameter(typeof(Ctx), "c");
        var fid = Expression.Parameter(typeof(ushort), "fid");
        Expression value = Expression.MakeMemberAccess(Expression.Convert(o, member.DeclaringType!), member);
        var u = Nullable.GetUnderlyingType(mt);
        bool nullable = u != null;
        u ??= mt;
        // Typed only when Newtonsoft writes the type itself (no converter matches it). Other types are
        // planned from the runtime type of each value (which also keeps self-referencing types finite).
        Kind? kind = KindOf(u) != null && PlanOf(u) is PrimitivePlan pp ? pp.Kind : null;
        Expression call;
        var ign = Expression.Constant(ignoreNull);
        string? method = kind switch
        {
            Kind.String => nameof(Writers.Str),
            Kind.Bool => nullable ? nameof(Writers.NBool) : nameof(Writers.Bool),
            Kind.Int64 => nullable ? nameof(Writers.NI64) : nameof(Writers.I64),
            Kind.UInt64 => nullable ? nameof(Writers.NU64) : nameof(Writers.U64),
            Kind.Double => nullable ? nameof(Writers.NF64) : nameof(Writers.F64),
            Kind.Single => nullable ? nameof(Writers.NF32) : nameof(Writers.F32),
            Kind.Decimal => nullable ? nameof(Writers.NDec) : nameof(Writers.Dec),
            Kind.Guid => nullable ? nameof(Writers.NGuid) : nameof(Writers.Guid),
            _ => null,
        };
        if (method != null)
        {
            var m = typeof(Writers).GetMethod(method)!;
            var arg = m.GetParameters()[2].ParameterType;
            call = m.GetParameters().Length == 4
                ? Expression.Call(m, c, fid, Expression.Convert(value, arg), ign)
                : Expression.Call(m, c, fid, Expression.Convert(value, arg));
        }
        else if (kind == Kind.Enum)
        {
            // enum (or enum?) as long, with its encodings cached per value
            var table = Expression.Constant(u);
            if (nullable)
            {
                var v = Expression.Variable(mt, "v");
                call = Expression.Block([v],
                    Expression.Assign(v, value),
                    Expression.Condition(Expression.Property(v, "HasValue"),
                        Expression.Call(typeof(Writers).GetMethod(nameof(Writers.Enum))!, c, fid, Expression.Convert(Expression.Property(v, "Value"), typeof(long)), table),
                        Expression.Call(typeof(Writers).GetMethod(nameof(Writers.Null))!, c, fid, ign)));
            }
            else
            {
                call = Expression.Call(typeof(Writers).GetMethod(nameof(Writers.Enum))!, c, fid, Expression.Convert(value, typeof(long)), table);
            }
        }
        else
        {
            call = Expression.Call(Expression.Field(c, nameof(Ctx.Enc)), typeof(TqbfNewtonsoftEncoder).GetMethod(nameof(WriteValue), BindingFlags.Instance | BindingFlags.NonPublic)!,
                c, fid, Expression.Convert(value, typeof(object)), ign, Expression.Constant(false), Expression.Constant(null, typeof(JsonConverter)));
        }
        return Expression.Lambda<Func<object, Ctx, ushort, bool>>(call, o, c, fid).Compile();
    }

    // -- writing ------------------------------------------------------------------------------------

    private sealed class Ctx
    {
        public TqbfNewtonsoftEncoder Enc = null!;
        public TqbfRecordWriter W = null!;
        public ITqbfPaths Paths = null!;
        public ConcurrentDictionary<(ObjectPlan, ushort), ushort[]> Fids = null!;
        public int Depth;
        /// <summary>Element starts of the arrays being written (a stack of ranges).</summary>
        public readonly List<int> Starts = new();
        private readonly Stack<List<object?>> _lists = new();

        /// <summary>An empty list for a sequence's elements (nested arrays each get their own).</summary>
        public List<object?> Rent() => _lists.Count > 0 ? _lists.Pop() : new List<object?>();

        public void Return(List<object?> l)
        {
            l.Clear();
            _lists.Push(l);
        }
    }

    /// <summary>A value's tag: an object entry's key, or an array element's tag byte.</summary>
    private static void Carrier(Ctx c, ushort fid, byte tag, bool element)
    {
        if (element) c.W.Byte(tag);
        else c.W.Key(fid, tag);
    }

    /// <summary>Write a value under key <paramref name="fid"/> (or as an array element). False when
    /// nothing is written (a null member Newtonsoft leaves out).</summary>
    private bool WriteValue(Ctx c, ushort fid, object? v, bool ignoreNull, bool element, JsonConverter? converter)
    {
        if (v is null)
        {
            if (!element && ignoreNull) return false;
            Carrier(c, fid, Tag.TagNull, element);
            return true;
        }
        if (converter != null)
        {
            WriteFallback(c, fid, v, converter, element);
            return true;
        }
        switch (PlanOf(v.GetType()))
        {
            case PrimitivePlan p:
                WritePrimitive(c, fid, v, p, element);
                break;
            case ObjectPlan op:
                Carrier(c, fid, Tag.TagObject, element);
                WriteObject(c, v, op, fid, top: false);
                break;
            case ArrayPlan:
                Carrier(c, fid, Tag.TagArray, element);
                WriteArray(c, fid, (IEnumerable)v);
                break;
            case FallbackPlan f:
                WriteFallback(c, fid, v, f.Converter, element);
                break;
        }
        return true;
    }

    private void WriteObject(Ctx c, object o, ObjectPlan op, ushort parent, bool top)
    {
        if (++c.Depth > MaxDepth)
            throw new JsonSerializationException($"Objects nested deeper than {MaxDepth} (a reference loop?).");
        if (!c.Fids.TryGetValue((op, parent), out var fids))
        {
            fids = new ushort[op.Props.Length];
            Array.Fill(fids, Unset);
            fids = c.Fids.GetOrAdd((op, parent), fids);
        }
        var w = c.W;
        int at = w.BeginObject();
        int count = 0;
        var props = op.Props;
        for (int i = 0; i < props.Length; i++)
        {
            if (top && i == op.IdIndex) continue;
            var p = props[i];
            if (p.ShouldSerialize != null && !p.ShouldSerialize(o)) continue;
            if (p.IsSpecified != null && !p.IsSpecified(o)) continue;
            ushort fid = fids[i];
            if (fid == Unset)
                fids[i] = fid = c.Paths.Id(parent, p.Name);
            if (p.Write(o, c, fid)) count++;
        }
        w.EndObject(at, count);
        c.Depth--;
    }

    private void WriteArray(Ctx c, ushort fid, IEnumerable items)
    {
        if (++c.Depth > MaxDepth)
            throw new JsonSerializationException($"Arrays nested deeper than {MaxDepth}.");
        // Lists by index, typed arrays without boxing, other sequences once into a pooled list.
        IList? list = items as IList;
        List<object?>? pooled = null;
        int n;
        if (list != null)
            n = list.Count;
        else
        {
            pooled = c.Rent();
            foreach (var x in items) pooled.Add(x);
            n = pooled.Count;
        }
        var w = c.W;
        w.Varint((ulong)n);
        if (n > 0)
        {
            // Every element with its tag first; then uniform arrays drop the tags (bools become 0/1).
            int tagAt = w.Position;
            w.Byte(0);
            int from = c.Starts.Count;
            // Exact types: the CLR also lets an enum array pass as an array of its underlying type.
            Type? at = _typedArrays ? list?.GetType() : null;
            switch (at == typeof(double[]) || at == typeof(long[]) || at == typeof(int[]) ? list : null)
            {
                case double[] d:
                    foreach (double x in d)
                    {
                        c.Starts.Add(w.Position);
                        WriteF64(c, fid, x, element: true);
                    }
                    break;
                case long[] l:
                    foreach (long x in l)
                    {
                        c.Starts.Add(w.Position);
                        w.Byte(Tag.TagInt);
                        w.Int(x);
                    }
                    break;
                case int[] l:
                    foreach (int x in l)
                    {
                        c.Starts.Add(w.Position);
                        w.Byte(Tag.TagInt);
                        w.Int(x);
                    }
                    break;
                default:
                    for (int i = 0; i < n; i++)
                    {
                        c.Starts.Add(w.Position);
                        WriteValue(c, fid, list != null ? list[i] : pooled![i], ignoreNull: false, element: true, null);
                    }
                    break;
            }
            int to = c.Starts.Count;
            static byte Uniform(byte t) => t is Tag.TagTrue or Tag.TagFalse ? Tag.TagBool : t;
            byte first = Uniform(w[c.Starts[from]]);
            bool uniform = true;
            for (int i = from + 1; i < to && uniform; i++)
                uniform = Uniform(w[c.Starts[i]]) == first;
            if (!uniform)
                w[tagAt] = Tag.TagMixed;
            else if (first == Tag.TagBool)
            {
                w[tagAt] = Tag.TagBool;
                for (int i = from; i < to; i++)
                    w[c.Starts[i]] = w[c.Starts[i]] == Tag.TagTrue ? (byte)1 : (byte)0;
            }
            else
            {
                w[tagAt] = first;
                w.RemoveBytes(c.Starts, from, to);
            }
            c.Starts.RemoveRange(from, to - from);
        }
        if (pooled != null) c.Return(pooled);
        c.Depth--;
    }

    private void WritePrimitive(Ctx c, ushort fid, object v, PrimitivePlan p, bool element)
    {
        switch (p.Kind)
        {
            case Kind.String: Carrier(c, fid, Tag.TagStr, element); c.W.Str((string)v); break;
            case Kind.Char: Carrier(c, fid, Tag.TagStr, element); c.W.Str(((char)v).ToString()); break;
            case Kind.Bool: Carrier(c, fid, (bool)v ? Tag.TagTrue : Tag.TagFalse, element); break;
            case Kind.Int64: Carrier(c, fid, Tag.TagInt, element); c.W.Int(Convert.ToInt64(v, CultureInfo.InvariantCulture)); break;
            case Kind.UInt64: WriteU64(c, fid, (ulong)v, element); break;
            case Kind.Double: WriteF64(c, fid, (double)v, element); break;
            case Kind.Single: WriteF32(c, fid, (float)v, element); break;
            case Kind.Decimal: Carrier(c, fid, Tag.TagFloat, element); c.W.Float(DecimalValue((decimal)v)); break;
            case Kind.Guid: WriteGuid(c, fid, (Guid)v, element); break;
            case Kind.Enum: WriteFixed(c, fid, EnumCode(p.Type, EnumKey(v)), element); break;
        }
    }

    /// <summary>An enum value as long (unchecked for ulong-based enums, as the compiled writers convert).</summary>
    private static long EnumKey(object v) =>
        Type.GetTypeCode(Enum.GetUnderlyingType(v.GetType())) == TypeCode.UInt64
            ? unchecked((long)Convert.ToUInt64(v, CultureInfo.InvariantCulture))
            : Convert.ToInt64(v, CultureInfo.InvariantCulture);

    private static void WriteU64(Ctx c, ushort fid, ulong v, bool element)
    {
        if (v <= long.MaxValue)
        {
            Carrier(c, fid, Tag.TagInt, element);
            c.W.Int((long)v);
        }
        else
        {
            Carrier(c, fid, Tag.TagU64, element);
            c.W.UInt64(v);
        }
    }

    private void WriteF64(Ctx c, ushort fid, double v, bool element)
    {
        if (double.IsFinite(v))
        {
            Carrier(c, fid, Tag.TagFloat, element);
            c.W.Float(v);
        }
        else
            WriteNonFinite(c, fid, v, element);
    }

    private void WriteF32(Ctx c, ushort fid, float v, bool element)
    {
        if (!float.IsFinite(v))
        {
            WriteNonFinite(c, fid, v, element);
            return;
        }
        // Newtonsoft writes float.ToString("R"); the import parses that text as a double.
        Span<char> b = stackalloc char[32];
        v.TryFormat(b, out int n, "R", CultureInfo.InvariantCulture);
        Carrier(c, fid, Tag.TagFloat, element);
        c.W.Float(double.Parse(b[..n], NumberStyles.Float, CultureInfo.InvariantCulture));
    }

    private void WriteNonFinite(Ctx c, ushort fid, double v, bool element)
    {
        if (_floats == FloatFormatHandling.DefaultValue)
        {
            Carrier(c, fid, Tag.TagFloat, element);
            c.W.Float(0.0);
            return;
        }
        Carrier(c, fid, Tag.TagStr, element);
        c.W.Str(double.IsNaN(v) ? "NaN" : v > 0 ? "Infinity" : "-Infinity");
    }

    private static double DecimalValue(decimal v)
    {
        // Newtonsoft writes decimal.ToString(); the import parses that text as a double.
        Span<char> b = stackalloc char[40];
        v.TryFormat(b, out int n, default, CultureInfo.InvariantCulture);
        return double.Parse(b[..n], NumberStyles.Float, CultureInfo.InvariantCulture);
    }

    private static void WriteGuid(Ctx c, ushort fid, Guid v, bool element)
    {
        Span<char> b = stackalloc char[36];
        v.TryFormat(b, out int n, "D");
        Carrier(c, fid, Tag.TagStr, element);
        c.W.Str(b[..n]);
    }

    private static void WriteFixed(Ctx c, ushort fid, Fixed f, bool element)
    {
        Carrier(c, fid, f.Tag, element);
        switch (f.Tag)
        {
            case Tag.TagStr: c.W.Str(f.Str!); break;
            case Tag.TagInt: c.W.Int(f.Int); break;
            case Tag.TagU64: c.W.UInt64((ulong)f.Int); break;
        }
    }

    /// <summary>How the settings write enum value <paramref name="key"/> of <paramref name="type"/>, from Newtonsoft itself.</summary>
    private Fixed EnumCode(Type type, long key)
    {
        var table = _enums.GetOrAdd(type, _ => new ConcurrentDictionary<long, Fixed>());
        if (table.TryGetValue(key, out var f))
            return f;
        object value = Enum.ToObject(type, key);
        using var doc = JsonDocument.Parse(SerializeFallback(value, null));
        var e = doc.RootElement;
        f = e.ValueKind switch
        {
            JsonValueKind.String => new Fixed(Tag.TagStr, 0, e.GetString()),
            JsonValueKind.Number when e.TryGetInt64(out long l) => new Fixed(Tag.TagInt, l, null),
            JsonValueKind.Number => new Fixed(Tag.TagU64, (long)e.GetUInt64(), null),
            _ => throw new JsonSerializationException($"Enum {type} serializes as {e.ValueKind}."),
        };
        return table.GetOrAdd(key, f);
    }

    private string SerializeFallback(object v, JsonConverter? converter)
    {
        var s = _serializer.Value!;
        var sw = new StringWriter(new StringBuilder(256), CultureInfo.InvariantCulture);
        using (var jw = new JsonTextWriter(sw) { Formatting = Formatting.None })
        {
            if (converter != null) converter.WriteJson(jw, v, s);
            else s.Serialize(jw, v);
        }
        return sw.ToString();
    }

    private void WriteFallback(Ctx c, ushort fid, object v, JsonConverter? converter, bool element)
    {
        using var doc = JsonDocument.Parse(SerializeFallback(v, converter));
        var e = doc.RootElement;
        Carrier(c, fid, Tag.JsonTag(e), element);
        c.W.JsonPayload(e, fid, c.Paths);
    }

    /// <summary>A whole document Newtonsoft writes itself.</summary>
    private static void WriteJsonDocument(Ctx c, JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object)
            throw new FormatException("A document must serialize as a JSON object.");
        string? id = null;
        var members = new List<(ushort Fid, JsonElement Value)>();
        var at = new Dictionary<string, int>(StringComparer.Ordinal);
        foreach (var p in root.EnumerateObject())
        {
            if (p.Name == "id")
            {
                id = p.Value.ValueKind == JsonValueKind.String ? p.Value.GetString() : throw new FormatException("The document `id` must be a string.");
                continue;
            }
            ushort fid = c.Paths.Id(TqbfFieldRegistry.NoParent, p.Name);
            if (at.TryGetValue(p.Name, out int slot)) members[slot] = (fid, p.Value);
            else { at[p.Name] = members.Count; members.Add((fid, p.Value)); }
        }
        c.W.Begin(id ?? throw new FormatException("The document has no `id`."));
        members.Sort((a, b) => c.Paths.Name(a.Fid).SequenceCompareTo(c.Paths.Name(b.Fid)));
        c.W.Varint((ulong)members.Count);
        foreach (var (fid, v) in members)
        {
            c.W.Key(fid, Tag.JsonTag(v));
            c.W.JsonPayload(v, fid, c.Paths);
        }
    }

    /// <summary>Typed member writers (the compiled member writers call these).</summary>
    private static class Writers
    {
        public static bool Null(Ctx c, ushort fid, bool ignoreNull)
        {
            if (ignoreNull) return false;
            c.W.Key(fid, Tag.TagNull);
            return true;
        }

        public static bool Str(Ctx c, ushort fid, string? v, bool ignoreNull)
        {
            if (v is null) return Null(c, fid, ignoreNull);
            c.W.Key(fid, Tag.TagStr);
            c.W.Str(v);
            return true;
        }

        public static bool Bool(Ctx c, ushort fid, bool v)
        {
            c.W.Key(fid, v ? Tag.TagTrue : Tag.TagFalse);
            return true;
        }

        public static bool NBool(Ctx c, ushort fid, bool? v, bool ignoreNull) => v.HasValue ? Bool(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool I64(Ctx c, ushort fid, long v)
        {
            c.W.Key(fid, Tag.TagInt);
            c.W.Int(v);
            return true;
        }

        public static bool NI64(Ctx c, ushort fid, long? v, bool ignoreNull) => v.HasValue ? I64(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool U64(Ctx c, ushort fid, ulong v)
        {
            WriteU64(c, fid, v, element: false);
            return true;
        }

        public static bool NU64(Ctx c, ushort fid, ulong? v, bool ignoreNull) => v.HasValue ? U64(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool F64(Ctx c, ushort fid, double v)
        {
            c.Enc.WriteF64(c, fid, v, element: false);
            return true;
        }

        public static bool NF64(Ctx c, ushort fid, double? v, bool ignoreNull) => v.HasValue ? F64(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool F32(Ctx c, ushort fid, float v)
        {
            c.Enc.WriteF32(c, fid, v, element: false);
            return true;
        }

        public static bool NF32(Ctx c, ushort fid, float? v, bool ignoreNull) => v.HasValue ? F32(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool Dec(Ctx c, ushort fid, decimal v)
        {
            c.W.Key(fid, Tag.TagFloat);
            c.W.Float(DecimalValue(v));
            return true;
        }

        public static bool NDec(Ctx c, ushort fid, decimal? v, bool ignoreNull) => v.HasValue ? Dec(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool Guid(Ctx c, ushort fid, Guid v)
        {
            WriteGuid(c, fid, v, element: false);
            return true;
        }

        public static bool NGuid(Ctx c, ushort fid, Guid? v, bool ignoreNull) => v.HasValue ? Guid(c, fid, v.Value) : Null(c, fid, ignoreNull);

        public static bool Enum(Ctx c, ushort fid, long v, Type type)
        {
            WriteFixed(c, fid, c.Enc.EnumCode(type, v), element: false);
            return true;
        }
    }
}
