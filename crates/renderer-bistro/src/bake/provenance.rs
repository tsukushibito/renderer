//! Recipe identity is computed exclusively from inputs. Artifact identity below
//! binds this report to the index and its content-addressed dependencies.
use crate::Error;
use renderer_bake::{BakeExecution, Lighting, ProbeSettings};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io;

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn execution(e: &BakeExecution) -> Value {
    json!({
        "method": format!("{:?}", e.method),
        "device": e.device,
        "driver": e.driver,
        "shader_sha256": e.shader.map(digest),
    })
}

fn host() -> Value {
    #[cfg(windows)]
    let (cpu, os_release) = (
        std::env::var("PROCESSOR_IDENTIFIER").ok(),
        std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ver"])
            .output()
            .ok()
            .filter(|v| v.status.success())
            .map(|v| String::from_utf8_lossy(&v.stdout).trim().to_string()),
    );
    #[cfg(target_os = "linux")]
    let (cpu, os_release) = (
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .map(|info| {
                // Exclude the changing frequency and repeated per-core records.
                info.lines()
                    .filter(|line| {
                        line.starts_with("model name")
                            || line.starts_with("microcode")
                            || line.starts_with("flags")
                            || line.starts_with("CPU part")
                            || line.starts_with("Features")
                    })
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s| !s.is_empty()),
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_string()),
    );
    #[cfg(not(any(windows, target_os = "linux")))]
    let (cpu, os_release): (Option<String>, Option<String>) = (None, None);
    json!({"os":std::env::consts::OS,"os_release":os_release,"arch":std::env::consts::ARCH,"cpu":cpu})
}

fn keyed(recipe: Value) -> Result<Value, Error> {
    let sha = digest(&serde_json::to_vec(&recipe)?);
    Ok(json!({"build_key_sha256":sha,"recipe":recipe}))
}

pub(super) fn record<const N: usize>(
    identity: &str,
    lights: [Lighting; N],
    settings: ProbeSettings,
    rays_per_texel: u32,
    surface: &BakeExecution,
    probes: &BakeExecution,
    index_sha256: &str,
) -> Result<Value, Error> {
    // Includes Rust codegen, enabled features, CLI recipe and embedded resources.
    let mut binary = std::fs::File::open(std::env::current_exe()?)?;
    let mut hash = Sha256::new();
    io::copy(&mut binary, &mut hash)?;
    let lights: Vec<_> = lights.iter().map(|l| json!({
        "sky_nits":l.sky_rgb_nits, "sun_rgb_lux":l.sun.rgb_lux(),
        "sun_direction":l.sun.direction(),
        "environment":l.environment.map(|v| json!({"texture":v.texture,"yaw":v.yaw_radians})),
    })).collect();
    let common = json!({
        "schema":1, "scene_lighting_identity":identity,"lighting":lights,"host":host(),
        "tool":{"package":env!("CARGO_PKG_NAME"),"version":env!("CARGO_PKG_VERSION"),
            "executable_sha256":format!("{:x}",hash.finalize())},
    });
    let surface = keyed(
        json!({"stage":"surface","common":common,"execution":execution(surface),
        "rays_per_texel":rays_per_texel}),
    )?;
    let probes = keyed(
        json!({"stage":"probe","common":common,"execution":execution(probes),
        "spacing":settings.spacing,"depth":settings.max_depth,"rays_per_probe":settings.rays_per_probe,
        "sh_order":format!("{:?}",settings.sh_order)}),
    )?;
    Ok(
        json!({"schema":1,"requested_backend":format!("{:?}",settings.backend),
        "surface":surface,"probes":probes,"index_sha256":index_sha256}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer_bake::{BakeBackend, BakeScene};

    #[test]
    fn cpu_probe_rebake_and_recipe_identity_are_independent_of_output_identity() -> Result<(), Error>
    {
        let scene = BakeScene::new(Vec::new(), Vec::new(), Vec::new())?;
        let lights = [Lighting::uniform_sky([1., 2., 3.])];
        let mut settings = ProbeSettings {
            backend: BakeBackend::Cpu,
            rays_per_probe: 8,
            max_depth: 0,
            ..Default::default()
        };
        let run = |settings| -> Result<_, Error> {
            let bake = scene.prepare_probe_bake(lights, settings)?;
            let path = bake.execution().clone();
            let mut bytes = Vec::new();
            bake.bake()?.write(&mut bytes)?;
            Ok((path, digest(&bytes)))
        };
        let (cpu, first) = run(settings)?;
        assert_eq!(run(settings)?.1, first);
        let before = record("fixture", lights, settings, 4, &cpu, &cpu, &first)?;
        let after = record("fixture", lights, settings, 4, &cpu, &cpu, "other-output")?;
        assert_eq!(before["probes"], after["probes"]);
        settings.spacing *= 2.;
        let changed = record("fixture", lights, settings, 4, &cpu, &cpu, &first)?;
        assert_ne!(before["probes"], changed["probes"]);
        assert_eq!(before["surface"], changed["surface"]);
        Ok(())
    }
}
