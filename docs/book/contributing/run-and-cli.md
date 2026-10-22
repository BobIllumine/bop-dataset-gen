# bop-run and bop-cli

`crates/bop-run/`, `crates/bop-cli/` · code reference:
[bop-run](../api/bop_run/index.html), [bop-gen](../api/bop_gen/index.html)

## bop-run

The one place that turns a loaded run into a running pipeline. It is shared
by the command line and the job server, so both run jobs the same way.

| Item | Role |
|---|---|
| `LoadedRun` | The exact configuration bytes, the validated `RunConfig`, and the directory relative paths resolve against. |
| `RendererKind` | `Wgpu` or `BlenderProc`, with the name the CLI and API use. |
| `generate` | Builds the renderers, picks the serial or parallel pipeline, and runs it. |
| `renderer_pool` | One renderer per configured render worker. |
| `GenerateOptions` | What a run needs beyond its configuration: cancellation, a commit observer, BlenderProc path overrides. |
| `GenerateError::exit_code` | Maps generation failures to `bop-gen` exit codes. |

Backends are behind the `wgpu` and `blenderproc` features. When one is not
compiled in, choosing it returns `GenerateError::Unavailable` instead of
failing to build.

## bop-cli

`src/main.rs` is the `bop-gen` binary: argument parsing (`parse` and the
`parse_*` helpers), `Overrides` for the `--seed` and `--scenes` flags, which
rewrite the configuration text before it is parsed, logging setup, and one
function per subcommand (`generate`, `simulate`, `render`, `validate`, `export_bop`, `doctor`, and
others). `src/bench.rs` is `bop-gen bench`; `src/interrupt.rs` turns the
first Ctrl-C or SIGTERM into cancellation.

Subcommands only parse and report. The work is done by `bop-run`,
`bop-pipeline`, and `bop-dataset`; if a subcommand starts doing work itself,
that code belongs in one of them.

The user-facing commands, exit codes, and environment variables are
documented in [Command line](../cli.md); `tests/exit_codes.rs` pins the exit
codes.

## Caveats

- `main.rs` is about 1,060 lines before its tests: hand-written argument
  parsing and every subcommand in one file. It is due to be split into one module per subcommand.
- `bop-gen render` builds its renderers itself (`render_wgpu`,
  `render_blenderproc` call the backend constructors through
  `bop_run::renderer_pool`), because `bop-run` only covers full generation.
  It is the one path where the CLI still names backends directly.
