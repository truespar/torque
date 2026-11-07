using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace Torque.Http.Auth;

/// <summary>
/// Generates scoped search API keys from a parent key.
/// Scoped keys embed filter_by, limit_hits, and other restrictions via HMAC-SHA256.
/// </summary>
public static class ScopedKeyHelper
{
    /// <summary>
    /// Generate a scoped search key from a parent key value and parameters.
    /// The scoped key embeds the parameters and is signed with HMAC-SHA256.
    /// </summary>
    /// <param name="parentKeyValue">The parent API key value to derive the scoped key from.</param>
    /// <param name="parameters">Search parameters to embed (e.g. filter_by, limit_hits).</param>
    /// <returns>A base64-encoded scoped key string.</returns>
    public static string GenerateScopedKey(string parentKeyValue, Dictionary<string, object> parameters)
    {
        var paramsJson = JsonSerializer.Serialize(parameters);
        var keyByteCount = Encoding.UTF8.GetByteCount(parentKeyValue);
        var paramsByteCount = Encoding.UTF8.GetByteCount(paramsJson);

        // Use stackalloc for small keys/params, otherwise rent
        Span<byte> keyBytes = keyByteCount <= 256 ? stackalloc byte[keyByteCount] : new byte[keyByteCount];
        Span<byte> paramsBytes = paramsByteCount <= 1024 ? stackalloc byte[paramsByteCount] : new byte[paramsByteCount];

        Encoding.UTF8.GetBytes(parentKeyValue, keyBytes);
        Encoding.UTF8.GetBytes(paramsJson, paramsBytes);

        Span<byte> digest = stackalloc byte[32]; // SHA-256 = 32 bytes
        HMACSHA256.HashData(keyBytes, paramsBytes, digest);

        var digestB64 = Convert.ToBase64String(digest);
        var prefix = parentKeyValue[..Math.Min(4, parentKeyValue.Length)];
        var raw = $"{digestB64}{prefix}{paramsJson}";
        return Convert.ToBase64String(Encoding.UTF8.GetBytes(raw));
    }
}
