# Capabilities and Plugins

A Plugin owns one or more versioned capability contracts and an independently
runnable implementation. Its repository manifest is the authority for identity,
release, methods, payload schemas, protocol, and runtime launcher.

Platform performs generic discovery, compatibility selection, package
verification, process supervision, and endpoint publication. Products call the
selected Plugin endpoint directly through a Plugin-owned client contract.
Platform never imports the implementation or handles capability payload bytes.
