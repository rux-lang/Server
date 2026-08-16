use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rux_application::{
    AccountUnitOfWork, ApiTokenErrorKind, ApiTokenService, ApiTokens, Authentication,
    AuthenticationErrorKind, AuthenticationService, Clock, CredentialGenerationError,
    CredentialGenerator, GitHubIdentityProvider, GitHubUserProfile, IdentityProviderError,
    IssueApiToken, TokenScope,
};
use rux_infrastructure::PostgresRepository;
use sqlx::{AssertSqlSafe, PgPool, query, query_scalar};
use time::{Duration, OffsetDateTime};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

struct SequenceCredentials(Mutex<VecDeque<[u8; 32]>>);

impl CredentialGenerator for SequenceCredentials {
    fn generate(&self) -> Result<[u8; 32], CredentialGenerationError> {
        self.0
            .lock()
            .expect("credential queue should work")
            .pop_front()
            .ok_or(CredentialGenerationError)
    }
}

struct FixedIdentityProvider;

#[async_trait]
impl GitHubIdentityProvider for FixedIdentityProvider {
    fn authorization_url(&self, state: &str) -> Result<String, IdentityProviderError> {
        Ok(format!("https://github.example/authorize?state={state}"))
    }

    async fn exchange_code(&self, _code: &str) -> Result<GitHubUserProfile, IdentityProviderError> {
        Ok(GitHubUserProfile {
            github_user_id: 7001,
            github_login: "Audit-Login".into(),
            display_name: None,
            avatar_url: None,
        })
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_failure_rolls_back_oauth_user_and_session(pool: PgPool) -> TestResult {
    reject_audits(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let service = AuthenticationService::new(
        repository,
        Arc::new(FixedIdentityProvider),
        Arc::new(FixedClock(OffsetDateTime::now_utc())),
        Arc::new(SequenceCredentials(Mutex::new(VecDeque::from([
            [1; 32], [2; 32], [3; 32],
        ])))),
    );
    let start = service.begin_github_login().await?;

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
    assert_table_count(&pool, "users", 0).await?;
    assert_table_count(&pool, "sessions", 0).await?;
    assert_table_count(&pool, "audit_records", 0).await?;
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_failure_rolls_back_token_issue(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let mut transaction = repository.begin_account().await?;
    let user = transaction
        .upsert_github_user(&GitHubUserProfile {
            github_user_id: 7002,
            github_login: "Audit-Token".into(),
            display_name: None,
            avatar_url: None,
        })
        .await?;
    transaction.commit().await?;
    reject_audits(&pool).await?;
    let service = ApiTokenService::new(
        Arc::new(repository),
        Arc::new(FixedClock(OffsetDateTime::now_utc())),
        Arc::new(SequenceCredentials(Mutex::new(VecDeque::from([[7; 32]])))),
    );

    let error = service
        .issue(
            user.id,
            IssueApiToken {
                display_name: "Release".into(),
                scopes: vec![TokenScope::Publish],
                expires_at: Some(OffsetDateTime::now_utc() + Duration::days(1)),
            },
        )
        .await
        .expect_err("audit failure should fail closed");

    assert_eq!(error.kind(), ApiTokenErrorKind::Unavailable);
    assert_table_count(&pool, "api_tokens", 0).await?;
    assert_table_count(&pool, "audit_records", 0).await?;
    Ok(())
}

async fn reject_audits(pool: &PgPool) -> TestResult {
    query(
        "CREATE FUNCTION reject_audit_write()
         RETURNS TRIGGER
         LANGUAGE plpgsql
         AS $$
         BEGIN
             RAISE EXCEPTION 'audit unavailable' USING ERRCODE = '55000';
         END;
         $$",
    )
    .execute(pool)
    .await?;
    query(
        "CREATE TRIGGER reject_audit_write
         BEFORE INSERT ON audit_records
         FOR EACH ROW EXECUTE FUNCTION reject_audit_write()",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn assert_table_count(pool: &PgPool, table: &str, expected: i64) -> TestResult {
    let count = query_scalar::<_, i64>(AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(pool)
        .await?;
    assert_eq!(count, expected);
    Ok(())
}
