use std::collections::HashMap;

use serde::Deserialize;

use super::{PipelineError, PrepareIndirectList, schema};

/// Application values fixed at a pipeline preparation boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrepareInputs<'a> {
    pub values: &'a [PrepareParameter<'a>],
    pub indirect_lists: &'a [PrepareIndirectList<'a>],
}

/// A value supplied for a YAML field written as `{param: name}`.
#[derive(Clone, Copy, Debug)]
pub struct PrepareParameter<'a> {
    pub name: &'a str,
    pub value: PrepareValue,
}

/// Accepted parameter types. The YAML field determines the required type.
#[derive(Clone, Copy, Debug)]
pub enum PrepareValue {
    Size(u64),
    Grid([u32; 2]),
    Color([f32; 4]),
    Depth(f32),
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum Parameter<T> {
    Literal(T),
    Named(ParameterName),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParameterName {
    param: String,
}

pub(super) trait ParameterType: Sized {
    fn from_value(value: PrepareValue) -> Option<Self>;
}

impl ParameterType for u64 {
    fn from_value(value: PrepareValue) -> Option<Self> {
        match value {
            PrepareValue::Size(value) => Some(value),
            _ => None,
        }
    }
}

impl ParameterType for [u32; 2] {
    fn from_value(value: PrepareValue) -> Option<Self> {
        match value {
            PrepareValue::Grid(value) => Some(value),
            _ => None,
        }
    }
}

impl ParameterType for [f32; 4] {
    fn from_value(value: PrepareValue) -> Option<Self> {
        match value {
            PrepareValue::Color(value) => Some(value),
            _ => None,
        }
    }
}

impl ParameterType for f32 {
    fn from_value(value: PrepareValue) -> Option<Self> {
        match value {
            PrepareValue::Depth(value) => Some(value),
            _ => None,
        }
    }
}

pub(super) struct Parameters<'a>(HashMap<&'a str, PrepareValue>);

impl<'a> Parameters<'a> {
    pub(super) fn new(values: &[PrepareParameter<'a>]) -> Result<Self, PipelineError> {
        let mut parameters = HashMap::with_capacity(values.len());
        for &PrepareParameter { name, value } in values {
            if name.trim().is_empty() || parameters.insert(name, value).is_some() {
                return Err(PipelineError::Invalid(format!(
                    "empty or duplicate preparation parameter {name:?}"
                )));
            }
        }
        Ok(Self(parameters))
    }

    pub(super) fn resolve<T: ParameterType>(&self, value: Parameter<T>) -> Result<T, String> {
        match value {
            Parameter::Literal(value) => Ok(value),
            Parameter::Named(ParameterName { param }) => {
                let value = self
                    .0
                    .get(param.as_str())
                    .ok_or_else(|| format!("missing preparation parameter {param:?}"))?;
                T::from_value(*value)
                    .ok_or_else(|| format!("preparation parameter {param:?} has the wrong type"))
            }
        }
    }
}

pub(super) fn assemble(fragments: &[&str]) -> Result<schema::Config, PipelineError> {
    if fragments
        .iter()
        .fold(0usize, |len, fragment| len.saturating_add(fragment.len()))
        > schema::MAX_YAML_BYTES
    {
        return Err(PipelineError::Invalid("YAML exceeds 1 MiB".into()));
    }

    let mut config = schema::Config::default();
    for fragment in fragments {
        let fragment = schema::parse(fragment)?;
        if fragment.version != 1 {
            return Err(PipelineError::Invalid(format!(
                "unsupported pipeline version {}",
                fragment.version
            )));
        }
        config.resources.extend(fragment.resources);
        config.draw_lists.extend(fragment.draw_lists);
        for (name, batches) in fragment.raster_batches {
            match config.raster_batches.entry(name) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(batches);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    return Err(PipelineError::Invalid(format!(
                        "duplicate raster batch list {:?}",
                        entry.key()
                    )));
                }
            }
        }
        config.passes.extend(fragment.passes);
        if let Some(output) = fragment.output
            && config.output.replace(output).is_some()
        {
            return Err(PipelineError::Invalid(
                "pipeline fragments must declare exactly one output".into(),
            ));
        }
    }
    Ok(config)
}
