using Cyrene.Capability.V1;
using Cyrene.Message.Connector.V1;
using Google.Protobuf.WellKnownTypes;

const string capabilityId = "message.connector.v1";
const string interfaceVersion = "1";
const string sendMessageMethod = "send_message";
const string inboundEventType = "inbound_message";

var conversation = new ConversationScope
{
    Vendor = "example.messaging.v1",
    AccountId = "10001",
    ConversationId = "456",
    Kind = ConversationKind.Group,
};

var inbound = new InboundMessagePayload
{
    MessageId = "9002",
    Conversation = conversation,
    SenderId = "123",
    SenderDisplayName = "Alice",
    Reply = new ReplyReference { MessageId = "777" },
    VendorExtension = new VendorExtension
    {
        Vendor = "example.messaging.v1",
        Facts =
        {
            new VendorFact { Name = "post_type", Value = "message" },
        },
    },
};
inbound.Content.Add(new MessageContentPart
{
    Mention = new MentionContent
    {
        Target = MentionTarget.User,
        TargetId = "654321",
        DisplayName = "Carol",
    },
});
inbound.Content.Add(new MessageContentPart
{
    Text = new TextContent { Text = "look" },
});
inbound.Content.Add(new MessageContentPart
{
    Image = new ImageContent
    {
        Reference = new AttachmentReference
        {
            RemoteUri = "https://example.test/image.png",
        },
        MimeType = "image/png",
    },
});

var applicationEvent = new CapabilityApplicationEvent
{
    SubscriptionId = "subscription-1",
    Capability = capabilityId,
    EventSequence = 1,
    EventType = inboundEventType,
    Payload = Any.Pack(inbound),
    Generation = 7,
    SourceId = "example-connector",
};
if (!applicationEvent.Payload.Is(InboundMessagePayload.Descriptor))
{
    throw new InvalidOperationException("Inbound Any type does not match generated descriptor.");
}
var unpackedInbound = applicationEvent.Payload.Unpack<InboundMessagePayload>();
if (unpackedInbound.Content.Count != 3 || unpackedInbound.Reply.MessageId != "777")
{
    throw new InvalidOperationException("Inbound connector payload did not round-trip.");
}

var send = new SendMessageRequest
{
    Conversation = conversation.Clone(),
    Reply = new ReplyReference { MessageId = "9002" },
};
send.Content.Add(new MessageContentPart
{
    Text = new TextContent { Text = "answer" },
});
var invoke = new InvokeCapabilityRequest
{
    Capability = capabilityId,
    InterfaceVersion = interfaceVersion,
    Method = sendMessageMethod,
    Request = Any.Pack(send),
};
if (!invoke.Request.Is(SendMessageRequest.Descriptor))
{
    throw new InvalidOperationException("Outbound Any type does not match generated descriptor.");
}

var delivery = new DeliveryResult
{
    Status = DeliveryStatus.RateLimited,
    Reason = "vendor rate limit",
    RetryAfter = Duration.FromTimeSpan(TimeSpan.FromSeconds(30)),
};
var response = new InvokeCapabilityResponse
{
    Response = Any.Pack(delivery),
};
if (!response.Response.Is(DeliveryResult.Descriptor)
    || response.Response.Unpack<DeliveryResult>().Status != DeliveryStatus.RateLimited)
{
    throw new InvalidOperationException("Delivery result Any did not round-trip.");
}

Console.WriteLine("message.connector.v1 generated C# contract TCK: PASS");
