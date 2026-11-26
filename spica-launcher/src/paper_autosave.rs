use crate::config::{generate_auth_token, BackupConfig};
use std::fs;
use std::io::ErrorKind;
use std::ops::Range;
use std::path::{Path, PathBuf};

const WORLD_DEFAULTS: &str = "paper-world-defaults.yml";
const GLOBAL_SETTINGS: &str = "paper-global.yml";
const AUTOSAVE_INTERVAL: &str = "100";
const SAVE_BATCH: &str = "4";

/// A private settings directory for one Purpur process. The operator's YAML stays in place.
pub struct PaperAutosave {
    directory: PathBuf,
}

impl PaperAutosave {
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Drop for PaperAutosave {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join(WORLD_DEFAULTS));
        let _ = fs::remove_file(self.directory.join(GLOBAL_SETTINGS));
        let _ = fs::remove_dir(&self.directory);
    }
}

/// Locate an exact generated scalar within a top-level section. Preserve the input bytes
/// around it and leave custom formats and non-stock values to the server operator.
fn stock_value(input: &str, section: &str, key: &str, expected: &str) -> Option<Range<usize>> {
    let mut in_section = false;
    let mut seen_section = false;
    let mut value_range = None;
    let mut offset = 0;
    for raw in input.split_inclusive('\n') {
        let start = offset;
        offset += raw.len();
        let trimmed = raw.trim_end();
        let line = if start == 0 {
            trimmed.trim_start_matches('\u{feff}')
        } else {
            trimmed
        };
        let start = start + trimmed.len() - line.len();
        let name = line.trim_start();
        if name.is_empty() || name.starts_with('#') {
            continue;
        }
        if line == name {
            let (label, tail) = line.split_once(':')?;
            if label == "<<" {
                return None;
            }
            in_section = label.trim_matches(['\'', '"']) == section;
            if in_section {
                if seen_section || !tail.trim().is_empty() {
                    return None;
                }
                seen_section = true;
            }
        } else if in_section && line.len() - name.len() == 2 {
            let (label, tail) = name.split_once(':')?;
            if label == "<<" {
                return None;
            }
            if label.trim_matches(['\'', '"']) == key {
                if value_range.is_some() || tail.strip_prefix(' ') != Some(expected) {
                    return None;
                }
                let value_start = start + 2 + label.len() + 2;
                value_range = Some(value_start..value_start + expected.len());
            }
        }
    }
    value_range
}

fn incremental_defaults(world_defaults: &str, bukkit: &str) -> Option<String> {
    stock_value(bukkit, "ticks-per", "autosave", "6000")?;
    let period = stock_value(world_defaults, "chunks", "auto-save-interval", "default")?;
    let batch = stock_value(
        world_defaults,
        "chunks",
        "max-auto-save-chunks-per-tick",
        "24",
    )?;
    let mut output = world_defaults.to_owned();
    let mut edits = [(period, AUTOSAVE_INTERVAL), (batch, SAVE_BATCH)];
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, new_value) in edits {
        output.replace_range(range, new_value);
    }
    Some(output)
}

pub fn prepare(work_dir: &Path, config: &BackupConfig) -> std::io::Result<Option<PaperAutosave>> {
    if !config.incremental_autosave
        || config.snapshot_command.is_empty()
        || work_dir.join("paper.yml").try_exists()?
    {
        return Ok(None);
    }
    let paper = work_dir.join("config");
    let global = paper.join(GLOBAL_SETTINGS);
    if !global.is_file() {
        return Ok(None);
    }
    let edited = match fs::read_to_string(paper.join(WORLD_DEFAULTS)).and_then(|defaults| {
        Ok(incremental_defaults(
            &defaults,
            &fs::read_to_string(work_dir.join("bukkit.yml"))?,
        ))
    }) {
        Ok(Some(content)) => content,
        Ok(None) => return Ok(None),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let directory = std::env::temp_dir().join(format!("spica-paper-{}", generate_auth_token()));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.recursive(false).create(&directory)?;
    let profile = PaperAutosave { directory };
    fs::copy(global, profile.directory.join(GLOBAL_SETTINGS))?;
    fs::write(profile.directory.join(WORLD_DEFAULTS), edited)?;
    Ok(Some(profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUKKIT: &str = "ticks-per:\n  autosave: 6000\n";
    const STOCK: &str =
        "chunks:\n  auto-save-interval: default\n  max-auto-save-chunks-per-tick: 24\n";

    #[test]
    fn patches_only_stock_scalars_and_preserves_unrelated_yaml() {
        let world = format!("\u{feff}# config\nother:\n  auto-save-interval: default\n{STOCK}")
            .replace('\n', "\r\n");
        let edited = incremental_defaults(&world, BUKKIT).unwrap();
        assert_eq!(
            edited,
            "\u{feff}# config\r\nother:\r\n  auto-save-interval: default\r\nchunks:\r\n  auto-save-interval: 100\r\n  max-auto-save-chunks-per-tick: 4\r\n"
        );
    }

    #[test]
    fn preserves_explicit_disabled_and_custom_saving_configuration() {
        assert!(incremental_defaults(STOCK, &BUKKIT.replace("6000", "0")).is_none());
        assert!(incremental_defaults(&STOCK.replace("default", "200"), BUKKIT).is_none());
        assert!(incremental_defaults(&STOCK.replace("24", "8"), BUKKIT).is_none());
        assert!(incremental_defaults(&format!("{STOCK}{STOCK}"), BUKKIT).is_none());
        assert!(
            incremental_defaults(&STOCK.replace("chunks:", "chunks: &shared"), BUKKIT).is_none()
        );
    }
}
