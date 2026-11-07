namespace Torque.Http.Tests;

public class TorqueHttpClientOptionsTests
{
    [Fact]
    public void Defaults_SingleNode()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        Assert.Single(o.Nodes);
        Assert.Equal("http://localhost:8108/", o.Nodes[0].ToString());
    }

    [Fact]
    public void BaseUrl_Getter_ReturnsTrimmed()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        Assert.Equal("http://localhost:8108", o.BaseUrl);
    }

    [Fact]
    public void BaseUrl_Setter_ReplacesNodes()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        o.BaseUrl = "http://myhost:9999";
        Assert.Single(o.Nodes);
        Assert.Equal("http://myhost:9999/", o.Nodes[0].ToString());
    }

    [Fact]
    public void BaseUrl_Setter_WithTrailingSlash()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        o.BaseUrl = "http://myhost:9999/";
        Assert.Single(o.Nodes);
    }

    [Fact]
    public void MultiNode_ViaNodes()
    {
        var o = new TorqueHttpClientOptions
        {
            ApiKey = "test",
            Nodes = [new("http://n1:8108"), new("http://n2:8108"), new("http://n3:8108")],
        };
        Assert.Equal(3, o.Nodes.Count);
    }

    [Fact]
    public void Defaults_Timeout()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        Assert.Equal(TimeSpan.FromSeconds(30), o.Timeout);
    }

    [Fact]
    public void Defaults_Retries()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        Assert.Equal(2, o.MaxRetries);
    }

    [Fact]
    public void Defaults_NodeCooldown()
    {
        var o = new TorqueHttpClientOptions { ApiKey = "test" };
        Assert.Equal(TimeSpan.FromSeconds(60), o.NodeCooldown);
    }
}
