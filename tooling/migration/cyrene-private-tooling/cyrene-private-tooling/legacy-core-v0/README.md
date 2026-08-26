# Legacy core v0 Python and compatibility source

This archive preserves files removed while narrowing `cyrene-core` to the Rust
host boundary. Original relative paths are retained under `source/`.

Included material:

- the old Python workspace, UV lock, SDKs, and generated protobuf bindings;
- Python tests and fixtures;
- the Python service-manifest validator and protobuf generator;
- Rust integration tests whose fixtures launched Python;
- the former Docker ignore file.

The files are historical input for the future API redesign. They are not part
of the active core, and this repository makes no claim that they run from the
archive path.
