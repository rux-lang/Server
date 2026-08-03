use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};

use crate::{
    AccountRepository, AuditEvent, GitHubUserProfile, NewSession, RepositoryConflict,
    RepositoryError, RepositoryErrorKind, SecretHash, WriteOutcome,
};

pub const OAUTH_STATE_LIFETIME_SECONDS: i64 = 10 * 60;
pub const SESSION_LIFETIME_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const SESSION_ROTATION_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const SESSION_TOUCH_INTERVAL_SECONDS: i64 = 5 * 60;
const CREDENTIAL_LENGTH: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityProviderErrorKind {
    AuthorizationFailed,
    Unavailable,
}

#[derive(Debug)]
pub struct IdentityProviderError {
    kind: IdentityProviderErrorKind,
}

impl IdentityProviderError {
    #[must_use]
    pub const fn new(kind: IdentityProviderErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> IdentityProviderErrorKind {
        self.kind
    }
}

impl fmt::Display for IdentityProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "identity provider request failed: {:?}",
            self.kind
        )
    }
}

impl Error for IdentityProviderError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationErrorKind {
    InvalidState,
    AccountConflict,
    AuthorizationFailed,
    ProviderUnavailable,
    AuthenticationRequired,
    InvalidCsrf,
    AuthenticationUnavailable,
}

#[derive(Debug)]
pub struct AuthenticationError {
    kind: AuthenticationErrorKind,
}

impl AuthenticationError {
    #[must_use]
    pub const fn new(kind: AuthenticationErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> AuthenticationErrorKind {
        self.kind
    }
}

impl fmt::Display for AuthenticationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "authentication failed: {:?}", self.kind)
    }
}

impl Error for AuthenticationError {}

#[derive(Debug)]
pub struct CredentialGenerationError;

impl fmt::Display for CredentialGenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("secure credential generation failed")
    }
}

impl Error for CredentialGenerationError {}

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

pub trait CredentialGenerator: Send + Sync {
    /// Generates one cryptographically secure credential.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system cannot provide secure randomness.
    fn generate(&self) -> Result<[u8; CREDENTIAL_LENGTH], CredentialGenerationError>;
}

#[async_trait]
pub trait GitHubIdentityProvider: Send + Sync {
    /// Builds the fixed provider authorization URL for a browser-bound state value.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured provider URL cannot be constructed.
    fn authorization_url(&self, state: &str) -> Result<String, IdentityProviderError>;

    async fn exchange_code(&self, code: &str) -> Result<GitHubUserProfile, IdentityProviderError>;
}

pub struct LoginStart {
    pub authorization_url: String,
    pub state: String,
}

pub struct CompletedLogin {
    pub session_credential: String,
    pub csrf_credential: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct AuthenticatedSession {
    pub user: crate::UserRecord,
    pub csrf_credential: String,
    pub session_credential: Option<String>,
    pub expires_at: OffsetDateTime,
    pub remaining_lifetime_seconds: i64,
}

#[async_trait]
pub trait Authentication: Send + Sync {
    async fn begin_github_login(&self) -> Result<LoginStart, AuthenticationError>;

    async fn complete_github_login(
        &self,
        code: &str,
        callback_state: &str,
        cookie_state: &str,
    ) -> Result<CompletedLogin, AuthenticationError>;

    async fn session(
        &self,
        session_credential: &str,
        csrf_credential: Option<&str>,
    ) -> Result<AuthenticatedSession, AuthenticationError>;

    async fn authenticate_mutation(
        &self,
        session_credential: &str,
        csrf_cookie: Option<&str>,
        csrf_header: Option<&str>,
    ) -> Result<AuthenticatedSession, AuthenticationError>;

    async fn logout(
        &self,
        session_credential: &str,
        csrf_cookie: Option<&str>,
        csrf_header: Option<&str>,
    ) -> Result<(), AuthenticationError>;
}

pub struct AuthenticationService {
    repository: Arc<dyn AccountRepository>,
    identity_provider: Arc<dyn GitHubIdentityProvider>,
    clock: Arc<dyn Clock>,
    credentials: Arc<dyn CredentialGenerator>,
}

impl AuthenticationService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn AccountRepository>,
        identity_provider: Arc<dyn GitHubIdentityProvider>,
        clock: Arc<dyn Clock>,
        credentials: Arc<dyn CredentialGenerator>,
    ) -> Self {
        Self {
            repository,
            identity_provider,
            clock,
            credentials,
        }
    }
}

#[async_trait]
impl Authentication for AuthenticationService {
    async fn begin_github_login(&self) -> Result<LoginStart, AuthenticationError> {
        let state = encode_credential(self.generate_credential()?);
        let authorization_url = self
            .identity_provider
            .authorization_url(&state)
            .map_err(|error| map_provider(&error))?;
        Ok(LoginStart {
            authorization_url,
            state,
        })
    }

    async fn complete_github_login(
        &self,
        code: &str,
        callback_state: &str,
        cookie_state: &str,
    ) -> Result<CompletedLogin, AuthenticationError> {
        if !oauth_states_match(callback_state, cookie_state) {
            return Err(AuthenticationError::new(
                AuthenticationErrorKind::InvalidState,
            ));
        }

        let profile = self
            .identity_provider
            .exchange_code(code)
            .await
            .map_err(|error| map_provider(&error))?;
        let session_credential = self.generate_credential()?;
        let csrf_credential = self.generate_credential()?;
        let expires_at = self
            .clock
            .now()
            .checked_add(Duration::seconds(SESSION_LIFETIME_SECONDS))
            .ok_or_else(|| {
                AuthenticationError::new(AuthenticationErrorKind::AuthenticationUnavailable)
            })?;

        let mut transaction = self
            .repository
            .begin_account()
            .await
            .map_err(|error| map_repository(&error))?;
        let user = match transaction.upsert_github_user(&profile).await {
            Ok(user) => user,
            Err(error) => {
                let mapped = map_repository(&error);
                let _ = transaction.rollback().await;
                return Err(mapped);
            }
        };
        let new_session = NewSession {
            user_id: user.id,
            secret_hash: hash_credential(&session_credential),
            csrf_hash: hash_credential(&csrf_credential),
            expires_at,
        };
        let created_session = match transaction.create_session(&new_session).await {
            Ok(session) => session,
            Err(error) => {
                let mapped = map_repository(&error);
                let _ = transaction.rollback().await;
                return Err(mapped);
            }
        };
        if let Err(error) = transaction
            .append_audit(&AuditEvent::session_created(user.id, created_session.id))
            .await
        {
            let mapped = map_repository(&error);
            let _ = transaction.rollback().await;
            return Err(mapped);
        }
        transaction
            .commit()
            .await
            .map_err(|error| map_repository(&error))?;

        Ok(CompletedLogin {
            session_credential: encode_credential(session_credential),
            csrf_credential: encode_credential(csrf_credential),
            expires_at,
        })
    }

    async fn session(
        &self,
        session_credential: &str,
        csrf_credential: Option<&str>,
    ) -> Result<AuthenticatedSession, AuthenticationError> {
        let credential =
            decode_credential(session_credential).ok_or_else(authentication_required)?;
        let secret_hash = hash_credential(&credential);
        let session = self
            .repository
            .session_by_secret_hash(secret_hash)
            .await
            .map_err(|error| map_repository(&error))?
            .ok_or_else(authentication_required)?;
        let now = self.clock.now();
        if !session_is_active(&session, now) {
            return Err(authentication_required());
        }
        let user = self
            .repository
            .user_by_id(session.user_id)
            .await
            .map_err(|error| map_repository(&error))?
            .filter(|user| user.anonymized_at.is_none() && user.github_login.is_some())
            .ok_or_else(authentication_required)?;

        let csrf_is_valid = csrf_credential
            .is_some_and(|credential| credential_matches_hash(credential, session.csrf_hash));
        let rotation_due = now - session.created_at >= Duration::seconds(SESSION_ROTATION_SECONDS);
        if rotation_due || !csrf_is_valid {
            return self.rotate_session(secret_hash, &session, user, now).await;
        }

        if now - session.last_seen_at >= Duration::seconds(SESSION_TOUCH_INTERVAL_SECONDS) {
            self.touch_session(session.id, now).await?;
        }

        Ok(AuthenticatedSession {
            user,
            csrf_credential: csrf_credential.unwrap_or_default().to_owned(),
            session_credential: None,
            expires_at: session.expires_at,
            remaining_lifetime_seconds: (session.expires_at - now).whole_seconds(),
        })
    }

    async fn authenticate_mutation(
        &self,
        session_credential: &str,
        csrf_cookie: Option<&str>,
        csrf_header: Option<&str>,
    ) -> Result<AuthenticatedSession, AuthenticationError> {
        let credential =
            decode_credential(session_credential).ok_or_else(authentication_required)?;
        let session = self
            .repository
            .session_by_secret_hash(hash_credential(&credential))
            .await
            .map_err(|error| map_repository(&error))?
            .ok_or_else(authentication_required)?;
        let now = self.clock.now();
        if !session_is_active(&session, now) {
            return Err(authentication_required());
        }
        if !csrf_credentials_match(csrf_cookie, csrf_header, session.csrf_hash) {
            return Err(AuthenticationError::new(
                AuthenticationErrorKind::InvalidCsrf,
            ));
        }
        let user = self
            .repository
            .user_by_id(session.user_id)
            .await
            .map_err(|error| map_repository(&error))?
            .filter(|user| user.anonymized_at.is_none() && user.github_login.is_some())
            .ok_or_else(authentication_required)?;
        if now - session.last_seen_at >= Duration::seconds(SESSION_TOUCH_INTERVAL_SECONDS) {
            self.touch_session(session.id, now).await?;
        }
        Ok(AuthenticatedSession {
            user,
            csrf_credential: csrf_cookie.unwrap_or_default().to_owned(),
            session_credential: None,
            expires_at: session.expires_at,
            remaining_lifetime_seconds: (session.expires_at - now).whole_seconds(),
        })
    }

    async fn logout(
        &self,
        session_credential: &str,
        csrf_cookie: Option<&str>,
        csrf_header: Option<&str>,
    ) -> Result<(), AuthenticationError> {
        let Some(credential) = decode_credential(session_credential) else {
            return Ok(());
        };
        let secret_hash = hash_credential(&credential);
        let Some(session) = self
            .repository
            .session_by_secret_hash(secret_hash)
            .await
            .map_err(|error| map_repository(&error))?
        else {
            return Ok(());
        };
        let now = self.clock.now();
        if !session_is_active(&session, now) {
            return Ok(());
        }
        if !csrf_credentials_match(csrf_cookie, csrf_header, session.csrf_hash) {
            return Err(AuthenticationError::new(
                AuthenticationErrorKind::InvalidCsrf,
            ));
        }

        let mut transaction = self
            .repository
            .begin_account()
            .await
            .map_err(|error| map_repository(&error))?;
        let locked = match transaction.lock_session_by_secret_hash(secret_hash).await {
            Ok(session) => session,
            Err(error) => {
                let mapped = map_repository(&error);
                let _ = transaction.rollback().await;
                return Err(mapped);
            }
        };
        let Some(locked) = locked.filter(|session| session_is_active(session, now)) else {
            transaction
                .rollback()
                .await
                .map_err(|error| map_repository(&error))?;
            return Ok(());
        };
        let outcome = match transaction.revoke_session(locked.id, now).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let mapped = map_repository(&error);
                let _ = transaction.rollback().await;
                return Err(mapped);
            }
        };
        if outcome == WriteOutcome::Applied
            && let Err(error) = transaction
                .append_audit(&AuditEvent::session_revoked(locked.user_id, locked.id))
                .await
        {
            let mapped = map_repository(&error);
            let _ = transaction.rollback().await;
            return Err(mapped);
        }
        transaction
            .commit()
            .await
            .map_err(|error| map_repository(&error))
    }
}

impl AuthenticationService {
    fn generate_credential(&self) -> Result<[u8; CREDENTIAL_LENGTH], AuthenticationError> {
        self.credentials.generate().map_err(|_| {
            AuthenticationError::new(AuthenticationErrorKind::AuthenticationUnavailable)
        })
    }

    async fn rotate_session(
        &self,
        secret_hash: SecretHash,
        session: &crate::SessionRecord,
        user: crate::UserRecord,
        now: OffsetDateTime,
    ) -> Result<AuthenticatedSession, AuthenticationError> {
        let session_credential = self.generate_credential()?;
        let csrf_credential = self.generate_credential()?;
        let mut transaction = self
            .repository
            .begin_account()
            .await
            .map_err(|error| map_repository(&error))?;
        let locked = match transaction.lock_session_by_secret_hash(secret_hash).await {
            Ok(session) => session,
            Err(error) => {
                let mapped = map_repository(&error);
                let _ = transaction.rollback().await;
                return Err(mapped);
            }
        };
        let Some(locked) = locked.filter(|locked| session_is_active(locked, now)) else {
            let _ = transaction.rollback().await;
            return Err(authentication_required());
        };
        if locked.id != session.id {
            let _ = transaction.rollback().await;
            return Err(authentication_required());
        }

        let replacement = NewSession {
            user_id: session.user_id,
            secret_hash: hash_credential(&session_credential),
            csrf_hash: hash_credential(&csrf_credential),
            expires_at: session.expires_at,
        };
        let replacement = match transaction.create_session(&replacement).await {
            Ok(session) => session,
            Err(error) => {
                let mapped = map_repository(&error);
                let _ = transaction.rollback().await;
                return Err(mapped);
            }
        };
        if let Err(error) = transaction.revoke_session(session.id, now).await {
            let mapped = map_repository(&error);
            let _ = transaction.rollback().await;
            return Err(mapped);
        }
        if let Err(error) = transaction
            .append_audit(&AuditEvent::session_rotated(
                session.user_id,
                session.id,
                replacement.id,
            ))
            .await
        {
            let mapped = map_repository(&error);
            let _ = transaction.rollback().await;
            return Err(mapped);
        }
        transaction
            .commit()
            .await
            .map_err(|error| map_repository(&error))?;

        Ok(AuthenticatedSession {
            user,
            csrf_credential: encode_credential(csrf_credential),
            session_credential: Some(encode_credential(session_credential)),
            expires_at: session.expires_at,
            remaining_lifetime_seconds: (session.expires_at - now).whole_seconds(),
        })
    }

    async fn touch_session(
        &self,
        id: crate::SessionId,
        now: OffsetDateTime,
    ) -> Result<(), AuthenticationError> {
        let mut transaction = self
            .repository
            .begin_account()
            .await
            .map_err(|error| map_repository(&error))?;
        if let Err(error) = transaction.touch_session(id, now).await {
            let mapped = map_repository(&error);
            let _ = transaction.rollback().await;
            return Err(mapped);
        }
        transaction
            .commit()
            .await
            .map_err(|error| map_repository(&error))
    }
}

fn encode_credential(credential: [u8; CREDENTIAL_LENGTH]) -> String {
    URL_SAFE_NO_PAD.encode(credential)
}

fn decode_credential(value: &str) -> Option<[u8; CREDENTIAL_LENGTH]> {
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    bytes.try_into().ok()
}

#[must_use]
pub fn oauth_states_match(callback: &str, cookie: &str) -> bool {
    let (Some(callback), Some(cookie)) = (decode_credential(callback), decode_credential(cookie))
    else {
        return false;
    };
    bool::from(callback.ct_eq(&cookie))
}

fn hash_credential(credential: &[u8; CREDENTIAL_LENGTH]) -> SecretHash {
    SecretHash::new(Sha256::digest(credential).into())
}

fn credential_matches_hash(value: &str, expected: SecretHash) -> bool {
    let Some(credential) = decode_credential(value) else {
        return false;
    };
    let actual = hash_credential(&credential);
    bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
}

fn csrf_credentials_match(
    cookie: Option<&str>,
    header: Option<&str>,
    expected: SecretHash,
) -> bool {
    let (Some(cookie), Some(header)) = (cookie, header) else {
        return false;
    };
    let (Some(cookie_bytes), Some(header_bytes)) =
        (decode_credential(cookie), decode_credential(header))
    else {
        return false;
    };
    bool::from(cookie_bytes.ct_eq(&header_bytes)) && credential_matches_hash(cookie, expected)
}

fn session_is_active(session: &crate::SessionRecord, now: OffsetDateTime) -> bool {
    session.revoked_at.is_none() && session.expires_at > now
}

fn authentication_required() -> AuthenticationError {
    AuthenticationError::new(AuthenticationErrorKind::AuthenticationRequired)
}

fn map_provider(error: &IdentityProviderError) -> AuthenticationError {
    AuthenticationError::new(match error.kind() {
        IdentityProviderErrorKind::AuthorizationFailed => {
            AuthenticationErrorKind::AuthorizationFailed
        }
        IdentityProviderErrorKind::Unavailable => AuthenticationErrorKind::ProviderUnavailable,
    })
}

fn map_repository(error: &RepositoryError) -> AuthenticationError {
    let kind = match error.kind() {
        RepositoryErrorKind::Conflict(
            RepositoryConflict::GitHubIdentity | RepositoryConflict::GitHubLogin,
        ) => AuthenticationErrorKind::AccountConflict,
        _ => AuthenticationErrorKind::AuthenticationUnavailable,
    };
    AuthenticationError::new(kind)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use uuid::Uuid;

    use crate::{
        AccountReader, AccountTransaction, AccountUnitOfWork, NewSession, SessionId, SessionRecord,
        UserId, UserRecord,
    };

    use super::*;

    struct FixedClock(OffsetDateTime);

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            self.0
        }
    }

    struct FixedCredentials(Mutex<VecDeque<[u8; CREDENTIAL_LENGTH]>>);

    impl CredentialGenerator for FixedCredentials {
        fn generate(&self) -> Result<[u8; CREDENTIAL_LENGTH], CredentialGenerationError> {
            self.0
                .lock()
                .expect("credential queue should not be poisoned")
                .pop_front()
                .ok_or(CredentialGenerationError)
        }
    }

    struct FakeProvider {
        exchanges: Mutex<usize>,
    }

    #[async_trait]
    impl GitHubIdentityProvider for FakeProvider {
        fn authorization_url(&self, state: &str) -> Result<String, IdentityProviderError> {
            Ok(format!("https://github.example/authorize?state={state}"))
        }

        async fn exchange_code(
            &self,
            _code: &str,
        ) -> Result<GitHubUserProfile, IdentityProviderError> {
            *self
                .exchanges
                .lock()
                .expect("counter should not be poisoned") += 1;
            Ok(GitHubUserProfile {
                github_user_id: 42,
                github_login: "octocat".into(),
                display_name: Some("The Octocat".into()),
                avatar_url: Some("https://avatars.example/octocat".into()),
            })
        }
    }

    #[derive(Default)]
    struct FakeState {
        user: Option<UserRecord>,
        session: Option<SessionRecord>,
        audits: Vec<AuditEvent>,
        audit_fails: bool,
        committed: bool,
        rolled_back: usize,
        revoked: bool,
        touched_at: Option<OffsetDateTime>,
    }

    #[derive(Clone, Default)]
    struct FakeRepository(Arc<Mutex<FakeState>>);

    #[async_trait]
    impl AccountReader for FakeRepository {
        async fn user_by_id(&self, id: UserId) -> Result<Option<UserRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should not be poisoned")
                .user
                .clone()
                .filter(|user| user.id == id))
        }

        async fn user_by_github_id(&self, _id: u64) -> Result<Option<UserRecord>, RepositoryError> {
            Ok(None)
        }

        async fn user_by_github_login(
            &self,
            login: &str,
        ) -> Result<Option<UserRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should not be poisoned")
                .user
                .clone()
                .filter(|user| {
                    user.github_login
                        .as_deref()
                        .is_some_and(|stored| stored.eq_ignore_ascii_case(login))
                }))
        }

        async fn session_by_secret_hash(
            &self,
            hash: SecretHash,
        ) -> Result<Option<SessionRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should not be poisoned")
                .session
                .clone()
                .filter(|session| session.secret_hash == hash))
        }
    }

    #[async_trait]
    impl AccountUnitOfWork for FakeRepository {
        async fn begin_account(&self) -> Result<Box<dyn AccountTransaction>, RepositoryError> {
            Ok(Box::new(FakeTransaction(self.0.clone())))
        }
    }

    struct FakeTransaction(Arc<Mutex<FakeState>>);

    #[async_trait]
    impl crate::AuditWriter for FakeTransaction {
        async fn append_audit(&mut self, event: &AuditEvent) -> Result<(), RepositoryError> {
            let mut state = self.0.lock().expect("state should not be poisoned");
            if state.audit_fails {
                return Err(RepositoryError::new(RepositoryErrorKind::Unexpected));
            }
            state.audits.push(event.clone());
            Ok(())
        }
    }

    #[async_trait]
    impl crate::AccountWriter for FakeTransaction {
        async fn upsert_github_user(
            &mut self,
            profile: &GitHubUserProfile,
        ) -> Result<UserRecord, RepositoryError> {
            let now = OffsetDateTime::UNIX_EPOCH;
            let user = UserRecord {
                id: UserId::new(Uuid::from_u128(7)),
                github_user_id: Some(profile.github_user_id),
                github_login: Some(profile.github_login.clone()),
                display_name: profile.display_name.clone(),
                avatar_url: profile.avatar_url.clone(),
                created_at: now,
                updated_at: now,
                anonymized_at: None,
            };
            self.0.lock().expect("state should not be poisoned").user = Some(user.clone());
            Ok(user)
        }

        async fn create_session(
            &mut self,
            session: &NewSession,
        ) -> Result<SessionRecord, RepositoryError> {
            let now = OffsetDateTime::UNIX_EPOCH;
            let record = SessionRecord {
                id: SessionId::new(Uuid::from_u128(9)),
                user_id: session.user_id,
                secret_hash: session.secret_hash,
                csrf_hash: session.csrf_hash,
                created_at: now,
                last_seen_at: now,
                expires_at: session.expires_at,
                revoked_at: None,
            };
            self.0.lock().expect("state should not be poisoned").session = Some(record.clone());
            Ok(record)
        }

        async fn touch_session(
            &mut self,
            _id: SessionId,
            at: OffsetDateTime,
        ) -> Result<WriteOutcome, RepositoryError> {
            self.0
                .lock()
                .expect("state should not be poisoned")
                .touched_at = Some(at);
            Ok(WriteOutcome::Applied)
        }

        async fn revoke_session(
            &mut self,
            _id: SessionId,
            _at: OffsetDateTime,
        ) -> Result<WriteOutcome, RepositoryError> {
            self.0.lock().expect("state should not be poisoned").revoked = true;
            Ok(WriteOutcome::Applied)
        }
    }

    #[async_trait]
    impl AccountTransaction for FakeTransaction {
        async fn lock_session_by_secret_hash(
            &mut self,
            hash: SecretHash,
        ) -> Result<Option<SessionRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should not be poisoned")
                .session
                .clone()
                .filter(|session| session.secret_hash == hash))
        }

        async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
            self.0
                .lock()
                .expect("state should not be poisoned")
                .committed = true;
            Ok(())
        }

        async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
            self.0
                .lock()
                .expect("state should not be poisoned")
                .rolled_back += 1;
            Ok(())
        }
    }

    fn service_at(
        repository: &FakeRepository,
        provider: Arc<FakeProvider>,
        now: OffsetDateTime,
    ) -> AuthenticationService {
        AuthenticationService::new(
            Arc::new(repository.clone()),
            provider,
            Arc::new(FixedClock(now)),
            Arc::new(FixedCredentials(Mutex::new(VecDeque::from([
                [1; CREDENTIAL_LENGTH],
                [2; CREDENTIAL_LENGTH],
                [3; CREDENTIAL_LENGTH],
                [4; CREDENTIAL_LENGTH],
                [5; CREDENTIAL_LENGTH],
            ])))),
        )
    }

    fn service(repository: &FakeRepository, provider: Arc<FakeProvider>) -> AuthenticationService {
        service_at(repository, provider, OffsetDateTime::UNIX_EPOCH)
    }

    #[tokio::test]
    async fn login_validates_state_and_commits_hashed_credentials() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let service = service(&repository, provider.clone());

        let start = service
            .begin_github_login()
            .await
            .expect("login should start");
        assert!(start.authorization_url.ends_with(&start.state));
        let completed = service
            .complete_github_login("code", &start.state, &start.state)
            .await
            .expect("callback should complete");

        assert_eq!(
            completed.expires_at,
            OffsetDateTime::UNIX_EPOCH + Duration::days(30)
        );
        assert_eq!(completed.session_credential, encode_credential([2; 32]));
        assert_eq!(completed.csrf_credential, encode_credential([3; 32]));
        let state = repository.0.lock().expect("state should not be poisoned");
        let session = state.session.as_ref().expect("session should be created");
        assert_eq!(session.secret_hash, hash_credential(&[2; 32]));
        assert_eq!(session.csrf_hash, hash_credential(&[3; 32]));
        assert_ne!(session.secret_hash, session.csrf_hash);
        assert!(state.committed);
        assert_eq!(state.audits.len(), 1);
        assert_eq!(state.audits[0].action(), "session_created");
        let projection = format!("{:?}", state.audits[0]);
        assert!(!projection.contains(&completed.session_credential));
        assert!(!projection.contains(&completed.csrf_credential));
        assert_eq!(*provider.exchanges.lock().expect("counter should work"), 1);
    }

    #[tokio::test]
    async fn invalid_state_stops_before_provider_or_persistence() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let service = service(&repository, provider.clone());

        let Err(error) = service
            .complete_github_login(
                "code",
                &encode_credential([8; 32]),
                &encode_credential([9; 32]),
            )
            .await
        else {
            panic!("mismatched state should fail");
        };

        assert_eq!(error.kind(), AuthenticationErrorKind::InvalidState);
        assert_eq!(*provider.exchanges.lock().expect("counter should work"), 0);
        assert!(
            repository
                .0
                .lock()
                .expect("state should not be poisoned")
                .session
                .is_none()
        );
        assert!(
            repository
                .0
                .lock()
                .expect("state should not be poisoned")
                .audits
                .is_empty()
        );
    }

    #[tokio::test]
    async fn logout_revokes_a_known_session_and_accepts_malformed_credentials() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let service = service(&repository, provider);
        let start = service
            .begin_github_login()
            .await
            .expect("login should start");
        let completed = service
            .complete_github_login("code", &start.state, &start.state)
            .await
            .expect("callback should complete");

        service
            .logout(
                &completed.session_credential,
                Some(&completed.csrf_credential),
                Some(&completed.csrf_credential),
            )
            .await
            .expect("logout should succeed");
        assert!(
            repository
                .0
                .lock()
                .expect("state should not be poisoned")
                .revoked
        );
        assert_eq!(
            repository
                .0
                .lock()
                .expect("state should not be poisoned")
                .audits
                .iter()
                .map(AuditEvent::action)
                .collect::<Vec<_>>(),
            ["session_created", "session_revoked"]
        );
        service
            .logout("not-a-credential", None, None)
            .await
            .expect("malformed cookies are already logged out");
    }

    #[tokio::test]
    async fn session_authentication_returns_user_and_touches_only_after_the_interval() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let login_service = service(&repository, provider.clone());
        let start = login_service
            .begin_github_login()
            .await
            .expect("login starts");
        let completed = login_service
            .complete_github_login("code", &start.state, &start.state)
            .await
            .expect("login completes");

        let now = OffsetDateTime::UNIX_EPOCH + Duration::minutes(5);
        let authenticated = service_at(&repository, provider, now)
            .session(
                &completed.session_credential,
                Some(&completed.csrf_credential),
            )
            .await
            .expect("session should authenticate");

        assert_eq!(authenticated.user.github_login.as_deref(), Some("octocat"));
        assert!(authenticated.session_credential.is_none());
        assert_eq!(authenticated.csrf_credential, completed.csrf_credential);
        assert_eq!(
            repository
                .0
                .lock()
                .expect("state should not be poisoned")
                .touched_at,
            Some(now)
        );
        assert_eq!(
            repository
                .0
                .lock()
                .expect("state should not be poisoned")
                .audits
                .len(),
            1,
            "last-seen touches are not business audit events"
        );
    }

    #[tokio::test]
    async fn mutation_authentication_requires_matching_stored_csrf_credentials() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let login_service = service(&repository, provider.clone());
        let start = login_service
            .begin_github_login()
            .await
            .expect("login starts");
        let completed = login_service
            .complete_github_login("code", &start.state, &start.state)
            .await
            .expect("login completes");

        let error = login_service
            .authenticate_mutation(
                &completed.session_credential,
                Some(&completed.csrf_credential),
                Some(&encode_credential([9; 32])),
            )
            .await
            .expect_err("mismatched CSRF should fail");
        assert_eq!(error.kind(), AuthenticationErrorKind::InvalidCsrf);

        let authenticated = service_at(
            &repository,
            provider,
            OffsetDateTime::UNIX_EPOCH + Duration::minutes(5),
        )
        .authenticate_mutation(
            &completed.session_credential,
            Some(&completed.csrf_credential),
            Some(&completed.csrf_credential),
        )
        .await
        .expect("matching CSRF should authenticate");
        assert_eq!(authenticated.user.id, UserId::new(Uuid::from_u128(7)));
        assert!(authenticated.session_credential.is_none());
    }

    #[tokio::test]
    async fn session_rotation_replaces_both_credentials_without_extending_expiry() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let login_service = service(&repository, provider.clone());
        let start = login_service
            .begin_github_login()
            .await
            .expect("login starts");
        let completed = login_service
            .complete_github_login("code", &start.state, &start.state)
            .await
            .expect("login completes");

        let authenticated = service_at(
            &repository,
            provider,
            OffsetDateTime::UNIX_EPOCH + Duration::days(7),
        )
        .session(
            &completed.session_credential,
            Some(&completed.csrf_credential),
        )
        .await
        .expect("session should rotate");

        assert_eq!(
            authenticated.session_credential.as_deref(),
            Some(encode_credential([1; 32]).as_str())
        );
        assert_eq!(authenticated.csrf_credential, encode_credential([2; 32]));
        assert_eq!(authenticated.expires_at, completed.expires_at);
        assert_eq!(authenticated.remaining_lifetime_seconds, 23 * 24 * 60 * 60);
        let state = repository.0.lock().expect("state should work");
        assert!(state.revoked);
        assert_eq!(
            state
                .audits
                .iter()
                .map(AuditEvent::action)
                .collect::<Vec<_>>(),
            ["session_created", "session_rotated"]
        );
    }

    #[tokio::test]
    async fn audit_failure_prevents_login_commit() {
        let repository = FakeRepository::default();
        repository.0.lock().expect("state should work").audit_fails = true;
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let service = service(&repository, provider);
        let start = service
            .begin_github_login()
            .await
            .expect("login should start");

        let Err(error) = service
            .complete_github_login("oauth-code-secret", &start.state, &start.state)
            .await
        else {
            panic!("audit failure should fail closed");
        };

        assert_eq!(
            error.kind(),
            AuthenticationErrorKind::AuthenticationUnavailable
        );
        let state = repository.0.lock().expect("state should work");
        assert!(!state.committed);
        assert_eq!(state.rolled_back, 1);
        assert!(state.audits.is_empty());
    }

    #[tokio::test]
    async fn expired_and_revoked_sessions_are_rejected_and_csrf_is_required_for_logout() {
        let repository = FakeRepository::default();
        let provider = Arc::new(FakeProvider {
            exchanges: Mutex::new(0),
        });
        let login_service = service(&repository, provider.clone());
        let start = login_service
            .begin_github_login()
            .await
            .expect("login starts");
        let completed = login_service
            .complete_github_login("code", &start.state, &start.state)
            .await
            .expect("login completes");

        let error = login_service
            .logout(&completed.session_credential, None, None)
            .await
            .expect_err("active sessions require CSRF");
        assert_eq!(error.kind(), AuthenticationErrorKind::InvalidCsrf);
        assert!(!repository.0.lock().expect("state should work").revoked);

        let error = service_at(
            &repository,
            provider.clone(),
            OffsetDateTime::UNIX_EPOCH + Duration::days(30),
        )
        .session(
            &completed.session_credential,
            Some(&completed.csrf_credential),
        )
        .await
        .expect_err("absolute expiry should be enforced");
        assert_eq!(
            error.kind(),
            AuthenticationErrorKind::AuthenticationRequired
        );

        {
            let mut state = repository.0.lock().expect("state should work");
            state
                .session
                .as_mut()
                .expect("session should exist")
                .revoked_at = Some(OffsetDateTime::UNIX_EPOCH + Duration::days(1));
        }
        let error = service_at(
            &repository,
            provider.clone(),
            OffsetDateTime::UNIX_EPOCH + Duration::days(2),
        )
        .session(
            &completed.session_credential,
            Some(&completed.csrf_credential),
        )
        .await
        .expect_err("revoked sessions should be rejected");
        assert_eq!(
            error.kind(),
            AuthenticationErrorKind::AuthenticationRequired
        );

        {
            let mut state = repository.0.lock().expect("state should work");
            state
                .session
                .as_mut()
                .expect("session should exist")
                .revoked_at = None;
            state
                .user
                .as_mut()
                .expect("user should exist")
                .anonymized_at = Some(OffsetDateTime::UNIX_EPOCH + Duration::days(1));
        }
        let error = service_at(
            &repository,
            provider,
            OffsetDateTime::UNIX_EPOCH + Duration::days(2),
        )
        .session(
            &completed.session_credential,
            Some(&completed.csrf_credential),
        )
        .await
        .expect_err("anonymized users should be rejected");
        assert_eq!(
            error.kind(),
            AuthenticationErrorKind::AuthenticationRequired
        );
    }
}
