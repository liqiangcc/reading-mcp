use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, Location, Section, SectionId};

use super::common::{content_hash, document_id, slugify, title_from_metadata};

#[derive(Default)]
pub struct OpenApiParser;

#[async_trait]
impl Parser for OpenApiParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let text = String::from_utf8(resource.bytes.clone()).map_err(|error| {
            ApplicationError::ParseFailed(format!("OpenAPI document is not UTF-8: {error}"))
        })?;
        let media = resource
            .media_type
            .0
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let value: Value = if media.contains("json") || text.trim_start().starts_with('{') {
            serde_json::from_str(&text).map_err(|error| {
                ApplicationError::ParseFailed(format!("invalid OpenAPI JSON: {error}"))
            })?
        } else {
            serde_saphyr::from_str(&text).map_err(|error| {
                ApplicationError::ParseFailed(format!("invalid OpenAPI YAML: {error}"))
            })?
        };
        let object = value.as_object().ok_or_else(|| {
            ApplicationError::ParseFailed("OpenAPI root must be an object".into())
        })?;
        let specification = object
            .get("openapi")
            .and_then(Value::as_str)
            .map(|version| format!("OpenAPI {version}"))
            .or_else(|| {
                object
                    .get("swagger")
                    .and_then(Value::as_str)
                    .map(|version| format!("Swagger {version}"))
            })
            .ok_or_else(|| {
                ApplicationError::ParseFailed(
                    "JSON/YAML document is not OpenAPI or Swagger (missing openapi/swagger)".into(),
                )
            })?;

        let info = object.get("info").and_then(Value::as_object);
        let title = info
            .and_then(|info| info.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| title_from_metadata(&resource.metadata, &resource.final_source));
        let mut root_sections = Vec::new();
        root_sections.push(overview_section(&specification, info));

        if let Some(paths) = object.get("paths").and_then(Value::as_object) {
            for (path, path_item) in paths {
                root_sections.push(path_section(path, path_item));
            }
        }
        if let Some(schemas) = object
            .get("components")
            .and_then(Value::as_object)
            .and_then(|components| components.get("schemas"))
            .and_then(Value::as_object)
            .or_else(|| object.get("definitions").and_then(Value::as_object))
        {
            root_sections.push(schema_section(schemas));
        }

        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let mut metadata = resource.metadata;
        metadata.insert("api_specification".into(), specification);
        metadata.insert(
            "api_path_count".into(),
            object
                .get("paths")
                .and_then(Value::as_object)
                .map(|paths| paths.len())
                .unwrap_or_default()
                .to_string(),
        );

        Ok(Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
            root_sections,
        })
    }
}

fn overview_section(
    specification: &str,
    info: Option<&serde_json::Map<String, Value>>,
) -> Section {
    let mut lines = vec![specification.to_string()];
    if let Some(info) = info {
        if let Some(version) = info.get("version").and_then(Value::as_str) {
            lines.push(format!("API version: {version}"));
        }
        if let Some(description) = info.get("description").and_then(Value::as_str) {
            lines.push(description.to_string());
        }
    }
    Section {
        id: SectionId("section://overview".into()),
        parent_id: None,
        title: "Overview".into(),
        level: 1,
        content: lines.join("\n\n"),
        location: Location {
            section_path: vec!["Overview".into()],
            native_location: Some("openapi:#/info".into()),
            ..Location::default()
        },
        children: vec![],
    }
}

fn path_section(path: &str, path_item: &Value) -> Section {
    let path_slug = slugify(path);
    let id = SectionId(format!("section://paths/{path_slug}"));
    let operations = path_item
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(method, _)| is_http_method(method))
                .map(|(method, operation)| operation_section(path, method, operation, &id))
                .collect()
        })
        .unwrap_or_default();
    Section {
        id,
        parent_id: None,
        title: path.to_string(),
        level: 1,
        content: path_item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        location: Location {
            section_path: vec![path.to_string()],
            native_location: Some(format!("openapi:#/paths/{}", pointer_escape(path))),
            ..Location::default()
        },
        children: operations,
    }
}

fn operation_section(
    path: &str,
    method: &str,
    operation: &Value,
    parent_id: &SectionId,
) -> Section {
    let method_upper = method.to_ascii_uppercase();
    let title = format!("{method_upper} {path}");
    let mut lines = Vec::new();
    if let Some(summary) = operation.get("summary").and_then(Value::as_str) {
        lines.push(summary.to_string());
    }
    if let Some(description) = operation.get("description").and_then(Value::as_str) {
        lines.push(description.to_string());
    }
    if let Some(operation_id) = operation.get("operationId").and_then(Value::as_str) {
        lines.push(format!("operationId: {operation_id}"));
    }
    if let Some(tags) = operation.get("tags").and_then(Value::as_array) {
        let tags = tags.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        if !tags.is_empty() {
            lines.push(format!("tags: {}", tags.join(", ")));
        }
    }
    if let Some(parameters) = operation.get("parameters").and_then(Value::as_array) {
        for parameter in parameters {
            if let Some(name) = parameter.get("name").and_then(Value::as_str) {
                let location = parameter
                    .get("in")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                lines.push(format!("parameter: {name} ({location})"));
            }
        }
    }
    if let Some(responses) = operation.get("responses").and_then(Value::as_object) {
        for (status, response) in responses {
            let description = response
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            lines.push(format!("response {status}: {description}"));
        }
    }
    Section {
        id: SectionId(format!(
            "{}/{method}",
            parent_id.0.trim_end_matches('/')
        )),
        parent_id: Some(parent_id.clone()),
        title: title.clone(),
        level: 2,
        content: lines.join("\n\n"),
        location: Location {
            section_path: vec![path.to_string(), title],
            native_location: Some(format!(
                "openapi:#/paths/{}/{}",
                pointer_escape(path),
                method.to_ascii_lowercase()
            )),
            ..Location::default()
        },
        children: vec![],
    }
}

fn schema_section(schemas: &serde_json::Map<String, Value>) -> Section {
    let parent_id = SectionId("section://schemas".into());
    let children = schemas
        .iter()
        .map(|(name, schema)| Section {
            id: SectionId(format!("section://schemas/{}", slugify(name))),
            parent_id: Some(parent_id.clone()),
            title: name.clone(),
            level: 2,
            content: serde_json::to_string_pretty(schema).unwrap_or_default(),
            location: Location {
                section_path: vec!["Schemas".into(), name.clone()],
                native_location: Some(format!("openapi:#/components/schemas/{}", pointer_escape(name))),
                ..Location::default()
            },
            children: vec![],
        })
        .collect();
    Section {
        id: parent_id,
        parent_id: None,
        title: "Schemas".into(),
        level: 1,
        content: String::new(),
        location: Location {
            section_path: vec!["Schemas".into()],
            native_location: Some("openapi:#/components/schemas".into()),
            ..Location::default()
        },
        children,
    }
}

fn is_http_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
    )
}

fn pointer_escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::OpenApiParser;
    use crate::application::ports::{Parser, RetrievedResource};
    use crate::domain::{DocumentSource, MediaType};

    #[tokio::test]
    async fn yaml_openapi_creates_operation_sections() {
        let parsed = OpenApiParser
            .parse(RetrievedResource {
                source: DocumentSource("memory:openapi.yaml".into()),
                final_source: DocumentSource("memory:openapi.yaml".into()),
                media_type: MediaType("application/yaml".into()),
                bytes: b"openapi: 3.1.0\ninfo:\n  title: Pet API\npaths:\n  /pets:\n    get:\n      summary: List pets\n      responses:\n        '200':\n          description: ok\n"
                    .to_vec(),
                etag: None,
                last_modified: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("OpenAPI YAML should parse");
        assert_eq!(parsed.title, "Pet API");
        assert!(
            parsed
                .root_sections
                .iter()
                .any(|section| section.title == "/pets")
        );
    }
}
