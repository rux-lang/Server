use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::openapi::content::Content;
use utoipa::openapi::response::ResponseBuilder;
use utoipa::openapi::{Ref, RefOr};
use utoipa::{ToResponse, ToSchema, openapi};

pub const PROBLEM_JSON: &str = "application/problem+json";
const PROBLEM_TYPE_BASE: &str = "https://api.rux-lang.dev/problems";

/// The success envelope used by every public JSON endpoint.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = DataEnvelope)]
pub struct DataEnvelope<T> {
    pub data: T,
}

impl<T> DataEnvelope<T> {
    pub const fn new(data: T) -> Self {
        Self { data }
    }
}

/// A single validation failure within an RFC 9457 problem.
#[must_use]
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ValidationError {
    #[schema(pattern = "^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$")]
    code: String,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(format = "json-pointer")]
    pointer: Option<String>,
}

impl ValidationError {
    /// Creates a validation failure.
    ///
    /// # Panics
    ///
    /// Panics when `code` is not a stable `snake_case` identifier.
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        let code = code.into();
        assert!(
            is_problem_code(&code),
            "validation error code must be snake_case"
        );
        Self {
            code,
            detail: detail.into(),
            pointer: None,
        }
    }

    /// Adds an RFC 6901 JSON Pointer locating the invalid request value.
    ///
    /// # Panics
    ///
    /// Panics when `pointer` is not an RFC 6901 JSON Pointer.
    pub fn with_pointer(mut self, pointer: impl Into<String>) -> Self {
        let pointer = pointer.into();
        assert!(
            is_json_pointer(&pointer),
            "validation error pointer must be an RFC 6901 JSON Pointer"
        );
        self.pointer = Some(pointer);
        self
    }
}

/// RFC 9457 problem details with stable Rux machine-readable extensions.
#[must_use]
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct Problem {
    #[serde(rename = "type")]
    #[schema(format = "uri")]
    r#type: String,
    title: String,
    #[schema(minimum = 400, maximum = 599)]
    status: u16,
    #[schema(pattern = "^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$")]
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(format = "uri-reference")]
    instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(min_items = 1)]
    errors: Option<Vec<ValidationError>>,
}

impl Problem {
    /// Creates a problem with its permanent type URI derived from `code`.
    ///
    /// # Panics
    ///
    /// Panics when `status` is not a 4xx or 5xx status, or when `code` is not a
    /// stable `snake_case` identifier.
    pub fn new(status: StatusCode, code: impl Into<String>, title: impl Into<String>) -> Self {
        assert!(
            status.is_client_error() || status.is_server_error(),
            "problem status must be a 4xx or 5xx status"
        );
        let code = code.into();
        assert!(is_problem_code(&code), "problem code must be snake_case");

        Self {
            r#type: format!("{PROBLEM_TYPE_BASE}/{code}"),
            title: title.into(),
            status: status.as_u16(),
            code,
            detail: None,
            instance: None,
            errors: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    pub fn with_errors(mut self, errors: Vec<ValidationError>) -> Self {
        if !errors.is_empty() {
            self.errors = Some(errors);
        }
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status)
            .expect("problem status is validated when the problem is constructed");
        (status, [(header::CONTENT_TYPE, PROBLEM_JSON)], Json(self)).into_response()
    }
}

pub struct ProblemResponse;

impl<'response> ToResponse<'response> for ProblemResponse {
    fn response() -> (&'response str, RefOr<openapi::response::Response>) {
        (
            "ProblemResponse",
            ResponseBuilder::new()
                .description("RFC 9457 problem details")
                .content(
                    PROBLEM_JSON,
                    Content::new(Some(Ref::from_schema_name("Problem"))),
                )
                .build()
                .into(),
        )
    }
}

fn is_problem_code(value: &str) -> bool {
    let mut segments = value.split('_');
    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty()
        || !first.as_bytes()[0].is_ascii_lowercase()
        || !first
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return false;
    }

    segments.all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn is_json_pointer(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if !value.starts_with('/') {
        return false;
    }

    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'~' && !matches!(bytes.next(), Some(b'0' | b'1')) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::json;

    use super::*;

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    struct ExampleDocument {
        display_name: &'static str,
    }

    #[test]
    fn success_envelopes_have_one_snake_case_data_member() {
        let single = DataEnvelope::new(ExampleDocument {
            display_name: "Rux",
        });
        assert_eq!(
            serde_json::to_value(single).expect("envelope should serialize"),
            json!({"data": {"display_name": "Rux"}})
        );

        let collection = DataEnvelope::new([1, 2, 3]);
        assert_eq!(
            serde_json::to_value(collection).expect("collection envelope should serialize"),
            json!({"data": [1, 2, 3]})
        );
    }

    #[tokio::test]
    async fn problems_use_rfc_media_type_and_matching_status() {
        let response = Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "The request is invalid",
        )
        .with_detail("One or more fields are invalid.")
        .with_instance("/v1/packages/rux/example")
        .with_errors(vec![
            ValidationError::new("invalid_name", "must contain only lowercase letters")
                .with_pointer("/name"),
        ])
        .into_response();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(response.headers()[header::CONTENT_TYPE], PROBLEM_JSON);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("problem body should be readable");
        let document: serde_json::Value =
            serde_json::from_slice(&body).expect("problem should be JSON");
        assert_eq!(document["status"], 422);
        assert_eq!(
            document["type"],
            "https://api.rux-lang.dev/problems/invalid_request"
        );
        assert_eq!(document["code"], "invalid_request");
        assert_eq!(document["errors"][0]["pointer"], "/name");
    }

    #[test]
    fn minimal_problems_omit_occurrence_specific_members() {
        let document = serde_json::to_value(Problem::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Not Found",
        ))
        .expect("problem should serialize");

        assert_eq!(
            document,
            json!({
                "type": "https://api.rux-lang.dev/problems/not_found",
                "title": "Not Found",
                "status": 404,
                "code": "not_found"
            })
        );
    }

    #[test]
    #[should_panic(expected = "problem code must be snake_case")]
    fn problem_codes_reject_non_snake_case_values() {
        let _ = Problem::new(StatusCode::BAD_REQUEST, "InvalidRequest", "Invalid request");
    }

    #[test]
    fn empty_validation_error_lists_are_omitted() {
        let document = serde_json::to_value(
            Problem::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid request",
            )
            .with_errors(Vec::new()),
        )
        .expect("problem should serialize");

        assert!(document.get("errors").is_none());
    }

    #[test]
    fn json_pointers_accept_only_rfc_6901_escapes() {
        let document = serde_json::to_value(
            ValidationError::new("invalid_name", "invalid name")
                .with_pointer("/package/a~1b/~0name"),
        )
        .expect("validation error should serialize");

        assert_eq!(document["pointer"], "/package/a~1b/~0name");
    }

    #[test]
    #[should_panic(expected = "validation error pointer must be an RFC 6901 JSON Pointer")]
    fn json_pointers_reject_invalid_escapes() {
        let _ = ValidationError::new("invalid_name", "invalid name").with_pointer("/name~2");
    }
}
