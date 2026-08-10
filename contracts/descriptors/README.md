# Contract descriptor baselines

`cyrene-core-v1.pb` is the checked-in descriptor baseline for the Core v1
module, including imports and source information. Its SHA-256 is recorded in
the Core v1 fixture manifest. Any contract change must regenerate the
descriptor and pass the Buf breaking check before the baseline is replaced.
