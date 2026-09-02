# Authorized Source Workspace Directory Navigation

`list_directory` is the navigation surface for explicitly authorized local roots. It discovers
only the roots (when `path` is omitted) or the direct children of one known directory. Direct
children are returned as either `directory` or `document`; a directory is never a `DocumentId` and
is never parsed, cached, persisted, or indexed.

## Request and response

```json
{
  "path": "/authorized/workspace/papers",
  "max_results": 100,
  "cursor": "dir1..."
}
```

`path` is optional. A missing path lists the current canonical authorized roots. A supplied path
must be a directory below a current authorized root. The server canonicalizes the root, requested
directory, and every child, then applies path-component containment. Parent traversal components,
external symlink targets, and sibling paths with a shared string prefix are not authorized.

```json
{
  "entries": [
    {"kind": "directory", "path": "/authorized/workspace/papers/kafka", "name": "kafka"},
    {"kind": "document", "path": "/authorized/workspace/papers/readme.md", "name": "readme.md", "media_type": "text/markdown", "size_bytes": 42}
  ],
  "complete": true,
  "next_cursor": null
}
```

Directory and document entries have distinct identities. A document entry can be passed to
`open_document`; a directory path can be passed to `list_directory` and then to
`list_documents` for document candidates.

## Ordering and continuation

Entries are bounded to at most 1,000 per page and are sorted by canonical path under
`directory-path/v1`. This is directory discovery order, not document discovery order,
`body-order/v1`, structure preorder, or sentence reading order.

`dir1` cursors use `directory-cursor/v1`. They bind the canonical authorized roots, canonical
requested path, ordered entry manifest, total entries, and next index; they do not bind a page
size or a document identity. A continuation re-canonicalizes and re-authorizes the current roots
and scope, then rebuilds the manifest. Root changes, child add/remove/rename, or discovery-visible
metadata changes return `STALE_CURSOR`; callers must restart from the first page. `complete=false`
always includes a cursor, while `complete=true` never does.

The capability is read-only and does not provide directory creation, deletion, rename, upload, or
arbitrary filesystem browsing.
