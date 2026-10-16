# Command line

```text
bpp [--log-format human|json] SUBCOMMAND

bpp doctor [--renderer wgpu|blenderproc|all]
bpp schema
bpp prepare-assets --config FILE
bpp simulate --config FILE --output CORPUS [--seed N] [--scenes N]
bpp render --config FILE --snapshots CORPUS --renderer BACKEND --output DATASET [--seed N] [--scenes N]
bpp generate --config FILE --renderer BACKEND --output DATASET [--resume] [--seed N] [--scenes N]
bpp validate DATASET
bpp export bop DATASET --output DIRECTORY     (not implemented yet)
bpp bench --config FILE --renderers BACKEND[,BACKEND] --output DIRECTORY   (not implemented yet)
```

Each command prints one JSON result object on stdout. Logs go to stderr.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 2 | Command-line or configuration error |
| 3 | The requested capability or runtime is unavailable |
| 4 | The requested number of valid scenes could not be produced |
| 5 | Dataset validation or export failure |
| 6 | Renderer or worker failure |

## Logging

Logs are human-readable by default. `--log-format json` writes one JSON object
per line instead. The `BPP_LOG` environment variable sets the filter using
`tracing` syntax and defaults to `info`. Use `BPP_LOG=debug` to see each
simulated and committed scene.

## Environment variables

| Variable | Effect |
|---|---|
| `BPP_LOG` | Log filter, for example `debug` or `bpp_pipeline=debug`. |
| `BPP_BLENDERPROC_EXECUTABLE` | BlenderProc executable to run instead of the configured one. |
| `BPP_BLENDERPROC_WORKER` | Worker script for `doctor`. Otherwise `doctor` looks in `blenderproc-worker/` next to the binary, then in the source tree. |
