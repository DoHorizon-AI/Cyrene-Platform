# Shared and enterprise plugins

This directory contains capabilities that do not belong to exactly one CYRENE
product: hardware probes, runtime builders, compatibility rules, deployment
material, and the preserved mixed enterprise bundle.

The enterprise bundle is intentionally not split in this checkpoint because its
Kotlin, Python, and Rust components share implicit behavior. Extract it only
after service contracts and parity tests exist.
