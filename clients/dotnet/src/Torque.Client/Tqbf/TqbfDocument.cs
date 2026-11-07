using System.Buffers;
using System.Text;
using System.Text.Json;

namespace Torque.Http.Tqbf;

/// <summary>
/// Builds one document for a <see cref="TqbfDocsWriter"/> from value events (start object,
/// property, value, ...), the shape of a JSON writer, and encodes it as a TQBF record (spec §5.2):
/// keys in UTF-8 byte order, integers and floats kept apart, floats as their exact IEEE-754 bits.
/// The values are JSON values: an adapter for an object serializer emits what that serializer
/// would write (a float written as "1.5" is the double 1.5).
/// </summary>
public sealed class TqbfDocument
{
    private enum Kind : byte { Null, False, True, Int, UInt, Float, String, Array, Object }

    // Rust record tags (src/tqbf/record.rs).
    private const byte TagInt = 0, TagFloat = 1, TagStr = 2, TagFalse = 3, TagTrue = 4, TagArray = 5,
        TagObject = 6, TagNull = 7, TagU64 = 8, TagBool = 9, TagMixed = 15;

    private Kind[] _kind = new Kind[256];
    private long[] _int = new long[256];
    private double[] _float = new double[256];
    private string?[] _str = new string?[256];
    private string?[] _key = new string?[256];
    private int[] _first = new int[256];
    private int[] _last = new int[256];
    private int[] _next = new int[256];
    private int[] _count = new int[256];
    private int _n;
    private readonly Stack<int> _open = new();
    private string? _pendingKey;
    private readonly ArrayBufferWriter<byte> _out = new(4096);
    private readonly ITqbfPaths _registry;

    internal TqbfDocument(ITqbfPaths registry) => _registry = registry;

    internal void Reset()
    {
        _n = 0;
        _open.Clear();
        _pendingKey = null;
        Array.Clear(_str, 0, _str.Length);
        Array.Clear(_key, 0, _key.Length);
        _open.Push(NewNode(Kind.Object, null));
    }

    private int NewNode(Kind kind, string? key)
    {
        if (_n == _kind.Length)
        {
            int size = _n * 2;
            Array.Resize(ref _kind, size);
            Array.Resize(ref _int, size);
            Array.Resize(ref _float, size);
            Array.Resize(ref _str, size);
            Array.Resize(ref _key, size);
            Array.Resize(ref _first, size);
            Array.Resize(ref _last, size);
            Array.Resize(ref _next, size);
            Array.Resize(ref _count, size);
        }
        int i = _n++;
        _kind[i] = kind;
        _key[i] = key;
        _first[i] = _last[i] = _next[i] = -1;
        _count[i] = 0;
        return i;
    }

    private int Add(Kind kind)
    {
        if (_open.Count == 0)
            throw new InvalidOperationException("The document is complete; start the next one.");
        int parent = _open.Peek();
        string? key = null;
        if (_kind[parent] == Kind.Object)
        {
            key = _pendingKey ?? throw new InvalidOperationException("A value inside an object needs a property name first.");
            _pendingKey = null;
        }
        int node = NewNode(kind, key);
        if (_last[parent] < 0) _first[parent] = node; else _next[_last[parent]] = node;
        _last[parent] = node;
        _count[parent]++;
        return node;
    }

    /// <summary>The name of the next value in the current object.</summary>
    public TqbfDocument Property(string name)
    {
        if (_open.Count == 0 || _kind[_open.Peek()] != Kind.Object)
            throw new InvalidOperationException("Property names belong inside an object.");
        _pendingKey = name ?? throw new ArgumentNullException(nameof(name));
        return this;
    }

    public TqbfDocument StartObject() { _open.Push(Add(Kind.Object)); return this; }

    public TqbfDocument EndObject()
    {
        if (_open.Count < 2 || _kind[_open.Peek()] != Kind.Object) throw new InvalidOperationException("No object to end.");
        _open.Pop();
        return this;
    }

    public TqbfDocument StartArray() { _open.Push(Add(Kind.Array)); return this; }

    public TqbfDocument EndArray()
    {
        if (_open.Count < 2 || _kind[_open.Peek()] != Kind.Array) throw new InvalidOperationException("No array to end.");
        _open.Pop();
        return this;
    }

    public TqbfDocument Null() { Add(Kind.Null); return this; }

    public TqbfDocument Bool(bool value) { Add(value ? Kind.True : Kind.False); return this; }

    public TqbfDocument Int(long value)
    {
        int n = Add(Kind.Int); // before indexing: Add may grow the arrays
        _int[n] = value;
        return this;
    }

    /// <summary>An unsigned integer; values that fit a long are stored as one.</summary>
    public TqbfDocument UInt(ulong value)
    {
        if (value <= long.MaxValue) return Int((long)value);
        int n = Add(Kind.UInt);
        _int[n] = unchecked((long)value);
        return this;
    }

    public TqbfDocument Float(double value)
    {
        if (!double.IsFinite(value)) throw new ArgumentOutOfRangeException(nameof(value), "JSON has no NaN or infinity.");
        int n = Add(Kind.Float);
        _float[n] = value;
        return this;
    }

    public TqbfDocument String(string value)
    {
        ArgumentNullException.ThrowIfNull(value);
        int n = Add(Kind.String);
        _str[n] = value;
        return this;
    }

    /// <summary>Add a whole document given as UTF-8 JSON text, read as Torque's JSON import reads it.</summary>
    internal void ReadJson(ReadOnlySpan<byte> utf8Json)
    {
        var reader = new Utf8JsonReader(utf8Json, new JsonReaderOptions { MaxDepth = 128 });
        if (!reader.Read() || reader.TokenType != JsonTokenType.StartObject)
            throw new FormatException("A document must be a JSON object.");
        while (reader.Read())
        {
            switch (reader.TokenType)
            {
                case JsonTokenType.PropertyName: Property(reader.GetString()!); break;
                case JsonTokenType.StartObject: StartObject(); break;
                case JsonTokenType.EndObject:
                    if (_open.Count == 1) { _open.Pop(); break; }
                    EndObject();
                    break;
                case JsonTokenType.StartArray: StartArray(); break;
                case JsonTokenType.EndArray: EndArray(); break;
                case JsonTokenType.String: String(reader.GetString()!); break;
                case JsonTokenType.True: Bool(true); break;
                case JsonTokenType.False: Bool(false); break;
                case JsonTokenType.Null: Null(); break;
                case JsonTokenType.Number: Number(ref reader); break;
            }
        }
        if (_open.Count != 0) throw new FormatException("Incomplete JSON document.");
    }

    // serde_json's reading of a number: integer text as i64, else u64, else f64; "-0" and integers
    // below i64::MIN as f64; text with a fraction or exponent as f64 (correctly rounded).
    private void Number(ref Utf8JsonReader reader)
    {
        ReadOnlySpan<byte> text = reader.HasValueSequence ? reader.ValueSequence.ToArray() : reader.ValueSpan;
        bool integer = text.IndexOfAny((byte)'.', (byte)'e', (byte)'E') < 0;
        if (integer && text.SequenceEqual("-0"u8)) Float(-0.0);
        else if (integer && reader.TryGetInt64(out long l)) Int(l);
        else if (integer && reader.TryGetUInt64(out ulong u)) UInt(u);
        else Float(reader.GetDouble());
    }

    /// <summary>Encode the finished document; returns the record bytes (valid until the next call).</summary>
    internal ReadOnlySpan<byte> Encode()
    {
        if (_open.Count > 1) throw new InvalidOperationException("The document has unclosed objects or arrays.");
        _out.Clear();
        string? id = null;
        for (int c = _first[0]; c >= 0; c = _next[c])
        {
            if (_key[c] == "id")
                id = _kind[c] == Kind.String ? _str[c] : throw new FormatException("The document `id` must be a string.");
        }
        if (id is null) throw new FormatException("The document has no `id`.");
        var idBytes = Encoding.UTF8.GetBytes(id);
        Varint((ulong)idBytes.Length);
        Bytes(idBytes);
        EncodeObject(0, TqbfFieldRegistry.NoParent, topLevel: true);
        return _out.WrittenSpan;
    }

    private void EncodeObject(int node, ushort parent, bool topLevel)
    {
        // Last value wins for a repeated key, as in serde_json; entries in key byte order.
        var members = new List<(ushort Fid, int Node)>(_count[node]);
        var at = new Dictionary<string, int>(StringComparer.Ordinal);
        for (int c = _first[node]; c >= 0; c = _next[c])
        {
            string key = _key[c]!;
            if (topLevel && key == "id") continue;
            ushort fid = _registry.Id(parent, key);
            if (at.TryGetValue(key, out int slot)) members[slot] = (fid, c);
            else { at[key] = members.Count; members.Add((fid, c)); }
        }
        members.Sort((a, b) => _registry.Name(a.Fid).SequenceCompareTo(_registry.Name(b.Fid)));
        Varint((ulong)members.Count);
        foreach (var (fid, c) in members)
        {
            Varint(((ulong)fid << 4) | ScalarTag(c));
            Payload(c, fid);
        }
    }

    private byte ScalarTag(int n) => _kind[n] switch
    {
        Kind.Null => TagNull,
        Kind.False => TagFalse,
        Kind.True => TagTrue,
        Kind.Int => TagInt,
        Kind.UInt => TagU64,
        Kind.Float => TagFloat,
        Kind.String => TagStr,
        Kind.Array => TagArray,
        _ => TagObject,
    };

    private byte ElementTag(int n) => _kind[n] is Kind.True or Kind.False ? TagBool : ScalarTag(n);

    private void Payload(int n, ushort fid)
    {
        switch (_kind[n])
        {
            case Kind.Int: Varint(ZigZag(_int[n])); break;
            case Kind.UInt: Varint(unchecked((ulong)_int[n])); break;
            case Kind.Float:
            {
                Span<byte> b = stackalloc byte[8];
                System.Buffers.Binary.BinaryPrimitives.WriteInt64LittleEndian(b, BitConverter.DoubleToInt64Bits(_float[n]));
                Bytes(b);
                break;
            }
            case Kind.String:
            {
                string s = _str[n]!;
                int len = Encoding.UTF8.GetByteCount(s);
                Varint((ulong)len);
                Encoding.UTF8.GetBytes(s, _out.GetSpan(len));
                _out.Advance(len);
                break;
            }
            case Kind.Array:
            {
                Varint((ulong)_count[n]);
                if (_count[n] == 0) break;
                byte t = ElementTag(_first[n]);
                bool uniform = true;
                for (int e = _first[n]; e >= 0; e = _next[e]) uniform &= ElementTag(e) == t;
                if (uniform)
                {
                    One(t);
                    for (int e = _first[n]; e >= 0; e = _next[e])
                    {
                        if (t == TagBool) One(_kind[e] == Kind.True ? (byte)1 : (byte)0);
                        else Payload(e, fid);
                    }
                }
                else
                {
                    One(TagMixed);
                    for (int e = _first[n]; e >= 0; e = _next[e])
                    {
                        One(ScalarTag(e));
                        Payload(e, fid);
                    }
                }
                break;
            }
            case Kind.Object: EncodeObject(n, fid, topLevel: false); break;
        }
    }

    private static ulong ZigZag(long v) => (ulong)((v << 1) ^ (v >> 63));

    private void Varint(ulong v)
    {
        var span = _out.GetSpan(10);
        int i = 0;
        while (v >= 0x80)
        {
            span[i++] = (byte)(v | 0x80);
            v >>= 7;
        }
        span[i++] = (byte)v;
        _out.Advance(i);
    }

    private void One(byte b)
    {
        _out.GetSpan(1)[0] = b;
        _out.Advance(1);
    }

    private void Bytes(ReadOnlySpan<byte> b)
    {
        b.CopyTo(_out.GetSpan(b.Length));
        _out.Advance(b.Length);
    }
}
