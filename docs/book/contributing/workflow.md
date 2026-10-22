# How we work

The aim is that every change is understood by the person who merges it, and
that "done" is decided before work starts, not after.

## 1. Start from an issue

Every change starts as a GitHub issue, using one of the templates. An issue
states:

- **Problem:** what is wrong or missing, and who it affects.
- **Done when:** a check anyone can repeat. A command and its expected
  output, a test that passes, or a number to reach: "asset download under
  1 s for an 8 MB file through the studio" rather than "faster downloads".
- **Out of scope:** what this issue will not do.

Issues are grouped into a milestone for the version they target.

## 2. Write a design note when the change is structural

A design note is needed when a change touches any of:

- a dataset or snapshot format, or anything that changes output bytes;
- determinism or seed derivation;
- the HTTP API;
- a crate boundary or a new crate;
- security: authentication, authorization, or what a submitted
  configuration may do.

Design notes live in `docs/design/` as `NNNN-short-title.md`, numbered in
order, and are reviewed like code. A note covers the problem, the options
considered and why one was chosen, what changes for users and for stored
data, and how it will be tested. Once merged, a note is not rewritten; a
later note replaces it.

## 3. Branch and build

- One branch per issue, named `<type>/<short-name>`.
- Commits named `<type>(<scope>): <summary>`; see the conventions below.
- Follow the [architecture rules](rules.md).
- Performance work starts by measuring. Record the before and after numbers
  with the command that produced them, from `bop-gen bench` or a named
  script, on a named machine.

### Branch and commit names

Branches are named `<type>/<short-name>`, with a type from the table below
and a few lowercase words joined by hyphens, for example
`fix/asset-download-stall` or `feat/projects`. Branches named
`dependabot/...` are created by Dependabot.

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
`<type>(<scope>): <summary>`, for example
`fix(server): stream assets with a content length`. The summary is in the
imperative, lowercase, without a final full stop.

| Type | For |
|---|---|
| `feat` | New behaviour users can notice. |
| `fix` | A bug fix. |
| `perf` | Faster or smaller, with the same behaviour. Include the numbers. |
| `refactor` | Restructuring with no change in behaviour or output. |
| `test` | Tests only. |
| `docs` | The guide, code comments, or other documentation only. |
| `ci` | GitHub workflows and other automation. |
| `chore` | Dependencies, version bumps, and other maintenance. |

| Scope | Covers |
|---|---|
| `core`, `assets`, `physics`, `dataset`, `pipeline`, `run`, `cli`, `server` | The crate of the same name without `bop-`. |
| `wgpu`, `blenderproc` | A renderer backend crate; `blenderproc` includes the Python worker. |
| `renderers` | Both backends, or `bop-render-api`. |
| `export` | Exporters, such as the BOP export. |
| `deps` | Dependency updates. |
| `docs` | With `chore` only: the docs tooling, such as the mdBook version or `book.toml`. New or changed documentation is the `docs` type instead. |

Leave out the scope when a change spans most of the workspace. A change
that breaks stored data, the configuration format, or the API adds `!`
after the scope, as in `feat(dataset)!: ...`, and explains the break in the
commit body and the changelog.

## 4. Check before asking for review

Run the local checks in [Development and CI](../development.md). Then open a
pull request using the template, which asks for:

- the issue it closes and how its "done when" was checked;
- what changed for users, in the changelog's "Unreleased" section;
- docs updated, and the API contract regenerated if routes changed;
- any architecture rule the change breaks, and why.

CI must pass before merging.

## 5. Review

The reviewer checks that the change does what the issue asked, follows the
rules, and can be explained. A reviewer who cannot explain why a change is
correct asks until they can; that is the point of the review, not a sign of
a weak one.

When code was written with an AI assistant, the same applies: the person
opening the pull request must be able to explain every design choice in it,
and the assistant's output is reviewed like anyone else's.

## 6. Release

1. Move the changelog's "Unreleased" entries under the new version.
2. Bump `version` in the workspace `Cargo.toml`.
3. Merge, then tag `v<version>` on `master`. The release workflow checks the
   tag against the workspace version, reruns CI, and publishes the archives
   and the server image.
