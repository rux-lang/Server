use std::time::Duration as StdDuration;

use async_trait::async_trait;
use rand::TryRngCore;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use reqwest::{Client, StatusCode, redirect};
use rux_application::{
    Clock, CredentialGenerationError, CredentialGenerator, GitHubIdentityProvider,
    GitHubUserProfile, IdentityProviderError, IdentityProviderErrorKind,
};
use serde::Deserialize;
use time::OffsetDateTime;
use url::Url;

const GITHUB_AUTHORIZE_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";
const GITHUB_API_VERSION: &str = "2026-03-10";
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct GitHubOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub callback_url: Url,
}

#[derive(Clone)]
pub struct GitHubOAuthClient {
    client: Client,
    client_id: String,
    client_secret: String,
    callback_url: Url,
    authorize_url: Url,
    token_url: Url,
    user_url: Url,
}

impl GitHubOAuthClient {
    /// Creates the production GitHub OAuth adapter.
    ///
    /// # Errors
    ///
    /// Returns an error when the fixed provider URLs or HTTP client cannot be configured.
    pub fn new(config: GitHubOAuthConfig) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::with_endpoints(
            config,
            Url::parse(GITHUB_AUTHORIZE_URL)?,
            Url::parse(GITHUB_TOKEN_URL)?,
            Url::parse(GITHUB_USER_URL)?,
        )?)
    }

    fn with_endpoints(
        config: GitHubOAuthConfig,
        authorize_url: Url,
        token_url: Url,
        user_url: Url,
    ) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .redirect(redirect::Policy::none())
            .timeout(StdDuration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            client_id: config.client_id,
            client_secret: config.client_secret,
            callback_url: config.callback_url,
            authorize_url,
            token_url,
            user_url,
        })
    }

    async fn token(&self, code: &str) -> Result<String, IdentityProviderError> {
        let response = self
            .client
            .post(self.token_url.clone())
            .header(ACCEPT, "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.callback_url.as_str()),
            ])
            .send()
            .await
            .map_err(|_| provider_unavailable())?;
        let status = response.status();
        if is_unavailable(status) {
            return Err(provider_unavailable());
        }
        if !status.is_success() {
            return Err(authorization_failed());
        }
        let body: TokenResponse = read_json(response).await?;
        body.access_token
            .filter(|token| !token.is_empty())
            .ok_or_else(authorization_failed)
    }

    async fn profile(&self, token: &str) -> Result<GitHubUserProfile, IdentityProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("rux-server"));
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(GITHUB_API_VERSION),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| authorization_failed())?,
        );
        headers
            .get_mut(AUTHORIZATION)
            .expect("header was inserted")
            .set_sensitive(true);

        let response = self
            .client
            .get(self.user_url.clone())
            .headers(headers)
            .send()
            .await
            .map_err(|_| provider_unavailable())?;
        let status = response.status();
        if is_unavailable(status) {
            return Err(provider_unavailable());
        }
        if !status.is_success() {
            return Err(authorization_failed());
        }
        let profile: UserResponse = read_json(response).await?;
        if profile.id == 0 || !valid_login(&profile.login) {
            return Err(authorization_failed());
        }

        Ok(GitHubUserProfile {
            github_user_id: profile.id,
            github_login: profile.login,
            display_name: profile.name.and_then(|name| truncate_utf8(name, 256)),
            avatar_url: profile
                .avatar_url
                .filter(|url| !url.is_empty() && url.len() <= 2048),
        })
    }
}

#[async_trait]
impl GitHubIdentityProvider for GitHubOAuthClient {
    fn authorization_url(&self, state: &str) -> Result<String, IdentityProviderError> {
        let mut url = self.authorize_url.clone();
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", self.callback_url.as_str())
            .append_pair("state", state);
        Ok(url.into())
    }

    async fn exchange_code(&self, code: &str) -> Result<GitHubUserProfile, IdentityProviderError> {
        let token = self.token(code).await?;
        self.profile(&token).await
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct UserResponse {
    id: u64,
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

async fn read_json<T: for<'de> Deserialize<'de>>(
    mut response: reqwest::Response,
) -> Result<T, IdentityProviderError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| provider_unavailable())? {
        if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(authorization_failed());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| authorization_failed())
}

fn is_unavailable(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

fn valid_login(login: &str) -> bool {
    if login.is_empty() || login.len() > 39 {
        return false;
    }
    let bytes = login.as_bytes();
    bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes.windows(2).all(|pair| pair != b"--")
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if value.len() > maximum_bytes {
        let mut boundary = maximum_bytes;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    Some(value)
}

fn authorization_failed() -> IdentityProviderError {
    IdentityProviderError::new(IdentityProviderErrorKind::AuthorizationFailed)
}

fn provider_unavailable() -> IdentityProviderError {
    IdentityProviderError::new(IdentityProviderErrorKind::Unavailable)
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

pub struct OsCredentialGenerator;

impl CredentialGenerator for OsCredentialGenerator {
    fn generate(&self) -> Result<[u8; 32], CredentialGenerationError> {
        let mut credential = [0; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut credential)
            .map_err(|_| CredentialGenerationError)?;
        Ok(credential)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    #[test]
    fn authorization_url_has_only_the_required_flow_parameters() {
        let client = GitHubOAuthClient::new(GitHubOAuthConfig {
            client_id: "client-id".into(),
            client_secret: "client-secret".into(),
            callback_url: Url::parse("https://api.example/v1/auth/github/callback")
                .expect("callback URL should parse"),
        })
        .expect("client should build");

        let url = Url::parse(
            &client
                .authorization_url("state-value")
                .expect("authorization URL should build"),
        )
        .expect("authorization URL should parse");
        let pairs: Vec<_> = url.query_pairs().collect();
        assert_eq!(url.as_str().split('?').next(), Some(GITHUB_AUTHORIZE_URL));
        assert_eq!(pairs.len(), 3);
        assert!(pairs.contains(&("client_id".into(), "client-id".into())));
        assert!(pairs.contains(&(
            "redirect_uri".into(),
            "https://api.example/v1/auth/github/callback".into()
        )));
        assert!(pairs.contains(&("state".into(), "state-value".into())));
    }

    #[test]
    fn profile_fields_follow_database_bounds() {
        assert!(valid_login("Owner-One"));
        assert!(!valid_login("-owner"));
        assert!(!valid_login("owner--one"));
        let long = "é".repeat(200);
        let bounded = truncate_utf8(long, 256).expect("name should remain present");
        assert_eq!(bounded.len(), 256);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn operating_system_credentials_are_not_constant() {
        let generator = OsCredentialGenerator;
        let first = generator.generate().expect("OS randomness should work");
        let second = generator.generate().expect("OS randomness should work");
        assert_ne!(first, [0; 32]);
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn exchange_posts_exact_fields_and_fetches_profile_with_safe_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("listener should have an address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = thread::spawn(move || {
            for body in [
                r#"{"access_token":"provider-token"}"#,
                r#"{"id":42,"login":"octocat","name":"The Octocat","avatar_url":"https://avatars.example/octocat"}"#,
            ] {
                let (mut stream, _) = listener.accept().expect("test request should connect");
                let request = read_request(&mut stream);
                captured
                    .lock()
                    .expect("request list should not be poisoned")
                    .push(request);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("test response should write");
            }
        });

        let base = Url::parse(&format!("http://{address}/")).expect("base URL should parse");
        let client = GitHubOAuthClient::with_endpoints(
            GitHubOAuthConfig {
                client_id: "client-id".into(),
                client_secret: "client-secret".into(),
                callback_url: Url::parse("http://localhost:8080/v1/auth/github/callback")
                    .expect("callback URL should parse"),
            },
            base.join("authorize").expect("authorize URL should build"),
            base.join("token").expect("token URL should build"),
            base.join("user").expect("user URL should build"),
        )
        .expect("client should build");

        let profile = client
            .exchange_code("provider-code")
            .await
            .expect("exchange should succeed");
        server.join().expect("test server should stop");

        assert_eq!(profile.github_user_id, 42);
        assert_eq!(profile.github_login, "octocat");
        let requests = requests
            .lock()
            .expect("request list should not be poisoned");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /token HTTP/1.1"));
        assert!(requests[0].contains("client_id=client-id"));
        assert!(requests[0].contains("client_secret=client-secret"));
        assert!(requests[0].contains("code=provider-code"));
        assert!(requests[0].contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fv1%2Fauth%2Fgithub%2Fcallback"
        ));
        let user_request = requests[1].to_ascii_lowercase();
        assert!(user_request.starts_with("get /user http/1.1"));
        assert!(user_request.contains("authorization: bearer provider-token"));
        assert!(user_request.contains("x-github-api-version: 2026-03-10"));
        assert!(user_request.contains("user-agent: rux-server"));
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = stream.read(&mut buffer).expect("test request should read");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            let Some(headers_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= headers_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(bytes).expect("test request should be UTF-8")
    }
}
