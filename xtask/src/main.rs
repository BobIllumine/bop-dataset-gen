//! Repository-maintenance command entry point.

use std::{
    ffi::OsString,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::ExitCode,
};

const USAGE: &str = "usage: xtask schema [--check] [--output PATH]";

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchemaMode {
    Write,
    Check,
}

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Schema { mode: SchemaMode, output: PathBuf },
}

#[derive(Debug)]
enum SchemaError {
    Drift(std::path::PathBuf),
    Io {
        path: std::path::PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Drift(path) => write!(formatter, "schema drift: {}", path.display()),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SchemaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Drift(_) => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let Command::Schema { mode, output } = parse_args(args)?;
    let generated = bpp_core::run_config_json_schema();
    write_or_check_schema(&output, generated.as_bytes(), mode).map_err(|error| {
        if matches!(error, SchemaError::Drift(_)) {
            format!("{error}; run `cargo run -p xtask -- schema`")
        } else {
            error.to_string()
        }
    })?;
    if matches!(mode, SchemaMode::Write) {
        println!("wrote {}", output.display());
    }
    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("schema")) {
        return Err(USAGE.to_owned());
    }

    let mut mode = SchemaMode::Write;
    let mut output = None;
    while let Some(argument) = args.next() {
        if argument == "--check" {
            if matches!(mode, SchemaMode::Check) {
                return Err(USAGE.to_owned());
            }
            mode = SchemaMode::Check;
        } else if argument == "--output" {
            if output.is_some() {
                return Err(USAGE.to_owned());
            }
            output = Some(PathBuf::from(args.next().ok_or_else(|| USAGE.to_owned())?));
        } else {
            return Err(USAGE.to_owned());
        }
    }

    Ok(Command::Schema {
        mode,
        output: output.unwrap_or_else(default_schema_output),
    })
}

fn default_schema_output() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest directory must be inside the repository root")
        .join("schemas/run-config.schema.json")
}

fn write_or_check_schema(
    output: &Path,
    generated: &[u8],
    mode: SchemaMode,
) -> Result<(), SchemaError> {
    if matches!(mode, SchemaMode::Check) {
        return match fs::read(output) {
            Ok(current) if current == generated => Ok(()),
            Ok(_) => Err(SchemaError::Drift(output.to_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(SchemaError::Drift(output.to_owned()))
            }
            Err(source) => Err(SchemaError::Io {
                path: output.to_owned(),
                source,
            }),
        };
    }

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|source| SchemaError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(output, generated).map_err(|source| SchemaError::Io {
        path: output.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{Command, SchemaMode, default_schema_output, parse_args, write_or_check_schema};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("bpp-xtask-schema-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("unique test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove test directory");
        }
    }

    #[test]
    fn write_creates_parent_directories_and_exact_schema_bytes() {
        let temp = TestDir::new();
        let output = temp.path().join("nested/run-config.schema.json");

        write_or_check_schema(&output, b"{\n}\n", SchemaMode::Write).expect("write schema");

        assert_eq!(fs::read(output).expect("read schema"), b"{\n}\n");
    }

    #[test]
    fn check_accepts_exact_bytes_without_rewriting_the_file() {
        let temp = TestDir::new();
        let output = temp.path().join("run-config.schema.json");
        fs::write(&output, b"{\n}\n").expect("seed schema");
        let before = fs::metadata(&output)
            .expect("schema metadata")
            .modified()
            .expect("modification time");

        write_or_check_schema(&output, b"{\n}\n", SchemaMode::Check).expect("clean check");

        assert_eq!(fs::read(&output).expect("read schema"), b"{\n}\n");
        assert_eq!(
            fs::metadata(output)
                .expect("schema metadata")
                .modified()
                .expect("modification time"),
            before
        );
    }

    #[test]
    fn check_reports_drift_without_rewriting_the_file() {
        let temp = TestDir::new();
        let output = temp.path().join("run-config.schema.json");
        fs::write(&output, b"stale\n").expect("seed stale schema");

        let error = write_or_check_schema(&output, b"fresh\n", SchemaMode::Check)
            .expect_err("drift must fail");

        assert_eq!(
            error.to_string(),
            format!("schema drift: {}", output.display())
        );
        assert_eq!(fs::read(output).expect("read stale schema"), b"stale\n");
    }

    #[test]
    fn schema_cli_parses_check_and_explicit_output() {
        let output = PathBuf::from("custom/schema.json");

        let command = parse_args([
            "schema".into(),
            "--check".into(),
            "--output".into(),
            output.clone().into_os_string(),
        ])
        .expect("valid schema command");

        assert_eq!(
            command,
            Command::Schema {
                mode: SchemaMode::Check,
                output
            }
        );
    }

    #[test]
    fn default_output_is_anchored_to_the_repository_root() {
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask is in repository root");

        assert_eq!(
            default_schema_output(),
            repository_root.join("schemas/run-config.schema.json")
        );
        assert!(default_schema_output().is_absolute());
    }
}
