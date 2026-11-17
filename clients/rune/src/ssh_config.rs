//! The machines named in `~/.ssh/config`, to offer setting phux up on.

/// Hosts nobody runs phux on: code forges answer ssh for git alone.
const FORGES: [&str; 6] = [
    "github.com",
    "gitlab.com",
    "bitbucket.org",
    "codeberg.org",
    "ssh.dev.azure.com",
    "git.sr.ht",
];

fn forge(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    FORGES
        .iter()
        .any(|forge| host == *forge || host.ends_with(&format!(".{forge}")))
}

/// The hosts in `$HOME/.ssh/config`; none when it can't be read.
pub fn read() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let path = std::path::Path::new(&home).join(".ssh/config");
    std::fs::read_to_string(path).map_or_else(|_| Vec::new(), |config| hosts(&config))
}

/// Each `Host` block's first literal alias, in file order. Patterns, code
/// forges and blocks that log in as `git` are left out.
// ponytail: `Include`d files aren't read; follow them once a config needs it.
pub fn hosts(config: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    // The block being read: its alias, and whether it's still a machine.
    let mut block: Option<(String, bool)> = None;
    let mut close = |block: &mut Option<(String, bool)>| {
        if let Some((alias, true)) = block.take()
            && !found.contains(&alias)
        {
            found.push(alias);
        }
    };
    for line in config.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or((line, ""));
        let value = value.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
        let value = value.trim().trim_matches('"');
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                close(&mut block);
                let alias = value
                    .split_whitespace()
                    .find(|alias| !alias.contains(['*', '?', '!']));
                block = alias.map(|alias| (alias.to_owned(), !forge(alias)));
            }
            "match" => close(&mut block),
            "hostname" if forge(value) => {
                if let Some((_, keep)) = &mut block {
                    *keep = false;
                }
            }
            "user" if value == "git" => {
                if let Some((_, keep)) = &mut block {
                    *keep = false;
                }
            }
            _ => {}
        }
    }
    close(&mut block);
    found
}

#[cfg(test)]
mod tests {
    use super::hosts;

    #[test]
    fn machines_come_from_host_blocks_and_forges_stay_out() {
        let config = "\
# my machines
Host *
  ServerAliasInterval 30
Host box box.lan
  HostName 10.0.0.5
  User me
Host=mini
Host github.com
  User git
Host work-git
  HostName github.com
Host gitea
  User git
Host *.internal !bastion
Match host foo exec true
  User root
host \"quoted\"
Host box
";
        assert_eq!(hosts(config), ["box", "mini", "quoted"]);
        assert!(hosts("").is_empty());
    }
}
