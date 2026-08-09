# Core maintenance scripts

Only repository-independent contract tooling belongs here.

- `generate_plugin_proto.py` regenerates Python plugin protocol bindings from
  `contracts/proto/plugin/v1` into the public Python SDK.

Legacy naming audits and destructive cleanup helpers remain in the private
migration archive and are not distributed with the core repository.
