use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingSample {
    pub label: f64,
    pub kind: String,
    pub source: String,
    pub features: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlModel {
    pub bias: f64,
    pub features: HashMap<String, f64>,
    pub kind_bias: HashMap<String, f64>,
    pub source_bias: HashMap<String, f64>,
}

impl Default for MlModel {
    fn default() -> Self {
        Self {
            bias: 0.0,
            features: HashMap::new(),
            kind_bias: HashMap::new(),
            source_bias: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrainOptions {
    pub iterations: usize,
    pub learning_rate: f64,
}

impl Default for TrainOptions {
    fn default() -> Self {
        Self {
            iterations: 30,
            learning_rate: 0.15,
        }
    }
}

impl MlModel {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read model file {}", path.display()))?;
        serde_json::from_str(&raw).context("parse ml model")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_string_pretty(self).context("serialize ml model")?;
        fs::write(path, raw).with_context(|| format!("write model file {}", path.display()))?;
        Ok(())
    }

    pub fn predict(&self, features: &HashMap<String, f64>, kind: &str, source: &str) -> f64 {
        let mut z = self.bias;
        for (name, value) in features {
            z += self.features.get(name).copied().unwrap_or_default() * *value;
        }
        z += self.kind_bias.get(kind).copied().unwrap_or_default();
        z += self.source_bias.get(source).copied().unwrap_or_default();
        sigmoid(z)
    }
}

pub fn train_model(samples: &[TrainingSample], options: &TrainOptions) -> MlModel {
    let mut model = MlModel::default();
    let mut feature_names = BTreeSet::new();
    let mut kind_names = BTreeSet::new();
    let mut source_names = BTreeSet::new();

    for sample in samples {
        feature_names.extend(sample.features.keys().cloned());
        kind_names.insert(sample.kind.clone());
        source_names.insert(sample.source.clone());
    }

    for name in feature_names {
        model.features.insert(name, 0.0);
    }
    for name in kind_names {
        model.kind_bias.insert(name, 0.0);
    }
    for name in source_names {
        model.source_bias.insert(name, 0.0);
    }

    for _ in 0..options.iterations {
        for sample in samples {
            let prediction = model.predict(&sample.features, &sample.kind, &sample.source);
            let error = prediction - sample.label;
            model.bias -= options.learning_rate * error;
            for (name, value) in &sample.features {
                if let Some(weight) = model.features.get_mut(name) {
                    *weight -= options.learning_rate * error * *value;
                }
            }
            if let Some(weight) = model.kind_bias.get_mut(&sample.kind) {
                *weight -= options.learning_rate * error;
            }
            if let Some(weight) = model.source_bias.get_mut(&sample.source) {
                *weight -= options.learning_rate * error;
            }
        }
    }

    model
}

fn sigmoid(value: f64) -> f64 {
    1.0 / (1.0 + (-value).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trained_model_prefers_positive_samples() {
        let positive = TrainingSample {
            label: 1.0,
            kind: "subcommand".to_string(),
            source: "history".to_string(),
            features: HashMap::from([("prefix_score".to_string(), 1.0)]),
        };
        let negative = TrainingSample {
            label: 0.0,
            kind: "subcommand".to_string(),
            source: "builtin-index".to_string(),
            features: HashMap::from([("prefix_score".to_string(), 0.2)]),
        };
        let model = train_model(
            &[positive.clone(), negative.clone()],
            &TrainOptions::default(),
        );
        assert!(
            model.predict(&positive.features, &positive.kind, &positive.source)
                > model.predict(&negative.features, &negative.kind, &negative.source)
        );
    }
}
