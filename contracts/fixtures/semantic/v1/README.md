# Kernel Semantic v1 fixture manifest

`manifest.json` binds the frozen, source-info-free Protobuf descriptor to the
semantic revision and shared TCK directory. The digest is checked by the
architecture-governance gate. Pull requests are additionally compared with
the target branch descriptor, so editing the local digest cannot hide a wire
breaking change.
