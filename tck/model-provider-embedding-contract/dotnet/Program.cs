using Cyrene.Capability.V1;
using Cyrene.Model.Provider.V1;
using Google.Protobuf;
using Google.Protobuf.WellKnownTypes;

var request = new EmbeddingsRequest
{
    Model = "deterministic-model",
    Inputs = { "alpha", "beta" },
};
var oldRequest = new InvokeCapabilityRequest
{
    Capability = "model.provider.v1",
    InterfaceVersion = "1",
    Method = "embeddings",
    Request = Any.Pack(request),
};
var oldRoundTrip = InvokeCapabilityRequest.Parser.ParseFrom(oldRequest.ToByteArray());
if (oldRoundTrip.HasBindingId)
{
    throw new InvalidOperationException("Omitted binding_id must retain implicit compatibility.");
}

var targeted = oldRequest.Clone();
targeted.BindingId = "openai-main";
var targetedRoundTrip = InvokeCapabilityRequest.Parser.ParseFrom(targeted.ToByteArray());
if (!targetedRoundTrip.HasBindingId || targetedRoundTrip.BindingId != "openai-main")
{
    throw new InvalidOperationException("Explicit binding_id did not round-trip.");
}
if (!targetedRoundTrip.Request.Is(EmbeddingsRequest.Descriptor))
{
    throw new InvalidOperationException("Embedding request Any type is not canonical.");
}

var batch = new EmbeddingBatch
{
    Dimensions = 2,
    Model = "deterministic-model",
};
batch.Vectors.Add(new EmbeddingVector { Values = { 1.0f, 2.0f } });
batch.Vectors.Add(new EmbeddingVector { Values = { 3.0f, 4.0f } });
var response = new EmbeddingsResponse { Embeddings = batch };
var packed = Any.Pack(response);
if (!packed.Is(EmbeddingsResponse.Descriptor)
    || packed.Unpack<EmbeddingsResponse>().Embeddings.Vectors.Count != request.Inputs.Count)
{
    throw new InvalidOperationException("Embedding response Any did not preserve batch cardinality.");
}

Console.WriteLine("model.provider.v1 embedding generated C# contract TCK: PASS");
