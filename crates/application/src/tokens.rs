use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    ApiTokenId, ApiTokenRecord, AuditEvent, Clock, CredentialGenerator, NewApiToken,
    RegistryTransaction, RepositoryConflict, RepositoryError, RepositoryErrorKind, SecretHash,
    TokenAuthorizationTransaction, TokenRepository, TokenScope, UserId, WriteOutcome,
};

const TOKEN_PREFIX: &str = "rux_pat_";
const DISPLAY_SECRET_LENGTH: usize = 8;
const MAX_ISSUE_ATTEMPTS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiTokenStatus {
    Active,
    Expired,
    Revoked,
}

impl ApiTokenStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiTokenSummary {
    pub display_name: String,
    pub token_prefix: String,
    pub scopes: Vec<TokenScope>,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
    pub status: ApiTokenStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueApiToken {
    pub display_name: String,
    pub scopes: Vec<TokenScope>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedApiToken {
    pub credential: String,
    pub token: ApiTokenSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizedApiToken {
    pub id: ApiTokenId,
    pub user_id: UserId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiTokenErrorKind {
    InvalidDisplayName,
    InvalidScopes,
    InvalidExpiration,
    AuthenticationRequired,
    InsufficientScope,
    Unavailable,
}

#[derive(Debug)]
pub struct ApiTokenError {
    kind: ApiTokenErrorKind,
}

impl ApiTokenError {
    #[must_use]
    pub const fn new(kind: ApiTokenErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> ApiTokenErrorKind {
        self.kind
    }
}

impl fmt::Display for ApiTokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "API token operation failed: {:?}", self.kind)
    }
}

impl Error for ApiTokenError {}

#[async_trait]
pub trait ApiTokens: Send + Sync {
    async fn issue(
        &self,
        user_id: UserId,
        request: IssueApiToken,
    ) -> Result<IssuedApiToken, ApiTokenError>;

    async fn list(&self, user_id: UserId) -> Result<Vec<ApiTokenSummary>, ApiTokenError>;

    async fn revoke(&self, user_id: UserId, prefix: &str) -> Result<(), ApiTokenError>;
}

#[async_trait]
pub trait TokenAuthorizer: Send + Sync {
    async fn authorize_publish(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError>;

    async fn authorize_namespace(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError>;

    async fn authorize_yank(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError>;
}

pub struct ApiTokenService {
    repository: Arc<dyn TokenRepository>,
    clock: Arc<dyn Clock>,
    credentials: Arc<dyn CredentialGenerator>,
}

impl ApiTokenService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn TokenRepository>,
        clock: Arc<dyn Clock>,
        credentials: Arc<dyn CredentialGenerator>,
    ) -> Self {
        Self {
            repository,
            clock,
            credentials,
        }
    }

    /// Authorizes one credential for a required scope inside the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns an authentication, authorization, or persistence error without committing or
    /// rolling back the caller-owned transaction.
    pub async fn authorize<T: TokenAuthorizationTransaction + ?Sized>(
        &self,
        transaction: &mut T,
        credential: &str,
        required_scope: TokenScope,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        let secret = decode_token(credential).ok_or_else(authentication_required)?;
        let token = transaction
            .lock_token_by_secret_hash(hash_secret(&secret))
            .await
            .map_err(|_| unavailable())?
            .filter(|token| token_is_active(token, self.clock.now()))
            .ok_or_else(authentication_required)?;
        if !token.scopes.contains(&required_scope) {
            return Err(ApiTokenError::new(ApiTokenErrorKind::InsufficientScope));
        }
        let user = transaction
            .lock_user_by_id(token.user_id)
            .await
            .map_err(|_| unavailable())?
            .filter(|user| user.anonymized_at.is_none() && user.github_login.is_some())
            .ok_or_else(authentication_required)?;
        transaction
            .touch_token(token.id, self.clock.now())
            .await
            .map_err(|_| unavailable())?;
        Ok(AuthorizedApiToken {
            id: token.id,
            user_id: user.id,
        })
    }
}

#[async_trait]
impl TokenAuthorizer for ApiTokenService {
    async fn authorize_publish(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        self.authorize(transaction, credential, TokenScope::Publish)
            .await
    }

    async fn authorize_namespace(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        self.authorize(transaction, credential, TokenScope::Namespace)
            .await
    }

    async fn authorize_yank(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        self.authorize(transaction, credential, TokenScope::Yank)
            .await
    }
}

#[async_trait]
impl ApiTokens for ApiTokenService {
    async fn issue(
        &self,
        user_id: UserId,
        mut request: IssueApiToken,
    ) -> Result<IssuedApiToken, ApiTokenError> {
        request.display_name = request.display_name.trim().to_owned();
        if request.display_name.is_empty() || request.display_name.len() > 100 {
            return Err(ApiTokenError::new(ApiTokenErrorKind::InvalidDisplayName));
        }
        request.scopes.sort_unstable();
        if request.scopes.is_empty()
            || request.scopes.len() > 3
            || request.scopes.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(ApiTokenError::new(ApiTokenErrorKind::InvalidScopes));
        }
        if request
            .expires_at
            .is_some_and(|expires_at| expires_at <= self.clock.now())
        {
            return Err(ApiTokenError::new(ApiTokenErrorKind::InvalidExpiration));
        }

        for _ in 0..MAX_ISSUE_ATTEMPTS {
            let secret = self.credentials.generate().map_err(|_| unavailable())?;
            let encoded = URL_SAFE_NO_PAD.encode(secret);
            let credential = format!("{TOKEN_PREFIX}{encoded}");
            let token_prefix = format!("{TOKEN_PREFIX}{}", &encoded[..DISPLAY_SECRET_LENGTH]);
            let new_token = NewApiToken {
                user_id,
                display_name: request.display_name.clone(),
                token_prefix,
                secret_hash: hash_secret(&secret),
                scopes: request.scopes.clone(),
                expires_at: request.expires_at,
            };
            let mut transaction = self
                .repository
                .begin_tokens()
                .await
                .map_err(|_| unavailable())?;
            match transaction.create_token(&new_token).await {
                Ok(record) => {
                    let event = AuditEvent::api_token_created(
                        user_id,
                        record.token_prefix.clone(),
                        record.scopes.clone(),
                        record.expires_at,
                    );
                    if transaction.append_audit(&event).await.is_err() {
                        let _ = transaction.rollback().await;
                        return Err(unavailable());
                    }
                    transaction.commit().await.map_err(|_| unavailable())?;
                    return Ok(IssuedApiToken {
                        credential,
                        token: summary(record, self.clock.now()),
                    });
                }
                Err(error) if is_credential_collision(&error) => {
                    transaction.rollback().await.map_err(|_| unavailable())?;
                }
                Err(_) => {
                    let _ = transaction.rollback().await;
                    return Err(unavailable());
                }
            }
        }
        Err(unavailable())
    }

    async fn list(&self, user_id: UserId) -> Result<Vec<ApiTokenSummary>, ApiTokenError> {
        let now = self.clock.now();
        self.repository
            .tokens_by_user_id(user_id)
            .await
            .map_err(|_| unavailable())
            .map(|tokens| {
                tokens
                    .into_iter()
                    .map(|token| summary(token, now))
                    .collect()
            })
    }

    async fn revoke(&self, user_id: UserId, prefix: &str) -> Result<(), ApiTokenError> {
        if !valid_safe_prefix(prefix) {
            return Ok(());
        }
        let mut transaction = self
            .repository
            .begin_tokens()
            .await
            .map_err(|_| unavailable())?;
        let Ok(token) = transaction.lock_token_by_prefix(user_id, prefix).await else {
            let _ = transaction.rollback().await;
            return Err(unavailable());
        };
        if let Some(token) = token {
            let Ok(outcome) = transaction.revoke_token(token.id, self.clock.now()).await else {
                let _ = transaction.rollback().await;
                return Err(unavailable());
            };
            if outcome == WriteOutcome::Applied
                && transaction
                    .append_audit(&AuditEvent::api_token_revoked(user_id, token.token_prefix))
                    .await
                    .is_err()
            {
                let _ = transaction.rollback().await;
                return Err(unavailable());
            }
        }
        transaction.commit().await.map_err(|_| unavailable())
    }
}

fn summary(token: ApiTokenRecord, now: OffsetDateTime) -> ApiTokenSummary {
    let status = if token.revoked_at.is_some() {
        ApiTokenStatus::Revoked
    } else if token.expires_at.is_some_and(|expires_at| expires_at <= now) {
        ApiTokenStatus::Expired
    } else {
        ApiTokenStatus::Active
    };
    ApiTokenSummary {
        display_name: token.display_name,
        token_prefix: token.token_prefix,
        scopes: token.scopes,
        created_at: token.created_at,
        last_used_at: token.last_used_at,
        expires_at: token.expires_at,
        revoked_at: token.revoked_at,
        status,
    }
}

fn token_is_active(token: &ApiTokenRecord, now: OffsetDateTime) -> bool {
    token.revoked_at.is_none() && token.expires_at.is_none_or(|expires_at| expires_at > now)
}

fn decode_token(value: &str) -> Option<[u8; 32]> {
    let encoded = value.strip_prefix(TOKEN_PREFIX)?;
    if encoded.len() != 43 {
        return None;
    }
    URL_SAFE_NO_PAD.decode(encoded).ok()?.try_into().ok()
}

fn valid_safe_prefix(value: &str) -> bool {
    value.len() == TOKEN_PREFIX.len() + DISPLAY_SECRET_LENGTH
        && value.starts_with(TOKEN_PREFIX)
        && value[TOKEN_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn hash_secret(secret: &[u8; 32]) -> SecretHash {
    SecretHash::new(Sha256::digest(secret).into())
}

fn is_credential_collision(error: &RepositoryError) -> bool {
    matches!(
        error.kind(),
        RepositoryErrorKind::Conflict(
            RepositoryConflict::TokenPrefix | RepositoryConflict::TokenSecret
        )
    )
}

fn authentication_required() -> ApiTokenError {
    ApiTokenError::new(ApiTokenErrorKind::AuthenticationRequired)
}

fn unavailable() -> ApiTokenError {
    ApiTokenError::new(ApiTokenErrorKind::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use uuid::Uuid;

    use time::Duration;

    use crate::{
        ApiTokenRecord, CredentialGenerationError, RepositoryError, TokenReader, TokenTransaction,
        TokenUnitOfWork, TokenWriter, UserRecord,
    };

    use super::*;

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            NOW
        }
    }

    struct FixedCredentials(Mutex<VecDeque<[u8; 32]>>);

    impl CredentialGenerator for FixedCredentials {
        fn generate(&self) -> Result<[u8; 32], CredentialGenerationError> {
            self.0
                .lock()
                .expect("credential queue should work")
                .pop_front()
                .ok_or(CredentialGenerationError)
        }
    }

    #[derive(Default)]
    struct FakeState {
        tokens: Vec<ApiTokenRecord>,
        user: Option<UserRecord>,
        audits: Vec<AuditEvent>,
        audit_fails: bool,
        credential_collisions: usize,
        committed: usize,
        rolled_back: usize,
    }

    #[derive(Clone, Default)]
    struct FakeRepository(Arc<Mutex<FakeState>>);

    #[async_trait]
    impl TokenReader for FakeRepository {
        async fn token_by_secret_hash(
            &self,
            hash: SecretHash,
        ) -> Result<Option<ApiTokenRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should work")
                .tokens
                .iter()
                .find(|token| token.secret_hash == hash)
                .cloned())
        }

        async fn tokens_by_user_id(
            &self,
            user_id: UserId,
        ) -> Result<Vec<ApiTokenRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should work")
                .tokens
                .iter()
                .filter(|token| token.user_id == user_id)
                .cloned()
                .collect())
        }
    }

    #[async_trait]
    impl TokenUnitOfWork for FakeRepository {
        async fn begin_tokens(&self) -> Result<Box<dyn TokenTransaction>, RepositoryError> {
            Ok(Box::new(FakeTransaction(self.0.clone())))
        }
    }

    struct FakeTransaction(Arc<Mutex<FakeState>>);

    #[async_trait]
    impl crate::AuditWriter for FakeTransaction {
        async fn append_audit(&mut self, event: &AuditEvent) -> Result<(), RepositoryError> {
            let mut state = self.0.lock().expect("state should work");
            if state.audit_fails {
                return Err(RepositoryError::new(RepositoryErrorKind::Unexpected));
            }
            state.audits.push(event.clone());
            Ok(())
        }
    }

    #[async_trait]
    impl TokenWriter for FakeTransaction {
        async fn create_token(
            &mut self,
            token: &NewApiToken,
        ) -> Result<ApiTokenRecord, RepositoryError> {
            let mut state = self.0.lock().expect("state should work");
            if state.credential_collisions > 0 {
                state.credential_collisions -= 1;
                return Err(RepositoryError::new(RepositoryErrorKind::Conflict(
                    RepositoryConflict::TokenPrefix,
                )));
            }
            let record = ApiTokenRecord {
                id: ApiTokenId::new(Uuid::from_u128(state.tokens.len() as u128 + 1)),
                user_id: token.user_id,
                display_name: token.display_name.clone(),
                token_prefix: token.token_prefix.clone(),
                secret_hash: token.secret_hash,
                scopes: token.scopes.clone(),
                created_at: NOW,
                last_used_at: None,
                expires_at: token.expires_at,
                revoked_at: None,
            };
            state.tokens.push(record.clone());
            Ok(record)
        }

        async fn touch_token(
            &mut self,
            id: ApiTokenId,
            at: OffsetDateTime,
        ) -> Result<WriteOutcome, RepositoryError> {
            let mut state = self.0.lock().expect("state should work");
            let Some(token) = state.tokens.iter_mut().find(|token| token.id == id) else {
                return Ok(WriteOutcome::NotFound);
            };
            token.last_used_at = Some(at);
            Ok(WriteOutcome::Applied)
        }

        async fn revoke_token(
            &mut self,
            id: ApiTokenId,
            at: OffsetDateTime,
        ) -> Result<WriteOutcome, RepositoryError> {
            let mut state = self.0.lock().expect("state should work");
            let Some(token) = state.tokens.iter_mut().find(|token| token.id == id) else {
                return Ok(WriteOutcome::NotFound);
            };
            if token.revoked_at.is_some() {
                return Ok(WriteOutcome::Unchanged);
            }
            token.revoked_at = Some(at);
            Ok(WriteOutcome::Applied)
        }
    }

    #[async_trait]
    impl TokenAuthorizationTransaction for FakeTransaction {
        async fn lock_user_by_id(
            &mut self,
            id: UserId,
        ) -> Result<Option<UserRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should work")
                .user
                .clone()
                .filter(|user| user.id == id))
        }

        async fn lock_token_by_secret_hash(
            &mut self,
            hash: SecretHash,
        ) -> Result<Option<ApiTokenRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should work")
                .tokens
                .iter()
                .find(|token| token.secret_hash == hash)
                .cloned())
        }

        async fn lock_token_by_prefix(
            &mut self,
            user_id: UserId,
            prefix: &str,
        ) -> Result<Option<ApiTokenRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should work")
                .tokens
                .iter()
                .find(|token| token.user_id == user_id && token.token_prefix == prefix)
                .cloned())
        }
    }

    #[async_trait]
    impl TokenTransaction for FakeTransaction {
        async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
            self.0.lock().expect("state should work").committed += 1;
            Ok(())
        }

        async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
            self.0.lock().expect("state should work").rolled_back += 1;
            Ok(())
        }
    }

    fn active_user(id: UserId) -> UserRecord {
        UserRecord {
            id,
            github_user_id: Some(42),
            github_login: Some("octocat".into()),
            display_name: None,
            avatar_url: None,
            created_at: NOW,
            updated_at: NOW,
            anonymized_at: None,
        }
    }

    fn service(repository: &FakeRepository, credentials: VecDeque<[u8; 32]>) -> ApiTokenService {
        ApiTokenService::new(
            Arc::new(repository.clone()),
            Arc::new(FixedClock),
            Arc::new(FixedCredentials(Mutex::new(credentials))),
        )
    }

    #[tokio::test]
    async fn issue_returns_the_credential_once_and_persists_only_its_hash_and_prefix() {
        let repository = FakeRepository::default();
        let service = service(&repository, VecDeque::from([[7; 32]]));
        let issued = service
            .issue(
                UserId::new(Uuid::from_u128(7)),
                IssueApiToken {
                    display_name: "  CI release  ".into(),
                    scopes: vec![TokenScope::Namespace, TokenScope::Publish],
                    expires_at: None,
                },
            )
            .await
            .expect("token should issue");

        assert!(issued.credential.starts_with(TOKEN_PREFIX));
        assert_eq!(issued.token.display_name, "CI release");
        assert_eq!(
            issued.token.scopes,
            [TokenScope::Publish, TokenScope::Namespace]
        );
        let state = repository.0.lock().expect("state should work");
        assert_eq!(state.committed, 1);
        assert_eq!(state.tokens[0].secret_hash, hash_secret(&[7; 32]));
        assert_eq!(state.tokens[0].token_prefix, issued.token.token_prefix);
        assert!(!issued.token.token_prefix.contains(&issued.credential));
        assert_eq!(state.audits.len(), 1);
        assert_eq!(state.audits[0].action(), "api_token_created");
        assert!(!format!("{:?}", state.audits[0]).contains(&issued.credential));
    }

    #[tokio::test]
    async fn validation_and_history_statuses_are_deterministic() {
        let repository = FakeRepository::default();
        let service = service(&repository, VecDeque::new());
        let error = service
            .issue(
                UserId::new(Uuid::from_u128(7)),
                IssueApiToken {
                    display_name: "name".into(),
                    scopes: vec![TokenScope::Publish, TokenScope::Publish],
                    expires_at: None,
                },
            )
            .await
            .expect_err("duplicate scopes should fail");
        assert_eq!(error.kind(), ApiTokenErrorKind::InvalidScopes);

        repository.0.lock().expect("state should work").tokens = vec![
            ApiTokenRecord {
                id: ApiTokenId::new(Uuid::from_u128(1)),
                user_id: UserId::new(Uuid::from_u128(7)),
                display_name: "expired".into(),
                token_prefix: "rux_pat_expired1".into(),
                secret_hash: SecretHash::new([1; 32]),
                scopes: vec![TokenScope::Publish],
                created_at: NOW - Duration::days(2),
                last_used_at: None,
                expires_at: Some(NOW),
                revoked_at: None,
            },
            ApiTokenRecord {
                id: ApiTokenId::new(Uuid::from_u128(2)),
                user_id: UserId::new(Uuid::from_u128(7)),
                display_name: "revoked".into(),
                token_prefix: "rux_pat_revoked1".into(),
                secret_hash: SecretHash::new([2; 32]),
                scopes: vec![TokenScope::Yank],
                created_at: NOW - Duration::days(1),
                last_used_at: None,
                expires_at: None,
                revoked_at: Some(NOW),
            },
        ];
        let history = service
            .list(UserId::new(Uuid::from_u128(7)))
            .await
            .expect("history should list");
        assert_eq!(history[0].status, ApiTokenStatus::Expired);
        assert_eq!(history[1].status, ApiTokenStatus::Revoked);
    }

    #[tokio::test]
    async fn issue_retries_a_safe_prefix_collision_with_fresh_randomness() {
        let repository = FakeRepository::default();
        repository
            .0
            .lock()
            .expect("state should work")
            .credential_collisions = 1;
        let service = service(&repository, VecDeque::from([[1; 32], [2; 32]]));

        let issued = service
            .issue(
                UserId::new(Uuid::from_u128(7)),
                IssueApiToken {
                    display_name: "CI".into(),
                    scopes: vec![TokenScope::Publish],
                    expires_at: None,
                },
            )
            .await
            .expect("second credential should issue");

        assert_eq!(decode_token(&issued.credential), Some([2; 32]));
        let state = repository.0.lock().expect("state should work");
        assert_eq!(state.rolled_back, 1);
        assert_eq!(state.committed, 1);
    }

    #[tokio::test]
    async fn authorization_enforces_scope_and_revoke_is_owner_scoped_and_idempotent() {
        let repository = FakeRepository::default();
        repository.0.lock().expect("state should work").user =
            Some(active_user(UserId::new(Uuid::from_u128(7))));
        let service = service(&repository, VecDeque::from([[9; 32]]));
        let issued = service
            .issue(
                UserId::new(Uuid::from_u128(7)),
                IssueApiToken {
                    display_name: "publisher".into(),
                    scopes: vec![TokenScope::Publish],
                    expires_at: Some(NOW + Duration::days(1)),
                },
            )
            .await
            .expect("token should issue");

        let mut transaction = repository.begin_tokens().await.expect("transaction");
        let denied = service
            .authorize(&mut *transaction, &issued.credential, TokenScope::Yank)
            .await
            .expect_err("wrong scope should fail");
        assert_eq!(denied.kind(), ApiTokenErrorKind::InsufficientScope);
        service
            .authorize(&mut *transaction, &issued.credential, TokenScope::Publish)
            .await
            .expect("publish scope should authorize");
        transaction.commit().await.expect("commit");

        service
            .revoke(UserId::new(Uuid::from_u128(8)), &issued.token.token_prefix)
            .await
            .expect("foreign revoke is hidden");
        assert!(
            repository.0.lock().expect("state should work").tokens[0]
                .revoked_at
                .is_none()
        );
        service
            .revoke(UserId::new(Uuid::from_u128(7)), &issued.token.token_prefix)
            .await
            .expect("owner can revoke");
        service
            .revoke(UserId::new(Uuid::from_u128(7)), &issued.token.token_prefix)
            .await
            .expect("revoke is idempotent");
        assert!(
            repository.0.lock().expect("state should work").tokens[0]
                .revoked_at
                .is_some()
        );
        assert_eq!(
            repository
                .0
                .lock()
                .expect("state should work")
                .audits
                .iter()
                .map(AuditEvent::action)
                .collect::<Vec<_>>(),
            ["api_token_created", "api_token_revoked"]
        );
    }

    #[tokio::test]
    async fn audit_failure_rolls_back_token_issue_without_returning_a_credential() {
        let repository = FakeRepository::default();
        repository.0.lock().expect("state should work").audit_fails = true;
        let service = service(&repository, VecDeque::from([[7; 32]]));

        let error = service
            .issue(
                UserId::new(Uuid::from_u128(7)),
                IssueApiToken {
                    display_name: "CI release".into(),
                    scopes: vec![TokenScope::Publish],
                    expires_at: None,
                },
            )
            .await
            .expect_err("audit failure should fail closed");

        assert_eq!(error.kind(), ApiTokenErrorKind::Unavailable);
        let state = repository.0.lock().expect("state should work");
        assert_eq!(state.committed, 0);
        assert_eq!(state.rolled_back, 1);
        assert!(state.audits.is_empty());
    }

    #[tokio::test]
    async fn authorization_hides_malformed_expired_revoked_and_anonymized_credentials() {
        let repository = FakeRepository::default();
        repository.0.lock().expect("state should work").user =
            Some(active_user(UserId::new(Uuid::from_u128(7))));
        let service = service(&repository, VecDeque::from([[5; 32]]));
        let issued = service
            .issue(
                UserId::new(Uuid::from_u128(7)),
                IssueApiToken {
                    display_name: "namespace automation".into(),
                    scopes: vec![TokenScope::Namespace],
                    expires_at: None,
                },
            )
            .await
            .expect("token should issue");

        let mut transaction = repository.begin_tokens().await.expect("transaction");
        let malformed = service
            .authorize(&mut *transaction, "not-a-token", TokenScope::Namespace)
            .await
            .expect_err("malformed credentials should fail");
        assert_eq!(malformed.kind(), ApiTokenErrorKind::AuthenticationRequired);
        transaction.rollback().await.expect("rollback");

        repository.0.lock().expect("state should work").tokens[0].expires_at = Some(NOW);
        let mut transaction = repository.begin_tokens().await.expect("transaction");
        let expired = service
            .authorize(&mut *transaction, &issued.credential, TokenScope::Namespace)
            .await
            .expect_err("expired credentials should fail");
        assert_eq!(expired.kind(), ApiTokenErrorKind::AuthenticationRequired);
        transaction.rollback().await.expect("rollback");

        {
            let mut state = repository.0.lock().expect("state should work");
            state.tokens[0].expires_at = None;
            state.tokens[0].revoked_at = Some(NOW);
        }
        let mut transaction = repository.begin_tokens().await.expect("transaction");
        let revoked = service
            .authorize(&mut *transaction, &issued.credential, TokenScope::Namespace)
            .await
            .expect_err("revoked credentials should fail");
        assert_eq!(revoked.kind(), ApiTokenErrorKind::AuthenticationRequired);
        transaction.rollback().await.expect("rollback");

        {
            let mut state = repository.0.lock().expect("state should work");
            state.tokens[0].revoked_at = None;
            state
                .user
                .as_mut()
                .expect("user should exist")
                .anonymized_at = Some(NOW);
        }
        let mut transaction = repository.begin_tokens().await.expect("transaction");
        let anonymized = service
            .authorize(&mut *transaction, &issued.credential, TokenScope::Namespace)
            .await
            .expect_err("anonymized accounts should fail");
        assert_eq!(anonymized.kind(), ApiTokenErrorKind::AuthenticationRequired);
        transaction.rollback().await.expect("rollback");
    }
}
