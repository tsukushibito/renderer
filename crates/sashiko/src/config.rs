//! Engine settings resolved once by the application host at startup.
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{io, num::NonZeroUsize, path::Path};

/// Startup configuration. Hosts may deserialize JSON provided in memory.
#[derive(Debug, Default, Deserialize)]
#[serde(try_from = "Map<String, Value>")]
pub struct EngineConfig {
    parallelism: Option<NonZeroUsize>,
}

// Both sections currently have one optional setting. Reject misspelled keys at startup.
fn optional_field(mut object: Map<String, Value>, name: &str) -> Result<Option<Value>, String> {
    let value = object.remove(name);
    if let Some(key) = object.keys().next() {
        return Err(format!("unknown setting {key:?}, expected {name:?}"));
    }
    Ok(value)
}

impl TryFrom<Map<String, Value>> for EngineConfig {
    type Error = String;

    fn try_from(root: Map<String, Value>) -> Result<Self, Self::Error> {
        let Some(jobs) = optional_field(root, "jobs")? else {
            return Ok(Self::default());
        };
        let Value::Object(settings) = jobs else {
            return Err("jobs must be an object".into());
        };
        let Some(value) = optional_field(settings, "parallelism")? else {
            return Ok(Self::default());
        };
        if value.as_str() == Some("auto") {
            return Ok(Self::default());
        }
        let n = value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .and_then(NonZeroUsize::new)
            .ok_or("jobs.parallelism must be \"auto\" or a positive integer")?;
        Ok(Self {
            parallelism: Some(n),
        })
    }
}

impl EngineConfig {
    /// Read a required JSON file at the path selected by the host.
    pub fn load(path: &Path) -> io::Result<Self> {
        let read = || -> io::Result<Self> { Ok(serde_json::from_slice(&std::fs::read(path)?)?) };
        read().map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", path.display())))
    }

    /// Resolve the host-inclusive budget. Supply the host's available parallelism
    /// for automatic sizing, or None when the platform cannot report it.
    pub fn parallelism(&self, available: impl FnOnce() -> Option<NonZeroUsize>) -> NonZeroUsize {
        self.parallelism
            .or_else(available)
            .unwrap_or(NonZeroUsize::MIN)
    }

    /// Start the configured native team, querying CPU availability only for auto.
    #[cfg(all(feature = "animation", not(target_arch = "wasm32")))]
    pub fn create_jobs(&self) -> io::Result<crate::jobs::JobSystem> {
        let n = self.parallelism(|| std::thread::available_parallelism().ok());
        crate::jobs::JobSystem::new(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_budget_uses_the_host_estimate_or_one() {
        for json in [
            r#"{}"#,
            r#"{"jobs":{}}"#,
            r#"{"jobs":{"parallelism":"auto"}}"#,
        ] {
            let config: EngineConfig = serde_json::from_str(json).unwrap();
            assert_eq!(config.parallelism(|| NonZeroUsize::new(12)).get(), 12);
            assert_eq!(config.parallelism(|| None).get(), 1);
        }
    }

    #[test]
    fn fixed_budget_selects_the_requested_team() {
        for n in [1, 4] {
            let config: EngineConfig =
                serde_json::from_value(serde_json::json!({"jobs":{"parallelism":n}})).unwrap();
            assert_eq!(
                config
                    .parallelism(|| panic!("fixed budget queried CPU count"))
                    .get(),
                n
            );
            #[cfg(all(feature = "animation", not(target_arch = "wasm32")))]
            assert_eq!(config.create_jobs().unwrap().parallelism().get(), n);
        }
    }

    #[test]
    fn malformed_budgets_fail_at_load() {
        for value in ["0", "-1", "1.5", "null", "true", "\"4\"", "\"automatic\""] {
            let json = format!(r#"{{"jobs":{{"parallelism":{value}}}}}"#);
            assert!(
                serde_json::from_str::<EngineConfig>(&json).is_err(),
                "{json}"
            );
        }
        for json in ["[]", r#"{"jobs":[4]}"#, r#"{"jobs":{"paralellism":4}}"#] {
            assert!(
                serde_json::from_str::<EngineConfig>(json).is_err(),
                "{json}"
            );
        }
    }
}
