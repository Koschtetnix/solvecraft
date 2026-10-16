//! Seeded goldens: what every robustness seed builds (`tests/robustness/goldens.json`).
//!
//! Each seed's random design (`fuzz::run`) is recorded: its steps, how it ended (ok, or the
//! kind and start of the error it was refused with) and, for what it left, the bodies' total
//! volume and area (within a relative 1e-6) and their face, edge and shell counts (exact). Any
//! change fails, the better and the worse alike: a seed that used to be refused and now builds
//! is recorded on purpose, with the reason. To record: `ROBUSTNESS_GOLDENS_BLESS="why" cargo
//! test --profile ci -p solidcraft-engine --lib robustness_goldens:: -- --include-ignored`
//! (docs/robustness.md). The reason goes into the file's `notes` with the seeds it changed.
//!
//! Pull requests that touch `crates/kernel` run a sample (`sample_0` … `sample_3`, every 8th
//! seed, split so the shards share them); pushes to main and the nightly run every seed
//! (`all_0` … `all_3`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::fuzz;

/// Seeds 0..SEEDS (about ten minutes for all of them on one core), each with up to this many steps (as `fixed_seeds_run_clean`).
const SEEDS: u64 = 96;
const MAX_STEPS: usize = 8;
const REL: f64 = 1e-6;

/// Tests share the file when they record.
static FILE: Mutex<()> = Mutex::new(());

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/robustness/goldens.json")
}

fn load() -> Value {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}))
}

/// How `got` differs from `want` (None when it matches within the tolerances).
fn differs(want: &Value, got: &Value) -> Option<String> {
    let mut out = Vec::new();
    for (k, g) in got.as_object().into_iter().flatten() {
        let w = &want[k];
        let same = match (w.as_f64(), g.as_f64()) {
            (Some(a), Some(b)) if matches!(k.as_str(), "volume" | "area") => (a - b).abs() <= REL * a.abs().max(b.abs()).max(1.0),
            _ => w == g,
        };
        if !same {
            out.push(format!("{k}: {w} -> {g}"));
        }
    }
    (!out.is_empty()).then(|| out.join("; "))
}

/// Platform records only override the seeds explicitly verified on that platform.
fn expected<'a>(file: &'a Value, seed: u64, platform: &str) -> &'a Value {
    let key = seed.to_string();
    file.get("platforms")
        .and_then(|platforms| platforms.get(platform))
        .and_then(|record| record.get("seeds"))
        .and_then(|seeds| seeds.get(&key))
        .unwrap_or(&file["seeds"][key])
}

#[test]
fn platform_records_keep_shared_references_for_every_other_seed_and_platform() {
    let file = json!({"seeds":{"1":{"volume":10},"2":{"volume":20}},"platforms":{"windows":{"seeds":{"1":{"volume":11}}}}});
    assert_eq!(expected(&file, 1, "windows")["volume"], 11);
    assert_eq!(expected(&file, 1, "linux")["volume"], 10);
    assert_eq!(expected(&file, 2, "windows")["volume"], 20);
}

fn check(part: u64, parts: u64, every: u64) {
    let seeds: Vec<u64> = (0..SEEDS).filter(|s| s % every == 0 && (s / every) % parts == part).collect();
    let got: BTreeMap<u64, Value> = seeds.iter().map(|s| (*s, fuzz::golden(*s, MAX_STEPS))).collect();
    let _lock = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut file = load();
    let changed: Vec<(u64, String)> = got
        .iter()
        .filter_map(|(seed, g)| {
            let w = expected(&file, *seed, std::env::consts::OS);
            if w.is_null() { Some((*seed, "new".to_string())) } else { differs(w, g).map(|d| (*seed, d)) }
        })
        .collect();
    if let Ok(reason) = std::env::var("ROBUSTNESS_GOLDENS_BLESS") {
        assert!(!reason.trim().is_empty(), "ROBUSTNESS_GOLDENS_BLESS needs the reason for the change");
        if changed.is_empty() {
            return;
        }
        if file.get("about").is_none() {
            file["about"] = json!(
                "What each robustness seed builds (crates/engine/src/tests/robustness_goldens.rs, docs/robustness.md). Record changes with ROBUSTNESS_GOLDENS_BLESS=\"why\"."
            );
        }
        let platform = std::env::consts::OS;
        let platform_record = file["platforms"][platform]["seeds"].is_object();
        for (seed, g) in &got {
            if platform_record {
                if changed.iter().any(|(changed, _)| changed == seed) {
                    file["platforms"][platform]["seeds"][seed.to_string()] = g.clone();
                }
            } else {
                file["seeds"][seed.to_string()] = g.clone();
            }
        }
        let mut notes = file["notes"].as_array().cloned().unwrap_or_default();
        let seeds: Vec<u64> = changed.iter().map(|(s, _)| *s).collect();
        // The parts of one record (the tests run side by side) share one note.
        match notes.last_mut() {
            Some(last) if last["reason"] == json!(reason) && (!platform_record || last["platform"] == json!(platform)) => {
                let mut all: Vec<u64> = last["seeds"].as_array().into_iter().flatten().filter_map(Value::as_u64).chain(seeds).collect();
                all.sort_unstable();
                all.dedup();
                last["seeds"] = json!(all);
            }
            _ => {
                let mut note = json!({"reason": reason, "seeds": seeds});
                if platform_record {
                    note["platform"] = json!(platform);
                }
                notes.push(note);
            }
        }
        file["notes"] = Value::Array(notes);
        // Seeds in order, one per line, so a change is a small diff.
        let seeds: BTreeMap<u64, Value> =
            file["seeds"].as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.parse::<u64>().ok()?, v.clone()))).collect();
        let mut s = format!("{{\n \"about\": {},\n \"seeds\": {{\n", file["about"]);
        let n = seeds.len();
        for (i, (k, v)) in seeds.iter().enumerate() {
            s += &format!("  \"{k}\": {}{}\n", v, if i + 1 < n { "," } else { "" });
        }
        let notes: Vec<String> = file["notes"].as_array().into_iter().flatten().map(|n| format!("  {n}")).collect();
        s += " },\n";
        if let Some(platforms) = file.get("platforms") {
            s += &format!(" \"platforms\": {platforms},\n");
        }
        s += &format!(" \"notes\": [\n{}\n ]\n}}\n", notes.join(",\n"));
        std::fs::write(path(), s).unwrap_or_else(|e| panic!("{}: {e}", path().display()));
        return;
    }
    assert!(
        changed.is_empty(),
        "{} seed(s) build differently from tests/robustness/goldens.json (if that is intended, record it with ROBUSTNESS_GOLDENS_BLESS=\"why\" and say why in the pull request):\n{}",
        changed.len(),
        changed.iter().map(|(s, d)| format!("seed {s}: {d}")).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn sample_0() {
    check(0, 4, 8);
}

#[test]
fn sample_1() {
    check(1, 4, 8);
}

#[test]
fn sample_2() {
    check(2, 4, 8);
}

#[test]
fn sample_3() {
    check(3, 4, 8);
}

#[test]
fn all_0() {
    check(0, 4, 1);
}

#[test]
fn all_1() {
    check(1, 4, 1);
}

#[test]
fn all_2() {
    check(2, 4, 1);
}

#[test]
fn all_3() {
    check(3, 4, 1);
}
