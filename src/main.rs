use clap::{Parser, Subcommand};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

type AppResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Parser)]
#[command(
    name = "reqcli",
    version,
    about = "A terminal-native API tester with YAML collections and request chaining",
    long_about = "Send HTTP requests, run named YAML requests, extract JSON values into variables, and fail builds when assertions fail."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Send a GET request to a URL.
    Get {
        /// Full request URL.
        url: String,
    },
    /// Send a POST request to a URL.
    Post {
        /// Full request URL.
        url: String,
        /// Optional text or JSON request body.
        #[arg(long)]
        body: Option<String>,
    },
    /// Run one named request from a YAML collection.
    Run {
        /// YAML collection path (for example, requests.yaml).
        collection: PathBuf,
        /// Request name as declared in the collection.
        request_name: String,
    },
    /// Run every request in a YAML collection in declaration order.
    Chain {
        /// YAML collection path (for example, requests.yaml).
        collection: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct Collection {
    requests: RequestSet,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RequestSet {
    /// Preferred format: a list preserves request order for chaining.
    List(Vec<NamedRequest>),
    /// Also accepts a name-to-request mapping for simple collections.
    Map(BTreeMap<String, RequestSpec>),
}

#[derive(Debug, Deserialize)]
struct NamedRequest {
    name: String,
    #[serde(flatten)]
    spec: RequestSpec,
}

#[derive(Debug, Deserialize, Clone)]
struct RequestSpec {
    method: String,
    url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    expect_status: Option<u16>,
    /// Maps runtime variable names to JSON Pointer paths, e.g. "/current/temperature_2m".
    #[serde(default)]
    extract: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    variables: HashMap<String, String>,
    /// Root-level variables are accepted too, for simple config files.
    #[serde(flatten)]
    extra: HashMap<String, String>,
}

#[derive(Debug)]
struct RequestOutcome {
    name: String,
    passed: bool,
}

fn main() -> ExitCode {
    match dispatch(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("ERROR: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> AppResult<bool> {
    match cli.command {
        Commands::Get { url } => send_direct("GET", &url, None),
        Commands::Post { url, body } => send_direct("POST", &url, body.as_deref()),
        Commands::Run {
            collection,
            request_name,
        } => run_one(&collection, &request_name),
        Commands::Chain { collection } => run_chain(&collection),
    }
}

fn send_direct(method_text: &str, url: &str, body: Option<&str>) -> AppResult<bool> {
    let method = parse_method(method_text)?;
    let request = Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("reqcli/", env!("CARGO_PKG_VERSION")))
        .build()?
        .request(method, url);
    let request = if let Some(body) = body {
        request.body(body.to_owned())
    } else {
        request
    };

    println!("Request: {method_text} {url}");
    match send(request) {
        Ok((status, response_body)) => {
            print!("Status: {status}\nBody: ");
            print_body(&response_body);
            println!("\nResponse received with status {status}");
            Ok((200..300).contains(&status))
        }
        Err(error) => Err(error.into()),
    }
}

fn run_one(collection_path: &Path, request_name: &str) -> AppResult<bool> {
    let mut requests = load_requests(collection_path)?;
    let position = requests.iter().position(|(name, _)| name == request_name);
    let Some(position) = position else {
        let names = requests
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("Request '{request_name}' was not found. Available requests: {names}").into());
    };

    let (name, spec) = requests.remove(position);
    let mut variables = load_variables()?;
    let outcome = execute_named_request(&name, &spec, &mut variables);
    let passed = outcome.passed;
    print_summary(&[outcome]);
    Ok(passed)
}

fn run_chain(collection_path: &Path) -> AppResult<bool> {
    let requests = load_requests(collection_path)?;
    if requests.is_empty() {
        return Err("The collection contains no requests.".into());
    }

    let mut variables = load_variables()?;
    let mut outcomes = Vec::with_capacity(requests.len());
    for (name, spec) in requests {
        outcomes.push(execute_named_request(&name, &spec, &mut variables));
    }

    print_summary(&outcomes);
    Ok(outcomes.iter().all(|outcome| outcome.passed))
}

fn execute_named_request(
    name: &str,
    spec: &RequestSpec,
    variables: &mut HashMap<String, String>,
) -> RequestOutcome {
    println!("\n=== Running: {name} ===");

    let result = execute_spec(spec, variables);
    match result {
        Ok((status, body, mut passed)) => {
            println!("Status: {status}");
            print!("Body: ");
            print_body(&body);
            println!();

            if let Some(expected) = spec.expect_status {
                passed = status == expected;
                if passed {
                    println!("Assertion: PASS (expected HTTP {expected})");
                } else {
                    println!("Assertion: FAIL (expected HTTP {expected}, received {status})");
                }
            } else if (200..300).contains(&status) {
                println!("Assertion: PASS (HTTP success status)");
            } else {
                println!("Assertion: FAIL (HTTP status {status} is not successful)");
                passed = false;
            }

            if !spec.extract.is_empty() {
                match serde_json::from_str::<Value>(&body) {
                    Ok(json) => {
                        for (variable, pointer) in &spec.extract {
                            match json.pointer(pointer).and_then(json_value_to_string) {
                                Some(value) => {
                                    println!("Extracted: {variable} = {value}");
                                    variables.insert(variable.clone(), value);
                                }
                                None => {
                                    eprintln!(
                                        "Extraction FAIL: variable '{variable}' could not be read from JSON pointer '{pointer}'"
                                    );
                                    passed = false;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!("Extraction FAIL: response body is not valid JSON ({error})");
                        passed = false;
                    }
                }
            }

            RequestOutcome {
                name: name.to_owned(),
                passed,
            }
        }
        Err(error) => {
            eprintln!("REQUEST ERROR: {error}");
            RequestOutcome {
                name: name.to_owned(),
                passed: false,
            }
        }
    }
}

fn execute_spec(
    spec: &RequestSpec,
    variables: &HashMap<String, String>,
) -> Result<(u16, String, bool), String> {
    let method = parse_method(&spec.method).map_err(|error| error.to_string())?;
    let url = render_template(&spec.url, variables)?;
    let body = spec
        .body
        .as_deref()
        .map(|value| render_template(value, variables))
        .transpose()?;

    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("reqcli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Could not create HTTP client: {error}"))?;
    let mut request = client.request(method, &url);

    for (name, value) in &spec.headers {
        let rendered_name = render_template(name, variables)?;
        let rendered_value = render_template(value, variables)?;
        let header_name = HeaderName::from_bytes(rendered_name.as_bytes())
            .map_err(|error| format!("Invalid header name '{rendered_name}': {error}"))?;
        let header_value = HeaderValue::from_str(&rendered_value)
            .map_err(|error| format!("Invalid value for header '{rendered_name}': {error}"))?;
        request = request.header(header_name, header_value);
    }

    if let Some(body) = body {
        request = request.body(body);
    }

    println!("{} {}", spec.method.to_uppercase(), url);
    let (status, response_body) = send(request).map_err(|error| error.to_string())?;
    let default_pass = (200..300).contains(&status);
    Ok((status, response_body, default_pass))
}

fn send(request: RequestBuilder) -> Result<(u16, String), reqwest::Error> {
    let response = request.send()?;
    let status = response.status().as_u16();
    let body = response.text()?;
    Ok((status, body))
}

fn parse_method(method: &str) -> AppResult<Method> {
    match method.trim().to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        other => Err(format!("Unsupported HTTP method: {other}. Supported methods: GET, POST, PUT, PATCH, DELETE").into()),
    }
}

fn load_requests(path: &Path) -> AppResult<Vec<(String, RequestSpec)>> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("Could not read collection '{}': {error}", path.display()))?;
    let collection: Collection = serde_yaml::from_str(&source)
        .map_err(|error| format!("Could not parse collection '{}': {error}", path.display()))?;

    let requests: Vec<(String, RequestSpec)> = match collection.requests {
        RequestSet::List(list) => list
            .into_iter()
            .map(|request| (request.name, request.spec))
            .collect(),
        RequestSet::Map(map) => map.into_iter().collect(),
    };

    let mut seen = std::collections::HashSet::new();
    for (name, _) in &requests {
        if name.trim().is_empty() {
            return Err("Request names must not be empty.".into());
        }
        if !seen.insert(name.clone()) {
            return Err(format!("Duplicate request name '{name}'.").into());
        }
    }
    Ok(requests)
}

fn load_variables() -> AppResult<HashMap<String, String>> {
    let path = Path::new("config.yaml");
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let source = fs::read_to_string(path)
        .map_err(|error| format!("Could not read config '{}': {error}", path.display()))?;
    let config: RawConfig = serde_yaml::from_str(&source)
        .map_err(|error| format!("Could not parse config '{}': {error}", path.display()))?;
    let mut variables = config.extra;
    variables.extend(config.variables);
    Ok(variables)
}

fn render_template(input: &str, variables: &HashMap<String, String>) -> Result<String, String> {
    let mut rendered = String::with_capacity(input.len());
    let mut remaining = input;

    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let after_open = &remaining[start + 2..];
        let Some(end) = after_open.find("}}") else {
            return Err(format!("Malformed template variable in '{input}'"));
        };
        let key = after_open[..end].trim();
        if key.is_empty() {
            return Err(format!("Empty template variable in '{input}'"));
        }
        let value = variables
            .get(key)
            .ok_or_else(|| format!("Unresolved variable '{{{{{key}}}}}' in '{input}'"))?;
        rendered.push_str(value);
        remaining = &after_open[end + 2..];
    }

    rendered.push_str(remaining);
    Ok(rendered)
}

fn json_value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => Some(value.to_string()),
    }
}

fn print_body(body: &str) {
    match serde_json::from_str::<Value>(body) {
        Ok(json) => match serde_json::to_string_pretty(&json) {
            Ok(pretty) => println!("{pretty}"),
            Err(_) => println!("{body}"),
        },
        Err(_) => println!("{body}"),
    }
}

fn print_summary(outcomes: &[RequestOutcome]) {
    let passed = outcomes.iter().filter(|outcome| outcome.passed).count();
    let failed = outcomes.len() - passed;
    println!("\n=== Collection Summary ===");
    for outcome in outcomes {
        println!(
            "{} {}",
            if outcome.passed { "PASS" } else { "FAIL" },
            outcome.name
        );
    }
    println!("Total: {} | Passed: {passed} | Failed: {failed}", outcomes.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_config_and_runtime_variables() {
        let variables = HashMap::from([
            ("base_url".to_owned(), "https://example.com".to_owned()),
            ("city".to_owned(), "Paris".to_owned()),
        ]);
        assert_eq!(
            render_template("{{base_url}}/weather?city={{city}}", &variables).unwrap(),
            "https://example.com/weather?city=Paris"
        );
    }

    #[test]
    fn rejects_unresolved_variables() {
        let variables = HashMap::new();
        assert!(render_template("https://example.com/{{missing}}", &variables).is_err());
    }

    #[test]
    fn reads_nested_json_pointer() {
        let json: Value = serde_json::from_str(r#"{"current":{"temperature_2m":17.7}}"#).unwrap();
        assert_eq!(
            json.pointer("/current/temperature_2m").and_then(json_value_to_string),
            Some("17.7".to_owned())
        );
    }

    #[test]
    fn supports_http_method_parsing_case_insensitively() {
        assert_eq!(parse_method("post").unwrap(), Method::POST);
        assert!(parse_method("TRACE").is_err());
    }
}
