using System.Buffers.Binary;
using System.Text;
using System.Text.Json;

namespace Torque.Http.Tqbf;

/// <summary>
/// Writes one TQBF record (spec §5.2) at a time, straight from typed values: an id, then the
/// top-level object. The caller writes every object's entries in key byte order and each value's
/// tag with its key (an array's element tag after its length). For object encoders that know their
/// shapes ahead (Torque.Http.Newtonsoft); <see cref="TqbfDocument"/> builds records from events.
/// </summary>
internal sealed class TqbfRecordWriter
{
    // Rust record tags (src/tqbf/record.rs).
    public const byte TagInt = 0, TagFloat = 1, TagStr = 2, TagFalse = 3, TagTrue = 4, TagArray = 5,
        TagObject = 6, TagNull = 7, TagU64 = 8, TagBool = 9, TagMixed = 15;

    private byte[] _buf = new byte[16 * 1024];
    private int _pos;

    /// <summary>The record written since <see cref="Begin"/>.</summary>
    public ReadOnlySpan<byte> Written => _buf.AsSpan(0, _pos);

    /// <summary>Bytes written so far.</summary>
    public int Position => _pos;

    public byte this[int at]
    {
        get => _buf[at];
        set => _buf[at] = value;
    }

    /// <summary>Remove the byte at each of <paramref name="at"/> (ascending positions), moving what
    /// follows down: an array written with a tag per element made uniform.</summary>
    public void RemoveBytes(List<int> at, int from, int to)
    {
        if (from >= to) return;
        int write = at[from];
        for (int i = from; i < to; i++)
        {
            int start = at[i] + 1;
            int end = i + 1 < to ? at[i + 1] : _pos;
            int n = end - start;
            Buffer.BlockCopy(_buf, start, _buf, write, n);
            write += n;
        }
        _pos = write;
    }

    /// <summary>Start a record: its external id, then the caller writes the top-level object.</summary>
    public void Begin(string id)
    {
        _pos = 0;
        Str(id);
    }

    private void Ensure(int n)
    {
        if (_pos + n > _buf.Length)
            Array.Resize(ref _buf, Math.Max(_buf.Length * 2, _pos + n));
    }

    /// <summary>Start an object; returns where its entry count goes.</summary>
    public int BeginObject()
    {
        Ensure(1);
        return _pos++;
    }

    /// <summary>Close the object started at <paramref name="at"/> with <paramref name="count"/> entries.</summary>
    public void EndObject(int at, int count)
    {
        if (count < 0x80)
        {
            _buf[at] = (byte)count;
            return;
        }
        Span<byte> v = stackalloc byte[10];
        int n = VarintTo(v, (ulong)count);
        Ensure(n - 1);
        Buffer.BlockCopy(_buf, at + 1, _buf, at + n, _pos - at - 1);
        v[..n].CopyTo(_buf.AsSpan(at));
        _pos += n - 1;
    }

    /// <summary>An object entry: the key's path id and the tag of its value.</summary>
    public void Key(ushort fid, byte tag) => Varint(((ulong)fid << 4) | tag);

    public void Int(long v) => Varint((ulong)((v << 1) ^ (v >> 63)));

    public void UInt64(ulong v) => Varint(v);

    public void Float(double v)
    {
        Ensure(8);
        BinaryPrimitives.WriteInt64LittleEndian(_buf.AsSpan(_pos), BitConverter.DoubleToInt64Bits(v));
        _pos += 8;
    }

    public void Str(string s) => Str(s.AsSpan());

    public void Str(ReadOnlySpan<char> s)
    {
        int max = Encoding.UTF8.GetMaxByteCount(s.Length);
        Ensure(10 + max);
        // Short strings: the length fits one byte whatever the encoding gives; else count exactly.
        int lenBytes = max < 0x80 ? 1 : VarintLen((ulong)Encoding.UTF8.GetByteCount(s));
        int n = Encoding.UTF8.GetBytes(s, _buf.AsSpan(_pos + lenBytes));
        VarintTo(_buf.AsSpan(_pos), (ulong)n);
        _pos += lenBytes + n;
    }

    public void Utf8(ReadOnlySpan<byte> s)
    {
        Varint((ulong)s.Length);
        Ensure(s.Length);
        s.CopyTo(_buf.AsSpan(_pos));
        _pos += s.Length;
    }

    public void Byte(byte b)
    {
        Ensure(1);
        _buf[_pos++] = b;
    }

    public void Varint(ulong v)
    {
        Ensure(10);
        _pos += VarintTo(_buf.AsSpan(_pos), v);
    }

    private static int VarintTo(Span<byte> out_, ulong v)
    {
        int i = 0;
        while (v >= 0x80)
        {
            out_[i++] = (byte)(v | 0x80);
            v >>= 7;
        }
        out_[i++] = (byte)v;
        return i;
    }

    private static int VarintLen(ulong v)
    {
        int n = 1;
        while (v >= 0x80)
        {
            v >>= 7;
            n++;
        }
        return n;
    }

    // -- JSON values, read as Torque's JSON import reads them ----------------------------------------

    /// <summary>The tag of a JSON value (numbers as serde_json reads them).</summary>
    public static byte JsonTag(JsonElement e) => e.ValueKind switch
    {
        JsonValueKind.Object => TagObject,
        JsonValueKind.Array => TagArray,
        JsonValueKind.String => TagStr,
        JsonValueKind.True => TagTrue,
        JsonValueKind.False => TagFalse,
        JsonValueKind.Null => TagNull,
        _ => NumberTag(e),
    };

    private static byte ElementTag(JsonElement e) => e.ValueKind is JsonValueKind.True or JsonValueKind.False ? TagBool : JsonTag(e);

    // serde_json's reading of a number: integer text as i64, else u64, else f64; "-0" and integers
    // below i64::MIN as f64; text with a fraction or exponent as f64 (correctly rounded).
    private static byte NumberTag(JsonElement e)
    {
        string text = e.GetRawText();
        bool integer = text.IndexOfAny(['.', 'e', 'E']) < 0;
        if (integer && text == "-0") return TagFloat;
        if (integer && e.TryGetInt64(out _)) return TagInt;
        if (integer && e.TryGetUInt64(out _)) return TagU64;
        return TagFloat;
    }

    /// <summary>The payload of a JSON value whose tag (<see cref="JsonTag"/>) is written; objects
    /// get their keys' ids under <paramref name="fid"/>, the key the value hangs under.</summary>
    public void JsonPayload(JsonElement e, ushort fid, ITqbfPaths paths)
    {
        switch (e.ValueKind)
        {
            case JsonValueKind.String:
                Str(e.GetString()!);
                break;
            case JsonValueKind.Number:
                switch (NumberTag(e))
                {
                    case TagInt: Int(e.GetInt64()); break;
                    case TagU64: UInt64(e.GetUInt64()); break;
                    default: Float(e.GetRawText() == "-0" ? -0.0 : e.GetDouble()); break;
                }
                break;
            case JsonValueKind.Array:
            {
                int n = e.GetArrayLength();
                Varint((ulong)n);
                if (n == 0) break;
                byte t = 0;
                bool uniform = true, first = true;
                foreach (var x in e.EnumerateArray())
                {
                    byte xt = ElementTag(x);
                    if (first) { t = xt; first = false; }
                    else uniform &= xt == t;
                }
                Byte(uniform ? t : TagMixed);
                foreach (var x in e.EnumerateArray())
                {
                    if (uniform && t == TagBool) { Byte(x.ValueKind == JsonValueKind.True ? (byte)1 : (byte)0); continue; }
                    if (!uniform) Byte(JsonTag(x));
                    JsonPayload(x, fid, paths);
                }
                break;
            }
            case JsonValueKind.Object:
            {
                // Last value wins for a repeated key; entries in key byte order.
                var members = new List<(ushort Fid, JsonElement Value)>();
                var at = new Dictionary<string, int>(StringComparer.Ordinal);
                foreach (var p in e.EnumerateObject())
                {
                    ushort id = paths.Id(fid, p.Name);
                    if (at.TryGetValue(p.Name, out int slot)) members[slot] = (id, p.Value);
                    else { at[p.Name] = members.Count; members.Add((id, p.Value)); }
                }
                members.Sort((a, b) => paths.Name(a.Fid).SequenceCompareTo(paths.Name(b.Fid)));
                Varint((ulong)members.Count);
                foreach (var (id, v) in members)
                {
                    Key(id, JsonTag(v));
                    JsonPayload(v, id, paths);
                }
                break;
            }
        }
    }
}
