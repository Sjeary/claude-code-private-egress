/// Escape a string for safe inclusion in a POSIX shell command.
///
/// Wraps the value in single quotes. POSIX single-quoted strings have no
/// escape sequences, variable expansion, or command substitution — the only
/// character that cannot appear is `'` itself. We handle that with the
/// standard `'\''` trick (end quote, escaped literal quote, reopen quote).
/// This is the same algorithm used by Python's `shlex.quote()`.
///
/// # Why not avoid shell escaping entirely?
///
/// SSH remote commands (`ssh host cmd args...`) are concatenated with spaces
/// and passed to the remote shell, so escaping is unavoidable for commands
/// that include user-supplied values. Breaking multi-command pipelines into
/// separate `exec()` calls would reduce the escaping surface but doesn't
/// eliminate it.
///
/// Rust `&str` cannot contain null bytes, so truncation attacks are not
/// possible.
///
/// The `shell-escape` crate implements the same algorithm but adds a
/// dependency for ~3 lines of logic. If this function needs to handle
/// non-UTF-8 `OsStr` inputs or platform-specific quoting, switching to
/// that crate would be worthwhile.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_string_unchanged() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn single_quote_escaped() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn multiple_single_quotes() {
        assert_eq!(shell_escape("a'b'c"), "'a'\\''b'\\''c'");
    }

    #[test]
    fn special_chars_preserved() {
        assert_eq!(shell_escape("hello world $FOO"), "'hello world $FOO'");
    }

    #[test]
    fn double_quotes_preserved() {
        assert_eq!(shell_escape("say \"hi\""), "'say \"hi\"'");
    }

    #[test]
    fn backticks_preserved() {
        assert_eq!(shell_escape("`cmd`"), "'`cmd`'");
    }

    #[test]
    fn newline_preserved() {
        assert_eq!(shell_escape("a\nb"), "'a\nb'");
    }
}
