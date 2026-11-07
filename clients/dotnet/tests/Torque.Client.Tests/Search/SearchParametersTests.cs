using Torque.Http.Search;

namespace Torque.Http.Tests.Search;

public class SearchParametersTests
{
    [Fact]
    public void ToQueryString_BasicSearch()
    {
        var p = new SearchParameters { Q = "laptop", QueryBy = "title" };
        var qs = p.ToQueryString();
        Assert.Contains("q=laptop", qs);
        Assert.Contains("query_by=title", qs);
        Assert.StartsWith("?", qs);
    }

    [Fact]
    public void ToQueryString_Empty_ReturnsEmpty()
    {
        var p = new SearchParameters();
        Assert.Equal("", p.ToQueryString());
    }

    [Fact]
    public void ToQueryString_FilterBy_UrlEncoded()
    {
        var p = new SearchParameters { Q = "*", QueryBy = "title", FilterBy = "price:>100 && brand:=Nike" };
        var qs = p.ToQueryString();
        Assert.Contains("filter_by=", qs);
        // The > and && should be URL-encoded
        Assert.DoesNotContain("&&", qs.Split("filter_by=")[1].Split('&')[0]);
    }

    [Fact]
    public void ToQueryString_Pagination()
    {
        var p = new SearchParameters { Q = "*", QueryBy = "title", Page = 3, PerPage = 25 };
        var qs = p.ToQueryString();
        Assert.Contains("page=3", qs);
        Assert.Contains("per_page=25", qs);
    }

    [Fact]
    public void ToQueryString_OffsetLimit()
    {
        var p = new SearchParameters { Q = "*", QueryBy = "title", Offset = 100, Limit = 50 };
        var qs = p.ToQueryString();
        Assert.Contains("offset=100", qs);
        Assert.Contains("limit=50", qs);
    }

    [Fact]
    public void ToQueryString_BooleanFields()
    {
        var p = new SearchParameters
        {
            Q = "test", QueryBy = "title",
            Prefix = true,
            PrioritizeExactMatch = false,
            ExhaustiveSearch = true,
            UseCache = true,
        };
        var qs = p.ToQueryString();
        Assert.Contains("prefix=true", qs);
        Assert.Contains("prioritize_exact_match=false", qs);
        Assert.Contains("exhaustive_search=true", qs);
        Assert.Contains("use_cache=true", qs);
    }

    [Fact]
    public void ToQueryString_Grouping()
    {
        var p = new SearchParameters
        {
            Q = "*", QueryBy = "title",
            GroupBy = "category",
            GroupLimit = 5,
            GroupMissingValues = true,
        };
        var qs = p.ToQueryString();
        Assert.Contains("group_by=category", qs);
        Assert.Contains("group_limit=5", qs);
        Assert.Contains("group_missing_values=true", qs);
    }

    [Fact]
    public void ToQueryString_Highlighting()
    {
        var p = new SearchParameters
        {
            Q = "test", QueryBy = "title",
            HighlightFields = "title,body",
            HighlightFullFields = "body",
            HighlightStartTag = "<b>",
            HighlightEndTag = "</b>",
            HighlightAffixNumTokens = 3,
            SnippetThreshold = 30,
        };
        var qs = p.ToQueryString();
        Assert.Contains("highlight_fields=", qs);
        Assert.Contains("highlight_full_fields=", qs);
        Assert.Contains("highlight_start_tag=", qs);
        Assert.Contains("highlight_end_tag=", qs);
        Assert.Contains("highlight_affix_num_tokens=3", qs);
        Assert.Contains("snippet_threshold=30", qs);
    }

    [Fact]
    public void ToQueryString_FieldSelection()
    {
        var p = new SearchParameters
        {
            Q = "*", QueryBy = "title",
            IncludeFields = "title,price",
            ExcludeFields = "description",
        };
        var qs = p.ToQueryString();
        Assert.Contains("include_fields=", qs);
        Assert.Contains("exclude_fields=", qs);
    }

    [Fact]
    public void ToQueryString_TypoTolerance()
    {
        var p = new SearchParameters
        {
            Q = "test", QueryBy = "title",
            NumTypos = 2,
            TypoTokensThreshold = 100,
            DropTokensThreshold = 5,
            DropTokensMode = "right_to_left",
        };
        var qs = p.ToQueryString();
        Assert.Contains("num_typos=2", qs);
        Assert.Contains("typo_tokens_threshold=100", qs);
        Assert.Contains("drop_tokens_threshold=5", qs);
        Assert.Contains("drop_tokens_mode=right_to_left", qs);
    }

    [Fact]
    public void ToQueryString_CurationHits()
    {
        var p = new SearchParameters
        {
            Q = "test", QueryBy = "title",
            PinnedHits = "doc1:1,doc2:2",
            HiddenHits = "doc3",
        };
        var qs = p.ToQueryString();
        Assert.Contains("pinned_hits=", qs);
        Assert.Contains("hidden_hits=", qs);
    }

    [Fact]
    public void ToQueryString_NameMatching()
    {
        var p = new SearchParameters
        {
            Q = "Osama bin Laden",
            QueryBy = "names",
            NameMatching = "always",
            NameScore = true,
            NameScoreCandidates = 200,
            NameScoreKind = "person",
            ExplainMatches = true
        };
        var qs = p.ToQueryString();
        Assert.Contains("name_matching=always", qs);
        Assert.Contains("name_score=true", qs);
        Assert.Contains("name_score_candidates=200", qs);
        Assert.Contains("name_score_kind=person", qs);
        Assert.Contains("explain_matches=true", qs);
    }

    [Fact]
    public void ToQueryString_VectorSearch()
    {
        var p = new SearchParameters
        {
            Q = "*", QueryBy = "title",
            VectorQuery = "embedding:([0.1,0.2,0.3], k:10)",
        };
        var qs = p.ToQueryString();
        Assert.Contains("vector_query=", qs);
    }

    [Fact]
    public void ToQueryString_Performance()
    {
        var p = new SearchParameters
        {
            Q = "test", QueryBy = "title",
            SearchCutoffMs = 500,
            MaxCandidates = 1000,
            LimitHits = 200,
        };
        var qs = p.ToQueryString();
        Assert.Contains("search_cutoff_ms=500", qs);
        Assert.Contains("max_candidates=1000", qs);
        Assert.Contains("limit_hits=200", qs);
    }

    [Fact]
    public void ToQueryString_Cache()
    {
        var p = new SearchParameters
        {
            Q = "test", QueryBy = "title",
            UseCache = true,
            CacheTtl = 60,
        };
        var qs = p.ToQueryString();
        Assert.Contains("use_cache=true", qs);
        Assert.Contains("cache_ttl=60", qs);
    }

    [Fact]
    public void ToQueryString_AllNulls_ReturnsEmpty()
    {
        var p = new SearchParameters();
        Assert.Equal("", p.ToQueryString());
    }
}
