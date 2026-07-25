using System.Text.Json;
using Sentry;

if (args.Length != 1)
{
    Console.Error.WriteLine("usage: MetricSdkCompatibility <dsn>");
    return 2;
}

using var watchdog = new Timer(
    _ =>
    {
        Console.Error.WriteLine("real .NET SDK sender exceeded its 15 second process deadline");
        Environment.Exit(124);
    },
    null,
    TimeSpan.FromSeconds(15),
    Timeout.InfiniteTimeSpan);

using var sdk = SentrySdk.Init(options =>
{
    options.Dsn = args[0];
    options.Environment = "sdk-compatibility";
    options.Release = "metric-dotnet-sdk-test@1.0.0";
    options.TracesSampleRate = 0;
    options.AutoSessionTracking = false;
    options.SendDefaultPii = false;
});

var eventId = SentrySdk.CaptureException(
    new MetricDotnetSdkCompatibilityException(
        "Metric real .NET SDK compatibility event"));
await SentrySdk.FlushAsync(TimeSpan.FromSeconds(8));

Console.WriteLine(
    JsonSerializer.Serialize(
        new
        {
            event_id = eventId.ToString(),
            flushed = true,
        }));
return 0;

internal sealed class MetricDotnetSdkCompatibilityException(string message)
    : Exception(message);
