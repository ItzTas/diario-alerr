#![allow(dead_code)]

pub mod gazette_site;
pub mod pdf;

use std::env;
use std::process::{Command, Stdio};

pub fn require_pdftotext() -> bool {
    let installed = Command::new("pdftotext")
        .arg("-v")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if installed {
        return true;
    }
    if env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("pdftotext is required when CI is set");
    }
    eprintln!("skipping: pdftotext is not installed");
    false
}
