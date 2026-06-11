use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

const BLOCK_START: &str = "# >>> Marsala Codex interception >>>";
const BLOCK_END: &str = "# <<< Marsala Codex interception <<<";

pub fn default_shell_file() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set; pass --shell-file explicitly")?;
    Ok(PathBuf::from(home).join(".bashrc"))
}

pub fn install(project_dir: &Path, shell_file: &Path) -> Result<()> {
    let project_dir = project_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve project directory {}",
            project_dir.display()
        )
    })?;
    let ca_path = project_dir.join("certs/marsala-ca.pem");
    if !ca_path.is_file() {
        bail!(
            "Marsala CA certificate is missing at {}; run `just mitm-ca-init` first",
            ca_path.display()
        );
    }

    let existing = read_optional(shell_file)?;
    let cleaned = remove_block(&existing)?;
    let block = render_block(&ca_path);
    let updated = append_block(&cleaned, &block);
    write_shell_file(shell_file, &updated)?;
    Ok(())
}

pub fn uninstall(shell_file: &Path) -> Result<bool> {
    let existing = read_optional(shell_file)?;
    let cleaned = remove_block(&existing)?;
    if cleaned == existing {
        return Ok(false);
    }
    write_shell_file(shell_file, &cleaned)?;
    Ok(true)
}

fn render_block(ca_path: &Path) -> String {
    let ca_path = shell_quote(&ca_path.to_string_lossy());
    format!(
        "{BLOCK_START}\n\
export HTTPS_PROXY=http://127.0.0.1:8788\n\
export https_proxy=http://127.0.0.1:8788\n\
export HTTP_PROXY=\n\
export http_proxy=\n\
export ALL_PROXY=\n\
export all_proxy=\n\
export NO_PROXY=\n\
export no_proxy=\n\
export CODEX_CA_CERTIFICATE={ca_path}\n\
export SSL_CERT_FILE={ca_path}\n\
export MARSALA_CODEX_INTERCEPTION=enabled\n\
if [[ $- == *i* ]]; then\n\
  printf '%s\\n' '[marsala] Codex interception environment enabled; proxy expected at 127.0.0.1:8788'\n\
fi\n\
{BLOCK_END}"
    )
}

fn read_optional(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to read shell startup file {}", path.display())),
    }
}

fn write_shell_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    fs::write(path, contents)
        .with_context(|| format!("failed to write shell startup file {}", path.display()))
}

fn append_block(existing: &str, block: &str) -> String {
    let existing = existing.trim_end_matches('\n');
    if existing.is_empty() {
        format!("{block}\n")
    } else {
        format!("{existing}\n\n{block}\n")
    }
}

fn remove_block(contents: &str) -> Result<String> {
    let Some(start) = contents.find(BLOCK_START) else {
        if contents.contains(BLOCK_END) {
            bail!("found Marsala block end marker without a start marker");
        }
        return Ok(contents.to_string());
    };
    let remainder = &contents[start..];
    let Some(relative_end) = remainder.find(BLOCK_END) else {
        bail!("found Marsala block start marker without an end marker");
    };
    let mut end = start + relative_end + BLOCK_END.len();
    if contents[end..].starts_with('\n') {
        end += 1;
    }

    let mut before = contents[..start].to_string();
    if before.ends_with("\n\n") {
        before.pop();
    }
    before.push_str(&contents[end..]);

    if before.contains(BLOCK_START) || before.contains(BLOCK_END) {
        bail!("multiple Marsala interception blocks found; remove duplicates manually");
    }
    Ok(before)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_ca() -> tempfile::TempDir {
        let tempdir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tempdir.path().join("certs")).expect("cert dir");
        fs::write(tempdir.path().join("certs/marsala-ca.pem"), "certificate").expect("ca cert");
        tempdir
    }

    #[test]
    fn install_is_idempotent_and_uses_absolute_paths() {
        let project = project_with_ca();
        let shell_file = project.path().join("home/.bashrc");
        fs::create_dir(shell_file.parent().expect("parent")).expect("home");
        fs::write(&shell_file, "# user content\nexport KEEP=1\n").expect("bashrc");

        install(project.path(), &shell_file).expect("first install");
        let once = fs::read_to_string(&shell_file).expect("read once");
        install(project.path(), &shell_file).expect("second install");
        let twice = fs::read_to_string(&shell_file).expect("read twice");

        assert_eq!(once, twice);
        assert_eq!(twice.matches(BLOCK_START).count(), 1);
        assert!(twice.contains("# user content\nexport KEEP=1"));
        assert!(twice.contains(&project.path().canonicalize().unwrap().display().to_string()));
        assert!(twice.contains("[marsala] Codex interception environment enabled"));
    }

    #[test]
    fn uninstall_removes_only_the_marked_block() {
        let project = project_with_ca();
        let shell_file = project.path().join(".bashrc");
        fs::write(&shell_file, "before\nafter\n").expect("bashrc");
        install(project.path(), &shell_file).expect("install");

        assert!(uninstall(&shell_file).expect("uninstall"));
        assert_eq!(fs::read_to_string(&shell_file).unwrap(), "before\nafter\n");
        assert!(!uninstall(&shell_file).expect("second uninstall"));
    }

    #[test]
    fn install_refuses_when_ca_is_missing() {
        let project = tempfile::tempdir().expect("tempdir");
        let shell_file = project.path().join(".bashrc");
        let error = install(project.path(), &shell_file).expect_err("missing CA");
        assert!(format!("{error:#}").contains("run `just mitm-ca-init` first"));
        assert!(!shell_file.exists());
    }
}
