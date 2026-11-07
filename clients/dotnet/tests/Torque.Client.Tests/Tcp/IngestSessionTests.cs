using Torque.Http.Tcp;

namespace Torque.Http.Tests.Tcp;

public class IngestSessionTests
{
    [Fact]
    public void InitialState_IsIdle()
    {
        var session = new IngestSession();
        Assert.Equal(IngestSession.State.Idle, session.CurrentState);
    }

    [Fact]
    public void TransitionToStreaming_FromIdle_Succeeds()
    {
        var session = new IngestSession();
        session.TransitionToStreaming();
        Assert.Equal(IngestSession.State.Streaming, session.CurrentState);
    }

    [Fact]
    public void TransitionToStreaming_FromStreaming_Throws()
    {
        var session = new IngestSession();
        session.TransitionToStreaming();
        Assert.Throws<InvalidOperationException>(() => session.TransitionToStreaming());
    }

    [Fact]
    public void ValidateStreaming_InStreamingState_Succeeds()
    {
        var session = new IngestSession();
        session.TransitionToStreaming();
        session.ValidateStreaming(); // no exception
    }

    [Fact]
    public void ValidateStreaming_InIdleState_Throws()
    {
        var session = new IngestSession();
        Assert.Throws<InvalidOperationException>(() => session.ValidateStreaming());
    }

    [Fact]
    public void TransitionToIdle_ResetsState()
    {
        var session = new IngestSession();
        session.TransitionToStreaming();
        session.TransitionToIdle();
        Assert.Equal(IngestSession.State.Idle, session.CurrentState);
    }

    [Fact]
    public void FullCycle_Idle_Streaming_Idle()
    {
        var session = new IngestSession();
        session.TransitionToStreaming();
        session.ValidateStreaming();
        session.TransitionToIdle();
        session.TransitionToStreaming(); // can start again
        Assert.Equal(IngestSession.State.Streaming, session.CurrentState);
    }
}
