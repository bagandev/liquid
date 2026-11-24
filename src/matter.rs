#[derive(Debug, Default)]
pub struct ParsedPage {
    pub layout: Option<String>,
    pub body: String,
    pub front_matter: serde_json::Map<String, serde_json::Value>,
}

pub fn extract_layout_and_body(source: &str) -> napi::Result<ParsedPage> {
    let trimmed = source.trim_start();

    // 1. Check for Jekyll-style YAML front matter (--- ... ---)
    if let Some(rest) = trimmed.strip_prefix("---") {
        // Ensure "---" is followed by a newline so horizontal rules aren't misread
        if let Some(after_open) = rest
            .strip_prefix('\n')
            .or_else(|| rest.strip_prefix("\r\n"))
        {
            if let Some(end_idx) = after_open.find("\n---") {
                let yaml_str = &after_open[..end_idx];
                let body_start = end_idx + 4; // skip "\n---"
                let body = after_open[body_start..]
                    .trim_start_matches(['\r', '\n'])
                    .to_string();

                // Parse full YAML (arrays, nested objects, numbers, booleans) into JSON Value
                let parsed_yaml: serde_json::Value =
                    serde_yml::from_str(yaml_str).map_err(|e| {
                        napi::Error::from_reason(format!("Invalid YAML front matter: {}", e))
                    })?;

                let mut front_matter = match parsed_yaml {
                    serde_json::Value::Object(map) => map,
                    serde_json::Value::Null => serde_json::Map::new(),
                    _ => {
                        return Err(napi::Error::from_reason(
                            "YAML front matter must be a key-value mapping",
                        ));
                    }
                };

                // Extract and remove `layout` from `page` variables
                let layout = front_matter
                    .remove("layout")
                    .and_then(|v| v.as_str().map(|s| s.to_string()));

                return Ok(ParsedPage {
                    layout,
                    body,
                    front_matter,
                });
            }
        }
    }

    // 2. Fallback: support {% layout "default" %} tag at the top of a template
    if let Some(rest) = trimmed.strip_prefix("{%") {
        if let Some(end_tag) = rest.find("%}") {
            let tag_inner = rest[..end_tag].trim();
            if let Some(layout_arg) = tag_inner.strip_prefix("layout") {
                let layout_name = layout_arg
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                let body = rest[end_tag + 2..]
                    .trim_start_matches(['\r', '\n'])
                    .to_string();

                return Ok(ParsedPage {
                    layout: if layout_name.is_empty() {
                        None
                    } else {
                        Some(layout_name)
                    },
                    body,
                    front_matter: serde_json::Map::new(),
                });
            }
        }
    }

    Ok(ParsedPage {
        layout: None,
        body: source.to_string(),
        front_matter: serde_json::Map::new(),
    })
}
