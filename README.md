# reqcli

**A terminal-native API tester with YAML collections, runtime variables, JSON extraction, and assertions.**

`reqcli` is a small Rust CLI for sending HTTP requests and running repeatable API checks without a graphical API client. Collections can pass values from one response into the next request, and failed requests or assertions produce a non-zero exit code—useful for scripts and CI.

## Features

- `GET` and `POST` one-off requests from the terminal
- YAML collections with sequential execution
- `{{variable}}` substitution from `config.yaml` and earlier response extractions
- JSON Pointer extraction, such as `/current/temperature_2m`
- Expected-status assertions and readable PASS/FAIL summaries
- 20-second request timeout
- Non-zero exit status when a request, assertion, or extraction fails
- Unit tests for variable substitution, JSON extraction, and HTTP method validation

## Requirements

- Rust and Cargo
- Network access for live HTTP requests

## Quick start

```powershell
cargo run -- get "https://api.open-meteo.com/v1/forecast?latitude=48.8566&longitude=2.3522&current=temperature_2m,relative_humidity_2m,wind_speed_10m"
```

Run the included two-request chain:

```powershell
cargo run -- chain requests.yaml
```

The first request retrieves live weather data from Open-Meteo and extracts its current temperature, humidity, and wind speed. The second sends those extracted values to HTTPBin, demonstrating runtime variable substitution across requests.

Run only one named request:

```powershell
cargo run -- run requests.yaml get_weather
```

Send a POST request:

```powershell
cargo run -- post "https://httpbin.org/post" --body "hello from reqcli"
```

Run checks:

```powershell
cargo fmt
cargo check
cargo test
```

## Collection format

The preferred `requests.yaml` format is an ordered list. Order matters when later requests use variables extracted by earlier requests.

```yaml
requests:
  - name: get_weather
    method: GET
    url: "{{base_url}}/v1/forecast?latitude=48.8566&longitude=2.3522&current=temperature_2m"
    expect_status: 200
    extract:
      temperature_c: "/current/temperature_2m"
```

Each request supports:

| Field | Required | Description |
|---|---|---|
| `name` | Yes | Unique request name |
| `method` | Yes | `GET`, `POST`, `PUT`, `PATCH`, or `DELETE` |
| `url` | Yes | URL; can contain `{{variables}}` |
| `body` | No | Text or JSON request body; can contain variables |
| `headers` | No | Map of request header names to values |
| `expect_status` | No | Expected HTTP status; defaults to any 2xx status |
| `extract` | No | Map of runtime variable names to JSON Pointer paths |

## Configuration

The bundle includes a local `config.yaml` for immediate use and a `config.example.yaml` template. `config.yaml` is ignored by Git so local values are not accidentally committed. `config.yaml` supports variables nested under `variables:`. Simple root-level string variables are also accepted.

```yaml
variables:
  base_url: "https://api.open-meteo.com"
  echo_url: "https://httpbin.org"
```

Do not commit secrets or real API keys. Add private configuration values to a local file and keep them out of public repositories.

## Exit behavior

- Exit code `0`: all requests and assertions passed.
- Non-zero exit code: request error, unsupported method, failed status assertion, missing extraction path, or invalid configuration/collection.

## Current scope

This is a portfolio-sized API testing CLI, not a complete replacement for Postman or a production API gateway. It currently uses a YAML collection, a local `config.yaml`, JSON response extraction, and a fixed 20-second request timeout. It does not yet support authentication helpers, retries, environment profiles, parallel execution, or redacted secret management.

## License

MIT. See [`LICENSE`](LICENSE).
