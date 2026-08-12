#[derive(Clone)]
pub struct Config {
    pub ollama_key: String,
    pub stt_api_key: String,
    pub password: String,
    pub session_secret: [u8; 32],
    pub github_token: String,
    pub git_name: String,
    pub git_email: String,
    pub mcp_public_url: String,
    pub mcp_issuer: String,
    pub mcp_host: String,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let (mcp_public_url, mcp_issuer, mcp_host) =
            parse_mcp_public_url(&required_var("BLOGGER_MCP_PUBLIC_URL")?)?;
        Ok(Self {
            ollama_key: required_var("OLLAMA_API_KEY")?,
            stt_api_key: required_var("OPENAI_API_KEY")?,
            password: required_var("BLOGGER_PASSWORD")?,
            session_secret: decode_session_secret(&required_var("BLOGGER_SESSION_SECRET")?)?,
            github_token: required_var("GITHUB_TOKEN")?,
            git_name: required_var("BLOGGER_GIT_NAME")?,
            git_email: required_var("BLOGGER_GIT_EMAIL")?,
            mcp_public_url,
            mcp_issuer,
            mcp_host,
        })
    }
}

fn parse_mcp_public_url(value: &str) -> Result<(String, String, String), String> {
    const ERROR: &str = "BLOGGER_MCP_PUBLIC_URL must be a public HTTPS URL ending in /mcp, for example https://mcp.example.com/mcp";
    let url = reqwest::Url::parse(value).map_err(|_| ERROR.to_owned())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/mcp"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ERROR.to_owned());
    }

    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    };
    Ok((
        url.to_string().trim_end_matches('/').to_owned(),
        url.origin().ascii_serialization(),
        host,
    ))
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
    use super::{decode_session_secret, parse_mcp_public_url};

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

    #[test]
    fn validates_the_canonical_mcp_url() {
        assert_eq!(
            parse_mcp_public_url("https://mcp.example.com/mcp").unwrap(),
            (
                "https://mcp.example.com/mcp".to_owned(),
                "https://mcp.example.com".to_owned(),
                "mcp.example.com".to_owned(),
            )
        );
        assert_eq!(
            parse_mcp_public_url("https://mcp.example.com:8443/mcp")
                .unwrap()
                .2,
            "mcp.example.com:8443"
        );
        for invalid in [
            "http://mcp.example.com/mcp",
            "https://mcp.example.com/",
            "https://user@mcp.example.com/mcp",
            "https://mcp.example.com/mcp?x=1",
            "not a url",
        ] {
            assert!(parse_mcp_public_url(invalid).is_err(), "accepted {invalid}");
        }
    }
}
