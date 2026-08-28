using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;
using Grpc.Core;
using Grpc.Net.Client;
using Cyrene.Capability.V1;

internal static class Program
{
    private const string DefaultTypeUrl = "type.cyrene.io/tck.JsonValue";

    public static async Task<int> Main(string[] args)
    {
        if (args.Length == 0)
        {
            PrintUsage();
            return 2;
        }

        try
        {
            return args[0].ToLowerInvariant() switch
            {
                "invoke" => await InvokeAsync(args[1..]),
                "events" => await EventsAsync(args[1..]),
                _ => UsageError("unknown command"),
            };
        }
        catch (RpcException error)
        {
            Console.Error.WriteLine($"grpc:{error.StatusCode}:{error.Status.Detail}");
            return 3;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"client:{error.GetType().Name}:{error.Message}");
            return 4;
        }
    }

    private static async Task<int> InvokeAsync(string[] args)
    {
        if (args.Length is < 5 or > 7)
        {
            return UsageError("invoke requires endpoint, capability, interface, method, request file, and optional deadline/cancellation milliseconds");
        }

        var endpoint = args[0];
        var capability = args[1];
        var interfaceVersion = args[2];
        var method = args[3];
        var requestFile = args[4];
        var deadline = ParseDeadline(args, 5);
        using var cancellationSource = CreateCancellationSource(args, 6);
        var requestPayload = ReadPayload(requestFile);

        using var channel = GrpcChannel.ForAddress(endpoint);
        var client = new CapabilityExecutionService.CapabilityExecutionServiceClient(channel);
        var request = new InvokeCapabilityRequest
        {
            Capability = capability,
            InterfaceVersion = interfaceVersion,
            Method = method,
            Request = ToAny(requestPayload),
        };

        var response = await client.InvokeCapabilityAsync(
            request,
            deadline: deadline,
            cancellationToken: cancellationSource?.Token ?? CancellationToken.None);
        switch (response.ResultCase)
        {
            case InvokeCapabilityResponse.ResultOneofCase.Response:
                Console.WriteLine(response.Response.TypeUrl);
                Console.WriteLine(response.Response.Value.ToStringUtf8());
                return 0;
            case InvokeCapabilityResponse.ResultOneofCase.Error:
                Console.Error.WriteLine($"execution:{response.Error.Code}:{response.Error.Message}");
                return 5;
            default:
                Console.Error.WriteLine("execution:empty response");
                return 6;
        }
    }

    private static async Task<int> EventsAsync(string[] args)
    {
        if (args.Length is < 4 or > 6)
        {
            return UsageError("events requires endpoint, capability, interface, filter file, and optional deadline/cancellation milliseconds");
        }

        var endpoint = args[0];
        var capability = args[1];
        var interfaceVersion = args[2];
        var filterFile = args[3];
        var deadline = ParseDeadline(args, 4);
        using var cancellationSource = CreateCancellationSource(args, 5);

        using var channel = GrpcChannel.ForAddress(endpoint);
        var client = new CapabilityExecutionService.CapabilityExecutionServiceClient(channel);
        var request = new SubscribeCapabilityEventsRequest
        {
            Capability = capability,
            InterfaceVersion = interfaceVersion,
        };
        if (!string.Equals(filterFile, "-", StringComparison.Ordinal))
        {
            request.Filter = ToAny(ReadPayload(filterFile));
        }

        using var call = client.SubscribeCapabilityEvents(
            request,
            deadline: deadline,
            cancellationToken: cancellationSource?.Token ?? CancellationToken.None);
        await foreach (var item in call.ResponseStream.ReadAllAsync())
        {
            switch (item.ItemCase)
            {
                case CapabilityEventStreamItem.ItemOneofCase.ApplicationEvent:
                    var applicationEvent = item.ApplicationEvent;
                    var payload = applicationEvent.Payload;
                    Console.WriteLine(
                        $"EVENT|{applicationEvent.SubscriptionId}|{applicationEvent.EventSequence}|{applicationEvent.EventType}|{payload.TypeUrl}|{Convert.ToBase64String(payload.Value.ToByteArray())}");
                    break;
                case CapabilityEventStreamItem.ItemOneofCase.StreamEnd:
                    var end = item.StreamEnd;
                    Console.WriteLine(
                        $"END|{end.SubscriptionId}|{end.Reason}|{end.Generation}|{end.SourceId}|{end.Error?.Code.ToString() ?? "NONE"}");
                    return end.Reason == CapabilityEventStreamEndReason.NormalCompletion ? 0 : 7;
                default:
                    Console.Error.WriteLine("stream:empty item");
                    return 8;
            }
        }

        Console.Error.WriteLine("stream:transport ended without stream_end");
        return 9;
    }

    private static Any ToAny(byte[] payload) => new()
    {
        TypeUrl = DefaultTypeUrl,
        Value = ByteString.CopyFrom(payload),
    };

    private static byte[] ReadPayload(string path) =>
        string.Equals(path, "-", StringComparison.Ordinal)
            ? Array.Empty<byte>()
            : File.ReadAllBytes(path);

    private static DateTime? ParseDeadline(string[] args, int index)
    {
        if (args.Length <= index)
        {
            return null;
        }

        var milliseconds = ParseMilliseconds(args[index], "deadline");
        return DateTime.UtcNow.AddMilliseconds(milliseconds);
    }

    private static CancellationTokenSource? CreateCancellationSource(string[] args, int index)
    {
        if (args.Length <= index)
        {
            return null;
        }

        return new CancellationTokenSource(TimeSpan.FromMilliseconds(
            ParseMilliseconds(args[index], "cancellation")));
    }

    private static long ParseMilliseconds(string raw, string label)
    {
        if (!long.TryParse(raw, out var milliseconds) || milliseconds <= 0)
        {
            throw new ArgumentException($"{label} milliseconds must be a positive integer");
        }
        return milliseconds;
    }

    private static int UsageError(string message)
    {
        Console.Error.WriteLine($"usage:{message}");
        PrintUsage();
        return 2;
    }

    private static void PrintUsage()
    {
        Console.Error.WriteLine("invoke <endpoint> <capability> <interface> <method> <request-file> [deadline-ms] [cancel-after-ms]");
        Console.Error.WriteLine("events <endpoint> <capability> <interface> <filter-file-or-> [deadline-ms] [cancel-after-ms]");
    }
}
