//! TLS to the printer, verified for what it is: a certificate that chains to
//! Bambu Lab's authorities and names the printer's serial number.
//!
//! The printer's certificate names it only by its common name, without the
//! subject alternative names that WebPKI checks, and its RTSPS port omits the
//! intermediate authority. So the chain is verified against the authorities
//! of `certs/bambu.pem`, the intermediates among them supplied too, and the
//! common name is compared with the serial number, whatever address the
//! printer was reached at.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, TrustAnchor, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, Error, SignatureScheme};

/// Bambu Lab's authorities: self-signed roots, cross-signed and intermediate
/// authorities.
const AUTHORITIES: &[u8] = include_bytes!("../certs/bambu.pem");

/// A TLS configuration that admits only the printer of serial `serial`.
pub fn printer_config(serial: &str) -> anyhow::Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = PrinterVerifier::new(serial, provider.signature_verification_algorithms)?;
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// The name the printer answers TLS to: its serial number.
pub fn printer_name(serial: &str) -> anyhow::Result<ServerName<'static>> {
    ServerName::try_from(serial.to_owned()).context("the serial number is not a valid TLS name")
}

#[derive(Debug)]
struct PrinterVerifier {
    serial: String,
    anchors: Vec<TrustAnchor<'static>>,
    intermediates: Vec<CertificateDer<'static>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl PrinterVerifier {
    fn new(serial: &str, algorithms: WebPkiSupportedAlgorithms) -> anyhow::Result<Self> {
        let mut anchors = Vec::new();
        let mut intermediates = Vec::new();
        for certificate in CertificateDer::pem_slice_iter(AUTHORITIES) {
            let certificate = certificate.context("certs/bambu.pem holds an unreadable certificate")?;
            let (_, parsed) = x509_parser::parse_x509_certificate(&certificate)
                .map_err(|error| anyhow!("certs/bambu.pem holds an unreadable certificate: {error}"))?;
            if parsed.subject() == parsed.issuer() {
                anchors.push(webpki::anchor_from_trusted_cert(&certificate)?.to_owned());
            } else {
                intermediates.push(certificate);
            }
        }
        Ok(Self { serial: serial.to_owned(), anchors, intermediates, algorithms })
    }
}

/// The common name of `certificate`, if it has exactly one.
fn common_name(certificate: &CertificateDer<'_>) -> Option<String> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate).ok()?;
    let mut names = parsed.subject().iter_common_name();
    let name = names.next()?.as_str().ok()?.to_owned();
    names.next().is_none().then_some(name)
}

impl ServerCertVerifier for PrinterVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        presented: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let certificate = webpki::EndEntityCert::try_from(end_entity)
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        let intermediates: Vec<CertificateDer<'_>> =
            presented.iter().cloned().chain(self.intermediates.iter().cloned()).collect();
        certificate
            .verify_for_usage(
                self.algorithms.all,
                &self.anchors,
                &intermediates,
                now,
                webpki::KeyUsage::server_auth(),
                None,
                None,
            )
            .map_err(|error| Error::InvalidCertificate(CertificateError::Other(rustls::OtherError(Arc::new(error)))))?;
        if common_name(end_entity).as_deref() != Some(self.serial.as_str()) {
            return Err(Error::InvalidCertificate(CertificateError::NotValidForName));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, certificate, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, certificate, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Install the process-wide default crypto provider, which libraries that
/// build their own TLS configurations rely on.
pub fn install_default_provider() {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_holds_roots_and_intermediates() {
        let algorithms = rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms;
        let verifier = PrinterVerifier::new("SERIAL", algorithms).unwrap();
        assert_eq!(verifier.anchors.len(), 3);
        assert_eq!(verifier.intermediates.len(), 3);
    }
}
