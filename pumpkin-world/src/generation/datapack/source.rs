//! Generic datapack scanner for world-generation registry JSON

use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use tracing::{debug, warn};

const MAX_CONTAINER_DEPTH: usize = 3;

/// A world-generation registry category and where its JSON lives under `data/<ns>/`
///
/// The `dimension` registry sits directly under `data/<ns>/dimension`, while the
/// rest live under `data/<ns>/worldgen/<category>`
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    /// `worldgen/noise_settings` → [`ChunkGeneratorSettings`] equivalents
    NoiseSettings,
    /// `worldgen/density_function` → nodes of the noise DAG
    DensityFunction,
    /// `worldgen/noise` → `DoublePerlinNoiseParameters`
    Noise,
    /// `dimension` → dimension definition (generator + biome source)
    Dimension,
}

impl Category {
    /// Every category Phase 0 indexes
    pub const ALL: &'static [Self] = &[
        Self::NoiseSettings,
        Self::DensityFunction,
        Self::Noise,
        Self::Dimension,
    ];

    /// Path segments under `data/<namespace>/` that identify this category
    const fn path_segments(self) -> &'static [&'static str] {
        match self {
            Self::NoiseSettings => &["worldgen", "noise_settings"],
            Self::DensityFunction => &["worldgen", "density_function"],
            Self::Noise => &["worldgen", "noise"],
            Self::Dimension => &["dimension"],
        }
    }

    /// Human-readable label for logging
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoiseSettings => "noise_settings",
            Self::DensityFunction => "density_function",
            Self::Noise => "noise",
            Self::Dimension => "dimension",
        }
    }
}

/// Raw (unparsed) world-generation JSON collected from datapack sources
#[derive(Default)]
pub struct RawWorldgen {
    entries: HashMap<Category, HashMap<String, String>>,
}

impl RawWorldgen {
    /// Access the raw JSON strings for a category (`namespace:path` → JSON)
    #[must_use]
    pub fn get(&self, category: Category) -> Option<&HashMap<String, String>> {
        self.entries.get(&category)
    }

    /// Number of indexed entries in a category
    #[must_use]
    pub fn count(&self, category: Category) -> usize {
        self.entries.get(&category).map_or(0, HashMap::len)
    }

    /// True if nothing was found in any category
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.values().all(HashMap::is_empty)
    }

    fn insert(&mut self, category: Category, id: String, json: String) {
        // Later sources override earlier ones, mirroring datapack precedence
        self.entries.entry(category).or_default().insert(id, json);
    }
}

/// Scan the given datapack sources and index all world-generation JSON
#[must_use]
pub fn scan(sources: &[PathBuf]) -> RawWorldgen {
    let mut out = RawWorldgen::default();
    for source in sources {
        collect_from_path(source, 0, &mut out);
    }
    out
}

/// Resolve a single configured path into the datapacks it represents and scan them
fn collect_from_path(path: &Path, depth: usize, out: &mut RawWorldgen) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            scan_zip(path, out);
        }
        return;
    }
    if !path.is_dir() {
        return;
    }
    // An unpacked datapack has a top-level `data/` directory
    if path.join("data").is_dir() {
        scan_folder(path, out);
        return;
    }
    // Otherwise treat it as a container of datapacks
    if depth >= MAX_CONTAINER_DEPTH {
        return;
    }
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.flatten() {
                collect_from_path(&entry.path(), depth + 1, out);
            }
        }
        Err(e) => debug!("Cannot read datapack container {}: {e}", path.display()),
    }
}

/// Scan a `.zip` datapack for worldgen JSON
fn scan_zip(path: &Path, out: &mut RawWorldgen) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            debug!("Cannot open datapack zip {}: {e}", path.display());
            return;
        }
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            warn!("Failed to read datapack zip {}: {e}", path.display());
            return;
        }
    };

    // Datapacks may hide per-version worldgen in `pack.mcmeta` overlays
    let overlay_dirs = {
        let mut mcmeta = String::new();
        match archive.by_name("pack.mcmeta") {
            Ok(mut f) => {
                let _ = f.read_to_string(&mut mcmeta);
                parse_overlay_dirs(&mcmeta)
            }
            Err(_) => Vec::new(),
        }
    };

    // Collect first, then insert in ascending priority
    let mut collected: Vec<(usize, Category, String, String)> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                debug!("Bad entry in {}: {e}", path.display());
                continue;
            }
        };
        if !entry.is_file() {
            continue;
        }
        let Some((priority, category, id)) = classify_with_overlays(entry.name(), &overlay_dirs)
        else {
            continue;
        };
        let mut contents = String::new();
        if let Err(e) = entry.read_to_string(&mut contents) {
            debug!("Failed to read zip entry {}: {e}", entry.name());
            continue;
        }
        collected.push((priority, category, id, contents));
    }
    collected.sort_by_key(|(priority, _, _, _)| *priority);
    for (_, category, id, contents) in collected {
        out.insert(category, id, contents);
    }
}

/// Active overlay directories from a datapack `pack.mcmeta`, ordered by `min_format` ascending
fn parse_overlay_dirs(mcmeta: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(mcmeta) else {
        return Vec::new();
    };
    let Some(entries) = value
        .get("overlays")
        .and_then(|o| o.get("entries"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut dirs: Vec<(i64, String)> = Vec::new();
    for entry in entries {
        let Some(dir) = entry.get("directory").and_then(serde_json::Value::as_str) else {
            continue;
        };
        // `min_format`, or the low end of `formats` (int, [lo,hi], or {min_inclusive})
        let min_format = entry
            .get("min_format")
            .and_then(serde_json::Value::as_i64)
            .or_else(|| match entry.get("formats") {
                Some(serde_json::Value::Number(n)) => n.as_i64(),
                Some(serde_json::Value::Array(a)) => a.first().and_then(serde_json::Value::as_i64),
                Some(serde_json::Value::Object(o)) => {
                    o.get("min_inclusive").and_then(serde_json::Value::as_i64)
                }
                _ => None,
            })
            .unwrap_or(0);
        dirs.push((min_format, dir.to_string()));
    }
    dirs.sort_by_key(|(format, _)| *format);
    dirs.into_iter().map(|(_, dir)| dir).collect()
}

/// Classify an archive entry, allowing an optional active-overlay prefix
fn classify_with_overlays(
    name: &str,
    overlay_dirs: &[String],
) -> Option<(usize, Category, String)> {
    if name.starts_with("data/") {
        let (category, id) = classify(name)?;
        return Some((0, category, id));
    }
    for (index, dir) in overlay_dirs.iter().enumerate() {
        if let Some(rest) = name
            .strip_prefix(dir.as_str())
            .and_then(|r| r.strip_prefix('/'))
            && rest.starts_with("data/")
        {
            let (category, id) = classify(rest)?;
            return Some((index + 1, category, id));
        }
    }
    None
}

/// Scan an unpacked datapack folder for worldgen JSON, including `pack.mcmeta`
fn scan_folder(root: &Path, out: &mut RawWorldgen) {
    scan_data_dir(&root.join("data"), out);
    let overlay_dirs = match std::fs::read_to_string(root.join("pack.mcmeta")) {
        Ok(mcmeta) => parse_overlay_dirs(&mcmeta),
        Err(_) => Vec::new(),
    };
    for dir in overlay_dirs {
        let overlay_data = root.join(&dir).join("data");
        if overlay_data.is_dir() {
            scan_data_dir(&overlay_data, out);
        }
    }
}

/// Scan a single `data/` directory (base or overlay) for worldgen JSON
fn scan_data_dir(data_dir: &Path, out: &mut RawWorldgen) {
    let Ok(namespaces) = std::fs::read_dir(data_dir) else {
        return;
    };
    for ns_entry in namespaces.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_json_files(&ns_path, &mut files);
        for file in files {
            // Build a forward-slash relative path `data/<ns>/...`
            let Ok(relative) = file.strip_prefix(data_dir) else {
                continue;
            };
            let mut rel = String::from("data/");
            let mut first = true;
            for component in relative.components() {
                if !first {
                    rel.push('/');
                }
                rel.push_str(&component.as_os_str().to_string_lossy());
                first = false;
            }
            let Some((category, id)) = classify(&rel) else {
                continue;
            };
            match std::fs::read_to_string(&file) {
                Ok(contents) => out.insert(category, id, contents),
                Err(e) => debug!("Failed to read worldgen file {}: {e}", file.display()),
            }
        }
    }
}

/// Recursively collect `.json` files under `dir`
fn collect_json_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, files);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
            files.push(path);
        }
    }
}

/// Classify a forward-slash `data/<ns>/...json` path into `(Category, "ns:id")`
fn classify(name: &str) -> Option<(Category, String)> {
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() < 3 || parts[0] != "data" {
        return None;
    }
    let namespace = parts[1];
    let after_ns = &parts[2..];

    for &category in Category::ALL {
        let segments = category.path_segments();
        if after_ns.len() <= segments.len() {
            continue;
        }
        if after_ns[..segments.len()] != *segments {
            continue;
        }
        // Guard against `worldgen/noise` also matching `worldgen/noise_settings`
        let rest = after_ns[segments.len()..].join("/");
        let id = rest.strip_suffix(".json")?;
        if id.is_empty() {
            return None;
        }
        return Some((category, format!("{namespace}:{id}")));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_worldgen_paths() {
        assert_eq!(
            classify("data/minecraft/worldgen/noise_settings/overworld.json"),
            Some((Category::NoiseSettings, "minecraft:overworld".to_string()))
        );
        assert_eq!(
            classify("data/terralith/worldgen/density_function/overworld/arch/total.json"),
            Some((
                Category::DensityFunction,
                "terralith:overworld/arch/total".to_string()
            ))
        );
        assert_eq!(
            classify("data/minecraft/dimension/overworld.json"),
            Some((Category::Dimension, "minecraft:overworld".to_string()))
        );
        assert_eq!(
            classify("data/terralith/worldgen/noise/sample.json"),
            Some((Category::Noise, "terralith:sample".to_string()))
        );
    }

    #[test]
    fn parses_and_applies_overlay_dirs() {
        // tectonic-style mcmeta: overlays sorted by min_format ascending
        let mcmeta = r#"{
            "pack": { "pack_format": 15 },
            "overlays": { "entries": [
                { "directory": "overlay.datapack", "min_format": 1, "max_format": 999 },
                { "directory": "overlay.26_2", "formats": [107, 999] },
                { "directory": "overlay.1_21_9", "min_format": 82, "max_format": 999 }
            ] }
        }"#;
        let dirs = parse_overlay_dirs(mcmeta);
        // Ascending by min_format: datapack(1) < 1_21_9(82) < 26_2(107)
        assert_eq!(dirs, vec!["overlay.datapack", "overlay.1_21_9", "overlay.26_2"]);

        // Base data/ is priority 0; overlays get their 1-based rank
        assert_eq!(
            classify_with_overlays("data/tectonic/worldgen/noise/ridge.json", &dirs),
            Some((0, Category::Noise, "tectonic:ridge".to_string()))
        );
        assert_eq!(
            classify_with_overlays(
                "overlay.datapack/data/tectonic/worldgen/density_function/__constants/noise/ridge.json",
                &dirs,
            ),
            Some((
                1,
                Category::DensityFunction,
                "tectonic:__constants/noise/ridge".to_string()
            ))
        );
        assert_eq!(
            classify_with_overlays(
                "overlay.26_2/data/minecraft/worldgen/noise_settings/overworld.json",
                &dirs,
            ),
            Some((3, Category::NoiseSettings, "minecraft:overworld".to_string()))
        );
        // A shipped-but-not-declared overlay is ignored
        assert!(
            classify_with_overlays(
                "overlay.terratonic/data/tectonic/worldgen/noise/ridge.json",
                &dirs,
            )
            .is_none()
        );
    }

    #[test]
    fn ignores_unrelated_paths() {
        // `noise_settings` must not be misread as the `noise` category
        assert_eq!(
            classify("data/minecraft/worldgen/noise_settings/overworld.json").map(|(c, _)| c),
            Some(Category::NoiseSettings)
        );
        assert!(classify("data/terralith/worldgen/biome/foo.json").is_none());
        assert!(classify("data/c/tags/worldgen/biome/is_lush.json").is_none());
        assert!(classify("pack.mcmeta").is_none());
        assert!(classify("data/minecraft/dimension/").is_none());
    }
}
