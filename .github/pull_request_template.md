## Change

<!-- What caller-visible behavior or interface changed? -->

## Runtime contract

<!-- Delete this section only when the change cannot reach decode, visualization, settings,
metadata, loader, I/O, or an FFI seam. See docs/runtime-contract.md. -->

| Cadence touched | Allocations and maximum | Locks / contention | Callbacks / adapters |
|---|---|---|---|
| Setup | | | |
| Per read/frame | | | |
| Control/shutdown | | | |

- [ ] Plugin-controlled values are validated and capped before allocation or iteration.
- [ ] Ownership is explicitly library, host, session, or frame scoped.
- [ ] No external callback or adapter runs while a host lock is held.
- [ ] Steady-state allocation/callback/capacity assertions were added or remain applicable.
- [ ] Common paths were checked for redundant full-buffer copies.

## Verification

<!-- Commands run and the interface-level behavior each test proves. -->
