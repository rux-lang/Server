use std::sync::{Arc, Mutex};
use uuid::Uuid;

use async_trait::async_trait;
use rux_application::{
    AccountUnitOfWork, ApiTokenError, ApiTokenErrorKind, ApiTokenService, ApiTokens,
    AuthorizedApiToken, Clock, CredentialGenerationError, CredentialGenerator, GitHubUserProfile,
    IssueApiToken, NamespaceActor, NamespaceErrorKind, NamespaceRole, NamespaceService, Namespaces,
    RegistryTransaction, TokenAuthorizer, TokenReader, TokenScope, UserRecord,
};
use rux_infrastructure::PostgresRepository;
use serde_json::Value;
use sqlx::{PgPool, query, query_as, query_scalar};
use time::{Duration, OffsetDateTime};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

struct MutableClock(Mutex<OffsetDateTime>);

impl MutableClock {
    fn set(&self, now: OffsetDateTime) {
        *self.0.lock().expect("clock should not be poisoned") = now;
    }
}

impl Clock for MutableClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().expect("clock should not be poisoned")
    }
}

struct NoTokens;

struct FixedCredentials;

impl CredentialGenerator for FixedCredentials {
    fn generate(&self) -> Result<[u8; 32], CredentialGenerationError> {
        Ok([7; 32])
    }
}

#[async_trait]
impl TokenAuthorizer for NoTokens {
    async fn authorize_publish(
        &self,
        _transaction: &mut dyn RegistryTransaction,
        _credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        Err(ApiTokenError::new(
            ApiTokenErrorKind::AuthenticationRequired,
        ))
    }

    async fn authorize_namespace(
        &self,
        _transaction: &mut dyn RegistryTransaction,
        _credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        Err(ApiTokenError::new(
            ApiTokenErrorKind::AuthenticationRequired,
        ))
    }

    async fn authorize_yank(
        &self,
        _transaction: &mut dyn RegistryTransaction,
        _credential: &str,
    ) -> Result<AuthorizedApiToken, ApiTokenError> {
        Err(ApiTokenError::new(
            ApiTokenErrorKind::AuthenticationRequired,
        ))
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn namespace_claim_invitation_and_membership_workflow(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let owner = create_user(&repository, 1001, "Owner-One").await?;
    let invitee = create_user(&repository, 1002, "Invitee-One").await?;
    let now = OffsetDateTime::now_utc() + Duration::days(1);
    let service = service(repository, now);

    let claimed = service
        .claim(NamespaceActor::Session(owner.id), "Rux_Tools")
        .await?;
    assert_eq!(claimed.name.as_str(), "Rux_Tools");
    assert_eq!(claimed.role, NamespaceRole::Owner);

    let invitation = service
        .invite(
            NamespaceActor::Session(owner.id),
            "rux-tools",
            "invitee-one",
            NamespaceRole::Maintainer,
        )
        .await?;
    assert_eq!(invitation.invited_user.github_login, "Invitee-One");
    assert_eq!(
        invitation.expires_at.unix_timestamp(),
        (now + Duration::days(7)).unix_timestamp()
    );
    assert_eq!(
        service
            .invite(
                NamespaceActor::Session(owner.id),
                "RUX-TOOLS",
                "Invitee-One",
                NamespaceRole::Owner,
            )
            .await
            .expect_err("a pending invitation must remain unique")
            .kind(),
        NamespaceErrorKind::PendingInvitation
    );

    let pending = service
        .my_invitations(NamespaceActor::Session(invitee.id))
        .await?;
    assert_eq!(pending.len(), 1);
    let membership = service
        .accept_invitation(NamespaceActor::Session(invitee.id), "rux_tools")
        .await?;
    assert_eq!(membership.role, NamespaceRole::Maintainer);
    assert!(
        service
            .my_invitations(NamespaceActor::Session(invitee.id))
            .await?
            .is_empty()
    );

    let promoted = service
        .set_member_role(
            NamespaceActor::Session(owner.id),
            "Rux-Tools",
            "INVITEE-ONE",
            NamespaceRole::Owner,
        )
        .await?;
    assert_eq!(promoted.role, NamespaceRole::Owner);
    service
        .remove_member(NamespaceActor::Session(owner.id), "Rux-Tools", "Owner-One")
        .await?;
    assert_eq!(
        service
            .remove_member(
                NamespaceActor::Session(invitee.id),
                "Rux-Tools",
                "Invitee-One",
            )
            .await
            .expect_err("the last owner cannot leave")
            .kind(),
        NamespaceErrorKind::LastOwner
    );

    let audits = query_as::<_, (String, Option<Uuid>, Option<Uuid>, String, Value)>(
        "SELECT action, actor_user_id, actor_token_id, subject_key, metadata
         FROM audit_records
         ORDER BY id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        audits
            .iter()
            .map(|(action, ..)| action.as_str())
            .collect::<Vec<_>>(),
        [
            "namespace_created",
            "namespace_invitation_created",
            "namespace_invitation_accepted",
            "namespace_member_role_changed",
            "namespace_member_removed",
        ]
    );
    assert!(audits.iter().all(|(_, user_id, token_id, subject, _)| {
        user_id.is_some() && token_id.is_none() && subject == "rux-tools"
    }));
    assert_eq!(
        audits[1].4["target_user_id"],
        Value::from(invitee.id.get().to_string())
    );
    assert_eq!(audits[2].4["role"], Value::String("maintainer".into()));
    assert_eq!(
        audits[3].4["previous_role"],
        Value::String("maintainer".into())
    );
    assert_eq!(audits[3].4["role"], Value::String("owner".into()));
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn normalized_claims_remain_unique_under_concurrency(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let first = create_user(&repository, 2001, "First-Owner").await?;
    let second = create_user(&repository, 2002, "Second-Owner").await?;
    let service = Arc::new(service(
        repository,
        OffsetDateTime::now_utc() + Duration::days(1),
    ));

    let first_claim = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .claim(NamespaceActor::Session(first.id), "Concurrent_Name")
                .await
        })
    };
    let second_claim = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .claim(NamespaceActor::Session(second.id), "concurrent-name")
                .await
        })
    };
    let results = [first_claim.await?, second_claim.await?];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one claim must conflict")
            .kind(),
        NamespaceErrorKind::NamespaceConflict
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn invitation_acceptance_and_revocation_are_serialized(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let owner = create_user(&repository, 3001, "Race-Owner").await?;
    let invitee = create_user(&repository, 3002, "Race-Invitee").await?;
    let service = Arc::new(service(
        repository,
        OffsetDateTime::now_utc() + Duration::days(1),
    ));
    service
        .claim(NamespaceActor::Session(owner.id), "Race_Space")
        .await?;
    service
        .invite(
            NamespaceActor::Session(owner.id),
            "race-space",
            "Race-Invitee",
            NamespaceRole::Maintainer,
        )
        .await?;

    let accept = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .accept_invitation(NamespaceActor::Session(invitee.id), "race-space")
                .await
        })
    };
    let revoke = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .revoke_invitation(
                    NamespaceActor::Session(owner.id),
                    "race-space",
                    "race-invitee",
                )
                .await
        })
    };
    let accepted = accept.await?;
    revoke.await??;
    let members = service
        .members(NamespaceActor::Session(owner.id), "race-space")
        .await?;
    assert_eq!(
        members
            .iter()
            .any(|member| member.user.github_login == "Race-Invitee"),
        accepted.is_ok()
    );
    if let Err(error) = accepted {
        assert_eq!(error.kind(), NamespaceErrorKind::InvitationNotFound);
    }
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_owner_removals_keep_one_owner_without_deadlock(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let first = create_user(&repository, 3501, "Owner-Alpha").await?;
    let second = create_user(&repository, 3502, "Owner-Beta").await?;
    let service = Arc::new(service(
        repository,
        OffsetDateTime::now_utc() + Duration::days(1),
    ));
    service
        .claim(NamespaceActor::Session(first.id), "Owner_Race")
        .await?;
    service
        .invite(
            NamespaceActor::Session(first.id),
            "owner-race",
            "Owner-Beta",
            NamespaceRole::Owner,
        )
        .await?;
    service
        .accept_invitation(NamespaceActor::Session(second.id), "owner-race")
        .await?;

    let first_removal = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .remove_member(
                    NamespaceActor::Session(first.id),
                    "owner-race",
                    "Owner-Beta",
                )
                .await
        })
    };
    let second_removal = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .remove_member(
                    NamespaceActor::Session(second.id),
                    "owner-race",
                    "Owner-Alpha",
                )
                .await
        })
    };
    let results = [first_removal.await?, second_removal.await?];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one removed actor must be forbidden")
            .kind(),
        NamespaceErrorKind::Forbidden
    );
    let remaining = if results[0].is_ok() {
        first.id
    } else {
        second.id
    };
    let members = service
        .members(NamespaceActor::Session(remaining), "owner-race")
        .await?;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].role, NamespaceRole::Owner);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn bearer_namespace_scope_authorizes_and_touches_the_token_atomically(
    pool: PgPool,
) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let user = create_user(&repository, 4001, "Token-Owner").await?;
    let now = OffsetDateTime::now_utc() + Duration::days(1);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let token_service = Arc::new(ApiTokenService::new(
        Arc::new(repository.clone()),
        clock.clone(),
        Arc::new(FixedCredentials),
    ));
    let issued = token_service
        .issue(
            user.id,
            IssueApiToken {
                display_name: "Namespace automation".into(),
                scopes: vec![TokenScope::Namespace],
                expires_at: None,
            },
        )
        .await?;
    let service = NamespaceService::new(Arc::new(repository.clone()), clock, token_service.clone());

    service
        .claim(
            NamespaceActor::Bearer(issued.credential.clone()),
            "Token_Space",
        )
        .await?;
    let stored = repository
        .tokens_by_user_id(user.id)
        .await?
        .pop()
        .expect("issued token should remain stored");
    assert_eq!(
        stored.last_used_at.map(OffsetDateTime::unix_timestamp),
        Some(now.unix_timestamp())
    );
    let claim_audit = query_as::<_, (Uuid, Option<Uuid>, String, Value)>(
        "SELECT actor_user_id, actor_token_id, subject_key, metadata
         FROM audit_records
         WHERE action = 'namespace_created'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(claim_audit.0, user.id.get());
    assert_eq!(claim_audit.1, Some(stored.id.get()));
    assert_eq!(claim_audit.2, "token-space");
    assert_eq!(claim_audit.3["display_name"], "Token_Space");
    let persisted_audit_text = query_scalar::<_, String>(
        "SELECT string_agg(subject_key || ' ' || metadata::text, ' ' ORDER BY id)
         FROM audit_records",
    )
    .fetch_one(&pool)
    .await?;
    assert!(!persisted_audit_text.contains(&issued.credential));
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_failure_rolls_back_the_namespace_mutation(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let owner = create_user(&repository, 4501, "Audit-Owner").await?;
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
    .execute(&pool)
    .await?;
    query(
        "CREATE TRIGGER reject_audit_write
         BEFORE INSERT ON audit_records
         FOR EACH ROW EXECUTE FUNCTION reject_audit_write()",
    )
    .execute(&pool)
    .await?;
    let service = service(
        repository.clone(),
        OffsetDateTime::now_utc() + Duration::days(1),
    );

    let error = service
        .claim(NamespaceActor::Session(owner.id), "Atomic_Audit")
        .await
        .expect_err("audit failure should fail closed");

    assert_eq!(error.kind(), NamespaceErrorKind::Unavailable);
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM namespaces WHERE normalized_name = 'atomic-audit'"
        )
        .fetch_one(&pool)
        .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM audit_records")
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_invitations_cannot_be_accepted_and_can_be_replaced(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let owner = create_user(&repository, 5001, "Expiry-Owner").await?;
    let invitee = create_user(&repository, 5002, "Expiry-Invitee").await?;
    let start = OffsetDateTime::now_utc() + Duration::days(1);
    let clock = Arc::new(MutableClock(Mutex::new(start)));
    let service = NamespaceService::new(
        Arc::new(repository.clone()),
        clock.clone(),
        Arc::new(NoTokens),
    );
    service
        .claim(NamespaceActor::Session(owner.id), "Expiry_Space")
        .await?;
    service
        .invite(
            NamespaceActor::Session(owner.id),
            "expiry-space",
            "Expiry-Invitee",
            NamespaceRole::Maintainer,
        )
        .await?;

    clock.set(start + Duration::days(8));
    assert_eq!(
        service
            .accept_invitation(NamespaceActor::Session(invitee.id), "expiry-space")
            .await
            .expect_err("expired invitation must not be accepted")
            .kind(),
        NamespaceErrorKind::InvitationExpired
    );
    service
        .invite(
            NamespaceActor::Session(owner.id),
            "expiry-space",
            "Expiry-Invitee",
            NamespaceRole::Owner,
        )
        .await?;
    let pending = service
        .my_invitations(NamespaceActor::Session(invitee.id))
        .await?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].role, NamespaceRole::Owner);
    assert_eq!(
        query_scalar::<_, String>("SELECT string_agg(action, ',' ORDER BY id) FROM audit_records")
            .fetch_one(&pool)
            .await?,
        "namespace_created,namespace_invitation_created,namespace_invitation_created"
    );
    Ok(())
}

fn service(repository: PostgresRepository, now: OffsetDateTime) -> NamespaceService {
    NamespaceService::new(
        Arc::new(repository),
        Arc::new(FixedClock(now)),
        Arc::new(NoTokens),
    )
}

async fn create_user(
    repository: &PostgresRepository,
    github_user_id: u64,
    github_login: &str,
) -> Result<UserRecord, Box<dyn std::error::Error>> {
    let mut transaction = repository.begin_account().await?;
    let user = transaction
        .upsert_github_user(&GitHubUserProfile {
            github_user_id,
            github_login: github_login.into(),
            display_name: Some(github_login.into()),
            avatar_url: None,
        })
        .await?;
    transaction.commit().await?;
    Ok(user)
}
