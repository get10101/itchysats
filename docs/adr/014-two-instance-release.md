# ADR 014 - Use a blue/green deployment-like approach for handling releases

```
Author: @thomaseizinger
Date: 09/02/2021
Status: Active
```

## Summary

Instead of replacing the existing production maker with a new version upon release, redirect users to a new version with each Umbrel release.
Extend the automation bot to handle two instances of the production maker.

## Context

Our primary users are on Umbrel.
With each Umbrel release, we get the chance to update the container that our users (the takers) use.
Umbrel pins the hash of the container and we currently hard-code the IP address and public key of the maker to connect to in the Umbrel configuration.

With new features, we may need to change the wire protocol in backwards-incompatible ways.
With a new release comes a now incompatible maker binary and we need to somehow migrate our users from one version to the next.

In an ideal world, we would not make any backwards-incompatible changes but instead _add_ new APIs (read: wire protocols) and deprecate old ones.
However, this requires defining and versioning our different wire protocols, otherwise we don't know, which protocol a client wants to speak.
Eventually, we want to do this.
Very likely, we will do this as part of introducing multiplexing (https://github.com/itchysats/itchysats/issues/1126) because that already requires some form of identifier for what a new substream should be used for.

In the meantime though, we need a release process that allows us to make changes without breaking existing users.
The proposal is to use a blue/green deployment style model.

In blue/green deployments, the new version of a software is deployed along-side the existing one and new users are (gradually) routed to the new instance.
Traditionally, this is done by a load-balancer.
In our situation, we can achieve this by changing the IP address & port in the Umbrel app configuration.
A user that upgrades to the new Umbrel release will get the new version of `Itchysats` and thus automatically connect to the new instance.

The production maker comes with an automation bot that handles incoming requests for placed orders etc.
With two releases of the maker running in parallel, the automation bot needs to be able to handle both of them.
Care needs to be taken on how the HTTP API of the maker changes with these releases.
This ADR however does not concern itself with the versioning of the HTTP API.

## Consequences

1. Be able to operate two production makers in parallel.
   Extend monitoring etc to differentiate both versions.
2. Automation bot to be able to handle two production makers.
3. Changes to the HTTP API need to be made carefully once this release model is adopted because the automation bot needs to be able to talk to different versions.
