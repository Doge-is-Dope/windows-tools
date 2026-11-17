use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use rcgen::{
    Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, BufReader},
    net::{IpAddr, SocketAddr},
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};

/// The certificate the host shows and its key.
pub struct Identity {
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl Identity {
    /// What `--prepare` wrote.
    pub fn from_files(certificate: &Path, key: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut cert = BufReader::new(fs::File::open(certificate)?);
        let certificates = rustls_pemfile::certs(&mut cert).collect::<io::Result<Vec<_>>>()?;
        let mut key = BufReader::new(fs::File::open(key)?);
        let key = rustls_pemfile::private_key(&mut key)?.ok_or("Missing TLS private key.")?;
        Ok(Self { certificates, key })
    }

    /// A certificate for `address` made now and kept in memory alone, with its fingerprint.
    pub fn ephemeral(address: &str) -> Result<(Self, String), Box<dyn std::error::Error>> {
        let (certificate, key) = self_signed(address)?;
        let certificate = certificate.der().clone();
        let fingerprint = fingerprint(&certificate);
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        Ok((
            Self {
                certificates: vec![certificate],
                key,
            },
            fingerprint,
        ))
    }
}

/// How long a certificate lasts. Controllers pin it, so its expiry does not end a connection.
const LIFETIME_DAYS: i64 = 365;

/// A certificate for `address` that signs itself. No CA vouches for it: a controller pins its
/// fingerprint, told it over a channel it trusts or seen on first use.
fn self_signed(address: &str) -> Result<(Certificate, KeyPair), rcgen::Error> {
    let now = time::OffsetDateTime::now_utc();
    let mut params = CertificateParams::new(vec![address.to_owned()])?;
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(LIFETIME_DAYS);
    params
        .distinguished_name
        .push(DnType::CommonName, "Windows Tools");
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate()?;
    Ok((params.self_signed(&key)?, key))
}

/// The hex SHA-256 of a certificate, as controllers pin it.
fn fingerprint(certificate: &[u8]) -> String {
    format!("{:x}", Sha256::digest(certificate))
}

/// A new directory only this user can read: the token and the key go there.
fn make_private(directory: &Path) -> io::Result<()> {
    if directory.try_exists()? {
        return Err(io::Error::other("The state directory already exists."));
    }
    fs::create_dir_all(directory)?;
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let granted = Command::new("icacls")
        .arg(directory)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(OI)(CI)F"))
        .stdout(Stdio::null())
        .status()?;
    if granted.success() {
        Ok(())
    } else {
        Err(io::Error::other("Could not restrict the state directory."))
    }
}

pub fn prepare(directory: &Path, address: &str) -> Result<(), Box<dyn std::error::Error>> {
    let ip: IpAddr = address.parse()?;
    if ip.is_unspecified() || !ip.is_ipv4() || !directory.is_absolute() {
        return Err("Choose an explicit IPv4 and absolute private state directory.".into());
    }
    make_private(directory)?;
    let (cert, key) = self_signed(address)?;
    let fingerprint = fingerprint(cert.der());
    for (name, text) in [
        ("server.pem", cert.pem()),
        ("server.key", key.serialize_pem()),
        (
            "tools.token",
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        ),
    ] {
        fs::write(directory.join(name), text)?;
    }
    let connection = json!({
        "url": format!("https://{address}:8767"),
        "tokenFile": directory.join("tools.token"),
        "certificateFile": directory.join("server.pem"),
        "keyFile": directory.join("server.key"),
        "fingerprint": fingerprint,
    });
    fs::write(
        directory.join("connection.json"),
        serde_json::to_string_pretty(&connection)?,
    )?;
    println!(
        "Connection metadata: {}\nCertificate SHA-256: {fingerprint}",
        directory.join("connection.json").display()
    );
    Ok(())
}
/// Serves `app` at `url`; `listening` runs once the port is bound.
pub async fn serve(
    app: Router,
    url: &str,
    identity: Identity,
    listening: impl FnOnce() -> io::Result<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = url
        .strip_prefix("https://")
        .ok_or("The URL must be https://<IP>:<port>.")?
        .parse()?;
    let acceptor = TlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(identity.certificates, identity.key)?,
    ));
    let listener = TcpListener::bind(address).await?;
    listening()?;
    eprintln!("Windows Tools listening on {url}");
    let limit = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let permit = limit.clone().acquire_owned().await?;
        // A failed accept (a reset connection, no handles left) ends that connection, not the host.
        let Ok((socket, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let acceptor = acceptor.clone();
        let service = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let _permit = permit;
            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(5), acceptor.accept(socket)).await
            {
                let _ = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(5))
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            }
        });
    }
}
