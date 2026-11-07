namespace Torque.Http.Models;

/// <summary>
/// Document language for stemming and text processing.
/// Wire values match Rust Language::to_u8().
/// </summary>
public enum Language : byte
{
    English = 0,
    Swedish = 1,
    Norwegian = 2,
    Danish = 3,
    Finnish = 4,
}
