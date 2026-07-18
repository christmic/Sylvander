use std::fs::{self, OpenOptions};
use std::io::Write;

use tracing::{info, warn};

const HOSTS_PATH: &str = "/etc/hosts";

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Ensure the branded-domain hosts entry exists at startup:
/// check → if missing, try a direct write → if that needs root, request GUI
/// admin authorization (macOS auth dialog). Non-fatal on cancel/failure.
pub fn ensure(domain: &str, port: u16) {
    let content = fs::read_to_string(HOSTS_PATH).unwrap_or_default();
    if has_entry(&content, domain) {
        return; // already present — never prompts again
    }
    // Already root (e.g. `sudo token9 serve`): write directly.
    if let Ok(mut f) = OpenOptions::new().append(true).open(HOSTS_PATH)
        && writeln!(f, "\n{}", entry_line(domain)).is_ok()
    {
        info!(%domain, "added hosts entry (127.0.0.1 -> {domain})");
        return;
    }
    // Not writable → request authorization from the user via a macOS admin prompt.
    if !valid_domain(domain) {
        warn!("skip hosts auto-add: domain '{domain}' has unexpected characters");
        return;
    }
    info!(%domain, "hosts entry missing — requesting administrator authorization…");
    // Friendly, non-technical dialog: benefit of allowing + reassurance if declining.
    let explain = format!(
        "想为 token9 设置一个好记的地址吗？\\n\\n\
         设置后，你可以直接用 http://{domain}:{port} 打开它。\\n\\n\
         · 点「好的」：帮你设置好，只需输入一次电脑密码。\\n\
         · 点「以后再说」：完全不影响使用，继续用 http://127.0.0.1:{port} 就好，随时可以再来开启。"
    );
    let sh = format!("echo '127.0.0.1  {domain}  # token9' >> /etc/hosts");
    let script = format!(
        "display dialog \"{explain}\" with title \"token9\" \
         buttons {{\"以后再说\", \"好的\"}} default button \"好的\"\n\
         do shell script \"{sh}\" with administrator privileges"
    );
    match std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
    {
        Ok(o) if o.status.success() => info!(%domain, "added hosts entry via authorization"),
        Ok(o) => warn!(
            "hosts entry not added (cancelled/failed): {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => warn!(error = %e, "could not run osascript to add hosts entry"),
    }
}

fn entry_line(domain: &str) -> String {
    format!("127.0.0.1\t{domain}\t# token9")
}

/// True if `/etc/hosts` already maps `domain` to 127.0.0.1.
fn has_entry(content: &str, domain: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return false;
        }
        let mut toks = line.split_whitespace();
        matches!(toks.next(), Some("127.0.0.1"))
            && toks
                .take_while(|t| !t.starts_with('#'))
                .any(|t| t == domain)
    })
}

pub fn status(domain: &str) -> anyhow::Result<()> {
    let content = fs::read_to_string(HOSTS_PATH).unwrap_or_default();
    if has_entry(&content, domain) {
        println!("installed: 127.0.0.1 -> {domain} (in {HOSTS_PATH})");
    } else {
        println!("not installed: no 127.0.0.1 -> {domain} entry in {HOSTS_PATH}");
        println!("run `token9 hosts install` to add it");
    }
    Ok(())
}

/// Print the client endpoint URL. token9 keeps its port (no privileged setup);
/// the branded domain is an optional one-line hosts entry (name -> loopback).
pub fn print_endpoint(domain: &str, port: u16) {
    let content = fs::read_to_string(HOSTS_PATH).unwrap_or_default();
    let hosts_ok = has_entry(&content, domain);

    println!("domain : {domain}");
    println!("port   : {port}");
    println!();
    println!("client base_url:");
    println!("  http://127.0.0.1:{port}            (works now, no setup)");
    println!("  http://{domain}:{port}   (branded name — needs the hosts line below)");
    println!();
    if hosts_ok {
        println!("hosts entry already present ✓  → use http://{domain}:{port}");
    } else {
        println!("optional, map the branded domain to loopback (one time):");
        println!("  echo '127.0.0.1  {domain}  # token9' | sudo tee -a {HOSTS_PATH}");
        println!("  (or run: token9 hosts install)");
    }
    println!();
    println!("verify: curl http://{domain}:{port}/healthz");
}

pub fn install(domain: &str) -> anyhow::Result<()> {
    let content = fs::read_to_string(HOSTS_PATH).unwrap_or_default();
    if has_entry(&content, domain) {
        println!("already installed: 127.0.0.1 -> {domain}");
        return Ok(());
    }

    let line = entry_line(domain);
    match OpenOptions::new().append(true).open(HOSTS_PATH) {
        Ok(mut f) => {
            writeln!(f, "\n{line}")?;
            println!("installed: {line}");
            println!("you can now use http://{domain}:<port>");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            println!("cannot write {HOSTS_PATH} (needs root). run this yourself:");
            println!("  echo '{line}' | sudo tee -a {HOSTS_PATH}");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
#[path = "../tests/unit/hosts.rs"]
mod tests;
