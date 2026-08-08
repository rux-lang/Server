use axum::routing::get;
use axum::{Json, Router};
use utoipa::openapi::content::Content;
use utoipa::openapi::header::Header;
use utoipa::openapi::response::{Response as OpenApiResponse, ResponseBuilder};
use utoipa::openapi::schema::{ObjectBuilder, SchemaType};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{Modify, OpenApi};

use crate::contract::{Problem, ProblemResponse, ValidationError};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Rux Registry API",
        version = "1.0.0",
        description = "Public HTTP API for the Rux package registry"
    ),
    servers((url = "/v1", description = "Public API v1")),
    paths(
        crate::auth::begin_github_login,
        crate::auth::github_callback,
        crate::auth::session,
        crate::auth::logout,
        crate::account::delete_account,
        crate::tokens::identify,
        crate::tokens::list_tokens,
        crate::tokens::issue_token,
        crate::tokens::revoke_token,
        crate::dashboard::dashboard,
        crate::namespaces::list_namespaces,
        crate::namespaces::claim_namespace,
        crate::namespaces::list_members,
        crate::namespaces::set_member_role,
        crate::namespaces::remove_member,
        crate::namespaces::list_namespace_invitations,
        crate::namespaces::invite_member,
        crate::namespaces::revoke_invitation,
        crate::namespaces::list_my_invitations,
        crate::namespaces::accept_invitation,
        crate::publication::publish_package,
        crate::resolver::resolver_index,
        crate::metadata::package_summary,
        crate::metadata::package_version,
        crate::search::search_packages,
        crate::playground::run_playground,
        crate::playground::playground_limits,
        crate::discovery::package_dependents,
        crate::discovery::keywords,
        crate::discovery::package_versions,
        crate::discovery::package_download_statistics,
        crate::discovery::highlights,
        crate::discovery::sitemap,
        crate::yanks::set_yank_state,
        crate::downloads::package_download,
        crate::downloads::package_download_head
    ),
    components(schemas(
        Problem,
        ValidationError,
        crate::auth::SessionDocument,
        crate::auth::SessionUser,
        crate::account::DeleteAccountRequest,
        crate::tokens::IdentityDocument,
        crate::tokens::IssueTokenRequest,
        crate::tokens::IssuedTokenDocument,
        crate::tokens::TokenDocument,
        crate::tokens::TokenScopeDocument,
        crate::tokens::TokenStatusDocument,
        crate::dashboard::DashboardDocument,
        crate::dashboard::DashboardCountsDocument,
        crate::dashboard::DashboardUserDocument,
        crate::dashboard::DashboardNamespaceDocument,
        crate::dashboard::DashboardPackageDocument,
        crate::dashboard::DashboardInvitationDocument,
        crate::dashboard::DashboardActivityKindDocument,
        crate::dashboard::DashboardActivityDocument,
        crate::dashboard::DashboardDownloadsDocument,
        crate::dashboard::DashboardDownloadLeaderDocument,
        crate::namespaces::ClaimNamespaceRequest,
        crate::namespaces::InviteNamespaceMemberRequest,
        crate::namespaces::UpdateNamespaceMemberRequest,
        crate::namespaces::NamespaceDocument,
        crate::namespaces::NamespaceMemberDocument,
        crate::namespaces::NamespaceInvitationDocument,
        crate::namespaces::NamespaceUserDocument,
        crate::namespaces::NamespaceRoleDocument,
        crate::publication::PublishedPackageVersionDocument,
        crate::metadata::PackageSummaryDocument,
        crate::metadata::PackageVersionDocument,
        crate::metadata::PackageTypeDocument,
        crate::metadata::PackageDependencyDocument,
        crate::metadata::ReadmeDocument,
        crate::metadata::LicenseDocument,
        crate::metadata::ChecksumDocument,
        crate::search::PackageSearchDocument,
        crate::search::SearchPageMeta,
        crate::search::PackageSearchResponse,
        crate::playground::PlaygroundModeDocument,
        crate::playground::PlaygroundProfileDocument,
        crate::playground::PlaygroundRunRequest,
        crate::playground::PlaygroundRunDocument,
        crate::playground::PlaygroundBuildDocument,
        crate::playground::PlaygroundProgramDocument,
        crate::playground::PlaygroundLimitsDocument,
        crate::discovery::DiscoveryPageMeta,
        crate::discovery::DependentRequirementDocument,
        crate::discovery::DependentPackageDocument,
        crate::discovery::DependentsResponse,
        crate::discovery::KeywordDocument,
        crate::discovery::KeywordsResponse,
        crate::discovery::VersionHistoryDocument,
        crate::discovery::VersionsResponse,
        crate::discovery::HighlightPackageDocument,
        crate::discovery::HighlightsDocument,
        crate::discovery::PackageDownloadDayDocument,
        crate::discovery::PackageDownloadStatisticsDocument,
        crate::discovery::SitemapEntryDocument,
        crate::discovery::SitemapResponse,
        crate::yanks::SetYankStateRequest,
        crate::yanks::PackageVersionYankStateDocument,
        crate::resolver::ResolverIndexDocument,
        crate::resolver::ResolverVersionDocument,
        crate::resolver::ResolverDependencyDocument
    ), responses(ProblemResponse)),
    modifiers(&ContractSchemas)
)]
struct ApiDoc;

struct ContractSchemas;

impl Modify for ContractSchemas {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .as_mut()
            .expect("the public API document declares components");
        components.schemas.insert(
            "DataEnvelope".to_owned(),
            ObjectBuilder::new()
                .description(Some(
                    "The success envelope used by every public JSON endpoint.",
                ))
                .property(
                    "data",
                    ObjectBuilder::new().schema_type(SchemaType::AnyValue),
                )
                .required("data")
                .into(),
        );
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "__Host-rux_session",
                "Opaque host-only browser session cookie",
            ))),
        );
        components.add_security_scheme(
            "csrf_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "__Host-rux_csrf",
                "Host-only browser CSRF credential",
            ))),
        );
        components.add_security_scheme(
            "csrf_header",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "CSRF credential echoed by the browser client",
            ))),
        );
        components.add_security_scheme(
            "bearer_token",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
        if let Some(RefOr::T(response)) = components.responses.get_mut("ProblemResponse") {
            response
                .headers
                .insert("X-Request-ID".to_owned(), request_id_header());
        }

        for path_item in openapi.paths.paths.values_mut() {
            for operation in [
                path_item.get.as_mut(),
                path_item.put.as_mut(),
                path_item.post.as_mut(),
                path_item.delete.as_mut(),
                path_item.options.as_mut(),
                path_item.head.as_mut(),
                path_item.patch.as_mut(),
                path_item.trace.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                for response in operation.responses.responses.values_mut() {
                    if let RefOr::T(response) = response {
                        response
                            .headers
                            .insert("X-Request-ID".to_owned(), request_id_header());
                    }
                }
                operation.responses.responses.insert(
                    "429".to_owned(),
                    problem_with_headers("The request rate limit was exceeded.", true).into(),
                );
                operation.responses.responses.insert(
                    "504".to_owned(),
                    problem_with_headers("The request exceeded its execution deadline.", false)
                        .into(),
                );
            }
        }
    }
}

fn request_id_header() -> Header {
    let mut header = Header::default();
    header.description = Some("Opaque server-generated request correlation identifier.".to_owned());
    header
}

fn problem_with_headers(description: &str, retry_after: bool) -> OpenApiResponse {
    let mut builder = ResponseBuilder::new()
        .description(description)
        .header("X-Request-ID", request_id_header())
        .content(
            crate::contract::PROBLEM_JSON,
            Content::new(Some(Ref::from_schema_name("Problem"))),
        );
    if retry_after {
        let mut header = Header::default();
        header.description = Some("Whole seconds until the client may retry.".to_owned());
        builder = builder.header("Retry-After", header);
    }
    builder.build()
}

pub fn router() -> Router {
    Router::new().route("/openapi/v1.json", get(document))
}

async fn document() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use axum::http::{Request, StatusCode, header};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn openapi_route_publishes_the_public_v1_contract() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/openapi/v1.json")
                    .body(axum::body::Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("OpenAPI route should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("OpenAPI body should be readable");
        let document: Value = serde_json::from_slice(&body).expect("OpenAPI should be JSON");

        assert_eq!(document["openapi"], "3.1.0");
        assert_eq!(document["info"]["title"], "Rux Registry API");
        assert_eq!(document["info"]["version"], "1.0.0");
        assert_eq!(document["servers"][0]["url"], "/v1");
        assert!(document["paths"].get("/auth/github").is_some());
        assert!(document["paths"].get("/auth/github/callback").is_some());
        assert!(document["paths"].get("/auth/session").is_some());
        assert!(document["paths"].get("/auth/logout").is_some());
        assert!(document["paths"].get("/account").is_some());
        assert!(document["paths"].get("/tokens").is_some());
        assert!(document["paths"].get("/dashboard").is_some());
        assert_account_contract(&document);
        assert_publication_contract(&document);
        assert_resolver_contract(&document);
        assert_metadata_contract(&document);
        assert_search_contract(&document);
        assert_playground_contract(&document);
        assert_discovery_contract(&document);
        assert_yank_contract(&document);
        assert_download_contract(&document);
        assert_abuse_control_contract(&document);
        assert!(document["paths"].get("/tokens/{token_prefix}").is_some());
        assert!(document["paths"].get("/namespaces").is_some());
        assert!(
            document["paths"]
                .get("/namespaces/{namespace}/members/{github_login}")
                .is_some()
        );
        assert!(
            document["paths"]
                .get("/namespaces/{namespace}/invitations/{github_login}")
                .is_some()
        );
        assert!(
            document["paths"]
                .get("/invitations/{namespace}/accept")
                .is_some()
        );

        assert_shared_contract(&document);
    }

    fn assert_abuse_control_contract(document: &Value) {
        let operation = &document["paths"]["/search"]["get"];
        assert_eq!(
            operation["responses"]["200"]["headers"]["X-Request-ID"]["schema"]["type"],
            "string"
        );
        assert_eq!(
            operation["responses"]["429"]["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
        assert_eq!(
            operation["responses"]["429"]["headers"]["Retry-After"]["schema"]["type"],
            "string"
        );
        assert!(operation["responses"]["504"].is_object());
        assert_eq!(
            document["components"]["responses"]["ProblemResponse"]["headers"]["X-Request-ID"]["schema"]
                ["type"],
            "string"
        );
    }

    fn assert_shared_contract(document: &Value) {
        let schemas = &document["components"]["schemas"];
        assert!(
            schemas.get("DataEnvelope").is_some(),
            "unexpected schemas: {schemas}"
        );
        assert!(schemas.get("Problem").is_some());
        assert!(schemas.get("ValidationError").is_some());
        assert!(schemas.get("NamespaceDocument").is_some());
        assert!(schemas.get("NamespaceInvitationDocument").is_some());
        assert_eq!(
            schemas["DataEnvelope"]["required"],
            serde_json::json!(["data"])
        );
        assert_eq!(
            schemas["DataEnvelope"]["properties"]["data"],
            serde_json::json!({})
        );
        assert_eq!(
            schemas["Problem"]["required"],
            serde_json::json!(["type", "title", "status", "code"])
        );
        assert_eq!(schemas["Problem"]["properties"]["type"]["format"], "uri");
        assert_eq!(schemas["Problem"]["properties"]["status"]["minimum"], 400);
        assert_eq!(schemas["Problem"]["properties"]["status"]["maximum"], 599);
        assert_eq!(
            schemas["ValidationError"]["required"],
            serde_json::json!(["code", "detail"])
        );
        assert_eq!(
            schemas["ValidationError"]["properties"]["pointer"]["format"],
            "json-pointer"
        );
        assert_eq!(schemas["Problem"]["properties"]["errors"]["minItems"], 1);

        let problem_response = &document["components"]["responses"]["ProblemResponse"];
        assert_eq!(
            problem_response["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
        assert_eq!(
            document["components"]["securitySchemes"]["session_cookie"]["in"],
            "cookie"
        );
        assert_eq!(
            document["components"]["securitySchemes"]["session_cookie"]["name"],
            "__Host-rux_session"
        );
        assert_eq!(
            document["components"]["securitySchemes"]["csrf_cookie"]["name"],
            "__Host-rux_csrf"
        );
        assert_eq!(
            document["components"]["securitySchemes"]["csrf_header"]["name"],
            "X-CSRF-Token"
        );
        assert_eq!(
            document["components"]["securitySchemes"]["bearer_token"]["scheme"],
            "bearer"
        );
    }

    fn assert_publication_contract(document: &Value) {
        assert!(document["paths"].get("/packages").is_some());
        assert!(
            document["paths"]["/packages"]["post"]["requestBody"]["content"]
                .get("multipart/form-data")
                .is_some()
        );
    }

    fn assert_account_contract(document: &Value) {
        let operation = &document["paths"]["/account"]["delete"];
        assert!(operation.is_object());
        for scheme in ["session_cookie", "csrf_cookie", "csrf_header"] {
            assert_eq!(operation["security"][0][scheme], serde_json::json!([]));
        }
        for status in ["204", "401", "403", "409", "422", "503"] {
            assert!(operation["responses"].get(status).is_some());
        }
        assert_eq!(
            document["components"]["schemas"]["DeleteAccountRequest"]["required"],
            serde_json::json!(["github_login"])
        );
    }

    fn assert_resolver_contract(document: &Value) {
        let operation = &document["paths"]["/index/{namespace}/{package}"]["get"];
        assert!(operation.is_object());
        assert_eq!(
            operation["responses"]["200"]["headers"]["ETag"]["schema"]["type"],
            "string"
        );
        assert!(operation["responses"].get("304").is_some());
        assert!(operation["responses"].get("404").is_some());
        assert!(operation["responses"].get("422").is_some());
        assert!(operation["responses"].get("503").is_some());
        assert!(
            document["components"]["schemas"]
                .get("ResolverIndexDocument")
                .is_some()
        );
    }

    fn assert_metadata_contract(document: &Value) {
        let summary = &document["paths"]["/packages/{namespace}/{package}"]["get"];
        let version = &document["paths"]["/packages/{namespace}/{package}/{version}"]["get"];
        assert!(summary.is_object());
        assert!(version.is_object());
        for status in ["200", "404", "422", "503"] {
            assert!(summary["responses"].get(status).is_some());
            assert!(version["responses"].get(status).is_some());
        }
        let schemas = &document["components"]["schemas"];
        for schema in [
            "PackageSummaryDocument",
            "PackageVersionDocument",
            "ReadmeDocument",
            "LicenseDocument",
            "ChecksumDocument",
        ] {
            assert!(schemas.get(schema).is_some(), "missing {schema} schema");
        }
        assert_eq!(
            schemas["PackageVersionDocument"]["properties"]["download_url"]["type"],
            "string"
        );
    }

    fn assert_download_contract(document: &Value) {
        let download = &document["paths"]["/packages/{namespace}/{package}/{version}/download"];
        for method in ["get", "head"] {
            let operation = &download[method];
            assert!(operation.is_object());
            assert_eq!(
                operation["responses"]["307"]["headers"]["Location"]["schema"]["type"],
                "string"
            );
            assert_eq!(
                operation["responses"]["307"]["headers"]["Cache-Control"]["schema"]["type"],
                "string"
            );
            for status in ["404", "422", "503"] {
                assert!(operation["responses"].get(status).is_some());
            }
        }

        let statistics = &document["paths"]["/packages/{namespace}/{package}/downloads"]["get"];
        assert!(statistics.is_object());
        for status in ["200", "404", "422", "503"] {
            assert!(statistics["responses"].get(status).is_some());
        }
        let schemas = &document["components"]["schemas"];
        for schema in [
            "PackageDownloadDayDocument",
            "PackageDownloadStatisticsDocument",
        ] {
            assert!(schemas.get(schema).is_some(), "missing {schema} schema");
        }
        assert_eq!(
            schemas["PackageDownloadDayDocument"]["properties"]["date"]["format"],
            "date"
        );
    }

    fn assert_playground_contract(document: &Value) {
        let run = &document["paths"]["/playground/run"]["post"];
        assert!(
            run.is_object(),
            "the playground run path should be published"
        );
        for status in ["200", "403", "422", "503", "504"] {
            assert!(
                run["responses"].get(status).is_some(),
                "missing playground run response {status}"
            );
        }

        let limits = &document["paths"]["/playground/limits"]["get"];
        assert!(
            limits.is_object(),
            "the playground limits path should be published"
        );
        assert!(limits["responses"].get("200").is_some());

        // Every document the endpoints name must resolve, or the contract
        // publishes a dangling reference.
        for schema in [
            "PlaygroundRunRequest",
            "PlaygroundRunDocument",
            "PlaygroundBuildDocument",
            "PlaygroundProgramDocument",
            "PlaygroundLimitsDocument",
            "PlaygroundModeDocument",
            "PlaygroundProfileDocument",
        ] {
            assert!(
                document["components"]["schemas"].get(schema).is_some(),
                "missing playground schema {schema}"
            );
        }
    }

    fn assert_search_contract(document: &Value) {
        let operation = &document["paths"]["/search"]["get"];
        assert!(operation.is_object());
        for status in ["200", "422", "503"] {
            assert!(operation["responses"].get(status).is_some());
        }
        let parameters = operation["parameters"]
            .as_array()
            .expect("search parameters should be an array");
        for name in [
            "q",
            "namespace",
            "keyword",
            "package_type",
            "limit",
            "cursor",
        ] {
            assert!(
                parameters.iter().any(|parameter| parameter["name"] == name),
                "missing search parameter {name}"
            );
        }
        let schemas = &document["components"]["schemas"];
        for schema in [
            "PackageSearchDocument",
            "SearchPageMeta",
            "PackageSearchResponse",
        ] {
            assert!(schemas.get(schema).is_some(), "missing {schema} schema");
        }
        assert_eq!(
            schemas["PackageSearchResponse"]["required"],
            serde_json::json!(["data", "meta"])
        );
    }

    fn assert_yank_contract(document: &Value) {
        let operation = &document["paths"]["/packages/{namespace}/{package}/{version}"]["patch"];
        assert!(operation.is_object());
        assert_eq!(
            operation["security"][0]["bearer_token"],
            serde_json::json!([])
        );
        for status in ["200", "401", "403", "404", "422", "503"] {
            assert!(operation["responses"].get(status).is_some());
        }
        let schemas = &document["components"]["schemas"];
        assert!(schemas.get("SetYankStateRequest").is_some());
        assert!(schemas.get("PackageVersionYankStateDocument").is_some());
        assert_eq!(
            schemas["SetYankStateRequest"]["required"],
            serde_json::json!(["yanked"])
        );
    }

    fn assert_discovery_contract(document: &Value) {
        for path in [
            "/packages/{namespace}/{package}/dependents",
            "/packages/{namespace}/{package}/versions",
            "/keywords",
            "/highlights",
            "/sitemap",
        ] {
            assert!(document["paths"][path]["get"].is_object(), "missing {path}");
        }
        for schema in [
            "DependentsResponse",
            "KeywordsResponse",
            "VersionsResponse",
            "HighlightsDocument",
            "SitemapEntryDocument",
            "SitemapResponse",
        ] {
            assert!(
                document["components"]["schemas"].get(schema).is_some(),
                "missing {schema} schema"
            );
        }
        assert!(
            document["paths"]["/packages/{namespace}/{package}/dependents"]["get"]["responses"]
                .get("404")
                .is_some()
        );
        assert_eq!(
            document["components"]["schemas"]["KeywordDocument"]["required"],
            serde_json::json!(["keyword", "normalized_keyword", "package_count"])
        );
    }
}
