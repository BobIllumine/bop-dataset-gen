# Design notes

A design note records a structural decision before it is built: what problem
it solves, which options were considered, why one was chosen, and what it
changes. When a note is needed is described in the contributor guide,
"How we work".

Notes are named `NNNN-short-title.md`, numbered in order. Once merged, a note
is not rewritten; a later note replaces it and says so.

## Template

```markdown
# NNNN: Title

Status: proposed | accepted | replaced by NNNN

## Problem

What is wrong or missing, and why it matters now.

## Options

Each option considered, with what it costs and what it gives.

## Decision

The chosen option and the reason.

## Consequences

What changes for users, for stored data and its compatibility, and for other
crates. What becomes harder.

## Testing

How the change will be shown to work.
```
