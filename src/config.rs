#[derive(Clone)]
pub struct Config {
    pub ollama_key: String,
    pub stt_api_key: String,
    pub password: String,
    pub session_secret: [u8; 32],
    pub github_token: String,
    pub git_name: String,
    pub git_email: String,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        Ok(Self {
            ollama_key: required_var("OLLAMA_API_KEY")?,
            stt_api_key: required_var("OPENAI_API_KEY")?,
            password: required_var("BLOGGER_PASSWORD")?,
            session_secret: decode_session_secret(&required_var("BLOGGER_SESSION_SECRET")?)?,
            github_token: required_var("GITHUB_TOKEN")?,
            git_name: required_var("BLOGGER_GIT_NAME")?,
            git_email: required_var("BLOGGER_GIT_EMAIL")?,
        })
    }
}

fn required_var(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => Err(format!(
            "required environment variable {name} is missing or empty"
        )),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!(
            "required environment variable {name} must be valid UTF-8"
        )),
    }
}

fn decode_session_secret(secret: &str) -> Result<[u8; 32], String> {
    const ERROR: &str = "BLOGGER_SESSION_SECRET must be exactly 64 hexadecimal characters; generate one with `openssl rand -hex 32`";

    if secret.len() != 64 {
        return Err(ERROR.to_string());
    }

    let mut decoded = [0; 32];
    for (index, pair) in secret.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(pair[0]).ok_or_else(|| ERROR.to_string())?;
        let low = hex_value(pair[1]).ok_or_else(|| ERROR.to_string())?;
        decoded[index] = high << 4 | low;
    }
    Ok(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::decode_session_secret;

    #[test]
    fn validates_and_decodes_session_secret() {
        let decoded = decode_session_secret(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .unwrap();
        assert_eq!(decoded, std::array::from_fn(|index| index as u8));

        assert!(decode_session_secret(&"a".repeat(63)).is_err());
        assert!(decode_session_secret(&"a".repeat(65)).is_err());
        let invalid = decode_session_secret(&format!("{}g", "a".repeat(63))).unwrap_err();
        assert!(invalid.contains("openssl rand -hex 32"));
        assert!(decode_session_secret(&"AB".repeat(32)).is_ok());
    }
}
