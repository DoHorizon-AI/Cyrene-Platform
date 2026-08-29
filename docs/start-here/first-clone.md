# How to Start Development: First-Clone Guide

Welcome to Cyrene! Follow this 5-minute guide to set up a clean, working multi-repository development workspace.

---

## Step 1: Clone the Platform Repository
```bash
mkdir Cyrene
cd Cyrene
git clone https://github.com/DoHorizon-AI/Cyrene-Platform.git Cyrene-Platform
```

## Step 2: Bootstrap Your Target Profile
Depending on what you want to build, choose a profile:
- **`core`**: Platform Kernel, SDKs, and Control Plane.
- **`training`**: Platform + Yield + Training Plugins.
- **`serving`**: Platform + Reactor + Serving Plugins.
- **`gateway`**: Platform + Exchange + Gateway Plugins.
- **`full`**: All public platform components and official plugins.

```bash
python Cyrene-Platform/tooling/workspace/workspace.py bootstrap --profile training
```

## Step 3: Run Workspace Diagnostics
```bash
python Cyrene-Platform/tooling/workspace/workspace.py doctor
python Cyrene-Platform/tooling/workspace/workspace.py status
```

## Step 4: Run Local Verification
```bash
python Cyrene-Platform/tooling/ci/verify.py
```
