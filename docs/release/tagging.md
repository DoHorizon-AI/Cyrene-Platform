# Platform Tagging and Release Identity

Platform release tags use
`v<MAJOR>.<MINOR>.<PATCH>[-<PRERELEASE>]` and point to an exact commit on the
protected `main` branch. Published tags are immutable; a correction receives a
new patch version.

Validate a candidate tag with:

```bash
python tooling/release/validate_tag.py --repo-path . --tag v0.4.2
```

Create a tag only after the exact commit passes local release checks and the
required Azure source gates. The repository's release automation prepares a
draft release; it does not define tags for Products, plugins, or a combined
distribution.
