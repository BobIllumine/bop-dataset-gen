//! Command-line entry point for BPP dataset generation.

use std::process::ExitCode;

fn main() -> ExitCode {
    run(std::env::args().skip(1))
}

fn run(arguments: impl IntoIterator<Item = String>) -> ExitCode {
    let arguments: Vec<_> = arguments.into_iter().collect();
    match arguments.as_slice() {
        [command] if command == "doctor" => doctor("wgpu"),
        [command, flag, renderer] if command == "doctor" && flag == "--renderer" => {
            doctor(renderer)
        }
        _ => {
            eprintln!("usage: bpp doctor [--renderer wgpu]");
            ExitCode::from(2)
        }
    }
}

#[cfg(feature = "wgpu")]
fn doctor(renderer: &str) -> ExitCode {
    if renderer != "wgpu" {
        eprintln!("renderer {renderer:?} is unavailable in this build");
        return ExitCode::from(3);
    }
    let config = bpp_core::WgpuRendererConfig::try_new(
        bpp_core::WgpuDevicePreference::Automatic,
        bpp_core::WgpuAdapterPreference::HighPerformance,
        2048,
        false,
    )
    .expect("fixed doctor configuration");
    match bpp_render_wgpu::WgpuRenderer::try_new(config) {
        Ok(renderer) => {
            let value = serde_json::json!({
                "renderer": "wgpu",
                "available": true,
                "backend": renderer.info(),
                "capabilities": renderer.capabilities(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("report serialization")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("wgpu unavailable: {error}");
            ExitCode::from(3)
        }
    }
}

#[cfg(not(feature = "wgpu"))]
fn doctor(renderer: &str) -> ExitCode {
    eprintln!("renderer {renderer:?} is unavailable in this build");
    ExitCode::from(3)
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn invalid_doctor_invocations_use_the_usage_exit_code() {
        assert_eq!(run([]), std::process::ExitCode::from(2));
        assert_eq!(
            run(["doctor".to_owned(), "--wat".to_owned()]),
            std::process::ExitCode::from(2)
        );
    }

    #[test]
    fn unavailable_renderer_uses_the_capability_exit_code() {
        assert_eq!(
            run([
                "doctor".to_owned(),
                "--renderer".to_owned(),
                "unknown".to_owned()
            ]),
            std::process::ExitCode::from(3)
        );
    }
}
