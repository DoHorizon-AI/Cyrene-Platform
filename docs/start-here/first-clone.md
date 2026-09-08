# First Platform Checkout

Clone Cyrene-Platform when working on generic contracts, Kernel mechanisms,
adapters, or SDK primitives:

```bash
git clone https://github.com/DoHorizon-AI/Cyrene-Platform.git
cd Cyrene-Platform
python tooling/ci/verify.py --scope all-light
cargo check --locked --workspace --all-targets
```

This repository builds without sibling Products or plugin repositories.
For a multi-repository development checkout, follow the bootstrap instructions
owned by [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
