# Release-note fragments

Add one new Markdown fragment for each user-facing feature, bug fix, security
change, operational behavior change, or user-facing documentation change.
Fragments are append-only: create a new file instead of editing one that has
already shipped.

```md
---
category: fixed
audience: users, operators
area: webui
action: none
breaking: false
---
The torrent list now keeps the selected rows when filters change, so bulk actions can be applied to the intended matches.
```

The frontmatter records the category (`added`, `changed`, `fixed`, `security`,
`removed`, or `deprecated`), audience (`users`, `operators`, or both), product
area, required action (or `none`), and breaking-change status. Write the body for
the person using or operating TorrentNG; keep it between 30 and 400 characters,
start with a capitalized sentence, and end with punctuation.

The pull-request and main-branch checks require a valid fragment or the exact
opt-out marker `release-note: none` on its own line for internal-only work. For
direct commits, put the marker on its own line in the commit message. The tag
workflow assembles fragments since the previous published release and uses the
same text for the GitHub release and Discord announcement. If a release range
contains no user-facing changes, its release page says so explicitly.

Preview a release range with:

```bash
python3 scripts/release_notes.py preview --base <previous-tag> --head <tag>
```
