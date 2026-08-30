# model.provider.v1 embedding contract TCK

This network-free TCK projects the canonical protobuf into each language for
which this repository has generation automation. No generated source is
checked in.

Run the gates on Linux from the repository root:

```bash
cargo test -p cy-proto --test model_provider_embedding_contract_tck --locked
cargo test -p cy-capability-execution-service --test service_tck embedding_tck --locked
dotnet run --project tck/model-provider-embedding-contract/dotnet/ModelProviderEmbeddingContractTck.csproj
bash tck/model-provider-embedding-contract/generate-bindings.sh
bash tck/model-provider-embedding-contract/run-jvm-tck.sh
```

The Rust payload TCK covers single/batch ordering, dimensions, domain errors,
empty/invalid requests, atomic maximum-batch failure, typed `Any`, and the CES
timeout/cancellation error boundary. The CES TCK uses two deterministic local
worker bindings and proves implicit compatibility, ambiguity, explicit target
isolation, and unknown binding behavior. The existing generic CES generation
test remains the authority for binding identity surviving a worker generation
change and for native deadline/cancellation behavior.

The C#, Python, and JVM gates prove generated API compilation, old CES request
wire compatibility with omitted `binding_id`, explicit binding selection, and
typed `Any` round trips.
