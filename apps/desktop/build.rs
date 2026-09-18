//! Stamp the build with the commit it came from, for the About window.
//!
//! Both values are optional: a build from a source tarball, or with no git
//! on PATH, simply shows the version alone.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (!text.is_empty()).then_some(text)
    };
    if let Some(commit) = git(&["rev-parse", "--short=7", "HEAD"]) {
        println!("cargo:rustc-env=COCO_COMMIT={commit}");
    }
    if let Some(date) = git(&["log", "-1", "--date=format:%b %-d, %Y", "--format=%cd"]) {
        println!("cargo:rustc-env=COCO_DATE={date}");
    }
    // Rebuild when HEAD moves. `.git/HEAD` alone is not enough: on a branch
    // it holds `ref: refs/heads/main` and a new commit changes the ref file
    // (or `packed-refs`), not HEAD itself. Harmless when the directory is
    // absent: Cargo then falls back to rerunning on any change in the crate.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/packed-refs");
        if let Some(target) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{target}");
        }
    }
}
