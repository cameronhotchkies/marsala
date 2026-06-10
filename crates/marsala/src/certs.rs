use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
};

use crate::config::MitmConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedCa {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

pub fn init_ca(config: &MitmConfig) -> Result<CreatedCa> {
    ensure_target_absent(&config.ca_cert_path, "CA certificate")?;
    ensure_target_absent(&config.ca_key_path, "CA private key")?;
    ensure_parent_dir(&config.ca_cert_path)?;
    ensure_parent_dir(&config.ca_key_path)?;

    let (cert_pem, key_pem) = generate_ca_pem()?;

    write_new_public_file(&config.ca_cert_path, cert_pem.as_bytes()).with_context(|| {
        format!(
            "failed to write CA certificate {}",
            config.ca_cert_path.display()
        )
    })?;
    write_new_private_file(&config.ca_key_path, key_pem.as_bytes()).with_context(|| {
        format!(
            "failed to write CA private key {}",
            config.ca_key_path.display()
        )
    })?;

    Ok(CreatedCa {
        cert_path: config.ca_cert_path.clone(),
        key_path: config.ca_key_path.clone(),
    })
}

fn generate_ca_pem() -> Result<(String, String)> {
    let signing_key = KeyPair::generate().context("failed to generate CA private key")?;
    let mut params = CertificateParams::default();
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, "Marsala Local MITM CA");
    params.distinguished_name = distinguished_name;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

    let cert = params
        .self_signed(&signing_key)
        .context("failed to generate self-signed CA certificate")?;

    Ok((cert.pem(), signing_key.serialize_pem()))
}

fn ensure_target_absent(path: &Path, label: &str) -> Result<()> {
    if path.exists() {
        bail!(
            "{label} already exists at {}; refusing to overwrite",
            path.display()
        );
    }
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };

    if parent.exists() {
        return Ok(());
    }

    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    restrict_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to restrict directory {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn write_new_public_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_new_public_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(unix)]
fn write_new_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_new_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::BufReader};

    use rustls_pemfile::{certs, private_key};

    use super::*;

    #[test]
    fn init_ca_creates_parseable_pem_files_without_overwrite() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = MitmConfig {
            ca_cert_path: tempdir.path().join("certs/marsala-ca.pem"),
            ca_key_path: tempdir.path().join("certs/marsala-ca-key.pem"),
            ..MitmConfig::default()
        };

        let created = init_ca(&config).expect("init ca");

        assert_eq!(created.cert_path, config.ca_cert_path);
        assert_eq!(created.key_path, config.ca_key_path);

        let cert_pem = fs::read(&config.ca_cert_path).expect("read cert");
        let key_pem = fs::read(&config.ca_key_path).expect("read key");

        let parsed_certs = certs(&mut BufReader::new(cert_pem.as_slice()))
            .collect::<Result<Vec<_>, _>>()
            .expect("parse cert pem");
        assert_eq!(parsed_certs.len(), 1);

        let parsed_key = private_key(&mut BufReader::new(key_pem.as_slice()))
            .expect("parse key pem")
            .expect("private key");
        assert!(!parsed_key.secret_der().is_empty());

        let error = init_ca(&config).expect_err("second init must not overwrite");
        assert!(format!("{error:#}").contains("refusing to overwrite"));
    }

    #[cfg(unix)]
    #[test]
    fn init_ca_restricts_created_directory_and_key_file() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let config = MitmConfig {
            ca_cert_path: tempdir.path().join("certs/marsala-ca.pem"),
            ca_key_path: tempdir.path().join("certs/marsala-ca-key.pem"),
            ..MitmConfig::default()
        };

        init_ca(&config).expect("init ca");

        let certs_mode = fs::metadata(tempdir.path().join("certs"))
            .expect("certs dir metadata")
            .permissions()
            .mode()
            & 0o777;
        let key_mode = fs::metadata(&config.ca_key_path)
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(certs_mode, 0o700);
        assert_eq!(key_mode, 0o600);
    }

    #[test]
    fn repo_gitignore_protects_default_certs_directory() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let gitignore = fs::read_to_string(repo_root.join(".gitignore")).expect("read .gitignore");

        assert!(
            gitignore.lines().any(|line| line.trim() == "/certs"),
            ".gitignore must protect generated Marsala CA material"
        );
    }
}
