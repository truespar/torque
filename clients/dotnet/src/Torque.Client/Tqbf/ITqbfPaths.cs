namespace Torque.Http.Tqbf;

/// <summary>
/// Ids of document paths (spec §5.1): per (parent id, key name) one id, the same for every record
/// of a file or segment build.
/// </summary>
internal interface ITqbfPaths
{
    /// <summary>The id of key <paramref name="name"/> under <paramref name="parent"/>, added when new.</summary>
    ushort Id(ushort parent, string name);

    /// <summary>The UTF-8 key name of an id handed out by <see cref="Id"/>.</summary>
    ReadOnlySpan<byte> Name(ushort id);
}
