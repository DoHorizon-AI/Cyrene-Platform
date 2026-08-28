# Message Connector Contract TCK

This TCK validates the EXPERIMENTAL `message.connector.v1` payload contract.
It is separate from the generic worker protocol and Capability Execution
Service TCKs and contains no AstrBot or vendor implementation code.

Run the Rust schema/`Any` tests:

```powershell
cargo test -p cy-proto --test message_connector_contract_tck
```

Compile and execute the generated C# contract consumer:

```powershell
dotnet restore .\tck\message-connector-contract\dotnet\MessageConnectorContractTck.csproj
dotnet run --project .\tck\message-connector-contract\dotnet\MessageConnectorContractTck.csproj --no-restore
```

Generate bindings for C#, Java, Kotlin, and Python without retaining generated
artifacts:

```powershell
.\tck\message-connector-contract\generate-bindings.ps1
```
