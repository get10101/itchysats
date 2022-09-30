# shared-bin

Crate for sharing code between the daemons that is application-specific like logger, rocket utilities, etc.
This crate should disappear over time so please refrain from adding code here.
Instead, try to remove this code by:

- Trying to upstream it if generic enough
- Inline into the specific binaries once the requirements diverge
- Extract a dedicated crate if possible
