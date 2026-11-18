use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use rcgen::{
    Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, BufReader},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer},
    },
};

/// The certificate the host shows and its key.
pub struct Identity {
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl Identity {
    fn from_files(certificate: &Path, key: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut cert = BufReader::new(fs::File::open(certificate)?);
        let certificates = rustls_pemfile::certs(&mut cert).collect::<io::Result<Vec<_>>>()?;
        let mut key = BufReader::new(fs::File::open(key)?);
        let key = rustls_pemfile::private_key(&mut key)?.ok_or("Missing TLS private key.")?;
        Ok(Self { certificates, key })
    }
}

/// How long a certificate lasts. Controllers pin it, so its expiry does not end a connection.
const LIFETIME_DAYS: i64 = 365;

/// A certificate for `address` that signs itself. No CA vouches for it: a controller pins its
/// fingerprint, which is in the line `--install` prints.
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

/// The port the host serves on.
pub const PORT: u16 = 8767;

/// Where this user's token, certificate and key are kept.
fn directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set.")?;
    Ok(PathBuf::from(local).join("WindowsTools").join("state"))
}

/// What the host serves with, as `--install` made it.
pub struct State {
    pub token: String,
    pub fingerprint: String,
    pub identity: Identity,
}

impl State {
    /// Makes the token and a certificate for `address` once: when they exist they are kept, so
    /// a controller's line stays the same through updates.
    pub fn create(address: &str) -> Result<(), Box<dyn std::error::Error>> {
        let ip: Ipv4Addr = address
            .parse()
            .map_err(|_| "Give an IPv4 address of this computer.")?;
        if ip.is_unspecified() || ip.is_loopback() {
            return Err("Give an IPv4 address the controller can reach.".into());
        }
        let directory = directory()?;
        if directory.try_exists()? {
            return Ok(());
        }
        make_private(&directory)?;
        let (cert, key) = self_signed(address)?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        for (name, text) in [
            ("server.pem", cert.pem()),
            ("server.key", key.serialize_pem()),
            ("tools.token", token),
        ] {
            fs::write(directory.join(name), text)?;
        }
        Ok(())
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let directory = directory()?;
        let missing = |error: io::Error| {
            format!(
                "{error}: {} is incomplete; delete it and run --install again.",
                directory.display()
            )
        };
        let token = fs::read_to_string(directory.join("tools.token"))
            .map_err(missing)?
            .trim()
            .to_owned();
        let identity =
            Identity::from_files(&directory.join("server.pem"), &directory.join("server.key"))?;
        let fingerprint = fingerprint(identity.certificates.first().ok_or("No certificate.")?);
        Ok(Self {
            token,
            fingerprint,
            identity,
        })
    }
}

/// Serves `app` on `address` alone, waiting for it to appear: at sign-in a network may not be up
/// yet.
pub async fn serve(
    app: Router,
    address: Ipv4Addr,
    identity: Identity,
) -> Result<(), Box<dyn std::error::Error>> {
    let acceptor = TlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(identity.certificates, identity.key)?,
    ));
    let listener = loop {
        match TcpListener::bind(SocketAddr::from((address, PORT))).await {
            Ok(listener) => break listener,
            Err(error) if error.kind() == io::ErrorKind::AddrNotAvailable => {
                tokio::time::sleep(Duration::from_secs(2)).await
            }
            Err(error) => return Err(error.into()),
        }
    };
    eprintln!("Windows Tools listening on {address}:{PORT}");
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
