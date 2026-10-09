//! `tinderbox update`: replace this binary with the latest GitHub release.
//! Downloading and unpacking go through the system's `curl` and `tar` (both
//! ship with Linux, macOS and Windows 10+; Windows' tar reads zips), which
//! keeps an HTTP and TLS stack out of the binary.

use std::path::Path;
use std::process::Command;

const REPO: &str = "aiguy110/tinderbox";

/// The release archive for this platform, and the directory inside it.
const PLATFORM: Option<(&str, &str)> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some(("tinderbox-linux-x86_64", ".tar.gz"))
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some(("tinderbox-macos-aarch64", ".tar.gz"))
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some(("tinderbox-windows-x86_64", ".zip"))
} else {
    None
};

const EXE: &str = if cfg!(windows) { "tinderbox.exe" } else { "tinderbox" };
/// Linux archives also carry the lock-screen scripts; they're updated too
/// where they sit next to the binary.
const SCRIPTS: &[&str] = if cfg!(target_os = "linux") { &["tinderbox-saver", "tinderbox-lock"] } else { &[] };

pub fn run(args: &[String]) -> Result<(), String> {
    let force = args.iter().any(|a| a == "--force");
    let (name, ext) = PLATFORM.ok_or("there's no release build for this platform; build from source instead")?;
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| format!("can't find this binary: {e}"))?;
    let dir = exe.parent().ok_or("can't find this binary's directory")?;
    // Left behind by the last update on Windows, where a running binary can
    // be renamed but not deleted.
    let _ = std::fs::remove_file(exe.with_extension("old"));

    let current = env!("CARGO_PKG_VERSION");
    let json = curl_text(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    let release: serde_json::Value = serde_json::from_str(&json).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    let tag = release["tag_name"].as_str().ok_or("unexpected reply from GitHub: no tag_name")?;
    let latest = tag.trim_start_matches('v');
    if !force && version(latest) <= version(current) {
        println!("tinderbox {current} is up to date (latest release: {tag}).");
        return Ok(());
    }
    let asset = format!("{name}{ext}");
    let url = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == asset.as_str()))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or(format!("release {tag} has no {asset}"))?;

    let tmp = std::env::temp_dir().join(format!("tinderbox-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("can't create {}: {e}", tmp.display()))?;
    let result = (|| {
        println!("Downloading {tag}...");
        let archive = tmp.join(&asset);
        run_tool(Command::new("curl").args(["-fsSL", "-o"]).arg(&archive).arg(url), "curl")?;
        run_tool(Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&tmp), "tar")?;
        let unpacked = tmp.join(name);
        install(&unpacked.join(EXE), &exe)?;
        for s in SCRIPTS {
            let (from, to) = (unpacked.join(s), dir.join(s));
            if to.exists() && from.exists() {
                install(&from, &to)?;
            }
        }
        Ok::<_, String>(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result?;
    println!("Updated tinderbox {current} to {latest} at {}.", exe.display());
    Ok(())
}

/// Put `from` in place of `to`, by copying next to it and renaming over it,
/// so `to` is never half-written (and a running binary keeps working).
fn install(from: &Path, to: &Path) -> Result<(), String> {
    let staged = to.with_extension("new");
    let denied = |e: std::io::Error| {
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied { " (try again with sudo or as administrator)" } else { "" };
        format!("can't write {}: {e}{hint}", to.display())
    };
    std::fs::copy(from, &staged).map_err(denied)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(denied)?;
    }
    // Windows won't replace a running binary, but will rename it.
    #[cfg(windows)]
    if to.exists() {
        let old = to.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(to, &old).map_err(denied)?;
    }
    std::fs::rename(&staged, to).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        denied(e)
    })
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "Accept: application/vnd.github+json", url])
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("couldn't reach GitHub: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("unexpected reply from GitHub: {e}"))
}

fn run_tool(cmd: &mut Command, tool: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("couldn't run {tool}: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(format!("{tool} failed: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// `1.2.3` as numbers, for comparing; anything unparseable counts as 0.
fn version(v: &str) -> Vec<u64> {
    v.split(['.', '-']).take(3).map(|n| n.parse().unwrap_or(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn versions_compare_numerically() {
        assert!(version("0.10.0") > version("0.9.3"));
        assert!(version("0.5.0") > version("0.4.0"));
        assert!(version("0.4.0") <= version("0.4.0"));
    }
}
