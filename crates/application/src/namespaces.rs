use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::IdentitySegment;
use time::{Duration, OffsetDateTime};

use crate::{
    ApiTokenId, AuditActor, AuditEvent, Clock, InvitationResolution, NamespaceInvitationRecord,
    NamespaceMemberRecord, NamespaceMembershipRecord, NamespaceRole, NewInvitation,
    RegistryRepository, RegistryTransaction, RepositoryConflict, RepositoryError,
    RepositoryErrorKind, TokenAuthorizer, UserId, UserRecord, WriteOutcome,
};

pub const NAMESPACE_INVITATION_LIFETIME_DAYS: i64 = 7;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NamespaceActor {
    Session(UserId),
    Bearer(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceUser {
    pub github_login: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceSummary {
    pub name: IdentitySegment,
    pub role: NamespaceRole,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceMember {
    pub user: NamespaceUser,
    pub role: NamespaceRole,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceInvitation {
    pub namespace: IdentitySegment,
    pub invited_user: NamespaceUser,
    pub invited_by: Option<NamespaceUser>,
    pub role: NamespaceRole,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceErrorKind {
    InvalidNamespace,
    InvalidGitHubLogin,
    AuthenticationRequired,
    InsufficientScope,
    Forbidden,
    NamespaceNotFound,
    UserNotFound,
    MemberNotFound,
    InvitationNotFound,
    InvitationExpired,
    NamespaceConflict,
    MemberExists,
    PendingInvitation,
    CannotInviteSelf,
    LastOwner,
    Unavailable,
}

#[derive(Debug)]
pub struct NamespaceError {
    kind: NamespaceErrorKind,
}

impl NamespaceError {
    #[must_use]
    pub const fn new(kind: NamespaceErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> NamespaceErrorKind {
        self.kind
    }
}

impl fmt::Display for NamespaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "namespace operation failed: {:?}", self.kind)
    }
}

impl Error for NamespaceError {}

#[async_trait]
pub trait Namespaces: Send + Sync {
    async fn list(&self, actor: NamespaceActor) -> Result<Vec<NamespaceSummary>, NamespaceError>;
    async fn claim(
        &self,
        actor: NamespaceActor,
        name: &str,
    ) -> Result<NamespaceSummary, NamespaceError>;
    async fn members(
        &self,
        actor: NamespaceActor,
        namespace: &str,
    ) -> Result<Vec<NamespaceMember>, NamespaceError>;
    async fn set_member_role(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
        role: NamespaceRole,
    ) -> Result<NamespaceMember, NamespaceError>;
    async fn remove_member(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
    ) -> Result<(), NamespaceError>;
    async fn invitations(
        &self,
        actor: NamespaceActor,
        namespace: &str,
    ) -> Result<Vec<NamespaceInvitation>, NamespaceError>;
    async fn invite(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
        role: NamespaceRole,
    ) -> Result<NamespaceInvitation, NamespaceError>;
    async fn my_invitations(
        &self,
        actor: NamespaceActor,
    ) -> Result<Vec<NamespaceInvitation>, NamespaceError>;
    async fn accept_invitation(
        &self,
        actor: NamespaceActor,
        namespace: &str,
    ) -> Result<NamespaceMember, NamespaceError>;
    async fn revoke_invitation(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
    ) -> Result<(), NamespaceError>;
}

pub struct NamespaceService {
    repository: Arc<dyn RegistryRepository>,
    clock: Arc<dyn Clock>,
    token_authorizer: Arc<dyn TokenAuthorizer>,
}

impl NamespaceService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn RegistryRepository>,
        clock: Arc<dyn Clock>,
        token_authorizer: Arc<dyn TokenAuthorizer>,
    ) -> Self {
        Self {
            repository,
            clock,
            token_authorizer,
        }
    }

    async fn authorize_read(&self, actor: &NamespaceActor) -> Result<UserRecord, NamespaceError> {
        match actor {
            NamespaceActor::Session(user_id) => self
                .repository
                .user_by_id(*user_id)
                .await
                .map_err(|_| unavailable())?
                .filter(active_user)
                .ok_or_else(authentication_required),
            NamespaceActor::Bearer(credential) => {
                let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
                let authorized = self
                    .token_authorizer
                    .authorize_namespace(&mut *transaction, credential)
                    .await
                    .map_err(|error| map_token_error(&error))?;
                let user = transaction
                    .lock_user_by_id(authorized.user_id)
                    .await
                    .map_err(|_| unavailable())?
                    .filter(active_user)
                    .ok_or_else(authentication_required)?;
                transaction.commit().await.map_err(|_| unavailable())?;
                Ok(user)
            }
        }
    }

    async fn authorize_mutation(
        &self,
        transaction: &mut dyn RegistryTransaction,
        actor: &NamespaceActor,
    ) -> Result<AuthorizedActor, NamespaceError> {
        match actor {
            NamespaceActor::Session(user_id) => {
                let user = transaction
                    .lock_user_by_id(*user_id)
                    .await
                    .map_err(|_| unavailable())?
                    .filter(active_user)
                    .ok_or_else(authentication_required)?;
                Ok(AuthorizedActor {
                    user,
                    token_id: None,
                })
            }
            NamespaceActor::Bearer(credential) => {
                let authorized = self
                    .token_authorizer
                    .authorize_namespace(transaction, credential)
                    .await
                    .map_err(|error| map_token_error(&error))?;
                let user = transaction
                    .lock_user_by_id(authorized.user_id)
                    .await
                    .map_err(|_| unavailable())?
                    .filter(active_user)
                    .ok_or_else(authentication_required)?;
                Ok(AuthorizedActor {
                    user,
                    token_id: Some(authorized.id),
                })
            }
        }
    }

    async fn locked_namespace(
        transaction: &mut dyn RegistryTransaction,
        namespace: &IdentitySegment,
    ) -> Result<crate::NamespaceRecord, NamespaceError> {
        transaction
            .lock_namespace_by_name(namespace)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::NamespaceNotFound))
    }

    async fn require_member(
        transaction: &mut dyn RegistryTransaction,
        namespace_id: crate::NamespaceId,
        user_id: UserId,
    ) -> Result<crate::NamespaceOwnerRecord, NamespaceError> {
        transaction
            .lock_namespace_role(namespace_id, user_id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::Forbidden))
    }

    async fn require_owner(
        transaction: &mut dyn RegistryTransaction,
        namespace_id: crate::NamespaceId,
        user_id: UserId,
    ) -> Result<(), NamespaceError> {
        let membership = Self::require_member(transaction, namespace_id, user_id).await?;
        if membership.role != NamespaceRole::Owner {
            return Err(NamespaceError::new(NamespaceErrorKind::Forbidden));
        }
        Ok(())
    }
}

struct AuthorizedActor {
    user: UserRecord,
    token_id: Option<ApiTokenId>,
}

impl AuthorizedActor {
    const fn audit_actor(&self) -> AuditActor {
        match self.token_id {
            Some(token_id) => AuditActor::token(self.user.id, token_id),
            None => AuditActor::session(self.user.id),
        }
    }
}

#[async_trait]
impl Namespaces for NamespaceService {
    async fn list(&self, actor: NamespaceActor) -> Result<Vec<NamespaceSummary>, NamespaceError> {
        let user = self.authorize_read(&actor).await?;
        self.repository
            .namespaces_by_user_id(user.id)
            .await
            .map_err(|_| unavailable())
            .map(|records| records.into_iter().map(namespace_summary).collect())
    }

    async fn claim(
        &self,
        actor: NamespaceActor,
        name: &str,
    ) -> Result<NamespaceSummary, NamespaceError> {
        let name = parse_namespace(name)?;
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let authorized = self.authorize_mutation(&mut *transaction, &actor).await?;
        let namespace = match transaction
            .create_namespace(&name, Some(authorized.user.id))
            .await
        {
            Ok(namespace) => namespace,
            Err(error) => {
                let kind = if matches!(
                    error.kind(),
                    RepositoryErrorKind::Conflict(RepositoryConflict::NamespaceIdentity)
                ) {
                    NamespaceErrorKind::NamespaceConflict
                } else {
                    NamespaceErrorKind::Unavailable
                };
                let _ = transaction.rollback().await;
                return Err(NamespaceError::new(kind));
            }
        };
        if transaction
            .set_namespace_owner(
                namespace.id,
                authorized.user.id,
                NamespaceRole::Owner,
                Some(authorized.user.id),
            )
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return Err(unavailable());
        }
        if transaction
            .append_audit(&AuditEvent::namespace_created(
                authorized.audit_actor(),
                &namespace.name,
            ))
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return Err(unavailable());
        }
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(NamespaceSummary {
            name: namespace.name,
            role: NamespaceRole::Owner,
            created_at: namespace.created_at,
            updated_at: namespace.updated_at,
        })
    }

    async fn members(
        &self,
        actor: NamespaceActor,
        namespace: &str,
    ) -> Result<Vec<NamespaceMember>, NamespaceError> {
        let namespace = parse_namespace(namespace)?;
        let user = self.authorize_read(&actor).await?;
        let record = self
            .repository
            .namespace_by_name(&namespace)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::NamespaceNotFound))?;
        self.repository
            .namespace_role(record.id, user.id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::Forbidden))?;
        self.repository
            .namespace_members(record.id)
            .await
            .map_err(|_| unavailable())?
            .into_iter()
            .map(namespace_member)
            .collect()
    }

    async fn set_member_role(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
        role: NamespaceRole,
    ) -> Result<NamespaceMember, NamespaceError> {
        let namespace = parse_namespace(namespace)?;
        validate_github_login(github_login)?;
        let now = self.clock.now();
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let authorized = self.authorize_mutation(&mut *transaction, &actor).await?;
        let namespace = Self::locked_namespace(&mut *transaction, &namespace).await?;
        Self::require_owner(&mut *transaction, namespace.id, authorized.user.id).await?;
        let target = transaction
            .user_by_github_login_in_transaction(github_login)
            .await
            .map_err(|_| unavailable())?
            .filter(active_user)
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::MemberNotFound))?;
        let membership = transaction
            .lock_namespace_role(namespace.id, target.id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::MemberNotFound))?;
        if membership.role == role {
            transaction.commit().await.map_err(|_| unavailable())?;
            return member(&target, membership.role, membership.created_at);
        }
        if membership.role == NamespaceRole::Owner
            && role == NamespaceRole::Maintainer
            && transaction
                .namespace_owner_count(namespace.id)
                .await
                .map_err(|_| unavailable())?
                <= 1
        {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::LastOwner));
        }
        let changed = transaction
            .set_namespace_owner(namespace.id, target.id, role, Some(authorized.user.id))
            .await
            .map_err(|_| unavailable())?;
        touch(&mut *transaction, namespace.id, now).await?;
        if transaction
            .append_audit(&AuditEvent::namespace_member_role_changed(
                authorized.audit_actor(),
                &namespace.name,
                target.id,
                membership.role,
                changed.role,
            ))
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return Err(unavailable());
        }
        transaction.commit().await.map_err(|_| unavailable())?;
        member(&target, changed.role, changed.created_at)
    }

    async fn remove_member(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
    ) -> Result<(), NamespaceError> {
        let namespace = parse_namespace(namespace)?;
        validate_github_login(github_login)?;
        let now = self.clock.now();
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let authorized = self.authorize_mutation(&mut *transaction, &actor).await?;
        let namespace = Self::locked_namespace(&mut *transaction, &namespace).await?;
        let actor_membership =
            Self::require_member(&mut *transaction, namespace.id, authorized.user.id).await?;
        let target = transaction
            .user_by_github_login_in_transaction(github_login)
            .await
            .map_err(|_| unavailable())?
            .filter(active_user);
        let is_self = target
            .as_ref()
            .is_some_and(|target| target.id == authorized.user.id);
        if actor_membership.role != NamespaceRole::Owner && !is_self {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::Forbidden));
        }
        let Some(target) = target else {
            transaction.commit().await.map_err(|_| unavailable())?;
            return Ok(());
        };
        let membership = transaction
            .lock_namespace_role(namespace.id, target.id)
            .await
            .map_err(|_| unavailable())?;
        let Some(membership) = membership else {
            transaction.commit().await.map_err(|_| unavailable())?;
            return Ok(());
        };
        if membership.role == NamespaceRole::Owner
            && transaction
                .namespace_owner_count(namespace.id)
                .await
                .map_err(|_| unavailable())?
                <= 1
        {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::LastOwner));
        }
        let outcome = transaction
            .remove_namespace_owner(namespace.id, target.id)
            .await
            .map_err(|_| unavailable())?;
        if outcome == WriteOutcome::Applied {
            touch(&mut *transaction, namespace.id, now).await?;
            if transaction
                .append_audit(&AuditEvent::namespace_member_removed(
                    authorized.audit_actor(),
                    &namespace.name,
                    target.id,
                    membership.role,
                ))
                .await
                .is_err()
            {
                let _ = transaction.rollback().await;
                return Err(unavailable());
            }
        }
        transaction.commit().await.map_err(|_| unavailable())
    }

    async fn invitations(
        &self,
        actor: NamespaceActor,
        namespace: &str,
    ) -> Result<Vec<NamespaceInvitation>, NamespaceError> {
        let namespace = parse_namespace(namespace)?;
        let user = self.authorize_read(&actor).await?;
        let record = self
            .repository
            .namespace_by_name(&namespace)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::NamespaceNotFound))?;
        let role = self
            .repository
            .namespace_role(record.id, user.id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::Forbidden))?;
        if role.role != NamespaceRole::Owner {
            return Err(NamespaceError::new(NamespaceErrorKind::Forbidden));
        }
        self.repository
            .pending_invitations_by_namespace(record.id, self.clock.now())
            .await
            .map_err(|_| unavailable())?
            .into_iter()
            .map(namespace_invitation)
            .collect()
    }

    async fn invite(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
        role: NamespaceRole,
    ) -> Result<NamespaceInvitation, NamespaceError> {
        let namespace_name = parse_namespace(namespace)?;
        validate_github_login(github_login)?;
        let now = self.clock.now();
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let authorized = self.authorize_mutation(&mut *transaction, &actor).await?;
        let namespace = Self::locked_namespace(&mut *transaction, &namespace_name).await?;
        Self::require_owner(&mut *transaction, namespace.id, authorized.user.id).await?;
        let target = transaction
            .user_by_github_login_in_transaction(github_login)
            .await
            .map_err(|_| unavailable())?
            .filter(active_user)
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::UserNotFound))?;
        if target.id == authorized.user.id {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::CannotInviteSelf));
        }
        if transaction
            .lock_namespace_role(namespace.id, target.id)
            .await
            .map_err(|_| unavailable())?
            .is_some()
        {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::MemberExists));
        }
        if let Some(existing) = transaction
            .lock_pending_invitation(namespace.id, target.id)
            .await
            .map_err(|_| unavailable())?
        {
            if existing.expires_at > now {
                let _ = transaction.rollback().await;
                return Err(NamespaceError::new(NamespaceErrorKind::PendingInvitation));
            }
            transaction
                .resolve_invitation(existing.id, InvitationResolution::Revoked, now)
                .await
                .map_err(|_| unavailable())?;
        }
        let invitation = transaction
            .create_invitation(&NewInvitation {
                namespace_id: namespace.id,
                invited_user_id: target.id,
                invited_by_user_id: Some(authorized.user.id),
                role,
                expires_at: now + Duration::days(NAMESPACE_INVITATION_LIFETIME_DAYS),
            })
            .await
            .map_err(|error| map_invitation_write(&error))?;
        touch(&mut *transaction, namespace.id, now).await?;
        if transaction
            .append_audit(&AuditEvent::namespace_invitation_created(
                authorized.audit_actor(),
                &namespace.name,
                target.id,
                invitation.role,
                invitation.expires_at,
            ))
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return Err(unavailable());
        }
        transaction.commit().await.map_err(|_| unavailable())?;
        Ok(NamespaceInvitation {
            namespace: namespace.name,
            invited_user: namespace_user(&target)?,
            invited_by: Some(namespace_user(&authorized.user)?),
            role: invitation.role,
            created_at: invitation.created_at,
            expires_at: invitation.expires_at,
        })
    }

    async fn my_invitations(
        &self,
        actor: NamespaceActor,
    ) -> Result<Vec<NamespaceInvitation>, NamespaceError> {
        let user = self.authorize_read(&actor).await?;
        self.repository
            .pending_invitations_by_user_id(user.id, self.clock.now())
            .await
            .map_err(|_| unavailable())?
            .into_iter()
            .map(namespace_invitation)
            .collect()
    }

    async fn accept_invitation(
        &self,
        actor: NamespaceActor,
        namespace: &str,
    ) -> Result<NamespaceMember, NamespaceError> {
        let namespace_name = parse_namespace(namespace)?;
        let now = self.clock.now();
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let authorized = self.authorize_mutation(&mut *transaction, &actor).await?;
        let namespace = Self::locked_namespace(&mut *transaction, &namespace_name).await?;
        if let Some(existing) = transaction
            .lock_namespace_role(namespace.id, authorized.user.id)
            .await
            .map_err(|_| unavailable())?
        {
            transaction.commit().await.map_err(|_| unavailable())?;
            return member(&authorized.user, existing.role, existing.created_at);
        }
        let invitation = transaction
            .lock_pending_invitation(namespace.id, authorized.user.id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| NamespaceError::new(NamespaceErrorKind::InvitationNotFound))?;
        if invitation.expires_at <= now {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::InvitationExpired));
        }
        let membership = transaction
            .set_namespace_owner(
                namespace.id,
                authorized.user.id,
                invitation.role,
                invitation.invited_by_user_id,
            )
            .await
            .map_err(|_| unavailable())?;
        if transaction
            .resolve_invitation(invitation.id, InvitationResolution::Accepted, now)
            .await
            .map_err(|_| unavailable())?
            != WriteOutcome::Applied
        {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::InvitationNotFound));
        }
        touch(&mut *transaction, namespace.id, now).await?;
        if transaction
            .append_audit(&AuditEvent::namespace_invitation_accepted(
                authorized.audit_actor(),
                &namespace.name,
                authorized.user.id,
                membership.role,
            ))
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return Err(unavailable());
        }
        transaction.commit().await.map_err(|_| unavailable())?;
        member(&authorized.user, membership.role, membership.created_at)
    }

    async fn revoke_invitation(
        &self,
        actor: NamespaceActor,
        namespace: &str,
        github_login: &str,
    ) -> Result<(), NamespaceError> {
        let namespace_name = parse_namespace(namespace)?;
        validate_github_login(github_login)?;
        let now = self.clock.now();
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let authorized = self.authorize_mutation(&mut *transaction, &actor).await?;
        let namespace = Self::locked_namespace(&mut *transaction, &namespace_name).await?;
        let actor_membership = transaction
            .lock_namespace_role(namespace.id, authorized.user.id)
            .await
            .map_err(|_| unavailable())?;
        let target = transaction
            .user_by_github_login_in_transaction(github_login)
            .await
            .map_err(|_| unavailable())?
            .filter(active_user);
        let is_self = target
            .as_ref()
            .is_some_and(|target| target.id == authorized.user.id);
        if actor_membership.as_ref().map(|value| value.role) != Some(NamespaceRole::Owner)
            && !is_self
        {
            let _ = transaction.rollback().await;
            return Err(NamespaceError::new(NamespaceErrorKind::Forbidden));
        }
        if let Some(target) = target
            && let Some(invitation) = transaction
                .lock_pending_invitation(namespace.id, target.id)
                .await
                .map_err(|_| unavailable())?
        {
            let outcome = transaction
                .resolve_invitation(invitation.id, InvitationResolution::Revoked, now)
                .await
                .map_err(|_| unavailable())?;
            if outcome == WriteOutcome::Applied {
                touch(&mut *transaction, namespace.id, now).await?;
                if transaction
                    .append_audit(&AuditEvent::namespace_invitation_revoked(
                        authorized.audit_actor(),
                        &namespace.name,
                        target.id,
                        invitation.role,
                    ))
                    .await
                    .is_err()
                {
                    let _ = transaction.rollback().await;
                    return Err(unavailable());
                }
            }
        }
        transaction.commit().await.map_err(|_| unavailable())
    }
}

async fn touch(
    transaction: &mut dyn RegistryTransaction,
    namespace_id: crate::NamespaceId,
    at: OffsetDateTime,
) -> Result<(), NamespaceError> {
    if transaction
        .touch_namespace(namespace_id, at)
        .await
        .map_err(|_| unavailable())?
        == WriteOutcome::NotFound
    {
        return Err(unavailable());
    }
    Ok(())
}

fn parse_namespace(value: &str) -> Result<IdentitySegment, NamespaceError> {
    IdentitySegment::new(value)
        .map_err(|_| NamespaceError::new(NamespaceErrorKind::InvalidNamespace))
}

fn validate_github_login(value: &str) -> Result<(), NamespaceError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 39
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || bytes
            .windows(2)
            .any(|pair| pair[0] == b'-' && pair[1] == b'-')
        || bytes
            .iter()
            .any(|byte| !byte.is_ascii_alphanumeric() && *byte != b'-')
    {
        return Err(NamespaceError::new(NamespaceErrorKind::InvalidGitHubLogin));
    }
    Ok(())
}

fn active_user(user: &UserRecord) -> bool {
    user.anonymized_at.is_none() && user.github_login.is_some()
}

fn namespace_user(user: &UserRecord) -> Result<NamespaceUser, NamespaceError> {
    Ok(NamespaceUser {
        github_login: user.github_login.clone().ok_or_else(unavailable)?,
        display_name: user.display_name.clone(),
        avatar_url: user.avatar_url.clone(),
    })
}

fn namespace_summary(record: NamespaceMembershipRecord) -> NamespaceSummary {
    NamespaceSummary {
        name: record.namespace.name,
        role: record.membership.role,
        created_at: record.namespace.created_at,
        updated_at: record.namespace.updated_at,
    }
}

fn member(
    user: &UserRecord,
    role: NamespaceRole,
    created_at: OffsetDateTime,
) -> Result<NamespaceMember, NamespaceError> {
    Ok(NamespaceMember {
        user: namespace_user(user)?,
        role,
        created_at,
    })
}

fn namespace_member(record: NamespaceMemberRecord) -> Result<NamespaceMember, NamespaceError> {
    let NamespaceMemberRecord { membership, user } = record;
    member(&user, membership.role, membership.created_at)
}

fn namespace_invitation(
    record: NamespaceInvitationRecord,
) -> Result<NamespaceInvitation, NamespaceError> {
    Ok(NamespaceInvitation {
        namespace: record.namespace.name,
        invited_user: namespace_user(&record.invited_user)?,
        invited_by: record
            .invited_by_user
            .as_ref()
            .map(namespace_user)
            .transpose()?,
        role: record.invitation.role,
        created_at: record.invitation.created_at,
        expires_at: record.invitation.expires_at,
    })
}

fn map_token_error(error: &crate::ApiTokenError) -> NamespaceError {
    let kind = match error.kind() {
        crate::ApiTokenErrorKind::AuthenticationRequired => {
            NamespaceErrorKind::AuthenticationRequired
        }
        crate::ApiTokenErrorKind::InsufficientScope => NamespaceErrorKind::InsufficientScope,
        _ => NamespaceErrorKind::Unavailable,
    };
    NamespaceError::new(kind)
}

fn map_invitation_write(error: &RepositoryError) -> NamespaceError {
    if matches!(
        error.kind(),
        RepositoryErrorKind::Conflict(RepositoryConflict::PendingInvitation)
    ) {
        NamespaceError::new(NamespaceErrorKind::PendingInvitation)
    } else {
        unavailable()
    }
}

const fn authentication_required() -> NamespaceError {
    NamespaceError::new(NamespaceErrorKind::AuthenticationRequired)
}

const fn unavailable() -> NamespaceError {
    NamespaceError::new(NamespaceErrorKind::Unavailable)
}
