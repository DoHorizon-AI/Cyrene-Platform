# Tool and Plugin system

```mermaid
flowchart LR
    Product -->|capability requirement| Resolver[Platform resolver]
    Resolver -->|exact binding| Lifecycle[Platform package lifecycle]
    Lifecycle -->|start and observe| Plugin[Plugin-owned process]
    Lifecycle -->|opaque connection_ref| Product
    Product -->|direct versioned payload| Plugin
```

The Plugin repository manifest owns capability names, interface versions,
method and payload schemas, language, protocol, and package launcher. Platform
uses opaque ids for compatibility and never maintains hardware/model/precision
business taxonomies.
