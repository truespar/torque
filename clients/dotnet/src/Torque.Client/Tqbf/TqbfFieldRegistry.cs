using System.Buffers.Binary;
using System.Text;

namespace Torque.Http.Tqbf;

/// <summary>
/// The FIELDS section (spec §5.1): every JSON path of the documents gets an id, per (parent id,
/// key name), in first-seen order. Entries are written as stored-only; the builder applies the
/// schema's flags.
/// </summary>
internal sealed class TqbfFieldRegistry : ITqbfPaths
{
    public const ushort NoParent = 0xFFFF;
    private const ushort StoredOnly = 1 << 5;

    private readonly Dictionary<(ushort Parent, string Name), ushort> _ids = new();
    private readonly List<(ushort Parent, byte[] Name)> _entries = new();

    public int Count => _entries.Count;

    public ushort Id(ushort parent, string name)
    {
        if (_ids.TryGetValue((parent, name), out ushort id))
            return id;
        if (_entries.Count >= ushort.MaxValue - 1)
            throw new InvalidOperationException($"The documents have more than {ushort.MaxValue - 1} distinct paths.");
        byte[] bytes = Encoding.UTF8.GetBytes(name);
        if (bytes.Length > ushort.MaxValue)
            throw new ArgumentException($"Key names are at most {ushort.MaxValue} bytes.", nameof(name));
        id = (ushort)_entries.Count;
        _entries.Add((parent, bytes));
        _ids[(parent, name)] = id;
        return id;
    }

    public ReadOnlySpan<byte> Name(ushort id) => _entries[id].Name;

    /// <summary>u32 count; per entry u16 parent, u16 flags, u8 type (0), u16 name length, name.</summary>
    public byte[] Encode()
    {
        var out_ = new List<byte>(4 + _entries.Count * 24);
        Span<byte> b = stackalloc byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(b, (uint)_entries.Count);
        out_.AddRange(b.ToArray());
        foreach (var (parent, name) in _entries)
        {
            BinaryPrimitives.WriteUInt16LittleEndian(b, parent);
            out_.Add(b[0]); out_.Add(b[1]);
            BinaryPrimitives.WriteUInt16LittleEndian(b, StoredOnly);
            out_.Add(b[0]); out_.Add(b[1]);
            out_.Add(0);
            BinaryPrimitives.WriteUInt16LittleEndian(b, (ushort)name.Length);
            out_.Add(b[0]); out_.Add(b[1]);
            out_.AddRange(name);
        }
        return out_.ToArray();
    }
}
