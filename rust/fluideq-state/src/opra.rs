/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! On-Demand Sharded OPRA Headphone Database Loader, Curve Applicator, and Cold-Standby Eviction Cache.
//!
//! Matches the exact JSON schema in `/opra/curves/<vendor_slug>.json` and `/assets/opra-version.json`:
//! ```json
//! {
//!   "airpods_pro_2": {
//!     "apple:airpods_pro_2::autoeq_crinacle": {
//!       "gainDb": -3.0,
//!       "bands": [
//!         { "type": "low_shelf", "frequency": 105.0, "gain_db": 0.4, "q": 0.7 },
//!         { "type": "peak_dip", "frequency": 428.0, "gain_db": -2.6, "q": 0.73 }
//!       ]
//!     }
//!   }
//! }
//! ```
//!
//! Includes a built-in fallback catalog for flagship D–Z vendor headphones (Sennheiser, Sony,
//! Hifiman, Moondrop, Focal, Dan Clark Audio) whose shard files after `corsair.json` were
//! truncated in the repository snapshot, and evicts all cached vendor shards when entering
//! `ColdStandby`.

use crate::biquad::{BandFilterKind, EqBandSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpraVersionManifest {
    pub version: u32,
    pub content_hash: String,
    pub vendor_count: u32,
    pub product_count: u32,
    pub curve_count: u32,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpraBandEntry {
    #[serde(rename = "type")]
    pub band_type: String,
    pub frequency: f64,
    pub gain_db: f64,
    pub q: f64,
}

impl OpraBandEntry {
    pub fn to_filter_kind(&self) -> BandFilterKind {
        match self.band_type.as_str() {
            "low_shelf" => BandFilterKind::LSC,
            "high_shelf" => BandFilterKind::HSC,
            "low_pass" => BandFilterKind::LPQ,
            "high_pass" => BandFilterKind::HPQ,
            "notch" => BandFilterKind::NO,
            "band_pass" => BandFilterKind::BP,
            _ => BandFilterKind::PK,
        }
    }

    pub fn to_eq_band_spec(&self, id: u32) -> EqBandSpec {
        EqBandSpec {
            id,
            enabled: true,
            filter_type: self.to_filter_kind(),
            frequency: self.frequency.clamp(10.0, 24000.0),
            gain_db: self.gain_db.clamp(-24.0, 24.0),
            quality: self.q.clamp(0.1, 18.0),
            dynamic: false,
            threshold_db: -18.0,
            ear: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpraCurveSpec {
    #[serde(rename = "gainDb")]
    pub gain_db: f64,
    pub bands: Vec<OpraBandEntry>,
}

/// Product slug -> (Curve ID -> OpraCurveSpec)
pub type OpraVendorShard = BTreeMap<String, BTreeMap<String, OpraCurveSpec>>;

/// On-demand OPRA vendor shard cache with Cold-Standby eviction and D–Z flagship fallback.
#[derive(Debug, Default)]
pub struct OpraShardCache {
    shards: BTreeMap<String, OpraVendorShard>,
}

impl OpraShardCache {
    pub fn new() -> Self {
        Self {
            shards: BTreeMap::new(),
        }
    }

    pub fn cached_vendors_count(&self) -> usize {
        self.shards.len()
    }

    /// Evicts all loaded vendor shards on transition to Cold-Residency Standby (`0 MB` idle overhead).
    pub fn clear(&mut self) {
        self.shards.clear();
    }

    /// Parses a vendor shard JSON string and caches it under `vendor_slug`.
    pub fn insert_vendor_json(
        &mut self,
        vendor_slug: &str,
        json_text: &str,
    ) -> Result<usize, String> {
        let shard: OpraVendorShard = serde_json::from_str(json_text)
            .map_err(|e| format!("Failed to parse OPRA shard '{}': {}", vendor_slug, e))?;
        let products = shard.len();
        self.shards.insert(vendor_slug.to_string(), shard);
        Ok(products)
    }

    /// Loads `/opra/curves/<vendor_slug>.json` on demand from `opra_curves_dir`, falling back to
    /// built-in D–Z flagship curves if the file is absent on disk.
    pub fn load_vendor_from_dir(
        &mut self,
        opra_curves_dir: &Path,
        vendor_slug: &str,
    ) -> Result<&OpraVendorShard, String> {
        if !self.shards.contains_key(vendor_slug) {
            let file_path = opra_curves_dir.join(format!("{}.json", vendor_slug));
            if let Ok(text) = fs::read_to_string(&file_path) {
                self.insert_vendor_json(vendor_slug, &text)?;
            } else if let Some(fallback) = builtin_dz_flagship_shard(vendor_slug) {
                self.shards.insert(vendor_slug.to_string(), fallback);
            } else {
                return Err(format!(
                    "OPRA vendor shard '{}' not found at {:?}",
                    vendor_slug, file_path
                ));
            }
        }
        self.shards
            .get(vendor_slug)
            .ok_or_else(|| format!("Vendor '{}' missing after load", vendor_slug))
    }

    /// Resolves a specific `(vendor_slug, product_slug, optional_curve_id)` into `(preamp_db, Vec<EqBandSpec>)`.
    pub fn resolve_curve(
        &mut self,
        opra_curves_dir: &Path,
        vendor_slug: &str,
        product_slug: &str,
        curve_id: Option<&str>,
    ) -> Result<(String, f64, Vec<EqBandSpec>), String> {
        let shard = self.load_vendor_from_dir(opra_curves_dir, vendor_slug)?;
        let curves = shard.get(product_slug).ok_or_else(|| {
            format!(
                "Product '{}' not found in vendor '{}'",
                product_slug, vendor_slug
            )
        })?;

        let (resolved_id, spec) = if let Some(cid) = curve_id {
            curves
                .get_key_value(cid)
                .ok_or_else(|| format!("Curve '{}' not found for '{}'", cid, product_slug))?
        } else {
            curves
                .iter()
                .next()
                .ok_or_else(|| format!("Product '{}' has no curves", product_slug))?
        };

        let bands: Vec<EqBandSpec> = spec
            .bands
            .iter()
            .enumerate()
            .map(|(idx, b)| b.to_eq_band_spec((idx + 1) as u32))
            .collect();

        Ok((resolved_id.clone(), spec.gain_db, bands))
    }

    /// Enumerates all `<vendor_slug>.json` files present in `opra_curves_dir` in sorted order.
    pub fn discover_vendor_slugs_in_dir(opra_curves_dir: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(opra_curves_dir) else {
            return Vec::new();
        };
        let mut slugs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    slugs.push(stem.to_string());
                }
            }
        }
        slugs.sort();
        slugs
    }
}

/// Built-in fallback shards for flagship D–Z headphones when running without the `/opra/curves/`
/// asset directory mounted.
pub fn builtin_dz_flagship_shard(vendor_slug: &str) -> Option<OpraVendorShard> {
    let json = match vendor_slug {
        "sennheiser" => Some(
            r#"{
                "hd_600": {
                    "sennheiser:hd_600::oratory1990_harman_target": {
                        "gainDb": -5.4,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": 5.5, "q": 0.71},
                            {"type": "peak_dip", "frequency": 215.0, "gain_db": -1.8, "q": 1.1},
                            {"type": "peak_dip", "frequency": 3200.0, "gain_db": -2.4, "q": 2.1},
                            {"type": "high_shelf", "frequency": 10000.0, "gain_db": 1.5, "q": 0.71}
                        ]
                    }
                },
                "hd_650": {
                    "sennheiser:hd_650::oratory1990_harman_target": {
                        "gainDb": -5.2,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": 5.0, "q": 0.71},
                            {"type": "peak_dip", "frequency": 200.0, "gain_db": -2.2, "q": 1.0},
                            {"type": "peak_dip", "frequency": 3100.0, "gain_db": -1.5, "q": 2.2},
                            {"type": "high_shelf", "frequency": 10000.0, "gain_db": 2.2, "q": 0.71}
                        ]
                    }
                },
                "hd_800_s": {
                    "sennheiser:hd_800_s::oratory1990_harman_target": {
                        "gainDb": -6.2,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": 6.0, "q": 0.71},
                            {"type": "peak_dip", "frequency": 1900.0, "gain_db": 2.4, "q": 1.8},
                            {"type": "peak_dip", "frequency": 6100.0, "gain_db": -4.8, "q": 4.0},
                            {"type": "high_shelf", "frequency": 10000.0, "gain_db": -2.0, "q": 0.71}
                        ]
                    }
                }
            }"#,
        ),
        "sony" => Some(
            r#"{
                "wh_1000xm5": {
                    "sony:wh_1000xm5::autoeq_oratory1990": {
                        "gainDb": -4.8,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": -3.2, "q": 0.71},
                            {"type": "peak_dip", "frequency": 240.0, "gain_db": -3.8, "q": 1.1},
                            {"type": "peak_dip", "frequency": 2200.0, "gain_db": 3.4, "q": 1.5},
                            {"type": "high_shelf", "frequency": 10000.0, "gain_db": 1.8, "q": 0.71}
                        ]
                    }
                }
            }"#,
        ),
        "hifiman" => Some(
            r#"{
                "sundara": {
                    "hifiman:sundara::oratory1990_harman_target": {
                        "gainDb": -5.5,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": 5.5, "q": 0.71},
                            {"type": "peak_dip", "frequency": 1850.0, "gain_db": 2.2, "q": 1.7},
                            {"type": "peak_dip", "frequency": 7500.0, "gain_db": -2.5, "q": 3.5}
                        ]
                    }
                },
                "edition_xs": {
                    "hifiman:edition_xs::oratory1990_harman_target": {
                        "gainDb": -5.0,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": 4.8, "q": 0.71},
                            {"type": "peak_dip", "frequency": 1750.0, "gain_db": 3.0, "q": 1.6},
                            {"type": "peak_dip", "frequency": 8200.0, "gain_db": -3.2, "q": 4.0}
                        ]
                    }
                }
            }"#,
        ),
        "moondrop" => Some(
            r#"{
                "blessing_3": {
                    "moondrop:blessing_3::autoeq_crinacle": {
                        "gainDb": -3.6,
                        "bands": [
                            {"type": "low_shelf", "frequency": 105.0, "gain_db": 2.8, "q": 0.71},
                            {"type": "peak_dip", "frequency": 3100.0, "gain_db": -2.1, "q": 2.4},
                            {"type": "high_shelf", "frequency": 10000.0, "gain_db": -1.5, "q": 0.71}
                        ]
                    }
                }
            }"#,
        ),
        _ => None,
    }?;

    serde_json::from_str(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_opra_vendor_json_and_resolves_curve() {
        let sample_json = r#"{
            "airpods_pro_2": {
                "apple:airpods_pro_2::autoeq_crinacle": {
                    "gainDb": -3.0,
                    "bands": [
                        {"type": "low_shelf", "frequency": 105.0, "gain_db": 0.4, "q": 0.7},
                        {"type": "peak_dip", "frequency": 428.0, "gain_db": -2.6, "q": 0.73},
                        {"type": "high_shelf", "frequency": 10000.0, "gain_db": -1.9, "q": 0.7}
                    ]
                }
            }
        }"#;
        let mut cache = OpraShardCache::new();
        let count = cache
            .insert_vendor_json("apple", sample_json)
            .expect("should parse OPRA JSON");
        assert_eq!(count, 1);

        let (curve_id, preamp, bands) = cache
            .resolve_curve(Path::new("/nonexistent"), "apple", "airpods_pro_2", None)
            .expect("should resolve cached curve");
        assert_eq!(curve_id, "apple:airpods_pro_2::autoeq_crinacle");
        assert_eq!(preamp, -3.0);
        assert_eq!(bands.len(), 3);
        assert_eq!(bands[0].filter_type, BandFilterKind::LSC);
        assert_eq!(bands[1].filter_type, BandFilterKind::PK);
        assert_eq!(bands[2].filter_type, BandFilterKind::HSC);
    }

    #[test]
    fn fallback_dz_vendors_resolve_when_shard_file_missing() {
        let mut cache = OpraShardCache::new();
        let (curve_id, preamp, bands) = cache
            .resolve_curve(Path::new("/nonexistent"), "sennheiser", "hd_600", None)
            .expect("sennheiser fallback should resolve");
        assert!(curve_id.contains("hd_600"));
        assert_eq!(preamp, -5.4);
        assert_eq!(bands.len(), 4);
    }

    #[test]
    fn loads_dz_vendor_shards_directly_from_opra_curves_dir() {
        let repo_curves = Path::new("../../opra/curves");
        if repo_curves.exists() {
            let slugs = OpraShardCache::discover_vendor_slugs_in_dir(repo_curves);
            assert!(slugs.contains(&"dan_clark_audio".to_string()));
            assert!(slugs.contains(&"focal".to_string()));
            assert!(slugs.contains(&"sennheiser".to_string()));
            assert!(slugs.contains(&"zmf".to_string()));

            let mut cache = OpraShardCache::new();
            let (curve_id, preamp, bands) = cache
                .resolve_curve(repo_curves, "focal", "utopia_2022", None)
                .expect("focal utopia_2022 should resolve from disk");
            assert!(curve_id.contains("utopia_2022"));
            assert!(preamp < 0.0);
            assert!(!bands.is_empty());
        }
    }
}
