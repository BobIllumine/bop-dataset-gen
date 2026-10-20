//! Checks a submitted configuration and rewrites what the server controls.

use std::path::{Component, Path};

use bop_run::{LoadedRun, RendererKind};

use crate::settings::Settings;

/// A configuration the server accepted.
#[derive(Debug)]
pub struct AcceptedConfig {
    /// Configuration text with `[run] cache_path` pointing at the server cache.
    pub resolved_toml: String,
    /// Requested scene count.
    pub total_scenes: u32,
    /// Asset storage keys the run reads, relative to `assets/`.
    pub asset_paths: Vec<String>,
}

/// Parses `renderer` as an API renderer name compiled into this build.
///
/// # Errors
///
/// Returns a message for an unknown or uncompiled renderer.
pub fn parse_renderer(renderer: &str) -> Result<RendererKind, String> {
    let kind = match renderer {
        "wgpu" => RendererKind::Wgpu,
        "blenderproc" => RendererKind::BlenderProc,
        other => return Err(format!("unknown renderer {other:?}")),
    };
    let compiled = match kind {
        RendererKind::Wgpu => cfg!(feature = "wgpu"),
        RendererKind::BlenderProc => cfg!(feature = "blenderproc"),
    };
    if compiled {
        Ok(kind)
    } else {
        Err(format!(
            "renderer {renderer:?} is not available in this build"
        ))
    }
}

/// Validates `config` for this server.
///
/// Asset `source_path`s and the `[appearance]` environment map must be
/// relative paths without `..`; they name files uploaded below `assets/`. `[run] cache_path` is replaced with the server's
/// cache. `BlenderProc` paths in the configuration are ignored at run time.
///
/// # Errors
///
/// Returns a message describing the first problem.
pub fn accept(config: &str, settings: &Settings) -> Result<AcceptedConfig, String> {
    let mut table = config
        .parse::<toml::Table>()
        .map_err(|error| format!("config is not valid TOML: {error}"))?;
    let run = table
        .get_mut("run")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| "config has no [run] table".to_owned())?;
    run.insert(
        "cache_path".to_owned(),
        toml::Value::String(settings.cache_dir().display().to_string()),
    );
    let resolved_toml = toml::to_string(&table)
        .map_err(|error| format!("cannot write resolved config: {error}"))?;
    let loaded = LoadedRun::parse(resolved_toml.clone().into_bytes(), "/")
        .map_err(|error| format!("invalid config: {error}"))?;
    let plan = loaded.config().plan();
    let total_scenes = plan.run().valid_scene_count();
    if total_scenes > settings.max_scenes {
        return Err(format!(
            "valid_scene_count {total_scenes} exceeds this server's limit of {}",
            settings.max_scenes
        ));
    }
    let mut asset_paths = plan
        .assets()
        .iter()
        .map(|asset| storage_key(asset.source_path()))
        .chain(plan.appearance().environment_path().map(storage_key))
        .collect::<Result<Vec<_>, _>>()?;
    asset_paths.sort();
    asset_paths.dedup();
    Ok(AcceptedConfig {
        resolved_toml,
        total_scenes,
        asset_paths,
    })
}

/// Converts a relative path of normal components to a `/`-separated key.
///
/// # Errors
///
/// Returns a message for absolute paths, `..`, or non-UTF-8 components.
pub fn storage_key(path: &Path) -> Result<String, String> {
    let parts = path
        .components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .filter(|part| !part.starts_with('.'))
                .ok_or_else(|| format!("asset path {} has an invalid component", path.display())),
            _ => Err(format!(
                "asset path {} must be relative to the asset library, without ..",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parts.is_empty() {
        return Err("asset path is empty".to_owned());
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::storage_key;

    #[test]
    fn asset_keys_must_stay_inside_the_library() {
        assert_eq!(
            storage_key(Path::new("cup/cup.glb")).unwrap(),
            "cup/cup.glb"
        );
        assert!(storage_key(Path::new("./cup.glb")).is_err());
        assert!(storage_key(Path::new("/etc/passwd")).is_err());
        assert!(storage_key(Path::new("cup/../../secret")).is_err());
        assert!(storage_key(Path::new(".hidden/x.glb")).is_err());
        assert!(storage_key(Path::new("")).is_err());
    }
}
