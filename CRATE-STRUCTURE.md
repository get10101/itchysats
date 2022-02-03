## Why so many crates?

With every new feature, we invariably add more Rust code to this repository.
If we want to keep adding features at a fast pace, we must ensure that compilation times are as low as possible.

One way to minimise compilation times is to define a crate structure which enables parallel and incremental compilation.
With a monolithic approach, you are building _everything_ for any change you make.
With a sequential approach, you don't take advantage of multiple cores in your CPU.

As an example, let's look at a possible configuration for our binaries:

<!-- dprint-ignore -->
```
       +- maker -+-> maker-bin
      /           \
core-+             +-> tests
      \           /
       +- taker -+-> taker-bin
```

With such an approach, building `maker-bin` would not require building `taker`, `taker-bin` or `tests`.
Additionally, when building `tests`, the `maker` and `taker` crates would be allowed to build in parallel, speeding up the process.

## How to proceed?

Every time we introduce a new bit of code we should consider where it belongs in our crate graph.
For example, we may add an actor which is only used by the `maker` crate, so it wouldn't make sense to put in a crate which the `taker` crate also depends on.

But this process shouldn't just apply to code we add.
Every time we make changes we should look for opportunities to relocate code which appears to be in the wrong crate.
Achieving a better crate graph is an iterative and never-ending process, meaning that the current state of affairs at the time of writing this document is by no means perfect.
