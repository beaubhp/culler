#![no_main]

use std::{fs, path::PathBuf};

use cull_python::{DebugDefinitionsOptions, analyze_debug_definitions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let root = PathBuf::from("/tmp/cull-fuzz-project");
    let _ = fs::remove_dir_all(&root);
    if fs::create_dir_all(&root).is_err() {
        return;
    }
    if fs::write(root.join("module.py"), data).is_err() {
        return;
    }

    let _ = analyze_debug_definitions(DebugDefinitionsOptions {
        project_root: root,
        source_roots: Vec::new(),
        target_python: None,
    });
});
