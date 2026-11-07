namespace Torque.Http.Tcp;

/// <summary>
/// Ingest session state machine matching src/ingest/session.rs.
/// States: Idle → Streaming → Committed/Aborted (back to Idle).
/// </summary>
internal sealed class IngestSession
{
    public enum State { Idle, Streaming }

    public State CurrentState { get; private set; } = State.Idle;

    public void TransitionToStreaming()
    {
        if (CurrentState != State.Idle)
            throw new InvalidOperationException($"Cannot start ingest: session is {CurrentState}, expected Idle");
        CurrentState = State.Streaming;
    }

    public void ValidateStreaming()
    {
        if (CurrentState != State.Streaming)
            throw new InvalidOperationException($"Cannot send batch: session is {CurrentState}, expected Streaming");
    }

    public void TransitionToIdle()
    {
        CurrentState = State.Idle;
    }
}
