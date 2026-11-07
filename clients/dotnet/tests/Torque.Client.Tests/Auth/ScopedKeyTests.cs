using System.Text;
using Torque.Http.Auth;

namespace Torque.Http.Tests.Auth;

public class ScopedKeyTests
{
    [Fact]
    public void GenerateScopedKey_ReturnsBase64()
    {
        var key = ScopedKeyHelper.GenerateScopedKey("test-key-1234", new Dictionary<string, object>
        {
            ["filter_by"] = "company_id:123",
        });

        Assert.NotEmpty(key);
        // Should be valid Base64
        var decoded = Convert.FromBase64String(key);
        Assert.True(decoded.Length > 0);
    }

    [Fact]
    public void GenerateScopedKey_ContainsPrefix()
    {
        var parentKey = "abcdef123456";
        var key = ScopedKeyHelper.GenerateScopedKey(parentKey, new Dictionary<string, object>
        {
            ["filter_by"] = "user_id:42",
        });

        // Decode and check that the prefix is embedded
        var decoded = Encoding.UTF8.GetString(Convert.FromBase64String(key));
        Assert.Contains("abcd", decoded); // First 4 chars of parent key
    }

    [Fact]
    public void GenerateScopedKey_ContainsParameters()
    {
        var key = ScopedKeyHelper.GenerateScopedKey("mykey1234", new Dictionary<string, object>
        {
            ["filter_by"] = "org_id:999",
            ["limit_hits"] = 100,
        });

        var decoded = Encoding.UTF8.GetString(Convert.FromBase64String(key));
        Assert.Contains("org_id:999", decoded);
        Assert.Contains("limit_hits", decoded);
    }

    [Fact]
    public void GenerateScopedKey_DifferentParams_DifferentKeys()
    {
        var key1 = ScopedKeyHelper.GenerateScopedKey("same-key-123", new Dictionary<string, object>
        {
            ["filter_by"] = "user:1",
        });
        var key2 = ScopedKeyHelper.GenerateScopedKey("same-key-123", new Dictionary<string, object>
        {
            ["filter_by"] = "user:2",
        });

        Assert.NotEqual(key1, key2);
    }

    [Fact]
    public void GenerateScopedKey_DifferentParentKeys_DifferentKeys()
    {
        var params1 = new Dictionary<string, object> { ["filter_by"] = "user:1" };
        var key1 = ScopedKeyHelper.GenerateScopedKey("parent-key-aaa", params1);
        var key2 = ScopedKeyHelper.GenerateScopedKey("parent-key-bbb", params1);

        Assert.NotEqual(key1, key2);
    }

    [Fact]
    public void GenerateScopedKey_ShortKey_DoesNotThrow()
    {
        // Key shorter than 4 chars
        var key = ScopedKeyHelper.GenerateScopedKey("ab", new Dictionary<string, object>
        {
            ["filter_by"] = "x:1",
        });
        Assert.NotEmpty(key);
    }

    [Fact]
    public void GenerateScopedKey_EmptyParams()
    {
        var key = ScopedKeyHelper.GenerateScopedKey("mykey1234", new Dictionary<string, object>());
        Assert.NotEmpty(key);
    }

    [Fact]
    public void GenerateScopedKey_Deterministic()
    {
        var p = new Dictionary<string, object> { ["filter_by"] = "x:1" };
        var key1 = ScopedKeyHelper.GenerateScopedKey("key123", p);
        var key2 = ScopedKeyHelper.GenerateScopedKey("key123", p);
        Assert.Equal(key1, key2);
    }
}
