using Torque.Http.Http;

namespace Torque.Http.Tests.Http;

public class NodePoolTests
{
    [Fact]
    public void SingleNode_AlwaysReturnsSame()
    {
        var pool = new NodePool([new Uri("http://node1:8108")], TimeSpan.FromSeconds(60));
        var n1 = pool.GetNext();
        var n2 = pool.GetNext();
        var n3 = pool.GetNext();
        Assert.Equal(n1, n2);
        Assert.Equal(n2, n3);
    }

    [Fact]
    public void MultiNode_RoundRobins()
    {
        var nodes = new List<Uri>
        {
            new("http://node1:8108"),
            new("http://node2:8108"),
            new("http://node3:8108"),
        };
        var pool = new NodePool(nodes, TimeSpan.FromSeconds(60));

        var seen = new HashSet<string>();
        for (int i = 0; i < 6; i++)
            seen.Add(pool.GetNext().ToString());

        Assert.Equal(3, seen.Count);
    }

    [Fact]
    public void ReportFailure_MarksUnhealthyAfterThreshold()
    {
        var nodes = new List<Uri>
        {
            new("http://node1:8108"),
            new("http://node2:8108"),
        };
        var pool = new NodePool(nodes, TimeSpan.FromMinutes(5)); // long cooldown

        var node1 = new Uri("http://node1:8108");
        // 3 failures should mark unhealthy
        pool.ReportFailure(node1);
        pool.ReportFailure(node1);
        pool.ReportFailure(node1);

        // Now round-robin should skip node1 (until cooldown)
        // Get many nodes - should mostly get node2
        var counts = new Dictionary<string, int>();
        for (int i = 0; i < 20; i++)
        {
            var n = pool.GetNext().ToString();
            counts[n] = counts.GetValueOrDefault(n) + 1;
        }

        // node2 should be used more than node1
        Assert.True(counts.GetValueOrDefault("http://node2:8108/") > counts.GetValueOrDefault("http://node1:8108/"));
    }

    [Fact]
    public void ReportSuccess_RestoresHealth()
    {
        var nodes = new List<Uri>
        {
            new("http://node1:8108"),
            new("http://node2:8108"),
        };
        var pool = new NodePool(nodes, TimeSpan.FromMinutes(5));

        var node1 = new Uri("http://node1:8108");
        pool.ReportFailure(node1);
        pool.ReportFailure(node1);
        pool.ReportFailure(node1);
        pool.ReportSuccess(node1); // restore

        // Both nodes should now be used
        var seen = new HashSet<string>();
        for (int i = 0; i < 10; i++)
            seen.Add(pool.GetNext().ToString());

        Assert.Equal(2, seen.Count);
    }

    [Fact]
    public void AllUnhealthy_StillReturnsNode()
    {
        var nodes = new List<Uri>
        {
            new("http://node1:8108"),
            new("http://node2:8108"),
        };
        var pool = new NodePool(nodes, TimeSpan.FromMinutes(5));

        // Mark both unhealthy
        for (int i = 0; i < 3; i++)
        {
            pool.ReportFailure(new Uri("http://node1:8108"));
            pool.ReportFailure(new Uri("http://node2:8108"));
        }

        // Should still return a node (the oldest failure)
        var node = pool.GetNext();
        Assert.NotNull(node);
    }

    [Fact]
    public void ZeroNodes_Throws()
    {
        Assert.Throws<ArgumentOutOfRangeException>(() =>
            new NodePool([], TimeSpan.FromSeconds(60)));
    }

    [Fact]
    public void CooldownExpired_AllowsRetry()
    {
        var nodes = new List<Uri>
        {
            new("http://node1:8108"),
            new("http://node2:8108"),
        };
        // Very short cooldown
        var pool = new NodePool(nodes, TimeSpan.FromMilliseconds(1));

        var node1 = new Uri("http://node1:8108");
        pool.ReportFailure(node1);
        pool.ReportFailure(node1);
        pool.ReportFailure(node1);

        Thread.Sleep(10); // Wait for cooldown

        // After cooldown, node1 should be returned again
        var seen = new HashSet<string>();
        for (int i = 0; i < 10; i++)
            seen.Add(pool.GetNext().ToString());

        Assert.Equal(2, seen.Count);
    }
}
