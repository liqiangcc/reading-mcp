#!/usr/bin/env python3
"""List and group the pull requests merged into main since a release tag.

The output is the raw material for `docs/releases/<version>-draft.md` and for a Release Issue:
every merged PR with its number, title, merge commit, merge date and the files it touched,
grouped by area, with the grouping rule printed next to each group.

Standard library plus `gh` only. Nothing here builds, tests or touches the network beyond the
`gh` calls, and nothing here writes to the repository unless `--out` is given.

Usage:

    scripts/release-notes.py --since v0.4.11                     # text, to stdout
    scripts/release-notes.py --since v0.4.11 --format markdown   # tables, for the draft
    scripts/release-notes.py --since v0.4.11 --format json       # for other tooling
    scripts/release-notes.py --since v0.4.11 --constants         # + version constants that moved

Exit status is 0 when at least one merged PR was found, 1 otherwise (a tag with no merges after
it is almost always a wrong `--since`).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from typing import Iterable

# ---------------------------------------------------------------------------------------------
# Areas. The order here is the order the groups are printed in.
# ---------------------------------------------------------------------------------------------

TOOLS = "tools-contract"
STORAGE = "storage"
READING = "reading-segmentation"
PDF = "pdf"
EPUB = "epub"
SEARCH = "search"
PERF = "performance"
CI = "ci-scripts"
DOCS = "docs"
OTHER = "other"

GROUP_ORDER = [TOOLS, STORAGE, READING, PDF, EPUB, SEARCH, PERF, CI, DOCS, OTHER]

GROUP_TITLES = {
    TOOLS: "tools / contract",
    STORAGE: "storage",
    READING: "reading / segmentation",
    PDF: "PDF",
    EPUB: "EPUB",
    SEARCH: "search",
    PERF: "performance",
    CI: "CI / scripts",
    DOCS: "docs",
    OTHER: "other",
}

# The rule that assigns a PR to each group, printed with the group. Keep in step with
# `classify()` below: this text is what the draft cites as the grouping evidence.
PATH_RULE = (
    "the paths vote: only the `src/` paths of a PR vote (its `tests/` paths vote only when it "
    "touches no `src/` file, and `docs/`, `.github/` and `scripts/` never vote); the area with the "
    "most voting paths wins, and a tie goes to the earlier area in the order "
    "PDF, EPUB, search, storage, tools/contract, reading/segmentation"
)

GROUP_RULES = {
    DOCS: (
        "type is `docs` (or `doc`); or, when nothing else matched, every touched path is under "
        "`docs/` or is a `*.md` file"
    ),
    CI: (
        "type is `ci` (or `build`); the scope names `ci`, `deploy`, `build`, `release`, `scripts` or "
        "`workflow`; type is `bench` and the PR is not a PDF/EPUB one (a measurement harness, not "
        "a change to a subsystem); or, when nothing else matched, every touched path is under "
        "`.github/`, `scripts/`, `deploy/` or `deploy-*`"
    ),
    PDF: (
        "type is not `docs`/`ci`, and the word `pdf` (or `pdfs`) appears in the conventional-commit "
        "scope or in the title; or the path vote lands on `src/parsing/pdf*`, "
        "`src/parsing/layout_pdf*`, `tests/pdf*` or `scripts/pdf*`"
    ),
    EPUB: (
        "type is not `docs`/`ci`, and the word `epub` (or `epubs`) appears in the scope or the "
        "title; or the path vote lands on `src/parsing/epub*`, `src/application/epub*`, "
        "`tests/epub*` or `scripts/*epub*`"
    ),
    SEARCH: (
        "scope or title names `search`, `lexical`, `index` or `retrieval`; or the path vote lands "
        "on `src/infrastructure/lexical.rs`, `src/infrastructure/sqlite_search_index.rs`, "
        "`src/application/search*` or `tests/*search*`"
    ),
    STORAGE: (
        "scope or title names `storage`, `sqlite`, `cache`, `db`, `disk`, `store`, `meta` or "
        "`prune`; or the path vote lands on `src/infrastructure/sqlite*`, "
        "`src/infrastructure/cache.rs`, `src/infrastructure/memory_index.rs`, "
        "`src/infrastructure/section_layout.rs`, `src/infrastructure/observability.rs`, "
        "`tests/*storage*`, `tests/*sqlite*` or `tests/*meta*`"
    ),
    READING: (
        "scope or title names `read`, `reader`, `segmentation`, `text-unit`, `sentence`, "
        "`structure`, `ingest`, `document`, `open`, `parsing`, `html`, `locator` or `repository`; "
        "or the path vote lands on `src/application/*`, `src/domain/segmentation*`, "
        "`src/domain/text_unit*`, `src/parsing/*` (not PDF/EPUB) or `src/retrieval/*`"
    ),
    TOOLS: (
        "scope or title names `mcp`, `tool`, `contract`, `runtime`, `v2`, `server`, `error` or "
        "`hint`; or the path vote lands on `src/mcp/*`, `src/runtime/*` or "
        "`tests/*contract*` / `tests/v2_*`"
    ),
    PERF: (
        "nothing above matched and the type is `perf` (a speed change whose scope and paths name "
        "no subsystem)"
    ),
    OTHER: "nothing above matched (listed so that an unclassified PR is visible, never dropped)",
}

# scope keyword -> area, checked in this order (first hit wins)
SCOPE_AREAS = [
    (SEARCH, ("search", "lexical", "index", "retrieval")),
    (STORAGE, ("storage", "sqlite", "cache", "db", "disk", "store", "meta", "prune")),
    (
        READING,
        (
            "read",
            "reads",
            "reader",
            "readers",
            "segmentation",
            "text-units",
            "textunit",
            "sentence",
            "structure",
            "ingest",
            "document",
            "documents",
            "open",
            "parsing",
            "html",
            "locator",
            "repository",
        ),
    ),
    (TOOLS, ("mcp", "tool", "tools", "contract", "runtime", "v2", "server", "error", "hint")),
    (CI, ("ci", "deploy", "build", "release", "scripts", "workflow", "workflows")),
]

# (label, regex) pairs; the label is what the report prints as `title:<label>`
TITLE_AREAS = [
    (SEARCH, (
        ("search", r"\bsearch"),
        ("lexical", r"\blexical\b"),
        ("index", r"\bindex\b"),
        ("retrieval", r"\bretrieval\b"),
    )),
    (STORAGE, (
        ("sqlite", r"\bsqlite\b"),
        ("storage", r"\bstorage\b"),
        ("cache", r"\bcache\b"),
        ("disk", r"\bdisk\b"),
        ("store", r"\bstore\b"),
        ("prune", r"\bprune\b"),
    )),
    (READING, (
        ("segmentation", r"\bsegmentation\b"),
        ("text-unit", r"\btext[- ]units?\b"),
        ("sentence", r"\bsentence"),
        ("structure", r"\bstructure\b"),
        ("ingest", r"\bingest\b"),
        ("open", r"\bopen(?:ing)?\b"),
        ("read", r"\bread(?:s|er|ers|ing)?\b"),
        ("locator", r"\blocator"),
        ("parsing", r"\bpars(?:e|er|ing)\b"),
        ("html", r"\bhtml\b"),
    )),
    (TOOLS, (
        ("tool", r"\btools?\b"),
        ("mcp", r"\bmcp\b"),
        ("contract", r"\bcontract\b"),
        ("v2", r"\bv2\b"),
        ("hint", r"\bhint\b"),
    )),
]

# path regex -> area, checked in this order (first hit over the touched paths wins)
PATH_AREAS = [
    (PDF, (r"^src/parsing/pdf", r"^src/parsing/layout_pdf", r"^tests/pdf", r"^scripts/pdf")),
    (EPUB, (r"^src/parsing/epub", r"^src/application/epub", r"^tests/epub", r"^scripts/.*epub")),
    (
        SEARCH,
        (
            r"^src/infrastructure/lexical\.rs",
            r"^src/infrastructure/sqlite_search_index\.rs",
            r"^src/application/search",
            r"^tests/.*search",
        ),
    ),
    (
        STORAGE,
        (
            r"^src/infrastructure/sqlite",
            r"^src/infrastructure/cache\.rs",
            r"^src/infrastructure/memory_index\.rs",
            r"^src/infrastructure/section_layout\.rs",
            r"^src/infrastructure/observability\.rs",
            r"^tests/.*(storage|sqlite|meta)",
        ),
    ),
    (TOOLS, (r"^src/mcp/", r"^src/runtime/", r"^tests/.*(contract|v2_)")),
    (
        READING,
        (
            r"^src/application/",
            r"^src/domain/segmentation",
            r"^src/parsing/",
            r"^src/retrieval/",
            r"^src/domain/text_unit",
        ),
    ),
]

CI_PATH_RE = re.compile(r"^(\.github/|scripts/|deploy/|deploy-)")
DOCS_PATH_RE = re.compile(r"(^docs/|\.md$)")

PR_SUBJECT_RE = re.compile(r"^Merge pull request #(\d+) from (\S+)")
CONVENTIONAL_RE = re.compile(r"^(?P<type>[a-zA-Z]+)(?:\((?P<scope>[^)]*)\))?!?:\s*(?P<rest>.*)$")

MERGE_FORMAT = "%H%x1f%ci%x1f%s"
CONST_RE = re.compile(r'const\s+(?P<name>[A-Z0-9_]+)\s*:\s*&\'?[a-z]*\s*str\s*=\s*"(?P<value>[^"]*)"')


# ---------------------------------------------------------------------------------------------
# git / gh helpers
# ---------------------------------------------------------------------------------------------


def git(repo: str, *args: str, check: bool = True) -> str:
    proc = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise SystemExit(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout


def gh(repo: str, *args: str) -> str | None:
    proc = subprocess.run(["gh", *args], cwd=repo, capture_output=True, text=True)
    if proc.returncode != 0:
        print(f"warning: gh {' '.join(args)} failed: {proc.stderr.strip()}", file=sys.stderr)
        return None
    return proc.stdout


def resolve_tag(repo: str, tag: str) -> str:
    return git(repo, "rev-parse", "--verify", f"{tag}^{{commit}}").strip()


def describe_commit(repo: str, rev: str) -> tuple[str, str]:
    out = git(repo, "show", "-s", "--format=%h%x1f%ci", rev).strip()
    sha, date = out.split("\x1f", 1)
    return sha, date


# ---------------------------------------------------------------------------------------------
# PR extraction
# ---------------------------------------------------------------------------------------------


@dataclass
class PullRequest:
    number: int
    title: str
    subject: str
    merge_commit: str
    date: str
    files: list[str] = field(default_factory=list)
    type: str = ""
    scope: str = ""
    group: str = OTHER
    matched_by: str = ""

    @property
    def short(self) -> str:
        return self.merge_commit[:7]


def merged_prs(repo: str, base_rev: str, since_rev: str) -> list[PullRequest]:
    """Every merge commit on the first-parent chain since the tag, newest first."""
    log = git(repo, "log", "--merges", "--first-parent", f"--format={MERGE_FORMAT}",
              f"{since_rev}..{base_rev}")
    prs: list[PullRequest] = []
    for line in log.splitlines():
        if not line.strip():
            continue
        sha, date, subject = line.split("\x1f", 2)
        match = PR_SUBJECT_RE.match(subject)
        if not match:
            print(f"warning: merge commit {sha[:7]} carries no PR number: {subject}", file=sys.stderr)
            continue
        prs.append(
            PullRequest(
                number=int(match.group(1)),
                title=subject,
                subject=subject,
                merge_commit=sha,
                date=date.strip(),
            )
        )
    return prs


def attach_files(repo: str, prs: Iterable[PullRequest]) -> None:
    for pr in prs:
        out = git(repo, "diff", "--name-only", "--no-renames", f"{pr.merge_commit}^1", pr.merge_commit)
        pr.files = [line for line in out.splitlines() if line.strip()]


def attach_titles_from_gh(repo: str, prs: list[PullRequest], limit: int) -> None:
    """One `gh pr list` for all of them; `gh pr view` only for the ones it did not return."""
    out = gh(repo, "pr", "list", "--state", "merged", "--limit", str(limit),
             "--json", "number,title,mergedAt,mergeCommit")
    by_number: dict[int, dict] = {}
    if out:
        try:
            for item in json.loads(out):
                by_number[int(item["number"])] = item
        except (json.JSONDecodeError, KeyError, TypeError) as exc:
            print(f"warning: could not parse gh pr list: {exc}", file=sys.stderr)

    missing = 0
    for pr in prs:
        item = by_number.get(pr.number)
        if item is None:
            single = gh(repo, "pr", "view", str(pr.number), "--json", "number,title,mergedAt")
            if single:
                try:
                    item = json.loads(single)
                except json.JSONDecodeError:
                    item = None
        if item is None:
            missing += 1
            pr.title = pr.subject
            continue
        pr.title = item.get("title") or pr.subject
    if missing:
        print(f"warning: {missing} PR titles fell back to the merge subject (gh)", file=sys.stderr)


def open_prs(repo: str, limit: int) -> list[dict]:
    out = gh(repo, "pr", "list", "--state", "open", "--limit", str(limit),
             "--json", "number,title,createdAt,headRefName")
    if not out:
        return []
    try:
        return json.loads(out)
    except json.JSONDecodeError as exc:
        print(f"warning: could not parse gh pr list --state open: {exc}", file=sys.stderr)
        return []


# ---------------------------------------------------------------------------------------------
# Grouping
# ---------------------------------------------------------------------------------------------


def split_conventional(title: str) -> tuple[str, str]:
    match = CONVENTIONAL_RE.match(title)
    if not match:
        return "", ""
    return match.group("type").lower(), (match.group("scope") or "").lower()


def word_in(haystack: str, word: str) -> bool:
    """Whole-word, case-insensitive: `PDF`, `pdfs` and `(pdf)` all count; `is_pdf` does not."""
    return re.search(rf"\b{word}s?\b", haystack, re.IGNORECASE) is not None


def scope_tokens(scope: str) -> list[str]:
    return [tok for tok in re.split(r"[^a-z0-9-]+", scope) if tok]


def path_area(paths: list[str]) -> tuple[str, str] | None:
    """The area most of the code paths belong to.

    Only `src/` paths vote; `tests/` paths vote only when the PR touches no `src/` file, and
    `docs/`, `.github/` and `scripts/` paths never vote: a design note or a workflow that mentions
    a subsystem does not make the PR a change to that subsystem.
    """
    voters = [p for p in paths if p.startswith("src/")]
    if not voters:
        voters = [p for p in paths if p.startswith("tests/")]
    if not voters:
        return None
    counts: dict[str, int] = {}
    rank: dict[str, int] = {}
    for path in voters:
        for index, (area, patterns) in enumerate(PATH_AREAS):
            if any(re.search(pattern, path) for pattern in patterns):
                counts[area] = counts.get(area, 0) + 1
                rank.setdefault(area, index)
                break
    if not counts:
        return None
    best = max(counts, key=lambda area: (counts[area], -rank[area]))
    return best, f"paths:{counts[best]}/{len(voters)}"


def classify(pr: PullRequest) -> tuple[str, str]:
    pr.type, pr.scope = split_conventional(pr.title)
    title = pr.title

    if pr.type in ("docs", "doc"):
        return DOCS, "type:docs"
    if pr.type in ("ci", "build"):
        return CI, f"type:{pr.type}"

    # A PDF or EPUB change is a PDF or EPUB change whatever the scope says: those two areas own
    # their own parsers, their own measurement notes and their own version constants.
    if word_in(pr.scope, "pdf") or word_in(title, "pdf"):
        return PDF, "scope:pdf" if word_in(pr.scope, "pdf") else "title:pdf"
    if word_in(pr.scope, "epub") or word_in(title, "epub"):
        return EPUB, "scope:epub" if word_in(pr.scope, "epub") else "title:epub"

    # `bench(...)` is measurement infrastructure: a harness, a corpus list or a profile workflow.
    # It adds numbers, not behaviour, so it belongs with CI/scripts unless it is a PDF/EPUB bench
    # (those two areas own their own measurement notes and corpora, handled above).
    if pr.type == "bench":
        return CI, "type:bench"

    tokens = scope_tokens(pr.scope)
    for area, keywords in SCOPE_AREAS:
        for token in tokens:
            if token in keywords:
                return area, f"scope:{pr.scope}"

    hit = path_area(pr.files)
    if hit is not None:
        return hit

    if pr.files and all(CI_PATH_RE.search(path) or DOCS_PATH_RE.search(path) for path in pr.files):
        if any(CI_PATH_RE.search(path) for path in pr.files):
            return CI, "paths:harness-only"
        return DOCS, "paths:docs-only"

    for area, patterns in TITLE_AREAS:
        for label, pattern in patterns:
            if re.search(pattern, title, re.IGNORECASE):
                return area, f"title:{label}"

    if pr.type == "perf":
        return PERF, "type:perf"

    return OTHER, "unmatched"


# ---------------------------------------------------------------------------------------------
# Version constants
# ---------------------------------------------------------------------------------------------


def version_constants(repo: str, rev: str) -> dict[str, str]:
    """Every `const NAME: &'static str = "value"` under src/ whose name mentions VERSION."""
    out = git(repo, "grep", "-h", "-E", "const [A-Z0-9_]+VERSION[A-Z0-9_]*: &", rev, "--", "src",
              check=False)
    consts: dict[str, str] = {}
    for line in out.splitlines():
        match = CONST_RE.search(line)
        if match:
            consts[match.group("name")] = match.group("value")
    return consts


def constants_diff(repo: str, since_rev: str, base_rev: str) -> list[dict[str, str]]:
    before = version_constants(repo, since_rev)
    after = version_constants(repo, base_rev)
    rows = []
    for name in sorted(set(before) | set(after)):
        old = before.get(name, "")
        new = after.get(name, "")
        if old == new:
            continue
        if not old:
            status = "added"
        elif not new:
            status = "removed"
        else:
            status = "changed"
        rows.append({"name": name, "before": old or "(absent)", "after": new or "(absent)",
                     "status": status})
    return rows


# ---------------------------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------------------------


def grouped(prs: list[PullRequest]) -> dict[str, list[PullRequest]]:
    buckets: dict[str, list[PullRequest]] = {group: [] for group in GROUP_ORDER}
    for pr in prs:
        pr.group, pr.matched_by = classify(pr)
        buckets[pr.group].append(pr)
    for bucket in buckets.values():
        bucket.sort(key=lambda pr: pr.number)
    return buckets


def perf_index(buckets: dict[str, list[PullRequest]]) -> list[PullRequest]:
    """Every `perf`/`bench` PR, whatever area it was grouped in.

    A speed change is also a change to the subsystem it speeds up, so each PR is counted once, in
    its subsystem's group; this is the second view, for a release note that has to say what got
    faster and by how much.
    """
    out: list[PullRequest] = []
    for group in GROUP_ORDER:
        for pr in buckets[group]:
            if pr.type in ("perf", "bench"):
                out.append(pr)
    return sorted(out, key=lambda pr: pr.number)


def render_text(meta: dict, buckets: dict[str, list[PullRequest]], open_list: list[dict],
                constants: list[dict[str, str]], max_files: int) -> str:
    lines = [
        f"# PRs merged into {meta['base_name']} since {meta['since']}",
        "",
        f"baseline tag  {meta['since']} = {meta['since_sha']} ({meta['since_date']})",
        f"head          {meta['base_name']} = {meta['base_sha']} ({meta['base_date']})",
        f"merged PRs    {meta['count']}  ({meta['merge_commits']} first-parent merge commits, "
        f"{meta['all_merges']} merge commits in range)",
        "",
        f"path rule: {PATH_RULE}",
        "",
    ]
    for group in GROUP_ORDER:
        prs = buckets[group]
        lines.append(f"## {GROUP_TITLES[group]} — {len(prs)} PRs")
        lines.append(f"rule: {GROUP_RULES[group]}")
        for pr in prs:
            files = pr.files
            if max_files > 0:
                shown = ", ".join(files[:max_files])
                more = f" (+{len(files) - max_files} more)" if len(files) > max_files else ""
                detail = f"{shown}{more}"
            else:
                detail = f"({len(files)} files, not listed)"
            lines.append(
                f"  #{pr.number:<4} {pr.short}  {pr.date[:10]}  [{pr.matched_by}]  {pr.title}"
            )
            lines.append(f"        files: {detail}")
        lines.append("")
    if constants:
        lines.append("## Version constants that moved")
        lines.append("rule: every `const *VERSION*: &'static str` under `src/`, "
                     f"diffed {meta['since']}..{meta['base_name']}")
        for row in constants:
            lines.append(f"  {row['status']:<8} {row['name']}: {row['before']} -> {row['after']}")
        lines.append("")
    if open_list is not None:
        lines.append(f"## Open pull requests that may still join — {len(open_list)}")
        lines.append("rule: `gh pr list --state open`")
        for pr in sorted(open_list, key=lambda item: item.get("number", 0)):
            lines.append(f"  #{pr.get('number')}  {pr.get('createdAt', '')[:10]}  "
                         f"{pr.get('headRefName', '')}  {pr.get('title', '')}")
        lines.append("")
    return "\n".join(lines)


def render_markdown(meta: dict, buckets: dict[str, list[PullRequest]], open_list: list[dict],
                    constants: list[dict[str, str]], max_files: int) -> str:
    out = [
        f"# PRs merged into `{meta['base_name']}` since `{meta['since']}`",
        "",
        "| | |",
        "| --- | --- |",
        f"| baseline tag | `{meta['since']}` = `{meta['since_sha']}` ({meta['since_date']}) |",
        f"| head | `{meta['base_name']}` = `{meta['base_sha']}` ({meta['base_date']}) |",
        f"| merged PRs | {meta['count']} |",
        f"| merge commits on the first-parent chain | {meta['merge_commits']} |",
        f"| merge commits in range (branch-side merges of main included) | {meta['all_merges']} |",
        "",
        f"Path rule: {PATH_RULE}.",
        "",
        "| group | PRs | rule |",
        "| --- | ---: | --- |",
    ]
    for group in GROUP_ORDER:
        out.append(f"| {GROUP_TITLES[group]} | {len(buckets[group])} | {GROUP_RULES[group]} |")
    out.append("")
    for group in GROUP_ORDER:
        prs = buckets[group]
        out.append(f"## {GROUP_TITLES[group]} ({len(prs)})")
        out.append("")
        out.append(f"Rule: {GROUP_RULES[group]}")
        out.append("")
        if not prs:
            out.append("_No merged PR in this range._")
            out.append("")
            continue
        out.append("| PR | title | merge commit | merged | matched by | files |")
        out.append("| ---: | --- | --- | --- | --- | --- |")
        for pr in prs:
            files = pr.files
            shown = "<br>".join(f"`{path}`" for path in files[:max_files])
            if len(files) > max_files:
                shown += f"<br>+{len(files) - max_files} more"
            title = pr.title.replace("|", "\\|")
            out.append(
                f"| #{pr.number} | {title} | `{pr.short}` | {pr.date[:10]} | `{pr.matched_by}` | "
                f"{shown} |"
            )
        out.append("")
    perf = perf_index(buckets)
    if perf:
        out.append(f"## Performance and measurement PRs, cross-cutting ({len(perf)})")
        out.append("")
        out.append("Rule: type is `perf` or `bench`, whatever area it was grouped in above. A speed "
                   "change is also a change to the subsystem it speeds up, so each of these PRs is "
                   "counted once, in its area group.")
        out.append("")
        out.append("| PR | area | title |")
        out.append("| ---: | --- | --- |")
        for pr in perf:
            out.append(f"| #{pr.number} | {GROUP_TITLES[pr.group]} | "
                       f"{pr.title.replace('|', chr(92) + '|')} |")
        out.append("")
    if constants:
        out.append("## Version constants that moved")
        out.append("")
        out.append("Rule: every `const *VERSION*: &'static str` under `src/`, diffed "
                   f"`{meta['since']}..{meta['base_name']}`.")
        out.append("")
        out.append("| constant | " + f"`{meta['since']}` | `{meta['base_name']}` | status |")
        out.append("| --- | --- | --- | --- |")
        for row in constants:
            out.append(f"| `{row['name']}` | `{row['before']}` | `{row['after']}` | {row['status']} |")
        out.append("")
    if open_list is not None:
        out.append(f"## Open pull requests that may still join ({len(open_list)})")
        out.append("")
        out.append("Rule: `gh pr list --state open`.")
        out.append("")
        if open_list:
            out.append("| PR | opened | branch | title |")
            out.append("| ---: | --- | --- | --- |")
            for pr in sorted(open_list, key=lambda item: item.get("number", 0)):
                out.append(f"| #{pr.get('number')} | {pr.get('createdAt', '')[:10]} | "
                           f"`{pr.get('headRefName', '')}` | {pr.get('title', '').replace('|', chr(92) + '|')} |")
        else:
            out.append("_None open._")
        out.append("")
    return "\n".join(out)


def render_json(meta: dict, buckets: dict[str, list[PullRequest]], open_list: list[dict],
                constants: list[dict[str, str]]) -> str:
    return json.dumps(
        {
            "baseline": {
                "tag": meta["since"],
                "sha": meta["since_sha"],
                "date": meta["since_date"],
            },
            "head": {
                "ref": meta["base_name"],
                "sha": meta["base_sha"],
                "date": meta["base_date"],
            },
            "merge_commits": meta["merge_commits"],
            "all_merge_commits": meta["all_merges"],
            "count": meta["count"],
            "groups": {
                group: {
                    "title": GROUP_TITLES[group],
                    "rule": GROUP_RULES[group],
                    "count": len(buckets[group]),
                    "prs": [
                        {
                            "number": pr.number,
                            "title": pr.title,
                            "merge_commit": pr.merge_commit,
                            "date": pr.date,
                            "type": pr.type,
                            "scope": pr.scope,
                            "matched_by": pr.matched_by,
                            "files": pr.files,
                        }
                        for pr in buckets[group]
                    ],
                }
                for group in GROUP_ORDER
            },
            "version_constants": constants,
            "performance_index": [
                {"number": pr.number, "title": pr.title, "group": pr.group,
                 "merge_commit": pr.merge_commit}
                for pr in perf_index(buckets)
            ],
            "open_prs": open_list,
        },
        indent=2,
        sort_keys=False,
    )


# ---------------------------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="List and group the PRs merged into main since a release tag.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--since", required=True, metavar="TAG",
                        help="release tag to start after, e.g. v0.4.11")
    parser.add_argument("--base", default="origin/main", metavar="REF",
                        help="end of the range (default: origin/main)")
    parser.add_argument("--repo", default=".", metavar="DIR",
                        help="repository to read (default: the current directory)")
    parser.add_argument("--format", choices=("text", "markdown", "json"), default="text")
    parser.add_argument("--out", metavar="FILE", help="write the output to FILE instead of stdout")
    parser.add_argument("--max-files", type=int, default=6,
                        help="files listed per PR before a count (default: 6)")
    parser.add_argument("--constants", action="store_true",
                        help="also diff the `*VERSION*` constants between the tag and the base")
    parser.add_argument("--no-gh", action="store_true",
                        help="do not call gh; use the merge subjects as titles")
    parser.add_argument("--no-open-prs", action="store_true",
                        help="do not list the open PRs")
    parser.add_argument("--gh-limit", type=int, default=500,
                        help="page size for `gh pr list --state merged` (default: 500)")
    args = parser.parse_args(argv)

    since_sha = resolve_tag(args.repo, args.since)
    base_sha = git(args.repo, "rev-parse", "--verify", f"{args.base}^{{commit}}").strip()
    since_short, since_date = describe_commit(args.repo, since_sha)
    base_short, base_date = describe_commit(args.repo, base_sha)

    prs = merged_prs(args.repo, base_sha, since_sha)
    attach_files(args.repo, prs)
    if not args.no_gh:
        attach_titles_from_gh(args.repo, prs, args.gh_limit)

    merge_commits = len(git(args.repo, "log", "--merges", "--first-parent", "--format=%H",
                            f"{since_sha}..{base_sha}").splitlines())
    all_merges = len(git(args.repo, "log", "--merges", "--format=%H",
                         f"{since_sha}..{base_sha}").splitlines())

    buckets = grouped(prs)
    constants = constants_diff(args.repo, since_sha, base_sha) if args.constants else []
    open_list: list[dict] | None = None
    if not args.no_open_prs and not args.no_gh:
        open_list = open_prs(args.repo, 100)

    meta = {
        "since": args.since,
        "since_sha": since_short,
        "since_date": since_date,
        "base_name": args.base,
        "base_sha": base_short,
        "base_date": base_date,
        "count": len(prs),
        "merge_commits": merge_commits,
        "all_merges": all_merges,
    }

    if args.format == "json":
        text = render_json(meta, buckets, open_list, constants)
    elif args.format == "markdown":
        text = render_markdown(meta, buckets, open_list, constants, args.max_files)
    else:
        text = render_text(meta, buckets, open_list, constants, args.max_files)

    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(text if text.endswith("\n") else text + "\n")
    else:
        print(text)

    if args.format != "json":
        counts = ", ".join(f"{GROUP_TITLES[g]}={len(buckets[g])}" for g in GROUP_ORDER)
        print(f"\n{len(prs)} merged PRs since {args.since}: {counts}", file=sys.stderr)
        if open_list is not None:
            print(f"{len(open_list)} open PRs may still join.", file=sys.stderr)

    return 0 if prs else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
