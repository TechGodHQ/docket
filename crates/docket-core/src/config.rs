//! Deployment configuration. Storage lives under the `/data` volume;
//! SQLite is the durable backend (docket-1).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level docket configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocketConfig {
    /// Root of the durable store. The `/data` volume in deployment;
    /// overridable for tests and local runs.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Bind port for the HTTP surface. Core and UI are strictly
    /// separated (design law (c)): this port IS the product surface a
    /// separate UI repo consumes.
    #[serde(default = "default_http_port")]
    pub http_port: u16,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("/data")
}

fn default_http_port() -> u16 {
    8951
}

impl Default for DocketConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            http_port: default_http_port(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_target_data_volume() {
        let cfg = DocketConfig::default();
        assert_eq!(cfg.data_dir, PathBuf::from("/data"));
        assert_eq!(cfg.http_port, 8951);
    }

    #[test]
    fn config_roundtrip() {
        let yaml = "data_dir: /tmp/docket-test\nhttp_port: 9000\n";
        let cfg: DocketConfig = serde_yaml::from_str(yaml).expect("parse");
        assert_eq!(cfg.http_port, 9000);
        let back = serde_yaml::to_string(&cfg).expect("serialize");
        let cfg2: DocketConfig = serde_yaml::from_str(&back).expect("re-parse");
        assert_eq!(cfg, cfg2);
    }
}
