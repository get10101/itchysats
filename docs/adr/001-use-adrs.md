# ADR 001 - Use architecture decision records

```
Author: @thomaseizinger
Date: 29/12/2021
Status: Active
```

## Context

ADRs document architecture decisions.
These decisions do not (yet) necessarily reflect the state of the codebase.
Part of the benefit of documenting only the _decision_ is that this documentation does not need to be updated as the code changes.
Instead, by reading all decisions, it becomes clear, what the current direction is and any discrepancies in what the code is currently doing are obvious.

## Format

All ADRs should roughly follow the same format.
They should state the primary author, when it was written and its current status.
The idea of the status is to make it clear upfront, whether or not the decision is still active or has been superseded by another decision.
As we learn more about our domain and the project evolves, past decisions will become obsolete.
In such a case, it is helpful to update the old ADR by changing the status to:

```
Status: Superseded by ADR XYZ.
```

Otherwise, the format is only loosely defined.
Include whatever you think is important to support the decision and provide context on why that decision was necessary.
