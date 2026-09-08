# JVM contract tooling

This directory contains the pinned Gradle wrapper used by Platform JVM contract
TCKs. It contains no Product lifecycle, gateway, coordinator, or deployable JVM
application. Generated JVM consumers live with their contracts under `tck/`.

The former `ProductRun` and reconciliation implementation is now retained by
Cyrene-Yield under `migration/jvm-product-control-plane` while its useful
behavior is folded into Yield-owned lifecycle state.
