using Torque.Http.Search;

namespace Torque.Http.Tests.Search;

public class SearchBuilderTests
{
    [Fact]
    public void Create_SetsQueryBy()
    {
        var p = SearchBuilder.Create("title").Build();
        Assert.Equal("title", p.QueryBy);
    }

    [Fact]
    public void FluentChaining_SetsAllProperties()
    {
        var p = SearchBuilder.Create("title,description")
            .Query("running shoes")
            .FilterBy("price:>50 && brand:=Nike")
            .SortBy("price:asc")
            .FacetBy("brand,category")
            .Page(2, perPage: 25)
            .Typos(2)
            .Prefix(true)
            .IncludeFields("title", "price")
            .ExcludeFields("description")
            .HighlightFields("title")
            .HighlightTags("<mark>", "</mark>")
            .TextMatchType("max_score")
            .Stopwords("en")
            .Build();

        Assert.Equal("title,description", p.QueryBy);
        Assert.Equal("running shoes", p.Q);
        Assert.Equal("price:>50 && brand:=Nike", p.FilterBy);
        Assert.Equal("price:asc", p.SortBy);
        Assert.Equal("brand,category", p.FacetBy);
        Assert.Equal(2, p.Page);
        Assert.Equal(25, p.PerPage);
        Assert.Equal(2, p.NumTypos);
        Assert.True(p.Prefix);
        Assert.Equal("title,price", p.IncludeFields);
        Assert.Equal("description", p.ExcludeFields);
        Assert.Equal("title", p.HighlightFields);
        Assert.Equal("<mark>", p.HighlightStartTag);
        Assert.Equal("</mark>", p.HighlightEndTag);
        Assert.Equal("max_score", p.TextMatchType);
        Assert.Equal("en", p.Stopwords);
    }

    [Fact]
    public void GroupBy_SetsGroupByAndLimit()
    {
        var p = SearchBuilder.Create("title")
            .Query("*")
            .GroupBy("category", limit: 3)
            .GroupMissingValues()
            .Build();

        Assert.Equal("category", p.GroupBy);
        Assert.Equal(3, p.GroupLimit);
        Assert.True(p.GroupMissingValues);
    }

    [Fact]
    public void Offset_SetsOffsetAndLimit()
    {
        var p = SearchBuilder.Create("title")
            .Offset(100, 50)
            .Build();

        Assert.Equal(100, p.Offset);
        Assert.Equal(50, p.Limit);
    }

    [Fact]
    public void VectorQuery_WithArray()
    {
        var p = SearchBuilder.Create("title")
            .VectorQuery("embedding", [0.1f, 0.2f, 0.3f], 10)
            .Build();

        Assert.NotNull(p.VectorQuery);
        Assert.Contains("embedding:", p.VectorQuery);
        Assert.Contains("k:10", p.VectorQuery);
    }

    [Fact]
    public void VectorQuery_WithRawString()
    {
        var p = SearchBuilder.Create("title")
            .VectorQuery("embedding:([0.1,0.2], k:5, flat_search_cutoff:20)")
            .Build();

        Assert.Equal("embedding:([0.1,0.2], k:5, flat_search_cutoff:20)", p.VectorQuery);
    }

    [Fact]
    public void PerformanceOptions()
    {
        var p = SearchBuilder.Create("title")
            .SearchCutoffMs(500)
            .MaxCandidates(1000)
            .LimitHits(200)
            .ExhaustiveSearch()
            .Build();

        Assert.Equal(500, p.SearchCutoffMs);
        Assert.Equal(1000, p.MaxCandidates);
        Assert.Equal(200, p.LimitHits);
        Assert.True(p.ExhaustiveSearch);
    }

    [Fact]
    public void CacheOptions()
    {
        var p = SearchBuilder.Create("title")
            .UseCache()
            .CacheTtl(120)
            .Build();

        Assert.True(p.UseCache);
        Assert.Equal(120, p.CacheTtl);
    }

    [Fact]
    public void CurationOptions()
    {
        var p = SearchBuilder.Create("title")
            .PinnedHits("doc1:1", "doc2:2")
            .HiddenHits("doc3")
            .Build();

        Assert.Equal("doc1:1,doc2:2", p.PinnedHits);
        Assert.Equal("doc3", p.HiddenHits);
    }

    [Fact]
    public void TypoOptions()
    {
        var p = SearchBuilder.Create("title")
            .TypoTokensThreshold(100)
            .DropTokensThreshold(5)
            .DropTokensMode("right_to_left")
            .PrioritizeExactMatch()
            .PrioritizeTokenPosition()
            .PrioritizeNumMatchingFields()
            .Build();

        Assert.Equal(100, p.TypoTokensThreshold);
        Assert.Equal(5, p.DropTokensThreshold);
        Assert.Equal("right_to_left", p.DropTokensMode);
        Assert.True(p.PrioritizeExactMatch);
        Assert.True(p.PrioritizeTokenPosition);
        Assert.True(p.PrioritizeNumMatchingFields);
    }

    [Fact]
    public void Build_ProducesValidQueryString()
    {
        var p = SearchBuilder.Create("title")
            .Query("test")
            .FilterBy("price:>10")
            .Page(1, 10)
            .Build();

        var qs = p.ToQueryString();
        Assert.StartsWith("?", qs);
        Assert.Contains("q=test", qs);
        Assert.Contains("query_by=title", qs);
        Assert.Contains("page=1", qs);
        Assert.Contains("per_page=10", qs);
    }

    [Fact]
    public void FacetOptions()
    {
        var p = SearchBuilder.Create("title")
            .FacetBy("brand,category")
            .MaxFacetValues(50)
            .FacetQuery("brand:Ni")
            .Build();

        Assert.Equal("brand,category", p.FacetBy);
        Assert.Equal(50, p.MaxFacetValues);
        Assert.Equal("brand:Ni", p.FacetQuery);
    }

    [Fact]
    public void HighlightOptions()
    {
        var p = SearchBuilder.Create("title")
            .HighlightFields("title", "body")
            .HighlightFullFields("body")
            .HighlightTags("<em>", "</em>")
            .HighlightAffixNumTokens(5)
            .SnippetThreshold(40)
            .Build();

        Assert.Equal("title,body", p.HighlightFields);
        Assert.Equal("body", p.HighlightFullFields);
        Assert.Equal("<em>", p.HighlightStartTag);
        Assert.Equal("</em>", p.HighlightEndTag);
        Assert.Equal(5, p.HighlightAffixNumTokens);
        Assert.Equal(40, p.SnippetThreshold);
    }
}
