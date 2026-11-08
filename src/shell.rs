use std::{env, fs};

/// The configured shell for the main pane.
///
/// Prefer the account shell from `/etc/passwd`, then `$SHELL`, then `/bin/sh`.
/// `$SHELL` can be stale or point at a POSIX-mode bash when launched from an
/// unusual context, so it is only a fallback.
pub fn default_shell() -> Vec<String> {
    let shell = account_shell()
        .or_else(|| env::var("SHELL").ok().filter(|shell| !shell.is_empty()))
        .unwrap_or_else(|| "/bin/sh".to_string());
    vec![shell]
}

fn account_shell() -> Option<String> {
    let user = env::var("USER").ok().or_else(|| env::var("LOGNAME").ok())?;
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    shell_from_passwd(&passwd, &user)
}

fn shell_from_passwd(contents: &str, user: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 || fields[0] != user {
            return None;
        }
        let shell = fields[6];
        if shell.is_empty() || shell.ends_with("nologin") || shell.ends_with("false") {
            return None;
        }
        Some(shell.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::shell_from_passwd;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
alice:x:1000:1000:Alice:/home/alice:/usr/bin/fish
bob:x:1001:1001:Bob:/home/bob:/usr/sbin/nologin
carol:x:1002:1002:Carol:/home/carol:/bin/false
";

    #[test]
    fn selects_the_users_login_shell() {
        assert_eq!(
            shell_from_passwd(PASSWD, "alice"),
            Some("/usr/bin/fish".to_string())
        );
    }

    #[test]
    fn ignores_missing_and_disabled_shells() {
        assert_eq!(shell_from_passwd(PASSWD, "nobody"), None);
        assert_eq!(shell_from_passwd(PASSWD, "bob"), None);
        assert_eq!(shell_from_passwd(PASSWD, "carol"), None);
    }

    #[test]
    fn ignores_malformed_lines() {
        assert_eq!(shell_from_passwd("alice:x:1000", "alice"), None);
    }
}
