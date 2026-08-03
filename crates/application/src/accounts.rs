use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::{AccountLifecycleRepository, AuditEvent, Clock, NamespaceRole, UserId, WriteOutcome};

pub const ANONYMIZED_TOKEN_NAME: &str = "Deleted account token";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountLifecycleErrorKind {
    AuthenticationRequired,
    ConfirmationMismatch,
    LastOwner,
    Unavailable,
}

#[derive(Debug)]
pub struct AccountLifecycleError {
    kind: AccountLifecycleErrorKind,
}

impl AccountLifecycleError {
    #[must_use]
    pub const fn new(kind: AccountLifecycleErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> AccountLifecycleErrorKind {
        self.kind
    }
}

impl fmt::Display for AccountLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "account lifecycle operation failed: {:?}",
            self.kind
        )
    }
}

impl Error for AccountLifecycleError {}

#[async_trait]
pub trait AccountLifecycle: Send + Sync {
    async fn delete_account(
        &self,
        user_id: UserId,
        github_login_confirmation: &str,
    ) -> Result<(), AccountLifecycleError>;
}

pub struct AccountLifecycleService {
    repository: Arc<dyn AccountLifecycleRepository>,
    clock: Arc<dyn Clock>,
}

impl AccountLifecycleService {
    #[must_use]
    pub fn new(repository: Arc<dyn AccountLifecycleRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { repository, clock }
    }

    async fn delete_in_transaction(
        &self,
        transaction: &mut dyn crate::AccountLifecycleTransaction,
        user_id: UserId,
        github_login_confirmation: &str,
    ) -> Result<(), AccountLifecycleError> {
        let user = transaction
            .lock_user_by_id(user_id)
            .await
            .map_err(|_| unavailable())?
            .filter(|user| user.anonymized_at.is_none())
            .ok_or_else(authentication_required)?;
        let github_login = user
            .github_login
            .as_deref()
            .ok_or_else(authentication_required)?;
        if github_login_confirmation != github_login {
            return Err(AccountLifecycleError::new(
                AccountLifecycleErrorKind::ConfirmationMismatch,
            ));
        }

        let memberships = transaction
            .lock_memberships_by_user_id(user_id)
            .await
            .map_err(|_| unavailable())?;
        for membership in memberships
            .iter()
            .filter(|membership| membership.membership.role == NamespaceRole::Owner)
        {
            let owners = transaction
                .namespace_owner_count(membership.namespace.id)
                .await
                .map_err(|_| unavailable())?;
            if owners <= 1 {
                return Err(AccountLifecycleError::new(
                    AccountLifecycleErrorKind::LastOwner,
                ));
            }
        }

        let now = self.clock.now();
        transaction
            .revoke_incoming_invitations_by_user_id(user_id, now)
            .await
            .map_err(|_| unavailable())?;
        transaction
            .remove_memberships_by_user_id(user_id)
            .await
            .map_err(|_| unavailable())?;
        transaction
            .revoke_sessions_by_user_id(user_id, now)
            .await
            .map_err(|_| unavailable())?;
        transaction
            .revoke_and_scrub_tokens_by_user_id(user_id, now, ANONYMIZED_TOKEN_NAME)
            .await
            .map_err(|_| unavailable())?;
        let outcome = transaction
            .anonymize_user(user_id, now)
            .await
            .map_err(|_| unavailable())?;
        if outcome != WriteOutcome::Applied {
            return Err(authentication_required());
        }
        transaction
            .append_audit(&AuditEvent::account_anonymized(user_id))
            .await
            .map_err(|_| unavailable())?;
        Ok(())
    }
}

#[async_trait]
impl AccountLifecycle for AccountLifecycleService {
    async fn delete_account(
        &self,
        user_id: UserId,
        github_login_confirmation: &str,
    ) -> Result<(), AccountLifecycleError> {
        let mut transaction = self
            .repository
            .begin_account_lifecycle()
            .await
            .map_err(|_| unavailable())?;
        let result = self
            .delete_in_transaction(&mut *transaction, user_id, github_login_confirmation)
            .await;
        if let Err(error) = result {
            let _ = transaction.rollback().await;
            return Err(error);
        }
        transaction.commit().await.map_err(|_| unavailable())
    }
}

fn authentication_required() -> AccountLifecycleError {
    AccountLifecycleError::new(AccountLifecycleErrorKind::AuthenticationRequired)
}

fn unavailable() -> AccountLifecycleError {
    AccountLifecycleError::new(AccountLifecycleErrorKind::Unavailable)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use uuid::Uuid;

    use rux_domain::IdentitySegment;
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        AccountLifecycleTransaction, AccountLifecycleUnitOfWork, AuditWriter, NamespaceId,
        NamespaceMembershipRecord, NamespaceOwnerRecord, NamespaceRecord, RepositoryError,
        RepositoryErrorKind, UserRecord,
    };

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            NOW
        }
    }

    #[derive(Clone)]
    struct FakeRepository(Arc<Mutex<State>>);

    struct State {
        user: Option<UserRecord>,
        memberships: Vec<NamespaceMembershipRecord>,
        owner_count: u64,
        cleanup_calls: Vec<&'static str>,
        token_name: Option<String>,
        audits: Vec<AuditEvent>,
        committed: bool,
        rolled_back: bool,
        fail_audit: bool,
    }

    impl Default for State {
        fn default() -> Self {
            Self {
                user: Some(active_user()),
                memberships: Vec::new(),
                owner_count: 2,
                cleanup_calls: Vec::new(),
                token_name: None,
                audits: Vec::new(),
                committed: false,
                rolled_back: false,
                fail_audit: false,
            }
        }
    }

    struct FakeTransaction(Arc<Mutex<State>>);

    #[async_trait]
    impl AccountLifecycleUnitOfWork for FakeRepository {
        async fn begin_account_lifecycle(
            &self,
        ) -> Result<Box<dyn AccountLifecycleTransaction>, RepositoryError> {
            Ok(Box::new(FakeTransaction(self.0.clone())))
        }
    }

    #[async_trait]
    impl AuditWriter for FakeTransaction {
        async fn append_audit(&mut self, event: &AuditEvent) -> Result<(), RepositoryError> {
            let mut state = self.0.lock().expect("state should work");
            if state.fail_audit {
                return Err(RepositoryError::new(RepositoryErrorKind::Unavailable));
            }
            state.audits.push(event.clone());
            Ok(())
        }
    }

    #[async_trait]
    impl AccountLifecycleTransaction for FakeTransaction {
        async fn lock_user_by_id(
            &mut self,
            _id: UserId,
        ) -> Result<Option<UserRecord>, RepositoryError> {
            Ok(self.0.lock().expect("state should work").user.clone())
        }

        async fn lock_memberships_by_user_id(
            &mut self,
            _user_id: UserId,
        ) -> Result<Vec<NamespaceMembershipRecord>, RepositoryError> {
            Ok(self
                .0
                .lock()
                .expect("state should work")
                .memberships
                .clone())
        }

        async fn namespace_owner_count(
            &mut self,
            _namespace_id: NamespaceId,
        ) -> Result<u64, RepositoryError> {
            Ok(self.0.lock().expect("state should work").owner_count)
        }

        async fn revoke_sessions_by_user_id(
            &mut self,
            _user_id: UserId,
            _at: OffsetDateTime,
        ) -> Result<u64, RepositoryError> {
            self.0
                .lock()
                .expect("state should work")
                .cleanup_calls
                .push("sessions");
            Ok(1)
        }

        async fn revoke_and_scrub_tokens_by_user_id(
            &mut self,
            _user_id: UserId,
            _at: OffsetDateTime,
            replacement_name: &str,
        ) -> Result<u64, RepositoryError> {
            let mut state = self.0.lock().expect("state should work");
            state.cleanup_calls.push("tokens");
            state.token_name = Some(replacement_name.to_owned());
            Ok(1)
        }

        async fn revoke_incoming_invitations_by_user_id(
            &mut self,
            _user_id: UserId,
            _at: OffsetDateTime,
        ) -> Result<u64, RepositoryError> {
            self.0
                .lock()
                .expect("state should work")
                .cleanup_calls
                .push("invitations");
            Ok(1)
        }

        async fn remove_memberships_by_user_id(
            &mut self,
            _user_id: UserId,
        ) -> Result<u64, RepositoryError> {
            self.0
                .lock()
                .expect("state should work")
                .cleanup_calls
                .push("memberships");
            Ok(1)
        }

        async fn anonymize_user(
            &mut self,
            _user_id: UserId,
            _at: OffsetDateTime,
        ) -> Result<WriteOutcome, RepositoryError> {
            self.0
                .lock()
                .expect("state should work")
                .cleanup_calls
                .push("user");
            Ok(WriteOutcome::Applied)
        }

        async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
            self.0.lock().expect("state should work").committed = true;
            Ok(())
        }

        async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
            self.0.lock().expect("state should work").rolled_back = true;
            Ok(())
        }
    }

    #[tokio::test]
    async fn deletion_cleans_account_data_and_records_one_audit() {
        let repository = repository(State::default());
        let service = service(&repository);

        service
            .delete_account(UserId::new(Uuid::from_u128(7)), "Octocat")
            .await
            .expect("account deletion should succeed");

        let state = repository.0.lock().expect("state should work");
        assert_eq!(
            state.cleanup_calls,
            ["invitations", "memberships", "sessions", "tokens", "user"]
        );
        assert_eq!(state.token_name.as_deref(), Some(ANONYMIZED_TOKEN_NAME));
        assert!(state.committed);
        assert_eq!(state.audits.len(), 1);
        assert_eq!(state.audits[0].action(), "account_anonymized");
    }

    #[tokio::test]
    async fn confirmation_is_case_sensitive_and_rolls_back_before_cleanup() {
        let repository = repository(State::default());
        let error = service(&repository)
            .delete_account(UserId::new(Uuid::from_u128(7)), "octocat")
            .await
            .expect_err("confirmation should preserve case");

        assert_eq!(
            error.kind(),
            AccountLifecycleErrorKind::ConfirmationMismatch
        );
        let state = repository.0.lock().expect("state should work");
        assert!(state.rolled_back);
        assert!(state.cleanup_calls.is_empty());
    }

    #[tokio::test]
    async fn sole_ownership_blocks_the_entire_operation() {
        let mut state = State::default();
        state.memberships.push(membership(NamespaceRole::Owner));
        state.owner_count = 1;
        let repository = repository(state);
        let error = service(&repository)
            .delete_account(UserId::new(Uuid::from_u128(7)), "Octocat")
            .await
            .expect_err("a final owner cannot leave");

        assert_eq!(error.kind(), AccountLifecycleErrorKind::LastOwner);
        let state = repository.0.lock().expect("state should work");
        assert!(state.rolled_back);
        assert!(state.cleanup_calls.is_empty());
    }

    #[tokio::test]
    async fn anonymized_accounts_and_audit_failures_fail_closed() {
        let mut inactive = State::default();
        inactive
            .user
            .as_mut()
            .expect("user should exist")
            .anonymized_at = Some(NOW);
        let inactive = repository(inactive);
        assert_eq!(
            service(&inactive)
                .delete_account(UserId::new(Uuid::from_u128(7)), "Octocat")
                .await
                .expect_err("anonymized account should not authenticate")
                .kind(),
            AccountLifecycleErrorKind::AuthenticationRequired
        );

        let failing = repository(State {
            fail_audit: true,
            ..State::default()
        });
        assert_eq!(
            service(&failing)
                .delete_account(UserId::new(Uuid::from_u128(7)), "Octocat")
                .await
                .expect_err("audit failure should fail closed")
                .kind(),
            AccountLifecycleErrorKind::Unavailable
        );
        assert!(failing.0.lock().expect("state should work").rolled_back);
    }

    fn repository(state: State) -> FakeRepository {
        FakeRepository(Arc::new(Mutex::new(state)))
    }

    fn service(repository: &FakeRepository) -> AccountLifecycleService {
        AccountLifecycleService::new(Arc::new(repository.clone()), Arc::new(FixedClock))
    }

    fn active_user() -> UserRecord {
        UserRecord {
            id: UserId::new(Uuid::from_u128(7)),
            github_user_id: Some(1),
            github_login: Some("Octocat".into()),
            display_name: Some("The Octocat".into()),
            avatar_url: Some("https://example.test/avatar.png".into()),
            created_at: NOW,
            updated_at: NOW,
            anonymized_at: None,
        }
    }

    fn membership(role: NamespaceRole) -> NamespaceMembershipRecord {
        NamespaceMembershipRecord {
            namespace: NamespaceRecord {
                id: NamespaceId::new(Uuid::from_u128(11)),
                name: IdentitySegment::new("Rux").expect("namespace should be valid"),
                created_by_user_id: Some(UserId::new(Uuid::from_u128(7))),
                created_at: NOW,
                updated_at: NOW,
            },
            membership: NamespaceOwnerRecord {
                namespace_id: NamespaceId::new(Uuid::from_u128(11)),
                user_id: UserId::new(Uuid::from_u128(7)),
                role,
                added_by_user_id: None,
                created_at: NOW,
            },
        }
    }
}
