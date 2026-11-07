using System.Runtime.InteropServices;
using System.Text;
using Torque.Http.Exceptions;

namespace Torque.Http.Tqbf;

/// <summary>
/// The native segment builder (ffi/torque-build, <c>torque_build.dll</c> / <c>libtorque_build.so</c>):
/// the same Rust code the server indexes with. Errors are read back with <c>tqb_last_error</c> on
/// the thread that made the failing call.
/// </summary>
internal static unsafe partial class TqbfNative
{
    private const string Lib = "torque_build";

    [StructLayout(LayoutKind.Sequential)]
    internal struct Options
    {
        public int DocsLevel;
        public uint Threads;
        public ulong PieceBytes;
        public uint Spill;
        public byte* SpillDir;
        public nuint SpillDirLen;
    }

    [LibraryImport(Lib)] internal static partial nint tqb_new(byte* schema, nuint schemaLen, Options* opts);
    [LibraryImport(Lib)] internal static partial void tqb_free(nint b);
    [LibraryImport(Lib)] internal static partial int tqb_path_id(nint b, ushort parent, byte* name, nuint nameLen, ushort* id);
    [LibraryImport(Lib)] internal static partial nint tqb_run_begin(nint b, ulong key);
    [LibraryImport(Lib)] internal static partial int tqb_run_add(nint r, byte* records, nuint len);
    [LibraryImport(Lib)] internal static partial ulong tqb_run_rejected(nint r);
    [LibraryImport(Lib)] internal static partial int tqb_run_end(nint r, byte** json, nuint* jsonLen);
    [LibraryImport(Lib)] internal static partial void tqb_run_free(nint r);
    [LibraryImport(Lib)] internal static partial int tqb_finish(nint b, byte* path, nuint pathLen, byte** json, nuint* jsonLen);
    [LibraryImport(Lib)] internal static partial void tqb_bytes_free(byte* p, nuint len);
    [LibraryImport(Lib)] internal static partial nuint tqb_last_error(byte* buf, nuint cap);

    /// <summary>The failing call's error, as an exception.</summary>
    internal static TqbfBuildException Error(string what)
    {
        nuint n = tqb_last_error(null, 0);
        var b = new byte[(int)n];
        fixed (byte* p = b)
            tqb_last_error(p, n);
        return new TqbfBuildException($"{what}: {Encoding.UTF8.GetString(b)}");
    }

    /// <summary>A buffer handed out by the library, as text; the buffer is released.</summary>
    internal static string Take(byte* p, nuint len)
    {
        try
        {
            return Encoding.UTF8.GetString(p, (int)len);
        }
        finally
        {
            tqb_bytes_free(p, len);
        }
    }
}

/// <summary>A segment build failed (in the native builder or in what it was given).</summary>
public sealed class TqbfBuildException : TorqueException
{
    public TqbfBuildException(string message) : base(message) { }
}
