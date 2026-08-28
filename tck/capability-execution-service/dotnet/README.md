# Generated .NET proving consumer

This is a generic consumer of `cyrene.capability.v1`. The project generates
the client from the checked-in public proto and never references the internal
`cy.plugin.v1` worker protocol.

Commands:

```text
dotnet run -- invoke <endpoint> <capability> <interface> <method> <request-file> [deadline-ms] [cancel-after-ms]
dotnet run -- events <endpoint> <capability> <interface> <filter-file-or-> [deadline-ms] [cancel-after-ms]
```

The request/filter files contain the capability's payload bytes. They are
wrapped in `google.protobuf.Any` at the public boundary; the generic client
does not know domain or worker fields.
