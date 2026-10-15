//! DM vault crypto (prototype of the `weavn-keys` crate, docs/DESIGN.md §7).
//!
//! Uses `age`: one encryption to many recipients = wrap the content key per
//! participant + once with the org escrow key. A participant decrypts with their
//! own identity; the org recovers with the escrow identity. Jira only ever holds
//! the base64 ciphertext.

use std::io::{Read, Write};
use std::str::FromStr;

use base64::prelude::{Engine as _, BASE64_STANDARD};
use serde::{Deserialize, Serialize};

/// An age keypair, stored as its bech32 string forms.
#[derive(Clone, Serialize, Deserialize)]
pub struct KeyPair {
    pub secret: String, // AGE-SECRET-KEY-1...
    pub public: String, // age1...
}

/// Generate a fresh X25519 keypair.
pub fn generate() -> KeyPair {
    let id = age::x25519::Identity::generate();
    let public = id.to_public().to_string();
    let secret = id.to_string().expose_secret().to_string();
    KeyPair { secret, public }
}

/// Encrypt `plaintext` to every recipient public key; return base64 ciphertext.
pub fn encrypt_to(recipient_publics: &[String], plaintext: &[u8]) -> Result<String, String> {
    let mut recips: Vec<Box<dyn age::Recipient + Send>> = Vec::new();
    for r in recipient_publics {
        let rec = age::x25519::Recipient::from_str(r).map_err(|e| format!("bad recipient: {e}"))?;
        recips.push(Box::new(rec));
    }
    let encryptor = age::Encryptor::with_recipients(recips).ok_or("no recipients")?;
    let mut out = Vec::new();
    let mut w = encryptor.wrap_output(&mut out).map_err(|e| e.to_string())?;
    w.write_all(plaintext).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(BASE64_STANDARD.encode(out))
}

/// Decrypt base64 ciphertext with one identity (a participant's or the escrow's).
/// Returns `Err` if that identity is not one of the recipients.
pub fn decrypt_with(secret: &str, b64: &str) -> Result<Vec<u8>, String> {
    let id = age::x25519::Identity::from_str(secret).map_err(|e| format!("bad identity: {e}"))?;
    let bytes = BASE64_STANDARD.decode(b64).map_err(|e| e.to_string())?;
    let decryptor = match age::Decryptor::new(&bytes[..]).map_err(|e| e.to_string())? {
        age::Decryptor::Recipients(d) => d,
        _ => return Err("not recipient-encrypted".into()),
    };
    let mut reader = decryptor
        .decrypt(std::iter::once(&id as &dyn age::Identity))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

// age's Identity::to_string returns a secret-wrapped string; bring the trait in.
use age::secrecy::ExposeSecret;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_reads_non_participant_cannot_and_escrow_recovers() {
        let alice = generate();
        let bob = generate();
        let carol = generate(); // not a recipient
        let escrow = generate();

        let ct = encrypt_to(
            &[alice.public.clone(), bob.public.clone(), escrow.public.clone()],
            b"meet at 3?",
        )
        .unwrap();

        // participant
        assert_eq!(decrypt_with(&bob.secret, &ct).unwrap(), b"meet at 3?");
        // escrow recovery
        assert_eq!(decrypt_with(&escrow.secret, &ct).unwrap(), b"meet at 3?");
        // outsider
        assert!(decrypt_with(&carol.secret, &ct).is_err());
    }
}
