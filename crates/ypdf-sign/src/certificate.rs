//! Reading a certificate for display.
//!
//! Everything here is what the certificate says about itself. None of it has
//! been checked against anything: an attacker can put any subject they like in
//! a certificate they issued themselves, so a name shown from here is evidence
//! of nothing until someone checks who issued it.

use der::Encode;
use x509_cert::Certificate;

use crate::model::CertificateInfo;

/// Read a certificate into the shape the reports use.
#[must_use]
pub fn describe(certificate: &Certificate) -> CertificateInfo {
    let tbs = &certificate.tbs_certificate;

    CertificateInfo {
        subject: tbs.subject.to_string(),
        issuer: tbs.issuer.to_string(),
        serial: hex(tbs.serial_number.as_bytes()),
        not_before: date(tbs.validity.not_before.to_unix_duration().as_secs()),
        not_after: date(tbs.validity.not_after.to_unix_duration().as_secs()),
        key_algorithm: key_algorithm(certificate),
        self_signed: tbs.subject == tbs.issuer,
    }
}

/// The public key algorithm, with its size where that is cheap to know.
fn key_algorithm(certificate: &Certificate) -> String {
    const RSA: &str = "1.2.840.113549.1.1.1";
    const EC: &str = "1.2.840.10045.2.1";
    const ED25519: &str = "1.3.101.112";

    let spki = &certificate.tbs_certificate.subject_public_key_info;
    let oid = spki.algorithm.oid.to_string();
    let bits = spki.subject_public_key.raw_bytes().len().saturating_mul(8);

    match oid.as_str() {
        RSA => format!("RSA, {} bit key", rsa_modulus_bits(spki).unwrap_or(bits)),
        EC => match bits {
            // An uncompressed point is one leading byte plus two coordinates.
            bits if bits >= 776 => "ECDSA P-384".to_string(),
            _ => "ECDSA P-256".to_string(),
        },
        ED25519 => "Ed25519".to_string(),
        other => format!("algorithm {other}"),
    }
}

/// The modulus size of an RSA key, which is the number people mean by "2048".
fn rsa_modulus_bits(spki: &x509_cert::spki::SubjectPublicKeyInfoOwned) -> Option<usize> {
    let der = spki.subject_public_key.raw_bytes();
    // RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }
    let key = pkcs1::RsaPublicKey::try_from(der).ok()?;
    let modulus = key.modulus.as_bytes();
    // DER integers carry a leading zero when the top bit is set.
    let significant = modulus.iter().position(|byte| *byte != 0).unwrap_or(0);
    Some(modulus.len().saturating_sub(significant).saturating_mul(8))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A Unix timestamp as `YYYY-MM-DD`.
///
/// Certificate dates are the one place where being off by a day matters and
/// nobody notices, so this is the civil-from-days algorithm rather than an
/// approximation with 365.25 in it.
fn date(seconds: u64) -> String {
    #[expect(
        clippy::cast_possible_wrap,
        reason = "certificate dates are well inside i64"
    )]
    let days = (seconds / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the Unix epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// A certificate as DER, for callers that want to export it.
#[must_use]
pub fn to_der(certificate: &Certificate) -> Option<Vec<u8>> {
    certificate.to_der().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_as_civil_dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_700_000_000), "2023-11-14");
        // A leap day, which is where an approximation goes wrong.
        assert_eq!(date(1_709_164_800), "2024-02-29");
    }

    #[test]
    fn serial_numbers_are_readable() {
        assert_eq!(hex(&[0x0a, 0xff, 0x01]), "0a:ff:01");
    }
}
