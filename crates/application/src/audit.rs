use rux_domain::{IdentitySegment, SemanticVersion};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{
    ApiTokenId, JsonObject, NamespaceRole, PackageVersionId, SessionId, TokenScope, UserId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditActor {
    user_id: UserId,
    token_id: Option<ApiTokenId>,
}

impl AuditActor {
    #[must_use]
    pub const fn session(user_id: UserId) -> Self {
        Self {
            user_id,
            token_id: None,
        }
    }

    #[must_use]
    pub const fn token(user_id: UserId, token_id: ApiTokenId) -> Self {
        Self {
            user_id,
            token_id: Some(token_id),
        }
    }

    #[must_use]
    pub const fn user_id(self) -> UserId {
        self.user_id
    }

    #[must_use]
    pub const fn token_id(self) -> Option<ApiTokenId> {
        self.token_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEvent {
    actor: AuditActor,
    action: &'static str,
    subject_type: &'static str,
    subject_key: String,
    metadata: JsonObject,
}

impl AuditEvent {
    #[must_use]
    pub fn account_anonymized(user_id: UserId) -> Self {
        Self::new(
            AuditActor::session(user_id),
            "account_anonymized",
            "user",
            user_id.get().to_string(),
            Map::new(),
        )
    }

    #[must_use]
    pub fn session_created(user_id: UserId, session_id: SessionId) -> Self {
        Self::new(
            AuditActor::session(user_id),
            "session_created",
            "session",
            session_id.get().to_string(),
            Map::new(),
        )
    }

    #[must_use]
    pub fn session_rotated(
        user_id: UserId,
        replaced_session_id: SessionId,
        replacement_session_id: SessionId,
    ) -> Self {
        Self::new(
            AuditActor::session(user_id),
            "session_rotated",
            "session",
            replaced_session_id.get().to_string(),
            metadata([(
                "replacement_session_id",
                Value::String(replacement_session_id.get().to_string()),
            )]),
        )
    }

    #[must_use]
    pub fn session_revoked(user_id: UserId, session_id: SessionId) -> Self {
        Self::new(
            AuditActor::session(user_id),
            "session_revoked",
            "session",
            session_id.get().to_string(),
            Map::new(),
        )
    }

    #[must_use]
    pub fn api_token_created(
        user_id: UserId,
        token_prefix: String,
        mut scopes: Vec<TokenScope>,
        expires_at: Option<OffsetDateTime>,
    ) -> Self {
        scopes.sort_unstable();
        let scopes = scopes
            .into_iter()
            .map(|scope| Value::String(scope.as_str().to_owned()))
            .collect();
        Self::new(
            AuditActor::session(user_id),
            "api_token_created",
            "api_token",
            token_prefix,
            metadata([
                ("scopes", Value::Array(scopes)),
                ("expires_at", optional_timestamp(expires_at)),
            ]),
        )
    }

    #[must_use]
    pub fn api_token_revoked(user_id: UserId, token_prefix: String) -> Self {
        Self::new(
            AuditActor::session(user_id),
            "api_token_revoked",
            "api_token",
            token_prefix,
            Map::new(),
        )
    }

    #[must_use]
    pub fn namespace_created(actor: AuditActor, namespace: &IdentitySegment) -> Self {
        Self::new(
            actor,
            "namespace_created",
            "namespace",
            namespace.normalized().to_owned(),
            metadata([("display_name", Value::String(namespace.as_str().to_owned()))]),
        )
    }

    #[must_use]
    pub fn namespace_member_role_changed(
        actor: AuditActor,
        namespace: &IdentitySegment,
        target_user_id: UserId,
        previous_role: NamespaceRole,
        role: NamespaceRole,
    ) -> Self {
        Self::new(
            actor,
            "namespace_member_role_changed",
            "namespace",
            namespace.normalized().to_owned(),
            metadata([
                (
                    "target_user_id",
                    Value::from(target_user_id.get().to_string()),
                ),
                ("previous_role", role_value(previous_role)),
                ("role", role_value(role)),
            ]),
        )
    }

    #[must_use]
    pub fn namespace_member_removed(
        actor: AuditActor,
        namespace: &IdentitySegment,
        target_user_id: UserId,
        previous_role: NamespaceRole,
    ) -> Self {
        Self::new(
            actor,
            "namespace_member_removed",
            "namespace",
            namespace.normalized().to_owned(),
            metadata([
                (
                    "target_user_id",
                    Value::from(target_user_id.get().to_string()),
                ),
                ("previous_role", role_value(previous_role)),
            ]),
        )
    }

    #[must_use]
    pub fn namespace_invitation_created(
        actor: AuditActor,
        namespace: &IdentitySegment,
        target_user_id: UserId,
        role: NamespaceRole,
        expires_at: OffsetDateTime,
    ) -> Self {
        Self::new(
            actor,
            "namespace_invitation_created",
            "namespace",
            namespace.normalized().to_owned(),
            metadata([
                (
                    "target_user_id",
                    Value::from(target_user_id.get().to_string()),
                ),
                ("role", role_value(role)),
                ("expires_at", timestamp(expires_at)),
            ]),
        )
    }

    #[must_use]
    pub fn namespace_invitation_accepted(
        actor: AuditActor,
        namespace: &IdentitySegment,
        target_user_id: UserId,
        role: NamespaceRole,
    ) -> Self {
        Self::new(
            actor,
            "namespace_invitation_accepted",
            "namespace",
            namespace.normalized().to_owned(),
            metadata([
                (
                    "target_user_id",
                    Value::from(target_user_id.get().to_string()),
                ),
                ("role", role_value(role)),
            ]),
        )
    }

    #[must_use]
    pub fn namespace_invitation_revoked(
        actor: AuditActor,
        namespace: &IdentitySegment,
        target_user_id: UserId,
        role: NamespaceRole,
    ) -> Self {
        Self::new(
            actor,
            "namespace_invitation_revoked",
            "namespace",
            namespace.normalized().to_owned(),
            metadata([
                (
                    "target_user_id",
                    Value::from(target_user_id.get().to_string()),
                ),
                ("role", role_value(role)),
            ]),
        )
    }

    #[must_use]
    pub fn package_version_published(
        actor: AuditActor,
        package_version_id: PackageVersionId,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Self {
        Self::new(
            actor,
            "package_version_published",
            "package_version",
            package_version_id.get().to_string(),
            metadata([
                ("namespace", Value::String(namespace.as_str().to_owned())),
                ("package", Value::String(package.as_str().to_owned())),
                ("version", Value::String(version.as_str().to_owned())),
            ]),
        )
    }

    #[must_use]
    pub fn package_version_yanked(
        actor: AuditActor,
        package_version_id: PackageVersionId,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Self {
        Self::package_version_state_changed(
            actor,
            "package_version_yanked",
            package_version_id,
            namespace,
            package,
            version,
        )
    }

    #[must_use]
    pub fn package_version_unyanked(
        actor: AuditActor,
        package_version_id: PackageVersionId,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Self {
        Self::package_version_state_changed(
            actor,
            "package_version_unyanked",
            package_version_id,
            namespace,
            package,
            version,
        )
    }

    #[must_use]
    pub const fn actor(&self) -> AuditActor {
        self.actor
    }

    #[must_use]
    pub const fn action(&self) -> &'static str {
        self.action
    }

    #[must_use]
    pub const fn subject_type(&self) -> &'static str {
        self.subject_type
    }

    #[must_use]
    pub fn subject_key(&self) -> &str {
        &self.subject_key
    }

    #[must_use]
    pub const fn metadata(&self) -> &JsonObject {
        &self.metadata
    }

    fn new(
        actor: AuditActor,
        action: &'static str,
        subject_type: &'static str,
        subject_key: String,
        metadata: JsonObject,
    ) -> Self {
        Self {
            actor,
            action,
            subject_type,
            subject_key,
            metadata,
        }
    }

    fn package_version_state_changed(
        actor: AuditActor,
        action: &'static str,
        package_version_id: PackageVersionId,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Self {
        Self::new(
            actor,
            action,
            "package_version",
            package_version_id.get().to_string(),
            metadata([
                ("namespace", Value::String(namespace.as_str().to_owned())),
                ("package", Value::String(package.as_str().to_owned())),
                ("version", Value::String(version.as_str().to_owned())),
            ]),
        )
    }
}

fn metadata<const N: usize>(values: [(&str, Value); N]) -> JsonObject {
    values
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn role_value(role: NamespaceRole) -> Value {
    Value::String(
        match role {
            NamespaceRole::Owner => "owner",
            NamespaceRole::Maintainer => "maintainer",
        }
        .to_owned(),
    )
}

fn optional_timestamp(value: Option<OffsetDateTime>) -> Value {
    value.map_or(Value::Null, timestamp)
}

fn timestamp(value: OffsetDateTime) -> Value {
    Value::String(
        value
            .format(&Rfc3339)
            .expect("UTC audit timestamps should format as RFC 3339"),
    )
}

#[cfg(test)]
mod tests {
    use rux_domain::IdentitySegment;
    use serde_json::json;
    use time::Duration;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn session_and_token_events_have_canonical_safe_projections() {
        let user_id = UserId::new(Uuid::from_u128(7));
        let anonymized = AuditEvent::account_anonymized(user_id);
        assert_event(
            &anonymized,
            user_id,
            None,
            "account_anonymized",
            "user",
            &user_id.get().to_string(),
        );
        assert!(anonymized.metadata().is_empty());

        let session_id = SessionId::new(Uuid::from_u128(11));
        let created = AuditEvent::session_created(user_id, session_id);
        assert_event(
            &created,
            user_id,
            None,
            "session_created",
            "session",
            &session_id.get().to_string(),
        );
        assert!(created.metadata().is_empty());

        let replacement_session_id = SessionId::new(Uuid::from_u128(12));
        let rotated = AuditEvent::session_rotated(user_id, session_id, replacement_session_id);
        assert_event(
            &rotated,
            user_id,
            None,
            "session_rotated",
            "session",
            &session_id.get().to_string(),
        );
        assert_eq!(
            Value::Object(rotated.metadata().clone()),
            json!({"replacement_session_id": replacement_session_id.get().to_string()})
        );

        let created = AuditEvent::api_token_created(
            user_id,
            "rux_pat_safe1234".into(),
            vec![TokenScope::Namespace, TokenScope::Publish],
            Some(OffsetDateTime::UNIX_EPOCH + Duration::days(1)),
        );
        assert_event(
            &created,
            user_id,
            None,
            "api_token_created",
            "api_token",
            "rux_pat_safe1234",
        );
        assert_eq!(
            Value::Object(created.metadata().clone()),
            json!({
                "expires_at": "1970-01-02T00:00:00Z",
                "scopes": ["publish", "namespace"]
            })
        );
    }

    #[test]
    fn namespace_events_preserve_actor_attribution_and_allowlisted_metadata() {
        let user_id = UserId::new(Uuid::from_u128(7));
        let token_id = ApiTokenId::new(Uuid::from_u128(8));
        let target_id = UserId::new(Uuid::from_u128(9));
        let namespace = IdentitySegment::new("Rux_Tools").expect("valid namespace");
        let actor = AuditActor::token(user_id, token_id);

        let created = AuditEvent::namespace_created(actor, &namespace);
        assert_event(
            &created,
            user_id,
            Some(token_id),
            "namespace_created",
            "namespace",
            "rux-tools",
        );
        assert_eq!(
            Value::Object(created.metadata().clone()),
            json!({"display_name": "Rux_Tools"})
        );

        let changed = AuditEvent::namespace_member_role_changed(
            actor,
            &namespace,
            target_id,
            NamespaceRole::Maintainer,
            NamespaceRole::Owner,
        );
        assert_eq!(
            Value::Object(changed.metadata().clone()),
            json!({
                "previous_role": "maintainer",
                "role": "owner",
                "target_user_id": target_id.get().to_string()
            })
        );

        let invited = AuditEvent::namespace_invitation_created(
            actor,
            &namespace,
            target_id,
            NamespaceRole::Maintainer,
            OffsetDateTime::UNIX_EPOCH + Duration::days(7),
        );
        assert_eq!(
            Value::Object(invited.metadata().clone()),
            json!({
                "expires_at": "1970-01-08T00:00:00Z",
                "role": "maintainer",
                "target_user_id": target_id.get().to_string()
            })
        );
    }

    #[test]
    fn publication_event_uses_stable_identity_metadata() {
        let actor = AuditActor::token(
            UserId::new(Uuid::from_u128(7)),
            ApiTokenId::new(Uuid::from_u128(8)),
        );
        let namespace = IdentitySegment::new("Rux_Tools").expect("valid namespace");
        let package = IdentitySegment::new("Example_Pkg").expect("valid package");
        let version = SemanticVersion::new("1.2.3+linux").expect("valid version");
        let version_id = PackageVersionId::new(Uuid::from_u128(42));
        let event = AuditEvent::package_version_published(
            actor, version_id, &namespace, &package, &version,
        );

        assert_event(
            &event,
            UserId::new(Uuid::from_u128(7)),
            Some(ApiTokenId::new(Uuid::from_u128(8))),
            "package_version_published",
            "package_version",
            &version_id.get().to_string(),
        );
        assert_eq!(
            Value::Object(event.metadata().clone()),
            json!({
                "namespace": "Rux_Tools",
                "package": "Example_Pkg",
                "version": "1.2.3+linux"
            })
        );

        for (event, action) in [
            (
                AuditEvent::package_version_yanked(
                    actor, version_id, &namespace, &package, &version,
                ),
                "package_version_yanked",
            ),
            (
                AuditEvent::package_version_unyanked(
                    actor, version_id, &namespace, &package, &version,
                ),
                "package_version_unyanked",
            ),
        ] {
            assert_event(
                &event,
                UserId::new(Uuid::from_u128(7)),
                Some(ApiTokenId::new(Uuid::from_u128(8))),
                action,
                "package_version",
                &version_id.get().to_string(),
            );
            assert_eq!(
                Value::Object(event.metadata().clone()),
                json!({
                    "namespace": "Rux_Tools",
                    "package": "Example_Pkg",
                    "version": "1.2.3+linux"
                })
            );
        }
    }

    #[test]
    fn closed_event_payloads_do_not_contain_request_secrets() {
        let event = AuditEvent::api_token_created(
            UserId::new(Uuid::from_u128(7)),
            "rux_pat_safe1234".into(),
            vec![TokenScope::Publish],
            None,
        );
        let projection = format!(
            "{} {} {} {:?}",
            event.action(),
            event.subject_type(),
            event.subject_key(),
            event.metadata()
        );
        for secret in [
            "oauth-code-secret",
            "oauth-state-secret",
            "session-cookie-secret",
            "csrf-secret",
            "rux_pat_full_bearer_credential_secret",
        ] {
            assert!(!projection.contains(secret));
        }
    }

    fn assert_event(
        event: &AuditEvent,
        user_id: UserId,
        token_id: Option<ApiTokenId>,
        action: &str,
        subject_type: &str,
        subject_key: &str,
    ) {
        assert_eq!(event.actor().user_id(), user_id);
        assert_eq!(event.actor().token_id(), token_id);
        assert_eq!(event.action(), action);
        assert_eq!(event.subject_type(), subject_type);
        assert_eq!(event.subject_key(), subject_key);
    }
}
