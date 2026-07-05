fn main() {
    let sha = run_git(&["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".into());
    // .git/index only changes on add/commit/checkout, not on every file edit, so this
    // can lag behind the true working-tree state until the next real rebuild trigger.
    let dirty = run_git(&["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let commit = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=OSMILOG_GIT_SHA={commit}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}

fn run_git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}
