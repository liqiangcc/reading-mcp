# Discovery Continuation Design

Status: accepted bounded design for the `list_documents` continuation phase.

This document freezes the `list_documents` continuation state machine. Directory navigation is a
separate `list_directory` capability; it does not change this document-discovery contract.

## 1. Use case

An Agent may use `list_documents` to discover an authorized local source, receive a bounded page,
and continue until the requested discovery scope is exhausted. The Agent must be able to tell the
difference between:

- an empty, complete scope;
- a bounded page that has an actionable continuation;
- a scope that changed and therefore cannot be continued truthfully.

Discovery produces source candidates only. It does not open, parse, normalize, index, or create a
`DocumentId`.

## 2. Request and response contract

The request evolves additively:

```text
path?                 # directory scope; canonicalized under current allowed roots
recursive             # default true
max_results           # positive; server-clamped
cursor?               # continuation of the same discovery scope
```

The response is:

```text
documents[]
complete
next_cursor?
```

`complete=false` always has `next_cursor`. `complete=true` never has `next_cursor`. An empty
configured-root set returns `documents=[]`, `complete=true`, and no cursor. A source candidate is
still only the existing path/name/media-type/size projection.

`max_results` is a page budget, not stream identity. A caller may change it between continuation
requests. The cursor position advances by the number of candidates returned, not by the requested
page budget.

## 3. Canonical discovery stream

The discovery stream is reconstructed for every request from a point-in-time filesystem scan:

1. Canonicalize the currently configured allowed roots and remove duplicate roots.
2. Resolve the requested `path` (or all configured roots) and authorize it against the current
   roots using path-component containment, not string-prefix containment.
3. Enumerate eligible regular files under the scope. With `recursive=false`, inspect only direct
   children; with `recursive=true`, traverse descendants.
4. For each candidate, collect only discovery metadata: canonical path, basename, supported media
   type, and byte size. Do not read the document body or invoke a parser/repository/index.
5. Sort the complete candidate list by canonical path using bytewise lexical order.

The resulting ordered candidate records are the stream. Their deterministic order is
`discovery-path/v1`.

The implementation may use an in-memory candidate vector for a request. It must not persist
candidate rows as canonical document state.

## 4. DiscoveryCursor

`DiscoveryCursor` is progress through a bounded discovery stream, not a source locator and not a
document identity. The encoded `discovery-cursor/v1` claims bind:

```text
schema_version
ordering_version = discovery-path/v1
scope_root(s)                 # canonical configured roots used by this request
requested_path?               # canonical requested directory, if present
recursive
candidate_manifest_hash       # hash of ordered candidate discovery records
total_candidates
next_index
```

The cursor does not contain a `DocumentId`, document content hash, normalized hash, or document
body. It also does not bind `max_results`.

Only a non-terminal cursor is issued: `0 < total_candidates` and
`next_index < total_candidates`. A malformed, tampered, impossible, or oversized cursor returns
`INVALID_CURSOR`. An unsupported cursor schema/order returns `STALE_CURSOR`.

## 5. Filesystem changes and truthful continuation

This design does not claim snapshot semantics. The filesystem is live and no directory snapshot is
persisted.

On a continuation request, the server re-authorizes the requested scope against the current
configured roots and rebuilds the current ordered candidate manifest. Continuation proceeds only
if all cursor-bound scope facts and the candidate manifest hash match. Any add/remove/rename or
discovery-visible metadata change that changes the manifest returns `STALE_CURSOR`; the caller must
restart discovery from the first page. A change in `max_results` alone is allowed because it does
not alter the stream.

This gives no-gap/no-overlap continuation under the accepted condition that the candidate manifest
is unchanged between pages. Changes during a scan are represented by that scan's candidate vector;
the server makes no stronger claim about concurrent filesystem mutations.

Current root authorization is re-applied on every continuation. A root removal, root change,
requested-path change, or authorization failure never becomes a way to access a newly unauthorized
path. The server fails closed with the existing blocked-source or stale-cursor error taxonomy.

## 6. Failure and degradation

- `max_results=0`: `INVALID_REQUEST`.
- malformed/oversized/tampered/impossible cursor: `INVALID_CURSOR`.
- cursor scope, recursive flag, or ordering mismatch: `CURSOR_TARGET_MISMATCH`.
- cursor schema/order version no longer supported: `STALE_CURSOR`.
- current manifest or bound identity changed: `STALE_CURSOR`.
- requested path outside current allowed roots: `BLOCKED_SOURCE`.
- configured roots unavailable: preserve the existing empty-root behavior when no roots canonicalize;
  report filesystem errors for an explicitly requested scope that cannot be read.

The discovery cursor never silently relocates to a nearby path, reuses an ordinal against a new
manifest, or opens a source to repair state.

## 7. Required evidence

The implementation PR must cover:

- more eligible files than a page limit, with every candidate exactly once;
- deterministic path order across repeated first-page scans;
- recursive and non-recursive streams isolated;
- changed page size across continuation pages;
- empty configured roots as an empty complete result;
- requested path authorization on both first and continued requests;
- cursor scope/recursive/order mismatch;
- malformed, tampered, oversized, and impossible cursors;
- candidate add/remove/rename or size change producing stale continuation;
- repository/parser/index side-effect absence;
- real stdio MCP continuation;
- existing document-discovery and directory-navigation stdio contracts remain separate.

Whole-document reading order remains a separate bounded design. Discovery order is not reading
order, structure preorder, or a sentence traversal order.
